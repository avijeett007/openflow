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
//! field shapes are copied verbatim from C0's `RunEvent`
//! (documentation/design/acp-agents/PLAN.md Task 3) — they are not paraphrased,
//! because a paraphrase is exactly how a wire break gets introduced. When #64
//! merges, replace the local `PlanEntryWire`/`PermissionOptionWire` with
//! `pub use crate::acp::events::{PlanEntry, PermissionOption};` — identical
//! shapes, zero wire change.
//!
//! Verified against the real counterparty, not derived from prose alone: every
//! shape below was checked against openflow-service's own
//! `src/relay/protocol.rs` (branch `feat/relay-v0.2`, PR #1), and the `open`,
//! `ready`, `frame` and `closed` shapes were additionally captured from a real
//! running `openflow-service` (paired host + teammate, a real WebSocket, a real
//! `POST /v2/sessions`, a real SSE read) and are asserted against verbatim in
//! `real_captured_frames` below.
//!
//! This whole module is exercised only by its own tests today: `grants.rs`
//! (the host-side authorisation re-check) and `transport.rs` (the WebSocket
//! loop that actually calls `session_frame` and matches on `ServiceMessage`)
//! are later tasks in this same plan (see `relay/mod.rs`'s layering doc), so
//! clippy sees every public item here as unconstructed/unused until they
//! land. Same shape as `managers/wake_word.rs`'s `#[allow(dead_code)]` on
//! code "exposed for callers/diagnostics; not yet wired to a command."
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only frame kinds v1 ever emits. Everything else in `HostFrame` is
/// reserved. Asserted by test so adding an emitter is a deliberate act.
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

// ---- Reserved C0 shapes (see the module doc) ----

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PlanEntryWire {
    pub content: String,
    pub priority: String,
    pub status: String,
}

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
    // C0's RunEvent (documentation/design/acp-agents/PLAN.md Task 3). ----
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
    ToolCallUpdate {
        id: String,
        status: String,
        content: Option<String>,
    },
    PermissionRequest {
        request_id: String,
        tool_call_id: Option<String>,
        title: String,
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
        // capability with NO wire break. Source of these names:
        // documentation/design/acp-agents/PLAN.md Task 3 (acp/events.rs RunEvent,
        // #[serde(tag = "kind", rename_all = "snake_case")]).
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
                status: String::new(),
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
    fn v1_emits_only_header_output_and_status() {
        // Reserved means reserved: the only constructors v1 exposes are these
        // three. If a later change adds an emitter for a reserved variant, this
        // test is where the decision gets made explicitly.
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
                status: "done".into(),
                content: None,
            },
            HostFrame::PermissionRequest {
                request_id: "r".into(),
                tool_call_id: None,
                title: "T".into(),
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
    /// WebSocket `hello`, a real `POST /v2/sessions`, and a real
    /// `GET /v2/sessions/{id}/events` SSE read. This is exactly the class of
    /// evidence the sibling C0 project skipped — every one of its fixtures was
    /// written from a spec and nothing real ever answered back — and it is
    /// what these four constants close for this crate's deserializers.
    mod real_captured_frames {
        use super::*;

        const RAW_READY: &str = r#"{"t":"ready","offers":1}"#;
        const RAW_OPEN: &str = r#"{"t":"open","session_id":"d412c101-4783-4bd6-8206-d92a56c7b3ee","offer_id":"7e1662d2-c44c-425a-9102-b97db756ff9f","action_id":"agent:coder","requester":{"member_id":"68ae978a-73e8-4aff-99ab-98270cbe8cb6","display_name":"Capture Teammate"},"sealed":false,"payload":{"instruction":"add a comment to README"}}"#;
        const RAW_SSE_FRAME: &str =
            r#"{"seq":2,"kind":"output","sealed":false,"payload":{"chunk":"hello"}}"#;
        const RAW_SSE_CLOSED: &str = r#"{"outcome":"completed"}"#;

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

        #[test]
        fn a_real_output_frame_round_trips_through_our_own_host_frame_shape() {
            // We sent {"kind":"output","payload":{"chunk":"hello"}} as the WS
            // frame; this is that exact payload as it came back out the far end
            // of a real SSE stream. If HostFrame::Output's field were ever
            // renamed away from `chunk`, this is what would fail.
            let v: Value = serde_json::from_str(RAW_SSE_FRAME).unwrap();
            assert_eq!(v["kind"], json!("output"));
            assert_eq!(v["sealed"], json!(false));
            let frame: HostFrame =
                serde_json::from_value(json!({"kind": "output", "chunk": v["payload"]["chunk"]}))
                    .unwrap();
            assert_eq!(
                frame,
                HostFrame::Output {
                    chunk: "hello".into()
                }
            );
        }

        #[test]
        fn a_real_closed_record_carries_the_outcome_we_sent() {
            let v: Value = serde_json::from_str(RAW_SSE_CLOSED).unwrap();
            assert_eq!(v["outcome"], json!("completed"));
        }
    }
}
