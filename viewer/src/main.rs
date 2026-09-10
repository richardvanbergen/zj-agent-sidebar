//! The "UI" half of the split — an ordinary terminal program. Not a Zellij
//! plugin, not wasm, no plugin permissions of any kind. It only:
//!   1. on launch, makes sure `watcher` is actually running (`ensure_watcher_running`),
//!   2. polls `watcher`'s JSON file and renders it,
//!   3. on Enter, shells out to `zellij action go-to-tab` / `focus-pane-id`.
//!
//! `watcher` is never auto-loaded at session start — it only ever exists
//! because a user opened `viewer`, which is the explicit action that implies
//! they want it. This is the thing this whole spike exists to prove: that
//! the "UI" can be a completely separate process from the thing that
//! watches Zellij state, and can own that thing's entire lifecycle too.

use crossterm::cursor;
use crossterm::event::{self, Event as CEvent, KeyCode};
use crossterm::terminal;
use crossterm::{execute, queue};
use shared::{Row, Snapshot, Status, PING_PIPE_NAME, SESSION_NAME_KEY};
use std::io::{stdout, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use terminal_colorsaurus::{color_palette, QueryOptions};

struct Theme {
    active_bg: String,
    bright_fg: String,
}

fn detect_theme() -> Option<Theme> {
    let palette = color_palette(QueryOptions::default()).ok()?;
    let lum = |r: u16, g: u16, b: u16| r as u32 + g as u32 + b as u32;
    let is_dark = lum(palette.background.r, palette.background.g, palette.background.b)
        <= lum(palette.foreground.r, palette.foreground.g, palette.foreground.b);
    let mix = |a: u16, b: u16, t: f32| -> u8 {
        let a = (a >> 8) as f32;
        let b = (b >> 8) as f32;
        (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
    };
    let active_bg = format!(
        "\x1b[48;2;{};{};{}m",
        mix(palette.background.r, palette.foreground.r, 0.18),
        mix(palette.background.g, palette.foreground.g, 0.18),
        mix(palette.background.b, palette.foreground.b, 0.18),
    );
    let extreme = if is_dark { 65535 } else { 0 };
    let bright_fg = format!(
        "\x1b[38;2;{};{};{}m",
        mix(palette.foreground.r, extreme, 0.45),
        mix(palette.foreground.g, extreme, 0.45),
        mix(palette.foreground.b, extreme, 0.45),
    );
    Some(Theme { active_bg, bright_fg })
}

/// `viewer` always runs as a Zellij-spawned pane, so `$ZELLIJ_SESSION_NAME`
/// is set directly in its environment — no subprocess, no plugin API needed
/// to learn this, unlike `watcher`. Falls back to "default" so running
/// `viewer` by hand outside any session still does something sane.
fn session_name() -> String {
    std::env::var("ZELLIJ_SESSION_NAME").unwrap_or_else(|_| "default".to_string())
}

/// Mirrors Zellij's own `ZELLIJ_TMP_DIR` (`zellij-utils/src/consts.rs`):
/// `std::env::temp_dir().join(format!("zellij-{uid}"))`. The watcher plugin
/// writes into its WASI-mounted `/tmp`, which Zellij maps onto exactly this
/// host directory — not the literal `/tmp`. If this drifts from Zellij's own
/// computation, the two processes silently stop agreeing on a path. The
/// session-scoped tail (`zj-agent-state/<session>/state.json`) comes from
/// `shared::state_path` so both sides only define that shape once.
fn state_path(session: &str) -> std::path::PathBuf {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "0".to_string());
    let rel = shared::state_path(session);
    let rel = rel.strip_prefix("/tmp/").unwrap_or(&rel);
    std::env::temp_dir()
        .join(format!("zellij-{uid}"))
        .join(rel)
}

fn read_snapshot(path: &std::path::Path) -> Option<Snapshot> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn current_generation(path: &std::path::Path) -> u64 {
    read_snapshot(path).map(|s| s.generation).unwrap_or(0)
}

/// The only place this program ever starts another program. Called once at
/// launch, before anything is rendered: probe for a live `watcher` by asking
/// it to bump `generation` and watching for that; if nothing answers within
/// the window, spawn one. The lockfile holds an advisory flock (fd-lock),
/// released by the kernel the instant the owning process dies, so a stale
/// lock is impossible — two `viewer`s launched at nearly the same moment
/// (the sidebar in one tab, a floating one you just opened in another)
/// can't both decide to spawn. This is a personal single-user tool; the
/// lock isn't guarding against real concurrent writers, only double-spawn.
fn ensure_watcher_running(path: &std::path::Path) {
    let before = current_generation(path);
    let _ = Command::new("zellij")
        .args(["pipe", "--name", PING_PIPE_NAME, "--", "ping"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if current_generation(path) != before {
            return; // something answered — watcher is alive
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let lock_path = path
        .parent()
        .map(|p| p.join("spawn.lock"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/zj-agent-state-spawn.lock"));
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Advisory lock, not an existence check: a `create_new` lockfile left
    // behind by a crashed viewer would block every future spawn forever.
    // An flock is released by the kernel the instant the owning process
    // dies, so a stale lock is impossible.
    let Ok(lock_file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    else {
        return; // couldn't even open the lock file — don't risk double-spawn
    };
    let mut lock = fd_lock::RwLock::new(lock_file);
    let Ok(_guard) = lock.try_write() else {
        return; // another viewer holds the lock and is already spawning
    };

    let url = format!("file:{}", shared::watcher_wasm_host_path());
    // `zellij plugin -c` takes ONE comma-separated `key=value,key=value`
    // string (`PluginUserConfiguration::from_str`, zellij-utils/src/input/layout.rs)
    // — not repeatable flags.
    let config = format!(
        "zj_agent_state_self_url={url},{SESSION_NAME_KEY}={}",
        session_name()
    );
    // `--skip-plugin-cache`: Zellij caches compiled plugins by file path,
    // not content hash (zj-herd hit this too — see its README). Without
    // this, reinstalling a rebuilt watcher.wasm in place would silently
    // keep serving the stale compiled version forever. The plugin is tiny
    // (~5ms to compile per Zellij's own logs), so always paying that cost
    // is cheap insurance against an entire class of "why isn't this
    // updating" confusion.
    let _ = Command::new("zellij")
        .args(["plugin", "--skip-plugin-cache", "-c", &config, "--", &url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

struct TabGroup {
    tab_name: String,
    rows: Vec<Row>,
}

fn grouped_rows(rows: Vec<Row>) -> Vec<TabGroup> {
    let mut map: std::collections::BTreeMap<usize, TabGroup> = std::collections::BTreeMap::new();
    for row in rows {
        map.entry(row.tab_position)
            .or_insert_with(|| TabGroup { tab_name: row.tab_name.clone(), rows: Vec::new() })
            .rows
            .push(row);
    }
    let mut groups: Vec<TabGroup> = map.into_values().collect();
    for g in &mut groups {
        g.rows.sort_by_key(|r| (r.status.severity(), r.pane_id));
    }
    groups.sort_by_key(|g| g.rows.iter().map(|r| r.status.severity()).min().unwrap_or(5));
    groups
}

fn flat_rows(groups: &[TabGroup]) -> Vec<&Row> {
    groups.iter().flat_map(|g| g.rows.iter()).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn jump_to(row: &Row) {
    jump_to_pane(&PaneHit {
        id: row.pane_id,
        tab_position: row.tab_position,
        focused: false,
    });
}

const DIM: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const FG_RESET: &str = "\x1b[39m";
const ACCENT: &str = "\x1b[35m";

const SPIN_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spin_glyph(frame: usize) -> char {
    SPIN_FRAMES[frame % SPIN_FRAMES.len()]
}

fn status_glyph(status: Status, frame: usize) -> char {
    if status == Status::Working {
        spin_glyph(frame)
    } else {
        status.glyph().chars().next().unwrap_or('?')
    }
}

fn render(out: &mut impl Write, groups: &[TabGroup], selected: usize, found: bool, frame: usize, theme: Option<&Theme>) {
    let width = terminal::size().map(|(c, _)| c as usize).unwrap_or(60);
    let _ = queue!(out, cursor::MoveTo(0, 0), terminal::Clear(terminal::ClearType::All));
    let _ = writeln!(out, "{BOLD}AGENTS{RESET}{DIM}{:>w$}{RESET}\r", "·1", w = width.saturating_sub(6));

    if !found {
        let _ = writeln!(out, "{DIM}starting watcher…{RESET}\r");
    } else if groups.is_empty() {
        let _ = writeln!(out, "{DIM}no agents{RESET}\r");
    } else {
        let mut idx = 0usize;
        for group in groups {
            let is_current_tab = group.rows.iter().any(|r| r.active);
            let spine = if is_current_tab { format!("{ACCENT}\u{258C}{RESET}") } else { " ".to_string() };
            let head = group.rows.iter().min_by_key(|r| r.status.severity()).map(|r| r.status).unwrap();
            let bold = if head.severity() < 3 { BOLD } else { "" };
            let _ = writeln!(
                out,
                "{spine}{}{}{bold} {}{RESET}\r",
                head.ansi(),
                status_glyph(head, frame),
                truncate(&group.tab_name, width.saturating_sub(4)),
            );
            for (i, row) in group.rows.iter().enumerate() {
                let branch = if i + 1 == group.rows.len() { "\u{2514}" } else { "\u{251C}" };
                let detail = row.message.as_deref().unwrap_or(&row.pane_title);
                let budget = width.saturating_sub(5);
                let detail = truncate(detail, budget);
                let plain_len = 5 + detail.chars().count();
                let is_cursor = idx == selected;
                let is_active = row.active;
                let text_style = match (is_active || is_cursor, theme) {
                    (true, Some(t)) => format!("{BOLD}{}", t.bright_fg),
                    (true, None) => BOLD.to_string(),
                    (false, _) => FG_RESET.to_string(),
                };
                let line = format!(
                    "{DIM}{branch}{FG_RESET} {}{}{FG_RESET} {text_style}{}",
                    row.status.ansi(),
                    status_glyph(row.status, frame),
                    detail,
                );
                if is_active {
                    let pad = " ".repeat(width.saturating_sub(plain_len));
                    match theme {
                        Some(t) => { let _ = writeln!(out, "{spine}{}{line}{pad}{RESET}\r", t.active_bg); }
                        None => { let _ = writeln!(out, "{spine}{BOLD}{line}{pad}{RESET}\r"); }
                    }
                } else {
                    let _ = writeln!(out, "{spine}{line}{RESET}\r");
                }
                idx += 1;
            }
            let _ = writeln!(out, "\r");
        }
    }

    let flat = flat_rows(groups);
    let working = flat.iter().filter(|r| r.status == Status::Working).count();
    let need_you = flat.iter().filter(|r| r.status.needs_you()).count();
    if groups.is_empty() {
        let _ = writeln!(out, "\r");
    }
    if need_you > 0 {
        let _ = writeln!(out, "{DIM}{working} working ·{RESET} {BOLD}\x1b[91m{need_you} need you{RESET}\r");
    } else {
        let _ = writeln!(out, "{DIM}{working} working{RESET}\r");
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<Row> {
        vec![
            Row { pane_id: 0, status: Status::Working, agent: "claude".into(), message: Some("refactor the render loop".into()), pane_title: "pane0".into(), tab_name: "Code".into(), tab_position: 0, active: true },
            Row { pane_id: 5, status: Status::Blocked, agent: "claude".into(), message: Some("needs approval to run migration".into()), pane_title: "pane5".into(), tab_name: "pinky".into(), tab_position: 1, active: false },
            Row { pane_id: 6, status: Status::Done, agent: "claude".into(), message: Some("tests passing".into()), pane_title: "pane6".into(), tab_name: "pinky".into(), tab_position: 1, active: false },
            Row { pane_id: 9, status: Status::Idle, agent: "claude".into(), message: None, pane_title: "pane9".into(), tab_name: "infra".into(), tab_position: 2, active: false },
        ]
    }

    #[test]
    fn render_prints_radar_style() {
        let groups = grouped_rows(sample_rows());
        let mut buf = Vec::new();
        render(&mut buf, &groups, 0, true, 0, None);
        let raw = String::from_utf8(buf).unwrap();
        let stripped: String = strip_ansi(&raw);
        println!("{stripped}");
        assert!(stripped.contains("pinky"));
        assert!(stripped.contains("\u{2514}"));
        assert!(stripped.contains("1 working"));
        assert!(stripped.contains("1 need you"));
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

const PANE_TITLE: &str = "zj-agents";

fn own_pane_id() -> Option<u32> {
    std::env::var("ZELLIJ_PANE_ID").ok()?.parse().ok()
}

/// A pane currently running `viewer`, located anywhere in the session.
struct PaneHit {
    id: u32,
    tab_position: usize,
    focused: bool,
}

fn agent_panes() -> Vec<PaneHit> {
    let Some(output) = Command::new("zellij")
        .args(["action", "list-panes", "--all", "--json"])
        .output()
        .ok()
    else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let Some(panes) = value.as_array() else {
        return Vec::new();
    };
    panes
        .iter()
        .filter_map(|pane| {
            let title = pane.get("title")?.as_str()?;
            if title != PANE_TITLE {
                return None;
            }
            let exited = pane.get("exited").and_then(|v| v.as_bool()).unwrap_or(true);
            if exited {
                return None;
            }
            Some(PaneHit {
                id: pane.get("id")?.as_u64()? as u32,
                tab_position: pane.get("tab_position")?.as_u64()? as usize,
                focused: pane.get("is_focused").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

fn current_tab_position() -> Option<usize> {
    let output = Command::new("zellij")
        .args(["action", "current-tab-info", "--json"])
        .output()
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    value.get("position").and_then(|v| v.as_u64()).map(|v| v as usize)
}

/// `go-to-tab` first: Zellij's own `focus-pane-id` does not switch tabs
/// (zj-herd hit the same gap on `focus_pane_with_id` inside the plugin API —
/// the CLI action shares the underlying limitation).
fn jump_to_pane(hit: &PaneHit) {
    let _ = Command::new("zellij")
        .args(["action", "go-to-tab", &(hit.tab_position + 1).to_string()])
        .status();
    let _ = Command::new("zellij")
        .args(["action", "focus-pane-id", &hit.id.to_string()])
        .status();
}

/// Turn this pane (opened tiled by the keybind's `Run`, or floating via the
/// Alt+G spawn path) into a pinned right-hand overlay: float it if it's
/// tiled, then set its coordinates — `--pinned true` keeps it on top even
/// when unfocused. Coordinates are explicit so a pane that was tiled
/// full-size doesn't float at fullscreen size.
fn present_as_overlay(own: Option<u32>, already_floating: bool) {
    let Some(id) = own else { return };
    let id = id.to_string();
    if !already_floating {
        let _ = Command::new("zellij")
            .args(["action", "toggle-pane-embed-or-floating", "-p", &id])
            .status();
    }
    let _ = Command::new("zellij")
        .args([
            "action",
            "change-floating-pane-coordinates",
            "-p",
            &id,
            "--width",
            "40%",
            "--height",
            "100%",
            "--x",
            "60%",
            "--y",
            "0",
            "--pinned",
            "true",
        ])
        .status();
}

/// `--jump`: if the agents pane exists anywhere in the session, jump to its
/// tab and focus it; if it doesn't, spawn one (floating) in the current tab.
fn run_jump() -> bool {
    let panes = agent_panes();
    if let Some(hit) = panes.first() {
        jump_to_pane(hit);
        return true;
    }
    // Not open yet: spawn it. `--floating` tells the spawned instance it is
    // already a floating pane so it pins itself instead of trying to float.
    let exe = std::env::current_exe().unwrap_or_else(|_| "viewer".into());
    let _ = Command::new("zellij")
        .args(["action", "new-pane", "--floating", "--close-on-exit", "--name", PANE_TITLE, "--"])
        .arg(&exe)
        .arg("--floating")
        .status();
    true
}

/// Default (no args): the Alt+A toggle, aware of where the pane lives.
/// - pane focused on us → close it
/// - pane in this tab, not focused → focus it
/// - pane in another tab → jump there (never kill it from afar — that was
///   the old behaviour that made Alt+A a trap)
/// - no pane anywhere → fall through and open one here
fn run_toggle() -> bool {
    let own = own_pane_id();
    let cur_tab = current_tab_position();
    let panes = agent_panes();
    let here = panes
        .iter()
        .find(|p| Some(p.tab_position) == cur_tab)
        .or_else(|| panes.iter().find(|p| Some(p.id) == own));
    if let Some(hit) = here {
        if own == Some(hit.id) && hit.focused {
            let _ = Command::new("zellij")
                .args(["action", "close-pane", "-p", &hit.id.to_string()])
                .status();
        } else {
            jump_to_pane(hit);
        }
        return true;
    }
    if let Some(hit) = panes.first() {
        jump_to_pane(hit);
        return true;
    }
    false
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--floating` is only ever set by `run_jump`'s spawn: that instance must
    // run the UI directly. It can't go through `run_toggle` — the spawn is
    // created with `--name zj-agents`, so the toggle would find itself and
    // instantly close it.
    let already_floating = args.iter().any(|a| a == "--floating");
    let handled = if args.iter().any(|a| a == "--jump") {
        run_jump()
    } else if already_floating {
        false
    } else {
        run_toggle()
    };
    if handled {
        return Ok(());
    }

    let path = state_path(&session_name());
    let theme = detect_theme();
    ensure_watcher_running(&path);
    let _ = Command::new("zellij")
        .args(["action", "rename-pane", PANE_TITLE])
        .status();
    present_as_overlay(own_pane_id(), already_floating);
    let mut stdout = stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

    let mut selected = 0usize;
    let mut frame = 0usize;
    let result = (|| -> std::io::Result<()> {
        loop {
            let snapshot = read_snapshot(&path);
            let found = snapshot.is_some();
            let groups = grouped_rows(snapshot.map(|s| s.rows).unwrap_or_default());
            let count = flat_rows(&groups).len();
            if selected >= count {
                selected = count.saturating_sub(1);
            }
            render(&mut stdout, &groups, selected, found, frame, theme.as_ref());
            frame = frame.wrapping_add(1);

            if event::poll(Duration::from_millis(300))? {
                if let CEvent::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if count > 0 {
                                selected = (selected + 1).min(count - 1);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                        KeyCode::Enter | KeyCode::Char('l') => {
                            if let Some(row) = flat_rows(&groups).get(selected) {
                                jump_to(row);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    })();

    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    result
}
