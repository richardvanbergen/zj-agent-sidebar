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
//! it as a plain pane: status glyph, agent, message. No file, no chime,
//! no navigation — the background `watcher` owns the file (that's what
//! `viewer` reads), and this pane is a pure view of the same event stream.
//!
//! Active-tab-only delivery means an instance in a background tab holds
//! stale geometry until its tab is activated (then PaneUpdate/TabUpdate
//! refresh it immediately). That's inherent to the layout-template approach
//! and matches zj-radar's behaviour.

use shared::{JoinState, Status, PING_PIPE_NAME, STATUS_PIPE_NAME};
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

const DIM: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const FG_RESET: &str = "\x1b[39m";

#[derive(Default)]
struct Sidebar {
    state: JoinState,
}

register_plugin!(Sidebar);

impl ZellijPlugin for Sidebar {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
        ]);
        subscribe(&[EventType::PaneUpdate, EventType::TabUpdate]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PaneUpdate(manifest) => {
                let panes = manifest
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
        let groups = shared::grouped_rows(self.state.rows());
        println!("{BOLD}AGENTS{RESET}");
        if groups.is_empty() {
            println!("{DIM}no agents{RESET}");
            return;
        }
        for group in &groups {
            let head = group
                .rows
                .iter()
                .min_by_key(|r| r.status.severity())
                .map(|r| r.status)
                .unwrap();
            println!(
                "{}{}{BOLD} {}{RESET}",
                head.ansi(),
                head.glyph(),
                truncate(&group.tab_name, width - 2),
            );
            for (i, row) in group.rows.iter().enumerate() {
                let branch = if i + 1 == group.rows.len() { "└" } else { "├" };
                let detail = row.message.as_deref().unwrap_or(&row.pane_title);
                println!(
                    "{DIM}  {branch}{FG_RESET} {}{}{FG_RESET} {}",
                    row.status.ansi(),
                    row.status.glyph(),
                    truncate(detail, width.saturating_sub(6)),
                );
            }
        }
        let all: Vec<_> = groups.iter().flat_map(|g| g.rows.iter()).collect();
        let working = all.iter().filter(|r| r.status == Status::Working).count();
        let need_you = all.iter().filter(|r| r.status.needs_you()).count();
        if need_you > 0 {
            println!("{DIM}{working} working{RESET} {BOLD}\x1b[91m{need_you} need you{RESET}");
        } else {
            println!("{DIM}{working} working{RESET}");
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
