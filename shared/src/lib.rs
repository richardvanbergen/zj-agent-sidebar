//! Wire/disk shape shared by `watcher` (writes it) and `viewer` (reads it).
//! Deliberately tiny: this is a validation spike for one question — can a
//! Zellij plugin's joined pane/tab/agent state cross the process boundary as
//! a plain file, and can a completely separate, non-wasm program drive
//! Zellij navigation off of it. Not a production wire format.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Working,
    Blocked,
    Idle,
    Done,
    Error,
    Unknown,
}

impl Status {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "working" => Status::Working,
            "blocked" => Status::Blocked,
            "idle" => Status::Idle,
            "done" => Status::Done,
            "error" => Status::Error,
            _ => Status::Unknown,
        }
    }

    pub fn glyph(&self) -> &'static str {
        match self {
            Status::Working => "\u{25D0}", // ◐
            Status::Blocked => "\u{25C6}", // ◆
            Status::Idle => "\u{25CB}",    // ○
            Status::Done => "\u{25CF}",    // ●
            Status::Error => "\u{2717}",   // ✗
            Status::Unknown => "\u{00B7}", // ·
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Blocked => "needs you",
            Status::Idle => "idle",
            Status::Done => "done",
            Status::Error => "error",
            Status::Unknown => "unknown",
        }
    }

    pub fn ansi(&self) -> &'static str {
        match self {
            Status::Working => "\x1b[33m",
            Status::Blocked => "\x1b[91m",
            Status::Idle => "\x1b[90m",
            Status::Done => "\x1b[32m",
            Status::Error => "\x1b[31m",
            Status::Unknown => "\x1b[90m",
        }
    }

    pub fn severity(&self) -> u8 {
        match self {
            Status::Error => 0,
            Status::Blocked => 0,
            Status::Working => 1,
            Status::Done => 2,
            Status::Idle => 3,
            Status::Unknown => 4,
        }
    }

    pub fn needs_you(&self) -> bool {
        matches!(self, Status::Blocked | Status::Error)
    }
}

/// One agent row, fully joined: pushed status + Zellij's own pane/tab geometry.
/// This is exactly what `watcher` writes and `viewer` reads — no other
/// negotiation between the two processes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub pane_id: u32,
    pub status: Status,
    pub agent: String,
    pub message: Option<String>,
    pub pane_title: String,
    pub tab_name: String,
    pub tab_position: usize,
    #[serde(default)]
    pub active: bool,
}

/// The whole snapshot `watcher` writes to disk on every state edge.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub generation: u64,
    pub rows: Vec<Row>,
}

/// One `zj-agent-state` state file per Zellij *session* — running several
/// sessions at once (very normal) each spawns its own `watcher`, and without
/// session-scoping they'd all fight over one shared file, each write
/// clobbering whichever other session wrote last. `session` is whatever
/// Zellij calls the session (e.g. `delighted-panda`); sanitized to a safe
/// path component since it's used as a directory name. `watcher` learns its
/// own session name from `SESSION_NAME_KEY` config (set by `viewer`, which
/// reads its own `$ZELLIJ_SESSION_NAME`) rather than from Zellij's
/// `ModeUpdate` event, which is subject to the same active-tab-only delivery
/// restriction background instances otherwise route around — better to not
/// depend on it at all.
pub fn state_path(session: &str) -> String {
    format!("/tmp/zj-agent-state/{}/state.json", sanitize_session(session))
}

fn sanitize_session(session: &str) -> String {
    session
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Where `watcher.wasm` is installed. `viewer` needs this to spawn it itself
/// on demand — `watcher` never needs to know its own path, it's only ever
/// told via `zj_agent_state_self_url` config at load time. Derived from
/// `$HOME` at runtime (not baked in at compile time) so the same binary
/// works on any machine it's installed to.
pub fn watcher_wasm_host_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    format!("{home}/.config/zellij/plugins/zj-agent-state-watcher.wasm")
}

/// Config key `viewer` sets when spawning `watcher`, carrying `viewer`'s own
/// `$ZELLIJ_SESSION_NAME` so `watcher` can compute a session-scoped
/// `state_path` without ever needing to learn its session name from Zellij.
pub const SESSION_NAME_KEY: &str = "zj_agent_state_session";

/// A dedicated pipe name `viewer` uses purely to ask "is anyone listening" —
/// `watcher` responds by bumping `Snapshot::generation` with no other state
/// change, so a caller can tell "alive" from "not running" by watching for
/// that bump, without it showing up as a fake agent row.
pub const PING_PIPE_NAME: &str = "zj_agent_state.ping.v1";
/// The pipe `watcher` actually listens on for real agent status.
pub const STATUS_PIPE_NAME: &str = "zj_agent_state.status.v1";
/// The pipe the user's keybinds use to talk to the `sidebar` (via
/// `MessagePlugin`): payloads are plain words — `toggle`, `hide`, `show`,
/// `jump`.
pub const SIDEBAR_PIPE_NAME: &str = "zj_agent_state.sidebar.v1";

/// Hook wire payload, shared by `watcher` and `sidebar` — both receive the
/// same `zellij pipe` broadcasts.
#[derive(Deserialize)]
pub struct StatusPayload {
    pub pane_id: u32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// One reported agent's status, before joining to geometry.
#[derive(Clone, Debug)]
pub struct StatusEntry {
    pub status: Status,
    pub agent: String,
    pub message: Option<String>,
}

/// Plain-data shapes so `shared` never depends on `zellij-tile` (which would
/// drag it into every native consumer). The wasm plugins convert from the
/// real `zellij_tile` event types.
pub struct PaneInfo {
    pub id: u32,
    pub title: String,
    pub tab_position: usize,
    pub is_focused: bool,
    pub is_plugin: bool,
}

pub struct TabInfo {
    pub position: usize,
    pub name: String,
    pub active: bool,
}

/// The PaneUpdate/TabUpdate/status-pipe join, shared verbatim by `watcher`
/// (which writes it to disk) and `sidebar` (which renders it). Push-only:
/// callers feed events in, state comes back out via `rows()`.
#[derive(Default)]
pub struct JoinState {
    reported: BTreeMap<u32, StatusEntry>,
    panes: BTreeMap<u32, (String, usize, bool)>, // pane_id -> (title, tab_position, active)
    tabs: BTreeMap<usize, String>,               // tab_position -> name
    active_tab: Option<usize>,
}

impl JoinState {
    pub fn apply_panes(&mut self, panes: Vec<PaneInfo>) {
        self.panes.clear();
        for pane in panes {
            if pane.is_plugin {
                continue;
            }
            let active = pane.is_focused && self.active_tab == Some(pane.tab_position);
            self.panes.insert(pane.id, (pane.title, pane.tab_position, active));
        }
        // Status for a pane that no longer exists is dead weight everywhere
        // downstream — drop it here, once.
        let live: Vec<u32> = self.panes.keys().copied().collect();
        self.reported.retain(|pane_id, _| live.contains(pane_id));
    }

    pub fn apply_tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs.clear();
        self.active_tab = None;
        for tab in tabs {
            if tab.active {
                self.active_tab = Some(tab.position);
            }
            self.tabs.insert(tab.position, tab.name);
        }
    }

    /// Returns the row's previous status so callers can chime on edges.
    pub fn apply_status(
        &mut self,
        pane_id: u32,
        status: Status,
        agent: String,
        message: Option<String>,
    ) -> Option<Status> {
        let previous = self.reported.get(&pane_id).map(|e| e.status);
        self.reported.insert(
            pane_id,
            StatusEntry { status, agent, message },
        );
        previous
    }

    pub fn reported_len(&self) -> usize {
        self.reported.len()
    }

    pub fn rows(&self) -> Vec<Row> {
        self.reported
            .iter()
            .filter_map(|(pane_id, entry)| {
                let (pane_title, tab_position, active) = self.panes.get(pane_id)?.clone();
                let tab_name = self
                    .tabs
                    .get(&tab_position)
                    .cloned()
                    .unwrap_or_else(|| format!("tab {}", tab_position + 1));
                Some(Row {
                    pane_id: *pane_id,
                    status: entry.status,
                    agent: entry.agent.clone(),
                    message: entry.message.clone(),
                    pane_title,
                    tab_name,
                    tab_position,
                    active,
                })
            })
            .collect()
    }
}

/// A tab's worth of rows, sorted worst-first inside the tab, tabs sorted
/// worst-first overall. Shared by `viewer` and `sidebar` so both orderings
/// agree.
pub struct TabGroup {
    pub tab_name: String,
    pub rows: Vec<Row>,
}

pub fn grouped_rows(rows: Vec<Row>) -> Vec<TabGroup> {
    let mut map: BTreeMap<usize, TabGroup> = BTreeMap::new();
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

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn state_path_scopes_by_session() {
        assert_eq!(state_path("delighted-panda"), "/tmp/zj-agent-state/delighted-panda/state.json");
        assert_ne!(state_path("session-a"), state_path("session-b"));
    }

    #[test]
    fn state_path_sanitizes_unsafe_characters() {
        assert_eq!(state_path("../../etc"), "/tmp/zj-agent-state/______etc/state.json");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only contract between `watcher` and `viewer` is this JSON shape —
    /// confirms it round-trips exactly as either side expects, without
    /// needing a live Zellij session to check it.
    #[test]
    fn snapshot_round_trips() {
        let snapshot = Snapshot {
            generation: 3,
            rows: vec![Row {
                pane_id: 7,
                status: Status::Blocked,
                agent: "claude".to_string(),
                message: Some("needs approval".to_string()),
                pane_title: "fix auth bug".to_string(),
                tab_name: "pinky".to_string(),
                tab_position: 2,
                active: false,
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.generation, 3);
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].pane_id, 7);
        assert_eq!(back.rows[0].status, Status::Blocked);
        assert_eq!(back.rows[0].tab_position, 2);
    }

    #[test]
    fn status_parse_matches_watcher_hook_vocabulary() {
        // These four strings are exactly what hooks/claude-status.sh sends.
        assert_eq!(Status::parse("working"), Status::Working);
        assert_eq!(Status::parse("blocked"), Status::Blocked);
        assert_eq!(Status::parse("idle"), Status::Idle);
        assert_eq!(Status::parse("done"), Status::Done);
        assert_eq!(Status::parse("anything-else"), Status::Unknown);
    }
}
