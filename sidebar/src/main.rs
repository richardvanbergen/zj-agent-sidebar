//! The always-visible sidebar — the zj-radar mechanism, our own rendering.
//! Zellij delivers `PaneUpdate`/`TabUpdate` only to plugin instances in the
//! *active* tab, so no single pane can watch the whole session. The one
//! mechanism Zellij gives for "in every tab, forever" is the layout's tab
//! templates (where Zellij's own tab-bar/status-bar live): one plugin pane
//! in the template means one instance of *this* plugin per tab, declared by
//! the user in their layout — never auto-loaded, never self-replicating.
//! See INSTALL.md for the template snippet.
//!
//! Each instance joins pushed agent status to Zellij's pane/tab geometry
//! itself (`shared::JoinState`, the same join `watcher` runs) and renders
//! it as a plain pane: status glyph, agent, message. No file, no chime —
//! the background `watcher` owns the file (that's what `viewer` reads),
//! and this pane is a pure view of the same event stream.
//!
//! Interaction:
//! - `hide_self()` / `show_self()` on the `toggle` pipe — the pane stays
//!   loaded (it keeps receiving events) and tracks its own visibility via
//!   the `Visible` event, so toggle is always correct no matter how it was
//!   hidden last.
//! - When focused, Up/Down move the selection and Enter jumps to that
//!   agent's pane (`switch_tab_to` + `show_pane_with_id`). Key events are
//!   only ever delivered to the focused instance, so per-instance selection
//!   never races across tabs.
//! - The keybind `jump` (Alt+G) is broadcast to *every* instance, so it
//!   deliberately ignores per-instance selection: every instance computes
//!   the same top-severity row and issues the identical jump — consistent
//!   last-writer-wins, no cross-tab coordination needed.
//! - Shared state: status pushes and pane/tab events are delivered
//!   selectively (a fresh instance starts from nothing), so every instance
//!   also reads `watcher`'s session state file — on load and again every
//!   `REFRESH_SECS` via a timer — and seeds its join state from it. All
//!   instances therefore converge on the same global state within seconds,
//!   regardless of delivery quirks. If the state file doesn't exist, the
//!   instance spawns the background `watcher` itself (via the
//!   `zj_agent_state_watcher_url` config key from the layout), so the
//!   sidebar is self-sufficient: no viewer ever needed.
//!
//! Active-tab-only delivery means an instance in a background tab holds
//! stale geometry until its tab is activated (then PaneUpdate/TabUpdate
//! refresh it immediately). That's inherent to the layout-template approach
//! and matches zj-radar's behaviour.

use shared::{JoinState, Row, Status, PING_PIPE_NAME, SESSION_NAME_KEY, SIDEBAR_PIPE_NAME, STATUS_PIPE_NAME};
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

/// Config key on the sidebar plugin (set in the layout) giving the `file:`
/// URL of `watcher.wasm`, so an instance that finds no state file can spawn
/// the watcher itself.
const WATCHER_URL_KEY: &str = "zj_agent_state_watcher_url";
/// How often each instance re-reads the shared state file.
const REFRESH_SECS: f64 = 2.0;

const DIM: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const FG_RESET: &str = "\x1b[39m";
/// One leading space so text never sits flush against the pane border.
const PAD: &str = "  ";

#[derive(Default)]
struct Sidebar {
    state: JoinState,
    selected: usize,
    /// Mirror of the pane's real visibility, confirmed by `Visible` events —
    /// never assumed, so `toggle` is right even if something else hid us.
    hidden: bool,
    /// `file:` URL of watcher.wasm from the layout config, if given.
    watcher_url: Option<String>,
    /// Whether we found (or spawned a watcher that will create) the state
    /// file — gates the one-time watcher spawn.
    has_state_file: bool,
}

impl Sidebar {
    fn session_name(&self) -> String {
        get_session_environment_variables()
            .get("ZELLIJ_SESSION_NAME")
            .cloned()
            .unwrap_or_else(|| "default".to_string())
    }

    /// Read `watcher`'s state file and merge it into the join state. The
    /// WASI `/tmp` is the same mount `watcher` writes to, so this is the
    /// shared ground truth between all instances of the session.
    fn refresh_from_file(&mut self) {
        let session = self.session_name();
        let Ok(raw) = std::fs::read_to_string(shared::state_path(&session)) else {
            return;
        };
        let Ok(snapshot) = serde_json::from_str::<shared::Snapshot>(&raw) else {
            return;
        };
        self.state.seed_from_rows(snapshot.rows);
    }

    fn ensure_watcher(&mut self) {
        if self.has_state_file {
            return;
        }
        if let Some(url) = self.watcher_url.clone() {
            let mut config = BTreeMap::new();
            config.insert(SESSION_NAME_KEY.to_string(), self.session_name());
            load_new_plugin(&url, config, true, false);
            self.has_state_file = true; // don't respawn on every render path
        }
    }

    /// Flat row list in render order, so selection and rendering agree.
    fn flat(&self) -> Vec<Row> {
        shared::grouped_rows(self.state.rows())
            .iter()
            .flat_map(|g| g.rows.iter())
            .cloned()
            .collect()
    }

    fn jump_to_row(&self, row: &Row) {
        switch_tab_to((row.tab_position + 1) as u32);
        show_pane_with_id(PaneId::Terminal(row.pane_id), false, true);
    }

    fn jump_to_selected(&self) {
        let rows = self.flat();
        if rows.is_empty() {
            return;
        }
        let selected = self.selected.min(rows.len() - 1);
        self.jump_to_row(&rows[selected]);
    }
}

register_plugin!(Sidebar);

impl ZellijPlugin for Sidebar {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.watcher_url = configuration.get(WATCHER_URL_KEY).cloned();
        self.has_state_file = std::fs::read_to_string(shared::state_path(&self.session_name())).is_ok();
        if !self.has_state_file {
            self.ensure_watcher();
        }
        self.refresh_from_file();
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[EventType::PaneUpdate, EventType::TabUpdate, EventType::Key, EventType::Timer]);
        set_timeout(REFRESH_SECS);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Visible(visible) => {
                self.hidden = !visible;
                true
            }
            Event::Key(key) => {
                if key.key_modifiers.is_empty() {
                    match key.bare_key {
                        BareKey::Down | BareKey::Char('j') => {
                            let count = self.flat().len();
                            if count > 0 {
                                self.selected = (self.selected + 1).min(count - 1);
                            }
                        }
                        BareKey::Up | BareKey::Char('k') => {
                            self.selected = self.selected.saturating_sub(1);
                        }
                        BareKey::Enter | BareKey::Right | BareKey::Char('l') => {
                            self.jump_to_selected();
                        }
                        _ => {}
                    }
                }
                true
            }
            Event::Timer(_) => {
                self.refresh_from_file();
                if !self.has_state_file {
                    // Watcher may have been slow to create the file; re-check.
                    self.has_state_file =
                        std::fs::read_to_string(shared::state_path(&self.session_name())).is_ok();
                    self.ensure_watcher();
                }
                set_timeout(REFRESH_SECS);
                true
            }
            Event::PaneUpdate(manifest) => {                let panes = manifest
                    .panes
                    .into_iter()
                    .flat_map(|(tab_position, pane_infos)| {
                        pane_infos
                            .into_iter()
                            .map(move |pane| shared::PaneInfo {
                                id: pane.id,
                                title: pane.title,
                                tab_position,
                                is_focused: pane.is_focused,
                                is_plugin: pane.is_plugin,
                            })
                    })
                    .collect();
                self.state.apply_panes(panes);
                true
            }
            Event::TabUpdate(tab_infos) => {
                let tabs = tab_infos
                    .into_iter()
                    .map(|tab| shared::TabInfo {
                        position: tab.position,
                        name: tab.name,
                        active: tab.active,
                    })
                    .collect();
                self.state.apply_tabs(tabs);
                true
            }
            _ => false,
        }
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if let PipeSource::Cli(ref pipe_id) = pipe_message.source {
            unblock_cli_pipe_input(pipe_id);
        }
        if pipe_message.name == SIDEBAR_PIPE_NAME {
            match pipe_message.payload.as_deref() {
                Some("toggle") => {
                    if self.hidden {
                        show_self(false);
                        self.hidden = false;
                    } else {
                        hide_self();
                        self.hidden = true;
                    }
                }
                Some("hide") => {
                    hide_self();
                    self.hidden = true;
                }
                Some("show") => {
                    show_self(false);
                    self.hidden = false;
                }
                Some("jump") => self.jump_to_selected(),
                _ => {}
            }
            return true;
        }
        if pipe_message.name == PING_PIPE_NAME {
            return true; // liveness is `watcher`'s business; ignore
        }
        if pipe_message.name != STATUS_PIPE_NAME {
            return false;
        }
        let Some(payload) = pipe_message.payload else {
            return false;
        };
        let Ok(parsed) = serde_json::from_str::<shared::StatusPayload>(payload.trim()) else {
            return false;
        };
        self.state.apply_status(
            parsed.pane_id,
            Status::parse(&parsed.status),
            parsed.agent.unwrap_or_else(|| "agent".to_string()),
            parsed.message.filter(|m| !m.trim().is_empty()),
        );
        true
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        let width = cols.max(10);
        println!("{BOLD}{PAD}AGENTS{RESET}");
        let groups = shared::grouped_rows(self.state.rows());
        if groups.is_empty() {
            println!("{PAD}{DIM}no agents{RESET}");
            return;
        }
        let mut idx = 0usize;
        for group in &groups {
            let head = group
                .rows
                .iter()
                .min_by_key(|r| r.status.severity())
                .map(|r| r.status)
                .unwrap();
            println!(
                "{PAD}{}{}{BOLD} {}{RESET}",
                head.ansi(),
                head.glyph(),
                truncate(&group.tab_name, width.saturating_sub(4)),
            );
            for (i, row) in group.rows.iter().enumerate() {
                let branch = if i + 1 == group.rows.len() { "└" } else { "├" };
                let detail = row.message.as_deref().unwrap_or(&row.pane_title);
                let is_selected = idx == self.selected;
                let style = if is_selected { BOLD } else { "" };
                println!(
                    "{PAD}{DIM}  {branch}{FG_RESET} {}{}{FG_RESET} {style}{}{RESET}",
                    row.status.ansi(),
                    row.status.glyph(),
                    truncate(detail, width.saturating_sub(8)),
                );
                idx += 1;
            }
        }
        let all: Vec<_> = groups.iter().flat_map(|g| g.rows.iter()).collect();
        let working = all.iter().filter(|r| r.status == Status::Working).count();
        let need_you = all.iter().filter(|r| r.status.needs_you()).count();
        if need_you > 0 {
            println!(
                "{PAD}{DIM}{working} working{RESET} {BOLD}\x1b[91m{need_you} need you{RESET}"
            );
        } else {
            println!("{PAD}{DIM}{working} working{RESET}");
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
