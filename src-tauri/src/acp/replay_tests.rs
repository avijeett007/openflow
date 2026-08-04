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
//! JSON-RPC frame per line, captured verbatim from live agents over stdio.
//! Nothing in it was written by us. Deduplicated by (agent, method, update kind,
//! exact key set), so every line is a SHAPE this crate must survive, not a
//! repetition.
//!
//! # Provenance, by capture wave
//!
//! | Lines   | Source                                                                     | Date       |
//! | ------- | -------------------------------------------------------------------------- | ---------- |
//! | 1–17    | `claude-agent-acp 0.64.2` (`npx -y @agentclientprotocol/claude-agent-acp`)  | 2026-08-03 |
//! | 18–26   | `kimi 0.31.0` (`kimi acp`)                                                 | 2026-08-03 |
//! | 27      | `claude-agent-acp 0.64.2`, `session_info_update`                           | 2026-08-03 |
//! | 28–29   | `claude-agent-acp 0.64.2` / `kimi 0.31.0` — the `session/prompt` RESPONSES | 2026-08-03 |
//! | 30–41   | `codex-acp 1.1.9` (`npx -y @agentclientprotocol/codex-acp`)                | 2026-08-04 |
//!
//! **The Codex wave (30–41) is why this file grew a third vendor.** Codex is a
//! third independent implementation of the same spec, which is exactly where a
//! divergence hides, and it found two:
//!
//! * Its edit `tool_call` carries **no `locations` at all** — the absolute path
//!   is stated ONLY in a `content` `diff` block. Every Codex file edit therefore
//!   reached the panel and the permanent File-sink record naming no file. Fixed
//!   in `protocol::stated_paths`; replayed by
//!   `a_real_codex_edit_states_its_file_only_in_a_diff_block_and_still_names_it`.
//! * Its `session/request_permission.toolCall` carries **no `title` at all** —
//!   `{toolCallId, kind, status, rawInput}` — so the permanent record logged a
//!   bare `"? "` and the permission CARD rendered an empty headline above an
//!   Allow button. Replayed by
//!   `a_real_codex_permission_request_has_no_title_and_must_still_say_something`.
//!
//! Lines 28–29 and Codex's own response frame are `session/prompt` RESULTS, not
//! notifications: they are the only place a real `stopReason` appears, and
//! `stopReason` is the original scar. Replaying them means the success value of
//! all three vendors is now proved by bytes those vendors wrote.
//!
//! Capture procedure and provenance: `verification/acp-agents/RESULTS.md` §11
//! and §12.

use serde_json::Value;

use crate::acp::events::{map_session_update, render_line, RunEvent};
use crate::acp::protocol::{
    PromptResult, RequestPermissionParams, SessionNotification, SessionUpdate,
};
use crate::managers::agent_run::permission_request_event;

/// The raw capture. `include_str!` on purpose: no I/O, no path resolution, and
/// the fixture cannot silently go missing at runtime — deleting it is a
/// compile error.
const FIXTURE: &str = include_str!("fixtures/real-agent-frames.jsonl");

struct Frame {
    /// A JSON-RPC *response* has no `method`; its `method` is the empty string
    /// here and its payload is `result` rather than `params`.
    method: String,
    params: Value,
    result: Value,
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
                result: v["result"].clone(),
            }
        })
        .collect()
}

/// Guard against the fixture being silently emptied or truncated: this test's
/// whole value is that it replays a broad set of real shapes.
#[test]
fn the_fixture_holds_real_frames_from_all_three_agents_and_every_method() {
    let frames = frames();
    assert!(
        frames.len() >= 40,
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
            >= 3,
        "need permission requests from ALL THREE agents — Kimi's is the one with no `kind`, \
         Codex's the one with no `title`"
    );
    assert!(
        frames
            .iter()
            .filter(|f| f.method.is_empty() && !f.result["stopReason"].is_null())
            .count()
            >= 3,
        "need the `session/prompt` RESPONSE from all three agents — `stopReason` is the \
         original scar and a response frame is the only place it appears"
    );
}

/// **The scar itself, driven by real bytes from three vendors.** Every captured
/// `session/prompt` response must parse to a MODELLED stop reason, and the
/// success value must be `EndTurn`. Before the fix `end_turn` fell through to
/// `Other`, and every successful run was reported as `Failed`.
///
/// Codex's response also carries `usage` and a `_meta.quota` block alongside
/// `stopReason`; a real capture is the only way to prove those extras do not
/// disturb the parse.
#[test]
fn every_captured_prompt_response_yields_a_modelled_stop_reason() {
    let mut checked = 0usize;
    for (i, f) in frames().iter().enumerate() {
        if !f.method.is_empty() || f.result["stopReason"].is_null() {
            continue;
        }
        let wire = f.result["stopReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let r: PromptResult = serde_json::from_value(f.result.clone()).unwrap_or_else(|e| {
            panic!(
                "line {}: a REAL session/prompt response failed to deserialize: {e}\n{}",
                i + 1,
                f.result
            )
        });
        let reason = r.stop_reason.unwrap_or_else(|| {
            panic!(
                "line {}: `stopReason` was present on the wire but lost",
                i + 1
            )
        });
        assert_ne!(
            reason,
            crate::acp::protocol::StopReason::Other,
            "line {}: the agent's real stop reason {wire:?} fell through to `Other` — that is \
             the original defect: `Other` renders a SUCCESSFUL run as Failed",
            i + 1
        );
        if wire == "end_turn" {
            assert_eq!(reason, crate::acp::protocol::StopReason::EndTurn);
        }
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected three vendors' responses, got {checked}"
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
            // A JSON-RPC response (no `method`) — the `session/prompt` result.
            // Covered by `every_captured_prompt_response_yields_a_modelled_stop_reason`.
            "" => assert!(
                !f.result["stopReason"].is_null(),
                "line {}: a captured response must carry a stopReason, else it is not a \
                 shape this crate reads",
                i + 1
            ),
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

/// **Codex divergence #1, driven by real bytes.** `codex-acp` 1.1.9 announces a
/// file edit with a `tool_call` that has **no `locations` key at all**; the
/// absolute path is stated only inside
/// `content: [{"type":"diff","path":…,"oldText":…,"newText":…}]`.
/// `claude-agent-acp` sends both, so a two-vendor capture could not see this.
///
/// Until `protocol::stated_paths` existed, this rendered as a bare
/// `▸ Editing files` — into the run panel AND the permanent File-sink record —
/// naming no file. Reverting `map_session_update`'s `stated_paths(...)` to
/// `locations.iter().map(...)` fails this test.
#[test]
fn a_real_codex_edit_states_its_file_only_in_a_diff_block_and_still_names_it() {
    let mut seen = 0usize;
    for (i, f) in frames().iter().enumerate() {
        if f.method != "session/update" {
            continue;
        }
        let raw = &f.params["update"];
        if raw["sessionUpdate"] != "tool_call" {
            continue;
        }
        // The shape under test: no `locations` on the wire, but a `diff` block
        // that carries a path.
        if raw.get("locations").is_some_and(|v| !v.is_null()) {
            continue;
        }
        let diff_paths: Vec<&str> = raw["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|c| c["type"] == "diff")
                    .filter_map(|c| c["path"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        if diff_paths.is_empty() {
            continue;
        }

        let n: SessionNotification = serde_json::from_value(f.params.clone()).unwrap();
        let Some(RunEvent::ToolCall { locations, .. }) = map_session_update(&n.update) else {
            panic!("line {}: tool_call did not map to an event", i + 1);
        };
        assert_eq!(
            locations,
            diff_paths
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<String>>(),
            "line {}: the agent stated the file it is editing in its `diff` block and we \
             dropped it — the run panel and the permanent record then name no file\n{raw}",
            i + 1
        );
        let line = render_line(&map_session_update(&n.update).unwrap()).unwrap();
        for p in &diff_paths {
            assert!(
                line.contains(p),
                "line {}: {p} never reached the permanent record: {line:?}",
                i + 1
            );
        }
        seen += 1;
    }
    assert!(
        seen >= 1,
        "the capture must retain Codex's location-less edit `tool_call` — it is the \
         third-vendor divergence this wave exists to pin"
    );
}

/// **Codex divergence #2, driven by real bytes.** `codex-acp` 1.1.9's
/// `session/request_permission.toolCall` is `{toolCallId, kind, status,
/// rawInput}` — **no `title`**. (Legal: the schema types that field as a
/// `ToolCallUpdate`, where only `toolCallId` is required. Kimi omits `kind`,
/// Codex omits `title`; between them almost nothing is guaranteed.)
///
/// So the fixture pins the wire truth — the title really is absent, and must
/// NOT be invented — while the rendered record must still say something. This
/// is the shape that produced a bare `"? "` in the File sink and an empty
/// headline above the Allow button in `PermissionPrompt`.
#[test]
fn a_real_codex_permission_request_has_no_title_and_must_still_say_something() {
    let mut seen_titleless = false;
    for (i, f) in frames().iter().enumerate() {
        if f.method != "session/request_permission" {
            continue;
        }
        let raw_title = f.params["toolCall"].get("title").and_then(|v| v.as_str());
        let p: RequestPermissionParams = serde_json::from_value(f.params.clone()).unwrap();
        match raw_title {
            Some(t) => assert_eq!(p.tool_call.title, t, "line {}", i + 1),
            None => {
                seen_titleless = true;
                assert_eq!(
                    p.tool_call.title,
                    "",
                    "line {}: an unstated title must stay empty, never be guessed",
                    i + 1
                );
                assert!(
                    !p.tool_call.kind.is_empty(),
                    "line {}: Codex states a `kind` even though it states no title — that is \
                     what the record falls back to",
                    i + 1
                );
                let e = permission_request_event("req-replay", &p.tool_call, &p.options);
                let line = render_line(&e).unwrap();
                assert_eq!(line, format!("? {}", p.tool_call.kind));
                assert_ne!(
                    line.trim(),
                    "?",
                    "line {}: the permanent record of a security decision must not be blank",
                    i + 1
                );
            }
        }
    }
    assert!(
        seen_titleless,
        "the capture must retain Codex's title-less permission request"
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
