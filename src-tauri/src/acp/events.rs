//! The run-event vocabulary. THIS IS A PUBLIC CONTRACT: C2 (shared agents) seals
//! and ships these frames over the relay — see DESIGN-collaborative-agents.md
//! §8.4. Additive changes only.
//!
//! Every event MUST also render a human-readable line (`render_line`). The
//! driver emits both, so `AgentRunInfo.output`, the File sink, the Notify
//! summary and any panel code that predates structured events all keep working
//! untouched. A variant with no line would silently vanish from all of them.
//!
//! Exercised end-to-end by this file's tests, but the driver that actually
//! constructs `AgentRunEvent` and calls these functions from a live run is a
//! later task — same situation as `protocol.rs`. Silence dead-code until it's
//! wired up.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

use crate::acp::protocol::SessionUpdate;

#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
pub struct PlanEntry {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    Text {
        text: String,
    },
    Thought {
        text: String,
    },
    Plan {
        entries: Vec<PlanEntry>,
    },
    ToolCall {
        id: String,
        title: String,
        tool_kind: String,
        status: String,
        locations: Vec<String>,
    },
    ToolCallUpdate {
        id: String,
        status: String,
        content: Option<String>,
    },
    PermissionRequest {
        request_id: String,
        tool_call_id: Option<String>,
        title: String,
        options: Vec<PermissionOption>,
    },
    PermissionResolved {
        request_id: String,
        outcome: String,
        automatic: bool,
    },
    TurnEnd {
        stop_reason: String,
    },
}

/// Emitted per structured update. Event name: `agent-run-event`. Sits ALONGSIDE
/// the existing `agent-run-output`, never replacing it.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct AgentRunEvent {
    pub run_id: String,
    pub event: RunEvent,
}

/// `None` for updates we deliberately do not model — dropped, never fatal.
pub fn map_session_update(u: &SessionUpdate) -> Option<RunEvent> {
    Some(match u {
        SessionUpdate::AgentMessageChunk { content } => RunEvent::Text {
            text: content.text.clone(),
        },
        SessionUpdate::AgentThoughtChunk { content } => RunEvent::Thought {
            text: content.text.clone(),
        },
        SessionUpdate::Plan { entries } => RunEvent::Plan {
            entries: entries
                .iter()
                .map(|e| PlanEntry {
                    content: e.content.clone(),
                    priority: e.priority.clone(),
                    status: e.status.clone(),
                })
                .collect(),
        },
        SessionUpdate::ToolCall {
            tool_call_id,
            title,
            kind,
            status,
            locations,
        } => RunEvent::ToolCall {
            id: tool_call_id.clone(),
            title: title.clone(),
            tool_kind: kind.clone(),
            status: status.clone(),
            locations: locations.iter().map(|l| l.path.clone()).collect(),
        },
        SessionUpdate::ToolCallUpdate {
            tool_call_id,
            status,
            content,
        } => RunEvent::ToolCallUpdate {
            id: tool_call_id.clone(),
            status: status.clone(),
            content: content.as_ref().map(|c| c.to_string()),
        },
        SessionUpdate::Unknown => return None,
    })
}

/// The human-readable line for the rolling buffer / File sink. Always `Some`.
pub fn render_line(e: &RunEvent) -> Option<String> {
    Some(match e {
        RunEvent::Text { text } => text.clone(),
        RunEvent::Thought { text } => format!("· {text}"),
        RunEvent::Plan { entries } => {
            let mut s = String::from("◇ plan");
            for en in entries {
                s.push_str(&format!("\n  - [{}] {}", en.status, en.content));
            }
            s
        }
        RunEvent::ToolCall {
            title, locations, ..
        } => {
            if locations.is_empty() {
                format!("▸ {title}")
            } else {
                format!("▸ {title} — {}", locations.join(", "))
            }
        }
        RunEvent::ToolCallUpdate { status, .. } => format!("  ✓ {status}"),
        RunEvent::PermissionRequest { title, .. } => format!("? {title}"),
        RunEvent::PermissionResolved {
            outcome, automatic, ..
        } => {
            if *automatic {
                format!("  → {outcome} (automatic)")
            } else {
                format!("  → {outcome}")
            }
        }
        RunEvent::TurnEnd { stop_reason } => format!("— turn ended: {stop_reason}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::protocol::{PlanEntryWire, SessionUpdate, TextContent, ToolLocation};

    // Gap flagged in Task 1's review: `Plan` was the one `SessionUpdate` variant
    // never exercised through `map_session_update` by the brief's own tests
    // (which only construct `RunEvent::Plan` by hand for the dual-emission
    // check below). Closes it so all six variants go through the real mapping.
    #[test]
    fn maps_plan_to_entries_and_renders_each_line() {
        let u = SessionUpdate::Plan {
            entries: vec![PlanEntryWire {
                content: "step one".into(),
                priority: "high".into(),
                status: "pending".into(),
            }],
        };
        let e = map_session_update(&u).unwrap();
        match &e {
            RunEvent::Plan { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].content, "step one");
                assert_eq!(entries[0].priority, "high");
                assert_eq!(entries[0].status, "pending");
            }
            _ => panic!("expected Plan"),
        }
        assert_eq!(
            render_line(&e).as_deref(),
            Some("◇ plan\n  - [pending] step one")
        );
    }

    #[test]
    fn maps_message_chunk_to_text_and_renders_raw() {
        let u = SessionUpdate::AgentMessageChunk {
            content: TextContent {
                text: "hello".into(),
            },
        };
        let e = map_session_update(&u).unwrap();
        assert!(matches!(&e, RunEvent::Text { text } if text == "hello"));
        assert_eq!(render_line(&e).as_deref(), Some("hello"));
    }

    #[test]
    fn maps_thought_chunk_and_renders_with_marker() {
        let u = SessionUpdate::AgentThoughtChunk {
            content: TextContent {
                text: "pondering".into(),
            },
        };
        let e = map_session_update(&u).unwrap();
        assert!(matches!(e, RunEvent::Thought { .. }));
        assert_eq!(render_line(&e).as_deref(), Some("· pondering"));
    }

    #[test]
    fn maps_tool_call_with_locations_and_renders_path() {
        let u = SessionUpdate::ToolCall {
            tool_call_id: "t1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: "pending".into(),
            locations: vec![ToolLocation {
                path: "/a/b.rs".into(),
            }],
        };
        let e = map_session_update(&u).unwrap();
        match &e {
            RunEvent::ToolCall {
                id,
                tool_kind,
                locations,
                ..
            } => {
                assert_eq!(id, "t1");
                assert_eq!(tool_kind, "read");
                assert_eq!(locations, &vec!["/a/b.rs".to_string()]);
            }
            _ => panic!("expected ToolCall"),
        }
        assert_eq!(render_line(&e).as_deref(), Some("▸ Read file — /a/b.rs"));
    }

    #[test]
    fn tool_call_without_locations_renders_title_only() {
        let u = SessionUpdate::ToolCall {
            tool_call_id: "t2".into(),
            title: "Think".into(),
            kind: "think".into(),
            status: "pending".into(),
            locations: vec![],
        };
        let e = map_session_update(&u).unwrap();
        assert_eq!(render_line(&e).as_deref(), Some("▸ Think"));
    }

    #[test]
    fn tool_call_update_renders_check_on_completed() {
        let u = SessionUpdate::ToolCallUpdate {
            tool_call_id: "t1".into(),
            status: "completed".into(),
            content: None,
        };
        let e = map_session_update(&u).unwrap();
        assert_eq!(render_line(&e).as_deref(), Some("  ✓ completed"));
    }

    #[test]
    fn unknown_update_maps_to_none_and_is_dropped() {
        assert!(map_session_update(&SessionUpdate::Unknown).is_none());
    }

    #[test]
    fn permission_request_and_resolution_render() {
        let req = RunEvent::PermissionRequest {
            request_id: "r1".into(),
            tool_call_id: Some("t1".into()),
            title: "Edit src/main.rs".into(),
            options: vec![PermissionOption {
                option_id: "allow".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            }],
        };
        assert_eq!(render_line(&req).as_deref(), Some("? Edit src/main.rs"));

        let auto = RunEvent::PermissionResolved {
            request_id: "r1".into(),
            outcome: "allow".into(),
            automatic: true,
        };
        assert_eq!(render_line(&auto).as_deref(), Some("  → allow (automatic)"));

        let manual = RunEvent::PermissionResolved {
            request_id: "r1".into(),
            outcome: "deny".into(),
            automatic: false,
        };
        assert_eq!(render_line(&manual).as_deref(), Some("  → deny"));
    }

    /// Exhaustiveness tripwire. Adding a variant to `RunEvent` breaks
    /// compilation HERE, next to the sample list below — which is the
    /// reminder to extend it. Dual emission is the non-breaking guarantee: a
    /// variant whose `render_line` returns `None` (or one nobody added to
    /// `every_event_variant_renders_a_line`'s sample list) silently vanishes
    /// from `AgentRunInfo.output`, the File sink and the notification
    /// summary. The `match` below has NO `_ =>` catch-all on purpose: a new
    /// variant must be listed here explicitly before the crate compiles
    /// again, forcing whoever added it to also extend the sample list.
    #[cfg(test)]
    fn _render_line_exhaustiveness_guard(e: &RunEvent) {
        match e {
            RunEvent::Text { .. } => (),
            RunEvent::Thought { .. } => (),
            RunEvent::Plan { .. } => (),
            RunEvent::ToolCall { .. } => (),
            RunEvent::ToolCallUpdate { .. } => (),
            RunEvent::PermissionRequest { .. } => (),
            RunEvent::PermissionResolved { .. } => (),
            RunEvent::TurnEnd { .. } => (),
        }
    }

    #[test]
    fn every_event_variant_renders_a_line() {
        // Dual emission is mandatory: a structured event with no text line would
        // silently vanish from the File sink and the existing panel.
        let variants = vec![
            RunEvent::Text { text: "t".into() },
            RunEvent::Thought { text: "t".into() },
            RunEvent::Plan {
                entries: vec![PlanEntry {
                    content: "step".into(),
                    priority: "high".into(),
                    status: "pending".into(),
                }],
            },
            RunEvent::ToolCall {
                id: "1".into(),
                title: "T".into(),
                tool_kind: "read".into(),
                status: "pending".into(),
                locations: vec![],
            },
            RunEvent::ToolCallUpdate {
                id: "1".into(),
                status: "completed".into(),
                content: None,
            },
            RunEvent::PermissionRequest {
                request_id: "r".into(),
                tool_call_id: None,
                title: "T".into(),
                options: vec![],
            },
            RunEvent::PermissionResolved {
                request_id: "r".into(),
                outcome: "allow".into(),
                automatic: false,
            },
            RunEvent::TurnEnd {
                stop_reason: "completed".into(),
            },
        ];
        for v in &variants {
            assert!(render_line(v).is_some(), "no text line for {v:?}");
        }
    }
}
