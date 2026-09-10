//! Wire/disk shape shared by `watcher` (writes it) and `viewer` (reads it).
//! Deliberately tiny: this is a validation spike for one question — can a
//! Zellij plugin's joined pane/tab/agent state cross the process boundary as
//! a plain file, and can a completely separate, non-wasm program drive
//! Zellij navigation off of it. Not a production wire format.

use serde::{Deserialize, Serialize};

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
