//! The relay wire contract.
//!
//! Two layers, deliberately separate:
//!  * The **envelope** (`HostMessage`/`ServiceMessage`) belongs to
//!    openflow-service and is frozen by DESIGN-relay-v02 §6. The service routes
//!    and stores it; it NEVER parses a payload (§3).
//!  * The **frame** (`HostFrame`) is OpenFlow's own vocabulary, carried inside
//!    an envelope's opaque `payload`, and is frozen by DESIGN-shared-agents §7.
//!
//! `HostFrame` reserves C0's richer vocabulary without emitting it, so merging
//! PR #64 adds capability with no wire break. The reserved variants' names and
//! field shapes are copied verbatim from C0's ACTUAL CODE:
//! `feat/acp-agents:src-tauri/src/acp/events.rs` (`RunEvent`) — NOT from
//! `documentation/design/acp-agents/PLAN.md` Task 3, which predates C0's own
//! final-review enrichments to `ToolCallUpdate` and `PermissionRequest` and is
//! stale (corrected 2026-08-04; DESIGN-shared-agents.md §7 now names the code,
//! not the plan doc, as authoritative). They are not paraphrased, because a
//! paraphrase is exactly how a wire break gets introduced. When #64 merges,
//! replace the local `PlanEntryWire`/`PermissionOptionWire` with
//! `pub use crate::acp::events::{PlanEntry, PermissionOption};` — identical
//! shapes, zero wire change.
//!
//! Verified against the real counterparty, not derived from prose alone: every
//! envelope shape below was checked against openflow-service's own
//! `src/relay/protocol.rs` (branch `feat/relay-v0.2`, PR #1). The `ready` ack
//! and `open` frame were additionally captured from a real running
//! `openflow-service` (paired host + teammate, a real WebSocket, a real
//! `POST /v2/sessions`) and are deserialized verbatim through this crate's own
//! `ServiceMessage` in `real_captured_frames` below — genuine round trips, not
//! hand-written JSON. A live round trip of this crate's OWN `HostFrame`/
//! `session_frame` output through a real service is the NEXT task's job:
//! `HostFrame` is internally tagged (`kind` lives inside the payload), so a
//! capture of it can only be genuine once something in this crate actually
//! sends it over a socket — protocol.rs itself does no I/O by design (see
//! `relay/mod.rs`'s layering doc), and the thing that sends it is
//! `managers::agent_host`'s host loop.
//!
//! This module now has a real production caller: `managers::agent_host`
//! constructs `HostMessage`/`session_frame`, matches on `ServiceMessage`, and
//! calls `parse_open_payload` on every inbound `open`. The per-item
//! `#[allow(dead_code)]` markers that stood in for that caller while it was an
//! unwritten task have therefore been removed — all but one, on
//! `V1_EMITTED_KINDS`, which is documentation rather than code.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only frame kinds v1 ever emits. Everything else in `HostFrame` is
/// reserved. Asserted by test so adding an emitter is a deliberate act.
// The one item here with no production caller: it documents the v1/reserved
// boundary for a reader and is asserted by its own test. Everything else in
// this module is now constructed or matched by `managers::agent_host`.
#[allow(dead_code)]
pub const V1_EMITTED_KINDS: [&str; 3] = ["header", "output", "status"];

/// One published offer (DESIGN-relay-v02 §6 `hello`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OfferWire {
    /// e.g. `agent:coder` — OpenFlow uses the agent's existing `binding_id`.
    pub action_id: String,
    pub label: String,
    /// Display only; the service never uses it to decide anything.
    pub project: String,
    /// Member ids allowed to invoke this offer. The service enforces a copy of
    /// this as a convenience; the host re-checks and is the real boundary.
    pub allowed: Vec<String>,
}

/// Host → service (DESIGN-relay-v02 §6).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMessage {
    /// Republished on every connect. The service replaces this host's offers
    /// wholesale, so a removed grant disappears immediately.
    Hello {
        offers: Vec<OfferWire>,
    },
    Frame {
        session_id: String,
        /// Duplicated from the payload's own tag so the service can index and
        /// audit without parsing. DERIVED — never hand-written. See
        /// `session_frame`.
        kind: String,
        payload: Value,
        /// DESIGN-relay-v02 §8: always `false` in v0.2. The wire shape does not
        /// change when sealing lands; only this flag and the payload bytes do.
        sealed: bool,
    },
    Closed {
        session_id: String,
        outcome: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Requester {
    #[serde(default)]
    pub member_id: String,
    #[serde(default)]
    pub display_name: String,
}

/// Service → host (DESIGN-relay-v02 §6), parsed tolerantly: a newer service
/// must never be able to kill the host loop.
///
/// Not modeled here: the `{"t":"ready","offers":N}` ack the service sends
/// after `hello` (confirmed for real — see `real_captured_frames` below, and
/// the service's own `ServiceFrame::Ready` in its `protocol.rs`). It falls
/// through to `Unknown` exactly like any other message this version does not
/// know, which is deliberate: this task defines the wire *types*, not the
/// connection loop that would await the ack. A later transport task either
/// adds a `Ready` variant here or matches on the raw JSON before typed parse.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServiceMessage {
    #[serde(rename_all = "snake_case")]
    Open {
        session_id: String,
        offer_id: String,
        /// NOT in DESIGN-relay-v02 §6 as frozen. See "spec gaps" #1 in
        /// documentation/design/shared-agents/PLAN.md: when the service supplies
        /// it, mapping an `open` back to a grant is direct; when it does not,
        /// the host resolves `offer_id` via `GET /v2/offers`. Confirmed present
        /// on every real `open` the service actually sends (it is a required
        /// field on the service's own `ServiceFrame::Open`); kept optional here
        /// anyway so an older service cannot crash this host.
        #[serde(default)]
        action_id: Option<String>,
        #[serde(default)]
        requester: Requester,
        #[serde(default)]
        payload: Value,
    },
    Stop {
        session_id: String,
    },
    /// Anything this version does not model. Logged and ignored — never fatal.
    #[serde(other)]
    Unknown,
}

/// What a requester sends to start a run. Not specified by either design doc
/// (see "spec gaps" #2); defined here, tolerantly, so the `curl` teammate
/// harness can pass a bare string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPayload {
    pub instruction: String,
}

pub fn parse_open_payload(v: &Value) -> Result<OpenPayload, String> {
    let instruction = match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("instruction")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => return Err("the session payload is neither a string nor an object".to_string()),
    };
    if instruction.trim().is_empty() {
        return Err("the session payload carries no instruction".to_string());
    }
    Ok(OpenPayload { instruction })
}

// ---- Reserved C0 shapes (see the module doc). Field-for-field from
// `feat/acp-agents:src-tauri/src/acp/events.rs` — verified against that file
// directly, not from `documentation/design/acp-agents/PLAN.md`, which is
// stale for exactly these shapes (corrected 2026-08-04). ----

/// = C0's `PlanEntry`, unchanged from the plan doc — checked against the code
/// anyway, since that's what's now authoritative.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PlanEntryWire {
    pub content: String,
    pub priority: String,
    pub status: String,
}

/// = C0's `PermissionOption`, unchanged from the plan doc.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PermissionOptionWire {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// The frame vocabulary carried inside an envelope's opaque payload.
/// **Open and versioned:** only `Header`, `Output` and `Status` are emitted in
/// v1 (`V1_EMITTED_KINDS`). The rest are RESERVED for PR #64 (DESIGN §7).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostFrame {
    Header {
        agent: String,
        project: String,
    },
    /// One raw stdout/stderr line — v1's only content frame.
    Output {
        chunk: String,
    },
    /// Terminal. A rendered `RunStatus`, not the enum, so the requester never
    /// needs OpenFlow's types.
    Status {
        status: String,
    },

    // ---- Reserved, unemitted in v1. Names and shapes copied verbatim from
    // C0's ACTUAL CODE, `feat/acp-agents:src-tauri/src/acp/events.rs`
    // (`RunEvent`) — not from `documentation/design/acp-agents/PLAN.md` Task 3,
    // which is stale for `ToolCallUpdate` and `PermissionRequest` below
    // (corrected 2026-08-04). ----
    Text {
        text: String,
    },
    Thought {
        text: String,
    },
    Plan {
        entries: Vec<PlanEntryWire>,
    },
    ToolCall {
        id: String,
        title: String,
        tool_kind: String,
        status: String,
        locations: Vec<String>,
    },
    /// A REFINEMENT of an existing `ToolCall`, not a replacement. Every field
    /// but `id` is `Option` and `None` means "the agent said nothing about
    /// this — leave it alone", never "reset it" (copied verbatim from C0's own
    /// doc comment on `RunEvent::ToolCallUpdate`). The plan doc's version of
    /// this variant had only `{ id, status: String, content: Option<String> }`
    /// — three fields, `status` required — which is what C0's code looked like
    /// *before* its own final review added `title`/`tool_kind`/`locations` and
    /// made `status` optional too.
    ToolCallUpdate {
        id: String,
        status: Option<String>,
        title: Option<String>,
        tool_kind: Option<String>,
        locations: Option<Vec<String>>,
        content: Option<String>,
    },
    /// `tool_kind` and `locations` are carried HERE rather than left to a
    /// frontend join on `tool_call_id` (copied verbatim from C0's own doc
    /// comment on `RunEvent::PermissionRequest`). ACP permits a permission
    /// request with no preceding `tool_call` at all, and the join then
    /// silently misses — leaving the card showing a bare title like "Edit
    /// file" with no indication of WHAT it touches, above an Allow button. The
    /// agent handed us both fields in the request itself; throwing them away
    /// and guessing them back is the one thing a permission gate must not do.
    /// The plan doc's version of this variant omitted both fields entirely —
    /// exactly the loss C0's final review closed, and exactly what this
    /// module's own reservation exists to carry forward without a wire break.
    PermissionRequest {
        request_id: String,
        tool_call_id: Option<String>,
        title: String,
        tool_kind: String,
        locations: Vec<String>,
        options: Vec<PermissionOptionWire>,
    },
    /// DESIGN §7's list omits this one; C0 defines it. Reserved anyway —
    /// omitting it is precisely the wire break §7 exists to prevent.
    PermissionResolved {
        request_id: String,
        outcome: String,
        automatic: bool,
    },
    TurnEnd {
        stop_reason: String,
    },
}

impl HostFrame {
    /// The envelope `kind` for this frame. Must equal the frame's own serde tag
    /// — pinned by `envelope_kind_never_drifts_from_the_frames_own_tag`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            HostFrame::Header { .. } => "header",
            HostFrame::Output { .. } => "output",
            HostFrame::Status { .. } => "status",
            HostFrame::Text { .. } => "text",
            HostFrame::Thought { .. } => "thought",
            HostFrame::Plan { .. } => "plan",
            HostFrame::ToolCall { .. } => "tool_call",
            HostFrame::ToolCallUpdate { .. } => "tool_call_update",
            HostFrame::PermissionRequest { .. } => "permission_request",
            HostFrame::PermissionResolved { .. } => "permission_resolved",
            HostFrame::TurnEnd { .. } => "turn_end",
        }
    }
}

/// Wrap a frame in a session envelope. The ONLY way a `HostMessage::Frame` is
/// constructed, so `kind` can never be typed by hand and drift from the payload.
pub fn session_frame(session_id: &str, frame: HostFrame) -> HostMessage {
    let kind = frame.kind_str().to_string();
    HostMessage::Frame {
        session_id: session_id.to_string(),
        kind,
        payload: serde_json::to_value(&frame).unwrap_or(Value::Null),
        sealed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hello_serializes_to_the_c1_envelope() {
        // DESIGN-relay-v02 §6: {"t":"hello","offers":[{action_id,label,project,allowed}]}
        let m = HostMessage::Hello {
            offers: vec![OfferWire {
                action_id: "agent:coder".into(),
                label: "Coder".into(),
                project: "/repo/site".into(),
                allowed: vec!["m-priya".into()],
            }],
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["t"], json!("hello"));
        assert_eq!(v["offers"][0]["action_id"], json!("agent:coder"));
        assert_eq!(v["offers"][0]["allowed"][0], json!("m-priya"));
    }

    #[test]
    fn every_session_frame_carries_sealed_false() {
        // DESIGN-relay-v02 §8: v0.2 is NOT end-to-end encrypted, and the wire
        // shape must not change when sealing lands — only the flag and bytes do.
        let m = session_frame(
            "s1",
            HostFrame::Output {
                chunk: "hello".into(),
            },
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["t"], json!("frame"));
        assert_eq!(v["session_id"], json!("s1"));
        assert_eq!(v["sealed"], json!(false));
        assert_eq!(v["kind"], json!("output"));
        // The payload is the frame itself — opaque to the service, which never
        // parses it (DESIGN-relay-v02 §3).
        assert_eq!(v["payload"]["chunk"], json!("hello"));
    }

    #[test]
    fn envelope_kind_never_drifts_from_the_frames_own_tag() {
        // The C0 lesson: two representations of the same value drift. The
        // envelope's `kind` is DERIVED from the frame, and this test proves the
        // derivation matches serde for every variant we can construct.
        for f in all_frames() {
            let envelope = serde_json::to_value(session_frame("s", f.clone())).unwrap();
            let payload = serde_json::to_value(&f).unwrap();
            assert_eq!(
                envelope["kind"], payload["kind"],
                "envelope kind must equal the frame's own serde tag for {f:?}"
            );
            assert_eq!(envelope["kind"], json!(f.kind_str()));
        }
    }

    #[test]
    fn v1_frames_pin_their_wire_kinds() {
        assert_eq!(
            HostFrame::Header {
                agent: "Coder".into(),
                project: "/r".into()
            }
            .kind_str(),
            "header"
        );
        assert_eq!(
            HostFrame::Output {
                chunk: String::new()
            }
            .kind_str(),
            "output"
        );
        assert_eq!(
            HostFrame::Status {
                status: "finished".into()
            }
            .kind_str(),
            "status"
        );
    }

    #[test]
    fn reserved_c0_variants_pin_the_kinds_that_merging_pr_64_will_emit() {
        // DESIGN-shared-agents §7: reserve C0's vocabulary so merging #64 adds
        // capability with NO wire break. Source of these names AND shapes:
        // `feat/acp-agents:src-tauri/src/acp/events.rs` (RunEvent,
        // #[serde(tag = "kind", rename_all = "snake_case")]) — the actual code,
        // not `documentation/design/acp-agents/PLAN.md`, which is stale for
        // `ToolCallUpdate`/`PermissionRequest` (corrected 2026-08-04; see
        // `reserved_variants_carry_every_field_c0_actually_emits` below for the
        // full-field pin).
        assert_eq!(
            HostFrame::Text {
                text: String::new()
            }
            .kind_str(),
            "text"
        );
        assert_eq!(
            HostFrame::Thought {
                text: String::new()
            }
            .kind_str(),
            "thought"
        );
        assert_eq!(HostFrame::Plan { entries: vec![] }.kind_str(), "plan");
        assert_eq!(
            HostFrame::ToolCall {
                id: String::new(),
                title: String::new(),
                tool_kind: String::new(),
                status: String::new(),
                locations: vec![],
            }
            .kind_str(),
            "tool_call"
        );
        assert_eq!(
            HostFrame::ToolCallUpdate {
                id: String::new(),
                status: None,
                title: None,
                tool_kind: None,
                locations: None,
                content: None,
            }
            .kind_str(),
            "tool_call_update"
        );
        assert_eq!(
            HostFrame::PermissionRequest {
                request_id: String::new(),
                tool_call_id: None,
                title: String::new(),
                tool_kind: String::new(),
                locations: vec![],
                options: vec![],
            }
            .kind_str(),
            "permission_request"
        );
        assert_eq!(
            HostFrame::PermissionResolved {
                request_id: String::new(),
                outcome: String::new(),
                automatic: false,
            }
            .kind_str(),
            "permission_resolved"
        );
        assert_eq!(
            HostFrame::TurnEnd {
                stop_reason: String::new()
            }
            .kind_str(),
            "turn_end"
        );
    }

    #[test]
    fn reserved_variants_carry_every_field_c0_actually_emits() {
        // Critical review finding: the plan doc this module originally cited
        // was stale. C0's own final review enriched `ToolCallUpdate` (three
        // fields, `status` required -> six fields, `status` optional) and
        // `PermissionRequest` (added `tool_kind`/`locations`, without which "a
        // permission card could ask a user to approve an action without
        // showing which file it touches"). This test serializes a
        // fully-populated instance of each and asserts every field C0's real
        // `RunEvent` carries is present on the wire by name — not just the
        // `kind` tag `reserved_c0_variants_pin_the_kinds...` already covers.
        // Renaming or dropping any key below is exactly the wire break this
        // reservation exists to prevent; see the break-and-revert evidence in
        // the task report for a field rename caught by this test, and a field
        // removal caught at compile time (Rust's exhaustive struct literals
        // make a silent drop here impossible, which is stronger than a
        // runtime check).
        let tcu = serde_json::to_value(HostFrame::ToolCallUpdate {
            id: "t1".into(),
            status: Some("completed".into()),
            title: Some("Edit file".into()),
            tool_kind: Some("edit".into()),
            locations: Some(vec!["/a/b.rs".into()]),
            content: Some("diff".into()),
        })
        .unwrap();
        assert_eq!(tcu["kind"], json!("tool_call_update"));
        assert_eq!(tcu["id"], json!("t1"));
        assert_eq!(tcu["status"], json!("completed"));
        assert_eq!(tcu["title"], json!("Edit file"));
        assert_eq!(tcu["tool_kind"], json!("edit"));
        assert_eq!(tcu["locations"], json!(["/a/b.rs"]));
        assert_eq!(tcu["content"], json!("diff"));

        let pr = serde_json::to_value(HostFrame::PermissionRequest {
            request_id: "r1".into(),
            tool_call_id: Some("t1".into()),
            title: "Edit file".into(),
            tool_kind: "edit".into(),
            locations: vec!["/a/b.rs".into()],
            options: vec![],
        })
        .unwrap();
        assert_eq!(pr["kind"], json!("permission_request"));
        assert_eq!(pr["request_id"], json!("r1"));
        assert_eq!(pr["tool_call_id"], json!("t1"));
        assert_eq!(pr["title"], json!("Edit file"));
        assert_eq!(
            pr["tool_kind"],
            json!("edit"),
            "without tool_kind, a permission card cannot say WHAT kind of \
             action it is approving"
        );
        assert_eq!(
            pr["locations"],
            json!(["/a/b.rs"]),
            "without locations, a permission card cannot say WHICH file it \
             touches — the exact gap C0's final review closed"
        );
    }

    #[test]
    fn v1_emits_only_header_output_and_status() {
        // NOT a regression guard: this asserts the const against its own
        // literal, so nothing external can make it fail — a review correctly
        // flagged this. It exists purely as a documentation anchor: a reader
        // who greps `V1_EMITTED_KINDS` lands on a named test that states the
        // v1/reserved boundary in prose, right next to the tests that DO
        // enforce something (`v1_frames_pin_their_wire_kinds` and
        // `reserved_c0_variants_pin_the_kinds_that_merging_pr_64_will_emit`).
        // If a later change adds an emitter for a reserved variant, editing
        // this constant (and this test) is where that decision becomes
        // visible in a diff.
        assert_eq!(V1_EMITTED_KINDS, ["header", "output", "status"]);
    }

    #[test]
    fn open_is_parsed_with_both_action_id_and_offer_id() {
        // See "spec gaps" #1: the host must tolerate an `open` that carries
        // action_id AND one that carries only offer_id.
        let with_action = json!({
            "t": "open", "session_id": "s1", "offer_id": "o1",
            "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": {"instruction": "add a comment to README"}
        });
        match serde_json::from_value::<ServiceMessage>(with_action).unwrap() {
            ServiceMessage::Open {
                session_id,
                offer_id,
                action_id,
                requester,
                payload,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(offer_id, "o1");
                assert_eq!(action_id.as_deref(), Some("agent:coder"));
                assert_eq!(requester.display_name, "Priya");
                assert_eq!(
                    parse_open_payload(&payload).unwrap().instruction,
                    "add a comment to README"
                );
            }
            m => panic!("expected Open, got {m:?}"),
        }

        let without_action = json!({
            "t": "open", "session_id": "s2", "offer_id": "o1",
            "requester": {"member_id": "m", "display_name": "M"},
            "payload": {"instruction": "hi"}
        });
        match serde_json::from_value::<ServiceMessage>(without_action).unwrap() {
            ServiceMessage::Open { action_id, .. } => assert!(action_id.is_none()),
            m => panic!("expected Open, got {m:?}"),
        }
    }

    #[test]
    fn open_payload_accepts_a_bare_string_for_the_curl_harness() {
        assert_eq!(
            parse_open_payload(&json!("do the thing"))
                .unwrap()
                .instruction,
            "do the thing"
        );
        assert_eq!(
            parse_open_payload(&json!({"instruction": "x"}))
                .unwrap()
                .instruction,
            "x"
        );
        // An empty or shapeless payload is a clear error, never a blank run.
        assert!(parse_open_payload(&json!({})).is_err());
        assert!(parse_open_payload(&json!({"instruction": "   "})).is_err());
        assert!(parse_open_payload(&json!(42)).is_err());
    }

    #[test]
    fn stop_parses_and_an_unknown_message_type_is_data_not_a_crash() {
        let stop = json!({"t": "stop", "session_id": "s1"});
        assert!(matches!(
            serde_json::from_value::<ServiceMessage>(stop).unwrap(),
            ServiceMessage::Stop { .. }
        ));
        // A newer service must never be able to kill this loop.
        let future = json!({"t": "ping_v3", "whatever": true});
        assert!(matches!(
            serde_json::from_value::<ServiceMessage>(future).unwrap(),
            ServiceMessage::Unknown
        ));
    }

    #[test]
    fn closed_reports_a_terminal_outcome() {
        let v = serde_json::to_value(HostMessage::Closed {
            session_id: "s1".into(),
            outcome: "finished".into(),
        })
        .unwrap();
        assert_eq!(v["t"], json!("closed"));
        assert_eq!(v["outcome"], json!("finished"));
    }

    fn all_frames() -> Vec<HostFrame> {
        vec![
            HostFrame::Header {
                agent: "A".into(),
                project: "/p".into(),
            },
            HostFrame::Output { chunk: "c".into() },
            HostFrame::Status {
                status: "finished".into(),
            },
            HostFrame::Text { text: "t".into() },
            HostFrame::Thought { text: "t".into() },
            HostFrame::Plan { entries: vec![] },
            HostFrame::ToolCall {
                id: "1".into(),
                title: "T".into(),
                tool_kind: "read".into(),
                status: "pending".into(),
                locations: vec![],
            },
            HostFrame::ToolCallUpdate {
                id: "1".into(),
                status: Some("done".into()),
                title: None,
                tool_kind: None,
                locations: None,
                content: None,
            },
            HostFrame::PermissionRequest {
                request_id: "r".into(),
                tool_call_id: None,
                title: "T".into(),
                tool_kind: "edit".into(),
                locations: vec!["/a/b.rs".into()],
                options: vec![],
            },
            HostFrame::PermissionResolved {
                request_id: "r".into(),
                outcome: "allow".into(),
                automatic: false,
            },
            HostFrame::TurnEnd {
                stop_reason: "completed".into(),
            },
        ]
    }

    /// Bytes captured from a REAL running `openflow-service` (branch
    /// `feat/relay-v0.2`), not hand-written: a paired host + teammate, a real
    /// WebSocket `hello`, and a real `POST /v2/sessions`. This is exactly the
    /// class of evidence the sibling C0 project skipped — every one of its
    /// fixtures was written from a spec and nothing real ever answered back —
    /// and these two constants are fed directly into this crate's own
    /// `ServiceMessage` deserializer below, not into hand-written JSON.
    ///
    /// Corrected 2026-08-04 (review Critical 2): this module used to also
    /// carry `RAW_SSE_FRAME`/`RAW_SSE_CLOSED` and two more tests claiming to
    /// verify `HostFrame`'s round trip against them. That claim was false.
    /// `HostFrame` is internally tagged (`#[serde(tag = "kind")]`), so its
    /// `kind` lives INSIDE the payload; the demo host that produced those two
    /// bytes strings (`openflow-service/examples/fake_host.rs`) is a hand-
    /// rolled script with no dependency on this crate and sends a FLAT
    /// payload with no `kind` key at all. `serde_json::from_value::<HostFrame>`
    /// on that flat shape fails with `missing field "kind"` — the two removed
    /// tests never actually deserialized it into a `HostFrame`; one hand-built
    /// a *new* `json!` value with the key injected, the other indexed a bare
    /// `Value` and constructed no type from this crate at all. A genuine
    /// capture of `HostFrame`/`session_frame`'s own output requires something
    /// in this crate to actually send it over a socket — `protocol.rs` does no
    /// I/O by design (see `relay/mod.rs`'s layering doc). That sender now
    /// exists (`managers::agent_host`'s host loop), so the capture is owed by
    /// the live end-to-end task, not by this module. Removed rather than left
    /// staged.
    mod real_captured_frames {
        use super::*;

        const RAW_READY: &str = r#"{"t":"ready","offers":1}"#;
        const RAW_OPEN: &str = r#"{"t":"open","session_id":"d412c101-4783-4bd6-8206-d92a56c7b3ee","offer_id":"7e1662d2-c44c-425a-9102-b97db756ff9f","action_id":"agent:coder","requester":{"member_id":"68ae978a-73e8-4aff-99ab-98270cbe8cb6","display_name":"Capture Teammate"},"sealed":false,"payload":{"instruction":"add a comment to README"}}"#;

        #[test]
        fn a_real_ready_ack_does_not_crash_the_service_message_parser() {
            // The service's own `ServiceFrame::Ready` (its protocol.rs) is not
            // modeled by our `ServiceMessage` in this task — see the doc comment
            // on `ServiceMessage`. It must still parse as data, never an error.
            match serde_json::from_str::<ServiceMessage>(RAW_READY).unwrap() {
                ServiceMessage::Unknown => {}
                other => panic!("expected Unknown for a real `ready` ack, got {other:?}"),
            }
        }

        #[test]
        fn a_real_open_frame_from_the_live_service_parses_with_both_ids() {
            match serde_json::from_str::<ServiceMessage>(RAW_OPEN).unwrap() {
                ServiceMessage::Open {
                    session_id,
                    offer_id,
                    action_id,
                    requester,
                    payload,
                } => {
                    assert_eq!(session_id, "d412c101-4783-4bd6-8206-d92a56c7b3ee");
                    assert_eq!(offer_id, "7e1662d2-c44c-425a-9102-b97db756ff9f");
                    assert_eq!(action_id.as_deref(), Some("agent:coder"));
                    assert_eq!(requester.display_name, "Capture Teammate");
                    assert_eq!(
                        parse_open_payload(&payload).unwrap().instruction,
                        "add a comment to README"
                    );
                }
                other => panic!("expected Open for a real captured `open`, got {other:?}"),
            }
        }
    }
}
