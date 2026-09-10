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
//!   last-writer-wins, no cross-tab coordination needed. Interactive
//!   selection (arrow keys + Enter) jumps to the selected row and only
//!   ever reaches the focused instance, so it can't race.
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

use shared::{
    JoinState, Row, Snapshot, Status, PING_PIPE_NAME, SESSION_NAME_KEY, SIDEBAR_PIPE_NAME,
    STATUS_PIPE_NAME, SYNC_PIPE_NAME,
};
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

/// Config key on the sidebar plugin (set in the layout) giving the `file:`
/// URL of `watcher.wasm`, so an instance that finds no state file can spawn
/// the watcher itself.
const WATCHER_URL_KEY: &str = "zj_agent_state_watcher_url";
/// First timer tick: fast, so a fresh instance shows the global state
/// almost immediately (host queries are only safe from event handlers).
const FIRST_TICK_SECS: f64 = 0.3;
/// Backstop tick after that: the watcher's sync pushes do the real-time
/// updates; this re-read only exists to recover if the watcher died.
const BACKSTOP_TICK_SECS: f64 = 10.0;

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
    /// Session name, lazily resolved — see `ensure_session`.
    session: Option<String>,
    /// Our own plugin pane id, lazily resolved (get_plugin_ids panics during
    /// load, same as all host queries).
    own_pane_id: Option<u32>,
    /// plugin-pane id -> tab position, from PaneUpdate (which includes
    /// plugin panes even though the join state filters them out).
    plugin_pane_tabs: BTreeMap<u32, usize>,
    /// Position of the active tab, from TabUpdate.
    active_tab: Option<usize>,
}

impl Sidebar {
    /// Lazily resolved: `get_session_environment_variables` panics during
    /// `load()` (host returns no payload before the handshake completes —
    /// shim.rs's unwrap on the missing response), so it's only ever called
    /// from an event handler, and only once.
    fn ensure_session(&mut self) -> bool {
        if self.session.is_some() {
            return true;
        }
        if let Ok(name) = std::env::var("ZELLIJ_SESSION_NAME") {
            self.session = Some(name);
            return true;
        }
        let Some(name) = get_session_environment_variables()
            .get("ZELLIJ_SESSION_NAME")
            .cloned()
        else {
            return false;
        };
        self.session = Some(name);
        true
    }

    /// Read `watcher`'s state file and merge it into the join state. The
    /// WASI `/tmp` is the same mount `watcher` writes to, so this is the
    /// shared ground truth between all instances of the session.
    fn refresh_from_file(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
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
            if let Some(session) = self.session.clone() {
                let mut config = BTreeMap::new();
                config.insert(SESSION_NAME_KEY.to_string(), session);
                load_new_plugin(&url, config, true, false);
                self.has_state_file = true; // don't respawn on every render path
            }
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

    /// The keybind `jump` is broadcast to every instance. "Jump to the
    /// sidebar" means: focus *this* pane — but only the instance living in
    /// the active tab may act, or every tab's instance would fight over
    /// focus. Each instance knows its own pane id (`get_plugin_ids` returns
    /// the pane id for plugin panes) and, from PaneUpdate, which tab that
    /// pane is in; the one whose tab matches the active tab focuses itself.
    /// (Stale geometry in background tabs is safe here: only the instance
    /// receiving fresh PaneUpdate/TabUpdate events — i.e. the active tab's
    /// — ever sees a match.)
    fn focus_self_in_active_tab(&mut self) {
        let Some(own) = self.own_pane_id else {
            return;
        };
        let Some(&tab) = self.plugin_pane_tabs.get(&own) else {
            return;
        };
        if self.active_tab == Some(tab) {
            show_pane_with_id(PaneId::Plugin(own), false, true);
        }
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
        set_selectable(true);
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[EventType::PaneUpdate, EventType::TabUpdate, EventType::Key, EventType::Timer]);
        set_timeout(FIRST_TICK_SECS);
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
                // First tick resolves the session (host queries are only
                // safe from event handlers, not load); ticks after that
                // exist only to recover if the watcher died — realtime
                // updates arrive via its sync broadcasts.
                if self.ensure_session() {
                    self.refresh_from_file();
                    if !self.has_state_file {
                        self.has_state_file = std::fs::read_to_string(shared::state_path(&self.session.clone().unwrap_or_default())).is_ok();
                        self.ensure_watcher();
                    }
                }
                if self.own_pane_id.is_none() {
                    self.own_pane_id = Some(get_plugin_ids().plugin_id);
                }
                set_timeout(BACKSTOP_TICK_SECS);
                true
            }
            Event::PaneUpdate(manifest) => {
                let mut plugin_pane_tabs = BTreeMap::new();
                let mut panes = Vec::new();
                for (tab_position, pane_infos) in manifest.panes {
                    for pane in pane_infos {
                        if pane.is_plugin {
                            plugin_pane_tabs.insert(pane.id, tab_position);
                        }
                        panes.push(shared::PaneInfo {
                            id: pane.id,
                            title: pane.title,
                            tab_position,
                            is_focused: pane.is_focused,
                            is_plugin: pane.is_plugin,
                        });
                    }
                }
                self.plugin_pane_tabs = plugin_pane_tabs;
                self.state.apply_panes(panes);
                true
            }
            Event::TabUpdate(tab_infos) => {
                let tabs = tab_infos
                    .into_iter()
                    .map(|tab| {
                        if tab.active {
                            self.active_tab = Some(tab.position);
                        }
                        shared::TabInfo {
                            position: tab.position,
                            name: tab.name,
                            active: tab.active,
                        }
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
                Some("jump") => self.focus_self_in_active_tab(),
                _ => {}
            }
            return true;
        }
        if pipe_message.name == SYNC_PIPE_NAME {
            // Watcher pushed the full snapshot — instant update, no file
            // read, no polling.
            if let Some(payload) = pipe_message.payload {
                if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&payload) {
                    self.state.seed_from_rows(snapshot.rows);
                }
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
