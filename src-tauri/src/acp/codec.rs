//! JSON-RPC 2.0 envelope handling for ACP's newline-delimited stdio framing.
//! Envelope-only: this module never interprets `params`, so protocol changes
//! land in `protocol.rs` and never here.

use serde::Deserialize;
use serde_json::{json, Value};

/// Hard cap on a single inbound line. ACP frames can legitimately be large
/// (tool output, file content), but an agent that never emits a newline must
/// not be able to grow our buffer without bound.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug)]
pub enum Frame {
    /// A reply to a request WE sent.
    Response {
        id: u64,
        result: Result<Value, JsonRpcError>,
    },
    /// A request FROM the agent that we must answer (e.g. permission).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// Fire-and-forget from the agent (e.g. `session/update`).
    Notification { method: String, params: Value },
}

#[derive(Debug)]
pub enum CodecError {
    TooLong,
    Malformed(String),
    /// Valid JSON but not a JSON-RPC frame we can act on.
    NotJsonRpc,
}

#[derive(Deserialize)]
struct RawFrame {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

pub fn classify(line: &str) -> Result<Frame, CodecError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(CodecError::TooLong);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(CodecError::Malformed("blank line".into()));
    }
    let raw: RawFrame =
        serde_json::from_str(trimmed).map_err(|e| CodecError::Malformed(e.to_string()))?;

    match (raw.id, raw.method) {
        // Request from the agent: has both an id and a method.
        (Some(id), Some(method)) => Ok(Frame::Request {
            id,
            method,
            params: raw.params.unwrap_or(Value::Null),
        }),
        // Response to us: id, no method.
        (Some(id), None) => {
            let id = id.as_u64().ok_or(CodecError::NotJsonRpc)?;
            if let Some(e) = raw.error {
                return Ok(Frame::Response {
                    id,
                    result: Err(JsonRpcError {
                        code: e.code,
                        message: e.message,
                    }),
                });
            }
            Ok(Frame::Response {
                id,
                result: Ok(raw.result.unwrap_or(Value::Null)),
            })
        }
        // Notification: method, no id.
        (None, Some(method)) => Ok(Frame::Notification {
            method,
            params: raw.params.unwrap_or(Value::Null),
        }),
        (None, None) => Err(CodecError::NotJsonRpc),
    }
}

pub fn encode_request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}

pub fn encode_notification(method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","method":method,"params":params}).to_string()
}

pub fn encode_result(id: &Value, result: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
}

pub fn encode_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_a_response() {
        let f = classify(r#"{"jsonrpc":"2.0","id":7,"result":{"sessionId":"s1"}}"#).unwrap();
        match f {
            Frame::Response { id, result } => {
                assert_eq!(id, 7);
                assert_eq!(result.unwrap()["sessionId"], json!("s1"));
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn classifies_an_error_response() {
        let f = classify(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}}"#)
            .unwrap();
        match f {
            Frame::Response {
                id: 7,
                result: Err(e),
            } => {
                assert_eq!(e.code, -32601);
                assert_eq!(e.message, "nope");
            }
            _ => panic!("expected error Response"),
        }
    }

    #[test]
    fn classifies_an_inbound_request_from_the_agent() {
        let line = r#"{"jsonrpc":"2.0","id":"a1","method":"session/request_permission",
                       "params":{"sessionId":"s1"}}"#;
        match classify(line).unwrap() {
            Frame::Request { id, method, params } => {
                assert_eq!(id, json!("a1"));
                assert_eq!(method, "session/request_permission");
                assert_eq!(params["sessionId"], json!("s1"));
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn classifies_a_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1"}}"#;
        match classify(line).unwrap() {
            Frame::Notification { method, .. } => assert_eq!(method, "session/update"),
            _ => panic!("expected Notification"),
        }
    }

    #[test]
    fn rejects_malformed_and_oversized_lines_without_panicking() {
        assert!(matches!(
            classify("not json at all"),
            Err(CodecError::Malformed(_))
        ));
        assert!(matches!(classify("{}"), Err(CodecError::NotJsonRpc)));
        let huge = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":"{}"}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        assert!(matches!(classify(&huge), Err(CodecError::TooLong)));
    }

    #[test]
    fn ignores_blank_and_whitespace_lines_as_malformed_not_panic() {
        assert!(classify("").is_err());
        assert!(classify("   ").is_err());
    }

    #[test]
    fn encoders_emit_single_line_json() {
        let r = encode_request(3, "session/prompt", json!({"sessionId":"s"}));
        assert!(
            !r.contains('\n'),
            "frames must be newline-delimited, not pretty-printed"
        );
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["jsonrpc"], json!("2.0"));
        assert_eq!(v["id"], json!(3));
        assert_eq!(v["method"], json!("session/prompt"));

        let e = encode_error(&json!("a1"), -32601, "Method not found");
        let v: Value = serde_json::from_str(&e).unwrap();
        assert_eq!(v["error"]["code"], json!(-32601));
        assert_eq!(v["id"], json!("a1"));
        assert!(
            v.get("result").is_none(),
            "an error reply must not carry result"
        );

        let n = encode_notification("session/cancel", json!({"sessionId":"s"}));
        let v: Value = serde_json::from_str(&n).unwrap();
        assert!(v.get("id").is_none(), "notifications carry no id");
    }
}
