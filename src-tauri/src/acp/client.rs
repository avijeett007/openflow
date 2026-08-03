//! The ACP client: id allocation, framing out, and demultiplexing in.
//!
//! Deliberately a PUMP, not a background task — the caller owns the loop. That
//! keeps `tokio::spawn` out of this file, so every test is deterministic with
//! no sleeps and no timing flakes.
//!
//! Nothing outside this module's own tests drives `pump()` yet — Task 7's
//! session manager is the caller that owns the loop. Silence dead-code until
//! it's wired up, same as `protocol.rs` and `codec.rs`.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::acp::codec::{
    classify, encode_error, encode_notification, encode_request, encode_result, CodecError, Frame,
    JsonRpcError,
};
use crate::acp::protocol::{RequestPermissionParams, SessionNotification};

pub trait AcpTransport: Send + Sync {
    fn send(&self, line: String) -> impl std::future::Future<Output = Result<(), String>> + Send;
    fn recv(&self) -> impl std::future::Future<Output = Option<String>> + Send;
}

#[derive(Debug)]
pub enum InboundRequest {
    RequestPermission {
        id: Value,
        params: RequestPermissionParams,
    },
    /// Any other agent→client request. We declared no fs/terminal capabilities,
    /// so this should not happen — but an unanswered request hangs the agent's
    /// turn forever, so we always reply.
    Unsupported { id: Value, method: String },
}

#[derive(Debug)]
pub enum ClientEvent {
    Update(SessionNotification),
    Inbound(InboundRequest),
    Closed,
}

#[derive(Debug)]
pub enum PumpItem {
    Response {
        id: u64,
        result: Result<Value, JsonRpcError>,
    },
    Event(ClientEvent),
}

pub struct AcpClient<T: AcpTransport> {
    transport: T,
    next_id: AtomicU64,
}

impl<T: AcpTransport> AcpClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn send_request(&self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id();
        self.transport
            .send(encode_request(id, method, params))
            .await?;
        Ok(id)
    }

    pub async fn send_notification(&self, method: &str, params: Value) -> Result<(), String> {
        self.transport
            .send(encode_notification(method, params))
            .await
    }

    pub async fn reply(&self, id: &Value, result: Value) -> Result<(), String> {
        self.transport.send(encode_result(id, result)).await
    }

    pub async fn reply_error(&self, id: &Value, code: i64, message: &str) -> Result<(), String> {
        self.transport.send(encode_error(id, code, message)).await
    }

    /// Read the next actionable item, skipping frames we cannot use. `None` when
    /// the transport closes (the child exited).
    pub async fn pump(&self) -> Option<PumpItem> {
        loop {
            let line = self.transport.recv().await?;
            match classify(&line) {
                Ok(Frame::Response { id, result }) => {
                    return Some(PumpItem::Response { id, result })
                }
                Ok(Frame::Request { id, method, params }) => {
                    let req = if method == "session/request_permission" {
                        match serde_json::from_value::<RequestPermissionParams>(params) {
                            Ok(p) => InboundRequest::RequestPermission { id, params: p },
                            Err(e) => {
                                log::warn!("acp: bad request_permission params: {e}");
                                InboundRequest::Unsupported { id, method }
                            }
                        }
                    } else {
                        InboundRequest::Unsupported { id, method }
                    };
                    return Some(PumpItem::Event(ClientEvent::Inbound(req)));
                }
                Ok(Frame::Notification { method, params }) => {
                    if method == "session/update" {
                        match serde_json::from_value::<SessionNotification>(params) {
                            Ok(n) => return Some(PumpItem::Event(ClientEvent::Update(n))),
                            Err(e) => log::warn!("acp: bad session/update: {e}"),
                        }
                    }
                    // Any other notification is not actionable in C0 — skip.
                }
                Err(CodecError::TooLong) => log::warn!("acp: dropped oversized frame"),
                Err(e) => log::debug!("acp: skipping unparseable line: {e:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Run a future to completion on a current-thread runtime with the time
    /// driver enabled (the repo enables the tokio `rt`+`time` features but uses
    /// no `#[tokio::test]`, so we build the runtime explicitly). Mirrors
    /// `a2a.rs`'s test helper.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// Scripted transport: replays canned lines, records what we sent.
    struct FakeTransport {
        inbound: Mutex<std::collections::VecDeque<String>>,
        sent: Mutex<Vec<String>>,
    }

    impl FakeTransport {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                inbound: Mutex::new(lines.iter().map(|s| s.to_string()).collect()),
                sent: Mutex::new(Vec::new()),
            }
        }
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl AcpTransport for FakeTransport {
        fn send(
            &self,
            line: String,
        ) -> impl std::future::Future<Output = Result<(), String>> + Send {
            self.sent.lock().unwrap().push(line);
            async { Ok(()) }
        }
        fn recv(&self) -> impl std::future::Future<Output = Option<String>> + Send {
            let next = self.inbound.lock().unwrap().pop_front();
            async move { next }
        }
    }

    #[test]
    fn request_ids_are_monotonic_and_encoded() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![]));
            let id1 = c.send_request("initialize", json!({})).await.unwrap();
            let id2 = c.send_request("session/new", json!({})).await.unwrap();
            assert!(id2 > id1);
            let sent = c.transport().sent();
            assert!(sent[0].contains(r#""method":"initialize""#));
            assert!(!sent[0].contains('\n'));
        });
    }

    #[test]
    fn pump_returns_a_response_for_our_request() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s1"}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Response { id, result } => {
                    assert_eq!(id, 1);
                    assert_eq!(result.unwrap()["sessionId"], json!("s1"));
                }
                other => panic!("expected Response, got {other:?}"),
            }
        });
    }

    #[test]
    fn pump_surfaces_session_update_notifications() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1",
                    "update":{"sessionUpdate":"agent_message_chunk",
                    "content":{"type":"text","text":"hi"}}}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Event(ClientEvent::Update(n)) => assert_eq!(n.session_id, "s1"),
                other => panic!("expected Update, got {other:?}"),
            }
        });
    }

    #[test]
    fn pump_surfaces_a_permission_request_from_the_agent() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":"a1","method":"session/request_permission",
                    "params":{"sessionId":"s1","toolCall":{"toolCallId":"t1","title":"Edit x",
                    "kind":"edit"},"options":[{"optionId":"o1","name":"Allow",
                    "kind":"allow_once"}]}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Event(ClientEvent::Inbound(InboundRequest::RequestPermission {
                    id,
                    params,
                })) => {
                    assert_eq!(id, json!("a1"));
                    assert_eq!(params.tool_call.kind, "edit");
                    assert_eq!(params.options.len(), 1);
                }
                other => panic!("expected RequestPermission, got {other:?}"),
            }
        });
    }

    #[test]
    fn malformed_permission_params_still_surface_so_the_agent_is_never_left_hanging() {
        // `toolCall` must be an object (`ToolCallWire`); a number can never
        // deserialize into it, regardless of the container's `#[serde(default)]`
        // (that only fills in *missing* fields, not badly-typed present ones).
        // This must still come out as `Unsupported`, not `RequestPermission` and
        // not silently dropped — an unanswered permission request hangs the
        // agent's turn forever.
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":9,"method":"session/request_permission",
                    "params":{"toolCall":123}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Event(ClientEvent::Inbound(InboundRequest::Unsupported {
                    id,
                    method,
                })) => {
                    assert_eq!(id, json!(9));
                    assert_eq!(method, "session/request_permission");
                }
                other => panic!("expected Unsupported, got {other:?}"),
            }
        });
    }

    #[test]
    fn unsupported_agent_request_is_surfaced_for_a_method_not_found_reply() {
        // We declared fs/terminal capabilities false, so an agent SHOULD NOT call
        // these — but if one does, we must answer, not hang its turn forever.
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                r#"{"jsonrpc":"2.0","id":9,"method":"fs/write_text_file","params":{}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Event(ClientEvent::Inbound(InboundRequest::Unsupported {
                    method,
                    ..
                })) => {
                    assert_eq!(method, "fs/write_text_file");
                }
                other => panic!("expected Unsupported, got {other:?}"),
            }
        });
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![
                "garbage not json",
                "{}",
                r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            ]));
            match c.pump().await.unwrap() {
                PumpItem::Response { id: 1, .. } => {}
                other => panic!("expected the good frame to survive, got {other:?}"),
            }
        });
    }

    #[test]
    fn closed_transport_yields_none() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![]));
            assert!(c.pump().await.is_none());
        });
    }

    #[test]
    fn reply_encodes_result_and_error_correctly() {
        block_on(async {
            let c = AcpClient::new(FakeTransport::new(vec![]));
            c.reply(&json!("a1"), json!({"outcome":"cancelled"}))
                .await
                .unwrap();
            c.reply_error(&json!(9), -32601, "Method not found")
                .await
                .unwrap();
            let sent = c.transport().sent();
            assert!(sent[0].contains(r#""result""#));
            assert!(sent[1].contains(r#""code":-32601"#));
        });
    }
}
