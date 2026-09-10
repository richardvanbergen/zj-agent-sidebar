//! The "watcher" half of the split. Joins pushed agent status with Zellij's
//! own pane/tab geometry (the same PaneUpdate/TabUpdate join zj-herd does),
//! then writes the result to disk as JSON and stops. It never renders a UI
//! of its own and never navigates — that is `viewer`'s job, in a separate,
//! non-wasm process.
//!
//! Loading strategy: `PaneUpdate`/`TabUpdate` are normally only delivered to
//! plugin instances living in the *active* tab (verified against Zellij's
//! own dispatch code, `screen.rs`'s `targeted_plugin_ids`) — which is why
//! zj-radar's rail is pinned into every single tab. There is a second path:
//! a plugin loaded via `load_new_plugin(.., load_in_background: true, ..)`
//! gets no pane at all (`tab_index: None`) and is registered in
//! `background_plugin_subscriptions`, which receives every subscribed event
//! unconditionally, regardless of the active tab. So instead of one instance
//! per tab — and instead of auto-loading at session start at all — this
//! plugin is only ever spawned on demand, by `viewer`, the moment a user
//! actually opens it and finds no watcher already running (`viewer`'s
//! `ensure_watcher_running`). Once spawned, its bootstrap pane immediately
//! promotes itself to a background instance and closes itself — one
//! instance, invisible, for the life of the session, but never started
//! without the user having done something that implies they want it.
//! Permission for the promoted instance is not a second prompt: Zellij's
//! permission cache keys purely on plugin `location` (verified in
//! `zellij_exports.rs`/`permission.rs`), not on the configuration map, so
//! the background instance's `request_permission` call resolves to the same
//! already-granted cache entry silently — hence `ChangeApplicationState`
//! (needed for `load_new_plugin` + `close_self`) is requested alongside
//! `ReadApplicationState`/`ReadCliPipes`, but the user only ever sees one
//! prompt, from the bootstrap pane, the first time it's ever spawned.
//!
//! Filesystem note: `shared::STATE_PATH` is a *guest* WASI path. Zellij
//! mounts the plugin's `/tmp` onto its own `ZELLIJ_TMP_DIR`
//! (`<host tmp>/zellij-<uid>/`), not the literal host `/tmp` — `viewer` has
//! to resolve that same host path itself, since it isn't a wasm plugin and
//! gets no such mount.

use serde::Deserialize;
use shared::{Row, Snapshot, Status, PING_PIPE_NAME, SESSION_NAME_KEY, STATUS_PIPE_NAME};
use std::collections::BTreeMap;
use std::io::Write;
use zellij_tile::prelude::*;

/// Config key set by the bootstrap instance on the background instance it
/// spawns, so the background instance knows not to spawn a third one, and
/// so the bootstrap instance (which lacks this key) knows it's the one that
/// should self-promote and then close its own pane.
const BACKGROUND_MARKER_KEY: &str = "zj_agent_state_bg";
/// Config key `viewer` sets to tell this instance its own load URL (needed
/// to call `load_new_plugin` on itself — there's no "what URL did I load
/// from" introspection call, so the caller passes it in instead of this
/// hardcoding a path at compile time).
const SELF_URL_KEY: &str = "zj_agent_state_self_url";
const CHIME_COMMAND: &[&str] = &["afplay", "/System/Library/Sounds/Glass.aiff"];

#[derive(Deserialize)]
struct StatusPayload {
    pane_id: u32,
    #[serde(default)]
    status: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

struct StatusEntry {
    status: Status,
    agent: String,
    message: Option<String>,
}

#[derive(Default)]
struct State {
    reported: BTreeMap<u32, StatusEntry>,
    panes: BTreeMap<u32, (String, usize, bool)>, // pane_id -> (title, tab_position, is_focused)
    tabs: BTreeMap<usize, String>,         // tab_position -> name
    active_tab: Option<usize>,
    generation: u64,
    last_write_ok: bool,
    last_error: String,
    is_background: bool,
    self_url: Option<String>,
    session_name: Option<String>,
}

impl State {
    fn rows(&self) -> Vec<Row> {
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

    /// The only side effect this plugin has: a plain WASI file write into the
    /// preopened `/tmp` — no `RunCommands` permission needed for this at
    /// all. No-ops until the session name is known (should be immediate —
    /// `viewer` sets it at spawn time) rather than ever writing to an
    /// unscoped path another session could collide with.
    fn write_snapshot(&mut self) {
        let Some(session) = self.session_name.clone() else {
            self.last_write_ok = false;
            self.last_error = "no session name yet".to_string();
            return;
        };
        self.generation += 1;
        let snapshot = Snapshot {
            generation: self.generation,
            rows: self.rows(),
        };
        let json = match serde_json::to_string_pretty(&snapshot) {
            Ok(j) => j,
            Err(e) => {
                self.last_write_ok = false;
                self.last_error = format!("serialize: {e}");
                return;
            }
        };
        // Write to a sibling temp file then rename: `File::create` truncates
        // in place, so `viewer`'s 300ms poll could read a half-written JSON.
        // Rename is atomic within the same directory — a reader sees either
        // the old snapshot or the complete new one, never a torn file.
        let path = shared::state_path(&session);
        let tmp_path = format!("{path}.tmp");
        let result = std::path::Path::new(&path)
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| std::fs::File::create(&tmp_path))
            .and_then(|mut f| f.write_all(json.as_bytes()))
            .and_then(|_| std::fs::rename(&tmp_path, &path));
        match result {
            Ok(()) => self.last_write_ok = true,
            Err(e) => {
                self.last_write_ok = false;
                self.last_error = e.to_string();
            }
        }
    }
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.is_background = configuration
            .get(BACKGROUND_MARKER_KEY)
            .map(|v| v == "1")
            .unwrap_or(false);
        self.self_url = configuration.get(SELF_URL_KEY).cloned();
        self.session_name = configuration.get(SESSION_NAME_KEY).cloned();
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::PaneUpdate,
            EventType::TabUpdate,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                // Only the bootstrap (paned) instance promotes+closes; the
                // background instance this spawns re-enters `load()` with
                // `BACKGROUND_MARKER_KEY` set and skips straight past this.
                if !self.is_background {
                    if let Some(url) = self.self_url.clone() {
                        let mut bg_config = BTreeMap::new();
                        bg_config.insert(BACKGROUND_MARKER_KEY.to_string(), "1".to_string());
                        if let Some(session) = self.session_name.clone() {
                            bg_config.insert(SESSION_NAME_KEY.to_string(), session);
                        }
                        load_new_plugin(&url, bg_config, true, false);
                        close_self();
                    }
                }
                true
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => true,
            Event::PaneUpdate(PaneManifest { panes }) => {
                self.panes.clear();
                for (tab_position, pane_infos) in panes {
                    for pane in pane_infos {
                        if pane.is_plugin {
                            continue;
                        }
                        let active = pane.is_focused && self.active_tab == Some(tab_position);
                        self.panes.insert(pane.id, (pane.title, tab_position, active));
                    }
                }
                let live: Vec<u32> = self.panes.keys().copied().collect();
                self.reported.retain(|pane_id, _| live.contains(pane_id));
                self.write_snapshot();
                true
            }
            Event::TabUpdate(tab_infos) => {
                self.tabs.clear();
                self.active_tab = None;
                for tab in tab_infos {
                    if tab.active {
                        self.active_tab = Some(tab.position);
                    }
                    self.tabs.insert(tab.position, tab.name);
                }
                self.write_snapshot();
                true
            }
            _ => false,
        }
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if let PipeSource::Cli(ref pipe_id) = pipe_message.source {
            unblock_cli_pipe_input(pipe_id);
        }
        if pipe_message.name == PING_PIPE_NAME {
            // Liveness probe only — bump generation, touch no agent state.
            self.write_snapshot();
            return true;
        }
        if pipe_message.name != STATUS_PIPE_NAME {
            return false;
        }
        let Some(payload) = pipe_message.payload else {
            return false;
        };
        let Ok(parsed) = serde_json::from_str::<StatusPayload>(payload.trim()) else {
            return false;
        };
        let status = Status::parse(&parsed.status);
        let previous = self.reported.get(&parsed.pane_id).map(|e| e.status);
        self.reported.insert(
            parsed.pane_id,
            StatusEntry {
                status,
                agent: parsed.agent.unwrap_or_else(|| "agent".to_string()),
                message: parsed.message.filter(|m| !m.trim().is_empty()),
            },
        );
        if previous != Some(status) && matches!(status, Status::Blocked | Status::Done | Status::Error) {
            run_command(CHIME_COMMAND, BTreeMap::new());
        }
        self.write_snapshot();
        true
    }

    fn render(&mut self, _rows: usize, _cols: usize) {
        // A tiny liveness line — proof the watcher is running, not a UI.
        println!(
            "watcher: {} pane(s) joined, gen {}, last write {}",
            self.reported.len(),
            self.generation,
            if self.last_write_ok {
                "ok".to_string()
            } else {
                format!("FAILED: {}", self.last_error)
            }
        );
    }
}
