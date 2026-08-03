//! Replay of REAL agent frames through OpenFlow's own deserializers.
//!
//! # Why this file exists
//!
//! Twice on this branch a wire assumption was taken from `DESIGN-acp-agents.md`
//! (written from prose) instead of from `@agentclientprotocol/sdk`'s
//! `schema/schema.json` (shipped inside the adapters themselves), and twice it
//! was wrong:
//!
//! 1. `StopReason` used an invented vocabulary, so every SUCCESSFUL run
//!    rendered as `Failed`. 465 tests were green throughout.
//! 2. `ToolCallUpdate.status` was modelled as always-present, so Claude Code's
//!    status-less refinements were read as `status: ""` — overwriting the tool
//!    call's real status in the UI, writing a bare `"  ✓ "` into the permanent
//!    File-sink record, and silently discarding the resolved file path the
//!    refinement existed to deliver.
//!
//! Both survived because every fixture in the suite was hand-written from the
//! same prose. A self-consistent suite built on an invented wire vocabulary is
//! indistinguishable from a correct one until something real answers back.
//!
//! `fixtures/real-agent-frames.jsonl` is that something real: one **unedited**
//! JSON-RPC frame per line, captured verbatim from live agents over stdio —
//! `kimi 0.31.0` (`kimi acp`) and `claude-agent-acp 0.64.2`
//! (`npx -y @agentclientprotocol/claude-agent-acp`). Nothing in it was written
//! by us. Deduplicated by (agent, method, update kind, exact key set), so every
//! line is a SHAPE this crate must survive, not a repetition.
//!
//! Capture procedure and provenance: `verification/acp-agents/RESULTS.md` §11.

use serde_json::Value;

use crate::acp::events::{map_session_update, render_line, RunEvent};
use crate::acp::protocol::{RequestPermissionParams, SessionNotification, SessionUpdate};

/// The raw capture. `include_str!` on purpose: no I/O, no path resolution, and
/// the fixture cannot silently go missing at runtime — deleting it is a
/// compile error.
const FIXTURE: &str = include_str!("fixtures/real-agent-frames.jsonl");

struct Frame {
    method: String,
    params: Value,
}

fn frames() -> Vec<Frame> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| {
                panic!("fixture line is not valid JSON ({e}); it must be a verbatim capture: {l}")
            });
            Frame {
                method: v["method"].as_str().unwrap_or_default().to_string(),
                params: v["params"].clone(),
            }
        })
        .collect()
}

/// Guard against the fixture being silently emptied or truncated: this test's
/// whole value is that it replays a broad set of real shapes.
#[test]
fn the_fixture_holds_real_frames_from_both_agents_and_both_methods() {
    let frames = frames();
    assert!(
        frames.len() >= 25,
        "the capture should cover a broad set of shapes, got {}",
        frames.len()
    );
    assert!(
        frames.iter().any(|f| f.method == "session/update"),
        "no session/update frames captured"
    );
    assert!(
        frames
            .iter()
            .filter(|f| f.method == "session/request_permission")
            .count()
            >= 2,
        "need permission requests from BOTH agents — Kimi's is the one with no `kind`"
    );
}

/// **The structural guard.** Every captured frame must deserialize through the
/// exact types the production driver uses. Before MF1's fix, the Claude Code
/// refinement frames in this file deserialized "successfully" into a LIE
/// (`status: ""`); `every_captured_frame_preserves_what_the_agent_actually_said`
/// below is what catches that. This one catches outright rejection.
#[test]
fn every_captured_frame_deserializes_through_the_production_types() {
    for (i, f) in frames().iter().enumerate() {
        match f.method.as_str() {
            "session/update" => {
                let n: SessionNotification = serde_json::from_value(f.params.clone())
                    .unwrap_or_else(|e| {
                        panic!(
                            "line {}: a REAL session/update frame failed to deserialize: {e}\n{}",
                            i + 1,
                            f.params
                        )
                    });
                assert_eq!(
                    n.session_id,
                    f.params["sessionId"].as_str().unwrap_or_default(),
                    "line {}: sessionId must survive verbatim",
                    i + 1
                );
                assert!(!n.session_id.is_empty(), "line {}: sessionId lost", i + 1);
                // Unmodelled variants are DROPPED, never fatal; modelled ones
                // must always render a text line (the dual-emission contract).
                if let Some(e) = map_session_update(&n.update) {
                    assert!(
                        render_line(&e).is_some(),
                        "line {}: {e:?} produced no text line",
                        i + 1
                    );
                }
            }
            "session/request_permission" => {
                let p: RequestPermissionParams = serde_json::from_value(f.params.clone())
                    .unwrap_or_else(|e| {
                        panic!(
                            "line {}: a REAL session/request_permission frame failed to \
                             deserialize: {e}\n{}",
                            i + 1,
                            f.params
                        )
                    });
                assert!(
                    !p.options.is_empty(),
                    "line {}: a permission request with no options is unanswerable",
                    i + 1
                );
            }
            other => panic!("line {}: unexpected captured method {other:?}", i + 1),
        }
    }
}

/// **The MF1 regression, driven by real bytes.** For every captured frame, a
/// field the agent did NOT send must come out absent, and a field it DID send
/// must come out with that value. Modelling `status` as a plain `String` makes
/// the first half fail on Claude Code's refinements; not modelling
/// `title`/`kind`/`locations` at all makes the second half fail on the same
/// frames.
#[test]
fn every_captured_frame_preserves_what_the_agent_actually_said() {
    let mut status_less_refinements = 0usize;
    let mut refinements_carrying_a_path = 0usize;

    for (i, f) in frames().iter().enumerate() {
        if f.method != "session/update" {
            continue;
        }
        let raw = &f.params["update"];
        if raw["sessionUpdate"] != "tool_call_update" {
            continue;
        }
        let n: SessionNotification = serde_json::from_value(f.params.clone()).unwrap();
        let SessionUpdate::ToolCallUpdate {
            status,
            title,
            kind,
            locations,
            ..
        } = &n.update
        else {
            panic!(
                "line {}: tool_call_update did not map to its variant",
                i + 1
            );
        };

        // Absent on the wire => absent in our model. Never "".
        assert_eq!(
            status.is_some(),
            raw.get("status").is_some_and(|v| !v.is_null()),
            "line {}: `status` presence must match the wire exactly — a status the agent \
             never sent must not be invented (it overwrites the tool call's real one and \
             is written into the permanent File-sink record)\n{raw}",
            i + 1
        );
        assert_eq!(
            title.is_some(),
            raw.get("title").is_some_and(|v| !v.is_null()),
            "line {}: `title` presence must match the wire\n{raw}",
            i + 1
        );
        assert_eq!(
            kind.is_some(),
            raw.get("kind").is_some_and(|v| !v.is_null()),
            "line {}: `kind` presence must match the wire\n{raw}",
            i + 1
        );
        assert_eq!(
            locations.is_some(),
            raw.get("locations").is_some_and(|v| !v.is_null()),
            "line {}: `locations` presence must match the wire — delivering the resolved \
             path is the entire purpose of a refinement\n{raw}",
            i + 1
        );

        // …and the SAME must hold of the `RunEvent` the panel and the File sink
        // actually consume. Checking only `SessionUpdate` would leave
        // `map_session_update` free to fill an absent field back in with a
        // default — which IS the bug, one layer down.
        let Some(RunEvent::ToolCallUpdate {
            title: ev_title,
            tool_kind: ev_kind,
            locations: ev_locations,
            status: ev_status,
            ..
        }) = map_session_update(&n.update)
        else {
            panic!("line {}: tool_call_update did not map to an event", i + 1);
        };
        assert_eq!(
            ev_status.is_some(),
            status.is_some(),
            "line {}: mapping must not invent a status the agent never sent\n{raw}",
            i + 1
        );
        assert_eq!(
            ev_title.is_some(),
            title.is_some(),
            "line {}: mapping must not invent a title\n{raw}",
            i + 1
        );
        assert_eq!(
            ev_kind.is_some(),
            kind.is_some(),
            "line {}: mapping must not invent a kind\n{raw}",
            i + 1
        );
        assert_eq!(
            ev_locations.is_some(),
            locations.is_some(),
            "line {}: mapping must not invent a locations list — an empty one reads as \
             'this tool call touches nothing'\n{raw}",
            i + 1
        );

        // Present on the wire => the SAME value, all the way into the event.
        if let Some(v) = raw.get("status").and_then(|v| v.as_str()) {
            assert_eq!(ev_status.as_deref(), Some(v), "line {}", i + 1);
        }
        if let Some(v) = raw.get("title").and_then(|v| v.as_str()) {
            assert_eq!(ev_title.as_deref(), Some(v), "line {}", i + 1);
        }
        if let Some(v) = raw.get("kind").and_then(|v| v.as_str()) {
            assert_eq!(ev_kind.as_deref(), Some(v), "line {}", i + 1);
        }
        if let Some(paths) = raw.get("locations").and_then(|v| v.as_array()) {
            let expected: Vec<String> = paths
                .iter()
                .map(|l| l["path"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(ev_locations.as_ref(), Some(&expected), "line {}", i + 1);
        }

        if status.is_none() {
            status_less_refinements += 1;
        }
        if status.is_none() && locations.as_ref().is_some_and(|l| !l.is_empty()) {
            refinements_carrying_a_path += 1;
        }
    }

    // These are the shapes that broke us. If a future re-capture drops them the
    // assertions above go vacuous, so pin that they are present.
    assert!(
        status_less_refinements >= 3,
        "the capture must retain Claude Code's status-less refinements — they are the bug \
         (got {status_less_refinements})"
    );
    assert!(
        refinements_carrying_a_path >= 1,
        "the capture must retain at least one status-less refinement that carries the \
         resolved path (got {refinements_carrying_a_path})"
    );
}

/// **The MF2 regression, driven by real bytes.** Kimi 0.31.0 sends a permission
/// request whose `toolCall` is `{toolCallId, title, content}` — *no `kind`*.
/// That must (a) deserialize, and (b) come out as an EMPTY kind rather than a
/// fabricated one, because an empty kind is what stops a persistent
/// "Always allow" from being recorded against a kind the agent never named.
#[test]
fn a_real_permission_request_without_a_kind_parses_and_keeps_the_kind_empty() {
    let mut seen_kindless = false;
    for f in frames() {
        if f.method != "session/request_permission" {
            continue;
        }
        let raw_kind = f.params["toolCall"].get("kind").and_then(|v| v.as_str());
        let p: RequestPermissionParams = serde_json::from_value(f.params.clone()).unwrap();
        match raw_kind {
            Some(k) => assert_eq!(p.tool_call.kind, k),
            None => {
                seen_kindless = true;
                assert_eq!(
                    p.tool_call.kind, "",
                    "an unstated kind must stay empty, never be guessed"
                );
            }
        }
        assert!(
            !p.tool_call.tool_call_id.is_empty(),
            "toolCallId is the one field the schema requires"
        );
    }
    assert!(
        seen_kindless,
        "the capture must retain Kimi's kind-less permission request — it is the F6 case"
    );
}

/// An explicit `null` is a shape the schema permits everywhere it permits
/// absence (`["string","null"]` + `x-deserialize-default-on-error`). No agent
/// in the capture sends one today, so this is the hand-written half — but it
/// covers the case that a bare `#[serde(default)] String` rejects outright,
/// taking the whole frame (and everything it was reporting) down with it.
#[test]
fn an_explicit_null_is_treated_as_absent_not_as_a_parse_failure() {
    let n: SessionNotification = serde_json::from_str(
        r#"{"sessionId":"s1","update":{"sessionUpdate":"tool_call_update",
            "toolCallId":"t1","status":null,"title":null,"kind":null,"locations":null}}"#,
    )
    .expect("an explicit null must not fail the frame");
    assert!(matches!(
        n.update,
        SessionUpdate::ToolCallUpdate {
            status: None,
            title: None,
            kind: None,
            locations: None,
            ..
        }
    ));

    let p: RequestPermissionParams = serde_json::from_str(
        r#"{"sessionId":"s1","toolCall":{"toolCallId":"t1","title":null,"kind":null,
            "status":null,"locations":null},"options":[]}"#,
    )
    .expect("an explicit null must not fail a permission request");
    assert_eq!(p.tool_call.title, "");
    assert_eq!(p.tool_call.kind, "");
    assert!(p.tool_call.locations.is_empty());

    let create: SessionNotification = serde_json::from_str(
        r#"{"sessionId":"s1","update":{"sessionUpdate":"tool_call","toolCallId":"t1",
            "title":null,"kind":null,"status":null,"locations":null}}"#,
    )
    .expect("an explicit null must not fail a tool_call");
    assert!(matches!(create.update, SessionUpdate::ToolCall { .. }));
}
