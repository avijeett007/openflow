//! The run-event vocabulary. THIS IS A PUBLIC CONTRACT: C2 (shared agents) seals
//! and ships these frames over the relay — see DESIGN-collaborative-agents.md
//! §8.4. Additive changes only.
//!
//! Every event MUST also render a human-readable line (`render_line`). The
//! driver emits both, so `AgentRunInfo.output`, the File sink, the Notify
//! summary and any panel code that predates structured events all keep working
//! untouched. A variant with no line would silently vanish from all of them.
//!
//! `managers::agent_run::drive_acp_run` is the driver that constructs these
//! from a live run; its `emit_run_event` is the single place the dual emission
//! happens, so no call site can forget half of it. Fully consumed, so this
//! module carries no dead-code allowance — an unused item here is a real one.

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
    /// A REFINEMENT of an existing `ToolCall`, not a replacement. Every field
    /// but `id` is `Option` and `None` means **"the agent said nothing about
    /// this — leave it alone"**, never "reset it". Consumers MUST merge, not
    /// overwrite: see `render_line` below and `runEventRows.ts`.
    ToolCallUpdate {
        id: String,
        status: Option<String>,
        title: Option<String>,
        tool_kind: Option<String>,
        locations: Option<Vec<String>>,
        content: Option<String>,
    },
    /// `tool_kind` and `locations` are carried HERE rather than left to a
    /// frontend join on `tool_call_id`. ACP permits a permission request with
    /// no preceding `tool_call` at all, and the join then silently misses —
    /// leaving the card showing a bare title like *"Edit file"* with no
    /// indication of WHAT it touches, above an Allow button. The agent handed
    /// us both fields in the request itself; throwing them away and guessing
    /// them back is the one thing a permission gate must not do.
    /// Empty `tool_kind` / empty `locations` mean the agent did not say.
    PermissionRequest {
        request_id: String,
        tool_call_id: Option<String>,
        title: String,
        tool_kind: String,
        locations: Vec<String>,
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
            title,
            kind,
            locations,
            content,
        } => RunEvent::ToolCallUpdate {
            id: tool_call_id.clone(),
            // Optionality preserved end to end. Collapsing any of these to a
            // default here would re-introduce the exact overwrite bug the
            // `Option`s exist to prevent.
            status: status.clone(),
            title: title.clone(),
            tool_kind: kind.clone(),
            locations: locations
                .as_ref()
                .map(|ls| ls.iter().map(|l| l.path.clone()).collect()),
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
        // Renders only what the refinement ACTUALLY carried. This line goes
        // into `AgentRunInfo.output` AND the File sink — the permanent record
        // — so a status-less refinement must not be written down as a bare
        // "  ✓ " implying the tool call reported something it never did.
        RunEvent::ToolCallUpdate {
            status,
            title,
            locations,
            ..
        } => {
            let mut s = String::from("  ");
            match status {
                Some(st) => s.push_str(&format!("✓ {st}")),
                // No status: this is a refinement (Claude Code's own wording).
                None => s.push('·'),
            }
            if let Some(t) = title {
                s.push(' ');
                s.push_str(t);
            }
            match locations {
                Some(l) if !l.is_empty() => {
                    s.push_str(" — ");
                    s.push_str(&l.join(", "));
                }
                _ => {}
            }
            s
        }
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
            status: Some("completed".into()),
            title: None,
            kind: None,
            locations: None,
            content: None,
        };
        let e = map_session_update(&u).unwrap();
        assert_eq!(render_line(&e).as_deref(), Some("  ✓ completed"));
    }

    /// The MF1 shape, verbatim from `claude-agent-acp@0.64.2`: a refinement
    /// that carries the RESOLVED PATH and no status at all. Before the fix
    /// `status` was a `#[serde(default)] String`, so this produced `status: ""`
    /// — written into `AgentRunInfo.output` and the File sink as a bare
    /// `"  ✓ "`, and used by the frontend to overwrite the tool call's real
    /// `pending`/`completed`. The path itself was not modelled and vanished.
    #[test]
    fn a_status_less_refinement_renders_its_path_and_never_claims_a_status() {
        let u = SessionUpdate::ToolCallUpdate {
            tool_call_id: "toolu_01V54kbxK7U3Fgz17XyuHBk3".into(),
            status: None,
            title: Some("Read README.md".into()),
            kind: Some("read".into()),
            locations: Some(vec![ToolLocation {
                path: "/repo/README.md".into(),
            }]),
            content: None,
        };
        let e = map_session_update(&u).unwrap();
        match &e {
            RunEvent::ToolCallUpdate {
                status,
                title,
                tool_kind,
                locations,
                ..
            } => {
                assert_eq!(*status, None, "an absent status must stay absent");
                assert_eq!(title.as_deref(), Some("Read README.md"));
                assert_eq!(tool_kind.as_deref(), Some("read"));
                assert_eq!(
                    locations.as_deref(),
                    Some(["/repo/README.md".to_string()].as_slice()),
                    "the resolved path is the entire point of a refinement"
                );
            }
            _ => panic!("expected ToolCallUpdate"),
        }
        let line = render_line(&e).unwrap();
        assert_eq!(line, "  · Read README.md — /repo/README.md");
        assert!(
            !line.contains('✓'),
            "the permanent record must not claim a status the agent never sent: {line:?}"
        );
    }

    /// The other real shape: id + `rawOutput` only. Nothing to say, and the
    /// line must still be non-empty (dual emission) without inventing a status.
    #[test]
    fn a_bare_refinement_still_renders_a_line_without_inventing_a_status() {
        let e = map_session_update(&SessionUpdate::ToolCallUpdate {
            tool_call_id: "toolu_019zBNm4wS9dAEmoyi8715MS".into(),
            status: None,
            title: None,
            kind: None,
            locations: None,
            content: None,
        })
        .unwrap();
        assert_eq!(render_line(&e).as_deref(), Some("  ·"));
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
            tool_kind: "edit".into(),
            locations: vec!["/repo/src/main.rs".into()],
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
                status: Some("completed".into()),
                title: None,
                tool_kind: None,
                locations: None,
                content: None,
            },
            // The refinement shape too: it must ALSO produce a line.
            RunEvent::ToolCallUpdate {
                id: "1".into(),
                status: None,
                title: None,
                tool_kind: None,
                locations: None,
                content: None,
            },
            RunEvent::PermissionRequest {
                request_id: "r".into(),
                tool_call_id: None,
                title: "T".into(),
                tool_kind: String::new(),
                locations: vec![],
                options: vec![],
            },
            RunEvent::PermissionResolved {
                request_id: "r".into(),
                outcome: "allow".into(),
                automatic: false,
            },
            RunEvent::TurnEnd {
                stop_reason: "end_turn".into(),
            },
        ];
        for v in &variants {
            assert!(render_line(v).is_some(), "no text line for {v:?}");
        }
    }
}
