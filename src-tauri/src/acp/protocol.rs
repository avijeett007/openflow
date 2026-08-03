//! Wire types only — no logic, no I/O. Nothing in the crate constructs most of
//! these yet: the codec, client and driver that consume them are later tasks
//! in this plan (see `acp/mod.rs`). Silence dead-code until they're wired up.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Completed,
    MaxStepsReached,
    Cancelled,
    RequestTimeout,
    /// Any reason this version does not model. Never fatal — mapped to a plainly
    /// stated failure rather than a panic.
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
            "completed" => StopReason::Completed,
            "max_steps_reached" => StopReason::MaxStepsReached,
            "cancelled" => StopReason::Cancelled,
            "request_timeout" => StopReason::RequestTimeout,
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

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ToolCallWire {
    pub tool_call_id: String,
    pub title: String,
    /// ACP tool kind: `read` | `edit` | `execute` | `delete` | `move` |
    /// `search` | `fetch` | `think` | `other`. Kept as a String so an unknown
    /// kind from a newer agent is data, not a parse failure.
    pub kind: String,
    pub status: String,
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
    #[serde(rename_all = "camelCase")]
    ToolCall {
        tool_call_id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        locations: Vec<ToolLocation>,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallUpdate {
        tool_call_id: String,
        #[serde(default)]
        status: String,
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

    #[test]
    fn stop_reason_parses_and_unknown_is_not_fatal() {
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("completed")).unwrap(),
            StopReason::Completed
        );
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("max_steps_reached")).unwrap(),
            StopReason::MaxStepsReached
        );
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("something_new")).unwrap(),
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
