//! Wire types only — no logic, no I/O.
//!
//! **The authority for every shape here is the schema the agents themselves
//! ship** (`@agentclientprotocol/sdk@1.3.0`, `schema/schema.json`), NOT
//! `DESIGN-acp-agents.md`. The design doc was written from prose and has been
//! wrong twice: once on `StopReason`'s vocabulary (see `StopReason` below) and
//! once on `ToolCallUpdate`'s optionality (see `SessionUpdate::ToolCallUpdate`).
//! Both bugs survived a fully green unit suite because every fixture was
//! written from the same prose. `fixtures/real-agent-frames.jsonl` +
//! `replay_tests` are the structural guard against a third.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Deserialize a field that may be **absent, `null`, or present** into `T`,
/// treating the first two as `T::default()`.
///
/// Bare `#[serde(default)]` only covers *absent*: an explicit `"status": null`
/// still fails to deserialize into a `String` and takes the WHOLE frame down.
/// The ACP schema marks almost every optional field
/// `x-deserialize-default-on-error` and types it `["string","null"]`, so a
/// null is a shape the protocol explicitly permits — and one agent sending a
/// single null must never cost us a frame.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// ACP protocol version we implement. Verified against the live agents in V1.
pub const SUPPORTED_PROTOCOL_VERSION: u16 = 1;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
}

impl ClientCapabilities {
    /// v1 declares NO fs/terminal capabilities: the agent runs on the user's own
    /// machine and already has the disk, so mediating its I/O buys visibility,
    /// not security. The real boundary is `session/request_permission`.
    /// DESIGN-acp-agents.md §2. V1 must confirm every target agent falls back to
    /// its own I/O rather than erroring.
    pub fn v1_defaults() -> Self {
        Self {
            fs: FsCapabilities {
                read_text_file: false,
                write_text_file: false,
            },
            terminal: false,
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u16,
    pub client_capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InitializeResult {
    pub protocol_version: u16,
    pub agent_capabilities: Value,
    pub auth_methods: Vec<AuthMethod>,
    pub agent_info: Option<AgentInfo>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams {
    pub cwd: String,
    /// Always empty in C0 — MCP pass-through is C4's concern (§11).
    pub mcp_servers: Vec<Value>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NewSessionResult {
    pub session_id: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text { text: String },
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

/// Why an agent stopped a prompt turn.
///
/// **The variants below are the ACP specification's, verbatim** — taken from the
/// schema the agents themselves ship (`@agentclientprotocol/sdk/schema/schema.json`,
/// `definitions.StopReason`), not from a prose summary of it.
///
/// History worth keeping: this enum previously read `Completed` / `MaxStepsReached` /
/// `RequestTimeout`, names that **do not exist anywhere in ACP**. Only `cancelled`
/// was ever right. Live verification (2026-08-03) showed every real agent returns
/// `end_turn` on success, which fell through to `Other` and rendered a *successful*
/// run as `RunStatus::Failed`. The whole unit suite was green throughout, because
/// its fixtures used the same fictional vocabulary. See
/// `verification/acp-agents/RESULTS.md` §2.
///
/// The fictional names are deliberately **not** kept as aliases: a test that still
/// passes against them is a test that never exercised anything real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// "The turn ended successfully." — the ONLY success value.
    EndTurn,
    /// The agent reached its maximum token budget.
    MaxTokens,
    /// The agent reached the maximum allowed agent requests between user turns.
    MaxTurnRequests,
    /// The agent declined to continue. A deliberate outcome, not a crash.
    Refusal,
    /// The client cancelled via `session/cancel`.
    Cancelled,
    /// Any reason this version does not model. Never fatal — mapped to a plainly
    /// stated failure rather than a panic. This fallback is what kept the
    /// vocabulary bug above from being a hard error, and it stays.
    Other,
}

// NB: `#[serde(other)]` is ONLY legal on internally/adjacently-tagged enums.
// `StopReason` arrives as a bare JSON string, so it is externally tagged and the
// attribute will not compile there — hence the hand-written impl. (`SessionUpdate`
// below IS internally tagged via `tag = "sessionUpdate"`, so it may use it.)
impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "max_turn_requests" => StopReason::MaxTurnRequests,
            "refusal" => StopReason::Refusal,
            "cancelled" => StopReason::Cancelled,
            _ => StopReason::Other,
        })
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptResult {
    pub stop_reason: Option<StopReason>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolLocation {
    pub path: String,
}

/// `session/request_permission`'s `toolCall`. **Schema-typed as a
/// `ToolCallUpdate`, so `toolCallId` is the ONLY required field** — Kimi
/// 0.31.0 really does send a permission request whose `toolCall` carries just
/// `{toolCallId, title, content}` with no `kind` at all (captured verbatim in
/// `fixtures/real-agent-frames.jsonl`). Absent fields become their defaults;
/// see `null_as_default` for why the explicit-`null` case needs help.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolCallWire {
    pub tool_call_id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub title: String,
    /// ACP tool kind: `read` | `edit` | `execute` | `delete` | `move` |
    /// `search` | `fetch` | `think` | `other`. Kept as a String so an unknown
    /// kind from a newer agent is data, not a parse failure. **Empty means the
    /// agent did not say** — never treat that as a real kind (see
    /// `agent_run::is_persistable_kind`).
    #[serde(deserialize_with = "null_as_default")]
    pub kind: String,
    #[serde(deserialize_with = "null_as_default")]
    pub status: String,
    #[serde(deserialize_with = "null_as_default")]
    pub locations: Vec<ToolLocation>,
    pub content: Option<Value>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PlanEntryWire {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TextContent {
    pub text: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk {
        content: TextContent,
    },
    AgentThoughtChunk {
        content: TextContent,
    },
    /// Schema `ToolCall`: `toolCallId` + `title` required, the rest optional.
    /// This is the CREATE, so an absent field genuinely means "empty" and
    /// flattening to a default loses nothing.
    #[serde(rename_all = "camelCase")]
    ToolCall {
        tool_call_id: String,
        #[serde(default, deserialize_with = "null_as_default")]
        title: String,
        #[serde(default, deserialize_with = "null_as_default")]
        kind: String,
        #[serde(default, deserialize_with = "null_as_default")]
        status: String,
        #[serde(default, deserialize_with = "null_as_default")]
        locations: Vec<ToolLocation>,
    },
    /// Schema `ToolCallUpdate`: **`toolCallId` is the only required field**, and
    /// every other one is a genuine three-way — present / absent / null — where
    /// *absent means "leave the existing value alone"*, not "reset it".
    ///
    /// This is why they are `Option` rather than `#[serde(default)]` scalars.
    /// `claude-agent-acp@0.64.2` sends refinement updates carrying `title`,
    /// `kind` and `locations` and **no `status`** — its own doc comment says a
    /// refining update "carries neither" — and modelling `status` as an
    /// always-present `String` turned those into `""`, which the frontend and
    /// `render_line` then wrote OVER the tool call's real `pending`/`completed`.
    /// Delivering the resolved file path is the entire purpose of such a
    /// refinement, and `title`/`kind`/`locations` were not modelled at all, so
    /// it was dropped. Both shapes are in `fixtures/real-agent-frames.jsonl`.
    #[serde(rename_all = "camelCase")]
    ToolCallUpdate {
        tool_call_id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        locations: Option<Vec<ToolLocation>>,
        #[serde(default)]
        content: Option<Value>,
    },
    Plan {
        #[serde(default)]
        entries: Vec<PlanEntryWire>,
    },
    /// Variants we deliberately do not model (available_commands,
    /// config_option_update, current_mode_update) and anything a newer agent
    /// invents. Dropped silently — never fatal.
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    /// Never read: one warm session has exactly one turn in flight (the turn
    /// guard enforces it), so every notification arriving on that session's
    /// pipe belongs to that turn — there is nothing to route by. Kept because
    /// it IS the wire shape, the schema requires it, and dropping it would let
    /// a future multi-session client silently lose the only thing that could
    /// disambiguate. `replay_tests` asserts it survives a real frame.
    #[allow(dead_code)]
    pub session_id: String,
    pub update: SessionUpdate,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PermissionOptionWire {
    pub option_id: String,
    pub name: String,
    /// e.g. `allow_once` | `allow_always` | `reject_once` | `reject_always`.
    pub kind: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestPermissionParams {
    pub session_id: String,
    pub tool_call: ToolCallWire,
    pub options: Vec<PermissionOptionWire>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum PermissionOutcome {
    #[serde(rename_all = "camelCase")]
    Selected {
        option_id: String,
    },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_params_serializes_camel_case() {
        let p = InitializeParams {
            protocol_version: SUPPORTED_PROTOCOL_VERSION,
            client_capabilities: ClientCapabilities::v1_defaults(),
            client_info: ClientInfo {
                name: "OpenFlow".into(),
                version: "0.16.0".into(),
            },
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["protocolVersion"], json!(SUPPORTED_PROTOCOL_VERSION));
        // v1 declares NO fs/terminal capabilities — the agent does its own I/O.
        assert_eq!(v["clientCapabilities"]["fs"]["readTextFile"], json!(false));
        assert_eq!(v["clientCapabilities"]["fs"]["writeTextFile"], json!(false));
        assert_eq!(v["clientCapabilities"]["terminal"], json!(false));
    }

    #[test]
    fn initialize_result_ignores_unknown_fields() {
        let v = json!({
            "protocolVersion": 1,
            "agentCapabilities": { "loadSession": true, "somethingNew": 42 },
            "authMethods": [],
            "unknownTopLevel": "ignored"
        });
        let r: InitializeResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.protocol_version, 1);
        assert!(r.auth_methods.is_empty());
    }

    #[test]
    fn session_update_parses_each_variant() {
        let text = json!({"sessionId":"s1","update":{
            "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}});
        let n: SessionNotification = serde_json::from_value(text).unwrap();
        assert!(matches!(n.update, SessionUpdate::AgentMessageChunk { .. }));

        let tool = json!({"sessionId":"s1","update":{
            "sessionUpdate":"tool_call","toolCallId":"t1","title":"Read file",
            "kind":"read","status":"pending","locations":[{"path":"/a/b.rs"}]}});
        let n: SessionNotification = serde_json::from_value(tool).unwrap();
        assert!(matches!(n.update, SessionUpdate::ToolCall { .. }));

        let unknown = json!({"sessionId":"s1","update":{"sessionUpdate":"brand_new_thing"}});
        let n: SessionNotification = serde_json::from_value(unknown).unwrap();
        assert!(matches!(n.update, SessionUpdate::Unknown));
    }

    /// Every value in the ACP schema's `StopReason` definition, spelled exactly as
    /// the wire spells it. If ACP adds a value, this test is where it lands.
    #[test]
    fn stop_reason_parses_every_spec_value_and_unknown_is_not_fatal() {
        for (wire, expected) in [
            ("end_turn", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxTokens),
            ("max_turn_requests", StopReason::MaxTurnRequests),
            ("refusal", StopReason::Refusal),
            ("cancelled", StopReason::Cancelled),
        ] {
            assert_eq!(
                serde_json::from_value::<StopReason>(json!(wire)).unwrap(),
                expected,
                "ACP wire value {wire:?} must parse to {expected:?}"
            );
        }
        // Unknown must never be fatal.
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("something_new")).unwrap(),
            StopReason::Other
        );
    }

    /// REGRESSION (2026-08-03): these three names were invented by the design doc
    /// and shipped through every task. No agent has ever emitted them. They must
    /// parse as `Other`, NOT be silently accepted as aliases — otherwise a fixture
    /// using them looks like it is testing the real protocol when it is not.
    #[test]
    fn the_fictional_pre_fix_stop_reasons_are_not_recognised() {
        for fictional in ["completed", "max_steps_reached", "request_timeout"] {
            assert_eq!(
                serde_json::from_value::<StopReason>(json!(fictional)).unwrap(),
                StopReason::Other,
                "{fictional:?} is not an ACP stop reason and must not be aliased"
            );
        }
    }

    /// The bug in one line: a real agent's success value must reach the success
    /// variant. Before the fix `end_turn` fell to `Other` → `RunStatus::Failed`.
    #[test]
    fn a_real_agents_success_value_is_the_success_variant() {
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("end_turn")).unwrap(),
            StopReason::EndTurn
        );
        assert_ne!(
            serde_json::from_value::<StopReason>(json!("end_turn")).unwrap(),
            StopReason::Other
        );
    }

    #[test]
    fn permission_outcome_serializes_selected_and_cancelled() {
        let sel = serde_json::to_value(PermissionOutcome::Selected {
            option_id: "allow".into(),
        })
        .unwrap();
        assert_eq!(sel["outcome"], json!("selected"));
        assert_eq!(sel["optionId"], json!("allow"));

        let can = serde_json::to_value(PermissionOutcome::Cancelled).unwrap();
        assert_eq!(can["outcome"], json!("cancelled"));
    }
}
