//! Flow OS increment 3 — a thin, hand-rolled **A2A (Agent-to-Agent)** client for
//! the `RemoteDriver`. A remote agent is a spec-compliant A2A server the user
//! points OpenFlow at; it joins the same agent registry, hotkeys, voice routing
//! and run panel as CLI/prompt agents (see `DESIGN-remote-agents.md`).
//!
//! Scope is deliberately narrow: the A2A **JSON-RPC binding** only (no gRPC /
//! HTTP+JSON), built on `reqwest` + `serde_json` with NO `a2a-*` crates (pre-1.0
//! churn + gRPC weight). We accept agent cards advertising `protocolVersion`
//! 0.3.x AND 1.0.x — the two shapes are wire-identical for the four methods we
//! use (`message/send`, `tasks/get`, `tasks/cancel`, `message/stream`).
//!
//! Everything here is pure/testable except the production `HttpA2aTransport`:
//! card parsing, endpoint selection, JSON-RPC envelope handling, the SSE frame
//! parser and the driver protocol loop all run against a mock transport with no
//! network (see the tests at the bottom + the driver tests in `agent_run.rs`).

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use specta::Type;
use tokio::sync::mpsc;
use tokio::time::Instant;

// ---------------------------------------------------------------------------
// Method strings (A2A JSON-RPC binding)
// ---------------------------------------------------------------------------

pub const METHOD_MESSAGE_SEND: &str = "message/send";
pub const METHOD_MESSAGE_STREAM: &str = "message/stream";
pub const METHOD_TASKS_GET: &str = "tasks/get";
pub const METHOD_TASKS_CANCEL: &str = "tasks/cancel";

// ---------------------------------------------------------------------------
// Request ids (no `uuid` dependency — a unique-per-request id is all JSON-RPC
// and A2A `messageId` need). Non-cryptographic; combines a process seed, the
// clock and a monotonic counter, formatted UUID-v4-shaped for server-friendliness.
// ---------------------------------------------------------------------------

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique request/message id, formatted like a v4 UUID.
pub fn new_request_id() -> String {
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Two 64-bit lanes mixed with splitmix64 so the hex is well-distributed.
    let a = splitmix64(nanos ^ (counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let b = splitmix64(counter ^ (nanos.rotate_left(32)));
    let time_low = (a >> 32) as u32;
    let time_mid = (a >> 16) as u16;
    let time_hi = ((a as u16) & 0x0FFF) | 0x4000; // version 4
    let clock = ((b >> 48) as u16 & 0x3FFF) | 0x8000; // variant 10xx
    let node = b & 0xFFFF_FFFF_FFFF;
    format!("{time_low:08x}-{time_mid:04x}-{time_hi:04x}-{clock:04x}-{node:012x}")
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

// ---------------------------------------------------------------------------
// Agent card — tolerant parse of BOTH the v0.3 and v1.0 shapes
// ---------------------------------------------------------------------------

/// A normalized transport interface extracted from a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub url: String,
    pub transport: String,
}

/// The bits of an agent card OpenFlow cares about, normalized across card
/// versions. Unknown fields are ignored (tolerant parse).
#[derive(Debug, Clone)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    pub streaming: bool,
    /// Transport interfaces in preference order (top-level/preferred first).
    pub interfaces: Vec<Interface>,
    pub auth_schemes: Vec<String>,
    pub skills: Vec<AgentCardSkill>,
}

/// UI-facing summary returned by `fetch_remote_agent_card` / persisted on the agent.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentCardSummary {
    pub name: String,
    pub version: String,
    pub streaming: bool,
    /// The resolved JSON-RPC endpoint (empty only if selection somehow failed —
    /// callers error out before building a summary in that case).
    pub endpoint: String,
    pub auth_schemes: Vec<String>,
    pub skills: Vec<AgentCardSkill>,
}

#[derive(Debug, Clone, Serialize, Type, PartialEq, Eq)]
pub struct AgentCardSkill {
    pub id: String,
    pub name: String,
}

/// Derive the well-known agent-card URL from a user-entered base URL. A URL that
/// already points at a card (`…/agent-card.json` or an `…/.well-known/…` path)
/// is accepted verbatim; otherwise we append the well-known path to the origin+path.
pub fn well_known_card_url(entered: &str) -> String {
    let trimmed = entered.trim();
    if trimmed.ends_with("agent-card.json") || trimmed.contains("/.well-known/") {
        return trimmed.to_string();
    }
    let base = trimmed.trim_end_matches('/');
    format!("{base}/.well-known/agent-card.json")
}

/// Parse an agent card JSON value into the normalized [`AgentCard`]. Accepts the
/// v0.3 shape (`url` + `preferredTransport` + `additionalInterfaces`) and the
/// v1.0 shape (`supportedInterfaces`), merging both when present.
pub fn parse_agent_card(v: &Value) -> Result<AgentCard, String> {
    if !v.is_object() {
        return Err("The agent card was not a JSON object.".to_string());
    }
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let streaming = v
        .get("capabilities")
        .and_then(|c| c.get("streaming"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut interfaces: Vec<Interface> = Vec::new();

    // v0.3 top-level url + preferredTransport. Per the A2A spec, when
    // `preferredTransport` is omitted the transport at `url` is JSONRPC.
    if let Some(url) = v.get("url").and_then(Value::as_str) {
        if !url.is_empty() {
            let transport = v
                .get("preferredTransport")
                .and_then(Value::as_str)
                .unwrap_or("JSONRPC")
                .to_string();
            interfaces.push(Interface {
                url: url.to_string(),
                transport,
            });
        }
    }
    // v0.3 additionalInterfaces: [{url, transport}].
    if let Some(arr) = v.get("additionalInterfaces").and_then(Value::as_array) {
        for it in arr {
            if let Some(url) = it.get("url").and_then(Value::as_str) {
                let transport = it
                    .get("transport")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                interfaces.push(Interface {
                    url: url.to_string(),
                    transport,
                });
            }
        }
    }
    // v1.0 supportedInterfaces: [{url, protocolBinding|transport, protocolVersion}].
    if let Some(arr) = v.get("supportedInterfaces").and_then(Value::as_array) {
        for it in arr {
            if let Some(url) = it.get("url").and_then(Value::as_str) {
                let transport = it
                    .get("protocolBinding")
                    .or_else(|| it.get("transport"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                interfaces.push(Interface {
                    url: url.to_string(),
                    transport,
                });
            }
        }
    }

    let auth_schemes = parse_auth_schemes(v);
    let skills = parse_skills(v);

    Ok(AgentCard {
        name,
        version,
        streaming,
        interfaces,
        auth_schemes,
        skills,
    })
}

fn parse_auth_schemes(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(obj) = v.get("securitySchemes").and_then(Value::as_object) {
        for scheme in obj.values() {
            if let Some(t) = scheme.get("type").and_then(Value::as_str) {
                if !out.iter().any(|s| s == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

fn parse_skills(v: &Value) -> Vec<AgentCardSkill> {
    v.get("skills")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let id = s.get("id").and_then(Value::as_str)?.to_string();
                    let name = s
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_string();
                    Some(AgentCardSkill { id, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Normalize a transport/binding label to a comparable token (lowercase, only
/// alphanumerics) so `JSONRPC`, `jsonrpc`, `JSON-RPC` all compare equal.
fn transport_token(t: &str) -> String {
    t.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_jsonrpc(t: &str) -> bool {
    transport_token(t) == "jsonrpc"
}

impl AgentCard {
    /// Select the JSON-RPC endpoint URL: the first interface (in preference
    /// order) whose transport is JSON-RPC. Errors clearly when none exists.
    pub fn select_jsonrpc_endpoint(&self) -> Result<String, String> {
        self.interfaces
            .iter()
            .find(|i| is_jsonrpc(&i.transport) && !i.url.is_empty())
            .map(|i| i.url.clone())
            .ok_or_else(|| {
                "This agent doesn't offer a JSON-RPC interface. OpenFlow only \
                 speaks the A2A JSON-RPC binding."
                    .to_string()
            })
    }

    pub fn summary(&self, endpoint: String) -> AgentCardSummary {
        AgentCardSummary {
            name: self.name.clone(),
            version: self.version.clone(),
            streaming: self.streaming,
            endpoint,
            auth_schemes: self.auth_schemes.clone(),
            skills: self.skills.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC envelope
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request body.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: String,
    pub method: String,
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: new_request_id(),
            method: method.to_string(),
            params,
        }
    }
}

/// Parse a JSON-RPC response envelope: **error before result** (a well-formed
/// error response can technically also carry a null `result`). Returns the
/// `result` value on success, or a formatted `code: message` error string.
pub fn parse_jsonrpc_envelope(v: &Value) -> Result<Value, String> {
    if let Some(err) = v.get("error") {
        if !err.is_null() {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(format!("{code}: {message}"));
        }
    }
    match v.get("result") {
        Some(result) if !result.is_null() => Ok(result.clone()),
        _ => Err("the response had neither a result nor an error".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Message / Part / Task text extraction
// ---------------------------------------------------------------------------

/// Build the `message/send`|`message/stream` params for an instruction.
pub fn build_send_params(instruction: &str) -> Value {
    json!({
        "message": {
            "role": "user",
            "parts": [{ "kind": "text", "text": instruction }],
            "messageId": new_request_id(),
        }
    })
}

/// Turn one A2A `Part` into a displayable line. Text parts yield their text;
/// non-text parts are summarized (`[file: name]` / `[data]`) rather than dumped.
pub fn part_to_text(part: &Value) -> Option<String> {
    let kind = part
        .get("kind")
        .or_else(|| part.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "file" => {
            let name = part
                .get("file")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("file");
            Some(format!("[file: {name}]"))
        }
        "data" => Some("[data]".to_string()),
        // "text" or an unlabeled part that nonetheless carries text.
        _ => part
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
    }
}

/// Collect the displayable text chunks from a `parts` array.
fn parts_text(parts: Option<&Value>) -> Vec<String> {
    parts
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(part_to_text).collect())
        .unwrap_or_default()
}

/// Extract text chunks from a `message` result (direct reply).
pub fn message_text(msg: &Value) -> Vec<String> {
    parts_text(msg.get("parts"))
}

/// Extract the NEW-this-snapshot text chunks from a `task`: its status message
/// parts plus every artifact's parts. Callers dedupe across polls.
pub fn task_text_chunks(task: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(status) = task.get("status") {
        out.extend(parts_text(
            status.get("message").and_then(|m| m.get("parts")),
        ));
    }
    if let Some(arts) = task.get("artifacts").and_then(Value::as_array) {
        for art in arts {
            out.extend(parts_text(art.get("parts")));
        }
    }
    out
}

/// The `state` string of a task/status object, if present.
pub fn task_state(task: &Value) -> Option<String> {
    task.get("status")
        .and_then(|s| s.get("state"))
        .or_else(|| task.get("state"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

/// The human message attached to a task/status, if any (used in Failed text).
pub fn task_status_message(task: &Value) -> Option<String> {
    let parts = task
        .get("status")
        .and_then(|s| s.get("message"))
        .and_then(|m| m.get("parts"));
    let text = parts_text(parts).join(" ");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

// ---------------------------------------------------------------------------
// send-result dispatch + task-state classification
// ---------------------------------------------------------------------------

/// What a `message/send` result represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendDispatch {
    /// A direct reply — the concatenated text is final.
    DirectMessage(Vec<String>),
    /// A long-running task to track by id (with any text already present).
    Task {
        id: String,
        initial_text: Vec<String>,
    },
    /// A task with no id — unusable (server bug); surfaced as a failure.
    UnusableTask,
}

/// Dispatch a `message/send` result on its `kind` (`"message"` vs `"task"`),
/// with a structural fallback for cards that omit `kind`.
pub fn dispatch_send_result(result: &Value) -> SendDispatch {
    let kind = result.get("kind").and_then(Value::as_str);
    let looks_like_task = result.get("status").is_some() || result.get("artifacts").is_some();
    let is_task = matches!(kind, Some("task")) || (kind.is_none() && looks_like_task);
    if is_task {
        match result.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => SendDispatch::Task {
                id: id.to_string(),
                initial_text: task_text_chunks(result),
            },
            _ => SendDispatch::UnusableTask,
        }
    } else {
        SendDispatch::DirectMessage(message_text(result))
    }
}

/// Terminal/non-terminal classification of an A2A task state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateClass {
    /// `submitted` / `working` — keep going.
    NonTerminal,
    /// `completed`.
    Finished,
    /// `failed` / `rejected` — carries the server message when present.
    Failed(Option<String>),
    /// `canceled`.
    Stopped,
    /// `input-required` / `auth-required` — multi-turn, unsupported in v1.
    NeedsInput,
    /// An unrecognized state string — treated as non-terminal (keep polling).
    Unknown,
}

/// Map an A2A task `state` string to its [`StateClass`].
pub fn classify_state(state: &str, server_message: Option<String>) -> StateClass {
    match state {
        "submitted" | "working" => StateClass::NonTerminal,
        "completed" => StateClass::Finished,
        "failed" | "rejected" => StateClass::Failed(server_message),
        "canceled" | "cancelled" => StateClass::Stopped,
        "input-required" | "auth-required" => StateClass::NeedsInput,
        _ => StateClass::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Poll-append dedupe
// ---------------------------------------------------------------------------

/// Tracks text chunks already emitted so re-polling a task (whose artifacts/
/// status message accumulate) doesn't re-print what the user already saw.
#[derive(Default)]
pub struct TextDedup {
    seen: HashSet<String>,
}

impl TextDedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return `Some(text)` the first time a chunk is seen, `None` afterwards.
    pub fn push(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        if self.seen.insert(text.to_string()) {
            Some(text.to_string())
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// SSE frame parser (manual; feed decoded text, get complete event data payloads)
// ---------------------------------------------------------------------------

/// A minimal Server-Sent-Events parser sufficient for A2A `message/stream`:
/// handles multi-line `data:` (joined with `\n`), comment lines (`:` prefix),
/// CRLF line endings, and events split across chunk boundaries. Non-`data`
/// fields (`event`, `id`, `retry`) are ignored — each A2A frame is a JSON-RPC
/// envelope carried entirely in `data`.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a decoded text chunk; return the `data` payload of every event that
    /// completed (was terminated by a blank line) within it.
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let mut line: String = self.buf.drain(..=nl).collect();
            line.pop(); // drop the '\n'
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                // Blank line — dispatch the buffered event (if any).
                if !self.data_lines.is_empty() {
                    out.push(self.data_lines.join("\n"));
                    self.data_lines.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment
            }
            let (field, value) = match line.find(':') {
                Some(i) => {
                    let field = line[..i].to_string();
                    let mut value = line[i + 1..].to_string();
                    if let Some(stripped) = value.strip_prefix(' ') {
                        value = stripped.to_string();
                    }
                    (field, value)
                }
                None => (line.clone(), String::new()),
            };
            if field == "data" {
                self.data_lines.push(value);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Transport (mirror of ServiceTransport: HTTP behind a trait so the driver is
// unit-testable against a mock with no network)
// ---------------------------------------------------------------------------

/// A stream of parsed JSON-RPC `result` values (one per SSE frame). Each item is
/// the frame's `result` object, or an error for a JSON-RPC-error frame / stream
/// transport error.
pub type A2aStream = Pin<Box<dyn Stream<Item = Result<Value, String>> + Send>>;

/// The A2A operations the driver needs, behind a trait. Owned args keep the
/// returned futures `'static` + `Send` (same pattern as `ServiceTransport`).
pub trait A2aTransport: Send + Sync + 'static {
    /// GET the agent card. `token` is used only for the 401-retry.
    fn fetch_card(
        &self,
        url: String,
        token: Option<String>,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send;

    /// POST a JSON-RPC call; returns the `result` (envelope error mapped to `Err`).
    fn call(
        &self,
        endpoint: String,
        token: Option<String>,
        method: String,
        params: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send;

    /// POST `message/stream`; returns a stream of per-frame `result` values.
    fn stream(
        &self,
        endpoint: String,
        token: Option<String>,
        method: String,
        params: Value,
    ) -> impl std::future::Future<Output = Result<A2aStream, String>> + Send;
}

/// Production transport backed by `reqwest`. Two clients: a 30s-timeout client
/// for card fetch + unary calls, and a no-overall-timeout client for the SSE
/// stream (an active stream's data keeps it alive; a total timeout would cut it).
pub struct HttpA2aTransport {
    unary: reqwest::Client,
    streaming: reqwest::Client,
}

impl Default for HttpA2aTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpA2aTransport {
    pub fn new() -> Self {
        let unary = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let streaming = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { unary, streaming }
    }
}

impl A2aTransport for HttpA2aTransport {
    async fn fetch_card(&self, url: String, token: Option<String>) -> Result<Value, String> {
        // Card fetch is an unauthenticated GET; if it 401s AND we have a token,
        // retry once with the bearer token (covers protected cards).
        let resp = self
            .unary
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Could not reach the agent: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            if let Some(t) = token {
                let resp2 = self
                    .unary
                    .get(&url)
                    .bearer_auth(t)
                    .send()
                    .await
                    .map_err(|e| format!("Could not reach the agent: {e}"))?;
                if !resp2.status().is_success() {
                    return Err(format!("HTTP {}", resp2.status().as_u16()));
                }
                return resp2
                    .json()
                    .await
                    .map_err(|e| format!("The agent card was not valid JSON: {e}"));
            }
            return Err("HTTP 401".to_string());
        }
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        resp.json()
            .await
            .map_err(|e| format!("The agent card was not valid JSON: {e}"))
    }

    async fn call(
        &self,
        endpoint: String,
        token: Option<String>,
        method: String,
        params: Value,
    ) -> Result<Value, String> {
        let req = JsonRpcRequest::new(&method, params);
        let mut builder = self.unary.post(&endpoint).json(&req);
        if let Some(t) = token {
            builder = builder.bearer_auth(t);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        // A2A returns JSON-RPC envelopes even on some non-2xx; parse the body and
        // let envelope error-handling win, falling back to the HTTP status.
        let status = resp.status();
        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                return Err(format!("HTTP {}: {e}", status.as_u16()));
            }
        };
        parse_jsonrpc_envelope(&body)
    }

    async fn stream(
        &self,
        endpoint: String,
        token: Option<String>,
        method: String,
        params: Value,
    ) -> Result<A2aStream, String> {
        let req = JsonRpcRequest::new(&method, params);
        let mut builder = self
            .streaming
            .post(&endpoint)
            .header("Accept", "text/event-stream")
            .json(&req);
        if let Some(t) = token {
            builder = builder.bearer_auth(t);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| format!("stream request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }

        struct StreamState<S> {
            inner: S,
            parser: SseParser,
            queue: std::collections::VecDeque<String>,
        }
        let state = StreamState {
            inner: resp.bytes_stream(),
            parser: SseParser::new(),
            queue: std::collections::VecDeque::new(),
        };

        let s = futures_util::stream::unfold(state, |mut st| async move {
            loop {
                // Drain already-parsed frames first, skipping non-JSON garbage.
                while let Some(data) = st.queue.pop_front() {
                    match serde_json::from_str::<Value>(&data) {
                        Ok(env) => return Some((parse_jsonrpc_envelope(&env), st)),
                        Err(_) => continue, // tolerate garbage frames
                    }
                }
                match st.inner.next().await {
                    Some(Ok(bytes)) => {
                        let text = String::from_utf8_lossy(&bytes);
                        for data in st.parser.feed(&text) {
                            st.queue.push_back(data);
                        }
                        continue;
                    }
                    Some(Err(e)) => {
                        return Some((Err(format!("stream error: {e}")), st));
                    }
                    None => return None,
                }
            }
        });
        Ok(Box::pin(s))
    }
}

// ---------------------------------------------------------------------------
// Driver protocol loop (the RemoteDriver's core — transport-agnostic, testable)
// ---------------------------------------------------------------------------

/// Terminal outcome of a remote run, mapped to a `RunStatus` by the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOutcome {
    /// `completed` → `Finished { code: 0 }`.
    Finished,
    /// Any error / `failed` / `rejected` / unsupported-multiturn / timeout.
    Failed(String),
    /// `canceled` on the server, or a local stop request.
    Stopped,
}

/// Production timings.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
pub const POLL_CAP: Duration = Duration::from_secs(15 * 60);

/// Drive one remote run to a terminal outcome. `on_output` receives each new
/// text chunk (streamed live by the driver into the run panel). `kill_rx`
/// requests a stop. `poll_interval`/`poll_cap` are injectable for tests.
#[allow(clippy::too_many_arguments)]
pub async fn run_remote_protocol<T, F>(
    transport: &T,
    endpoint: &str,
    token: Option<String>,
    instruction: &str,
    streaming: bool,
    kill_rx: &mut mpsc::UnboundedReceiver<()>,
    on_output: &mut F,
    poll_interval: Duration,
    poll_cap: Duration,
) -> RemoteOutcome
where
    T: A2aTransport,
    F: FnMut(&str) + Send,
{
    let deadline = Instant::now() + poll_cap;
    let mut dedup = TextDedup::new();

    if streaming {
        match stream_phase(
            transport,
            endpoint,
            &token,
            instruction,
            kill_rx,
            &mut dedup,
            on_output,
        )
        .await
        {
            StreamPhase::Terminal(outcome) => return outcome,
            StreamPhase::Stopped => return RemoteOutcome::Stopped,
            StreamPhase::Fallback(task_id) => {
                // Stream broke mid-way but we have a task id — degrade to polling.
                return poll_phase(
                    transport,
                    endpoint,
                    &token,
                    &task_id,
                    kill_rx,
                    &mut dedup,
                    on_output,
                    poll_interval,
                    deadline,
                )
                .await;
            }
        }
    }

    // Non-streaming: message/send, then dispatch.
    let result = match transport
        .call(
            endpoint.to_string(),
            token.clone(),
            METHOD_MESSAGE_SEND.to_string(),
            build_send_params(instruction),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return RemoteOutcome::Failed(e),
    };

    match dispatch_send_result(&result) {
        SendDispatch::DirectMessage(chunks) => {
            for c in chunks {
                if let Some(text) = dedup.push(&c) {
                    on_output(&text);
                }
            }
            RemoteOutcome::Finished
        }
        SendDispatch::UnusableTask => {
            RemoteOutcome::Failed("the server returned a task with no id".to_string())
        }
        SendDispatch::Task { id, initial_text } => {
            for c in initial_text {
                if let Some(text) = dedup.push(&c) {
                    on_output(&text);
                }
            }
            // The initial response may already be terminal.
            if let Some(state) = task_state(&result) {
                if let Some(outcome) = terminal_outcome(&state, task_status_message(&result)) {
                    return outcome;
                }
            }
            poll_phase(
                transport,
                endpoint,
                &token,
                &id,
                kill_rx,
                &mut dedup,
                on_output,
                poll_interval,
                deadline,
            )
            .await
        }
    }
}

/// Convert a state string into a terminal [`RemoteOutcome`], or `None` if the
/// state is non-terminal (keep going). Folds the `NeedsInput` case into the
/// actionable "multi-turn isn't supported yet" failure.
fn terminal_outcome(state: &str, server_message: Option<String>) -> Option<RemoteOutcome> {
    match classify_state(state, server_message) {
        StateClass::NonTerminal | StateClass::Unknown => None,
        StateClass::Finished => Some(RemoteOutcome::Finished),
        StateClass::Failed(msg) => Some(RemoteOutcome::Failed(match msg {
            Some(m) => format!("the agent reported: {m}"),
            None => "the agent reported a failure".to_string(),
        })),
        StateClass::Stopped => Some(RemoteOutcome::Stopped),
        StateClass::NeedsInput => Some(RemoteOutcome::Failed(
            "the agent needs interactive input — multi-turn isn't supported yet".to_string(),
        )),
    }
}

/// Poll `tasks/get` every `poll_interval` until the task is terminal, the poll
/// cap (`deadline`) is hit, or a stop is requested (→ `tasks/cancel`).
#[allow(clippy::too_many_arguments)]
async fn poll_phase<T, F>(
    transport: &T,
    endpoint: &str,
    token: &Option<String>,
    task_id: &str,
    kill_rx: &mut mpsc::UnboundedReceiver<()>,
    dedup: &mut TextDedup,
    on_output: &mut F,
    poll_interval: Duration,
    deadline: Instant,
) -> RemoteOutcome
where
    T: A2aTransport,
    F: FnMut(&str) + Send,
{
    loop {
        // Wait one interval OR a stop request. A stop request wins immediately.
        tokio::select! {
            biased;
            _ = kill_rx.recv() => {
                let _ = transport
                    .call(
                        endpoint.to_string(),
                        token.clone(),
                        METHOD_TASKS_CANCEL.to_string(),
                        json!({ "id": task_id }),
                    )
                    .await; // idempotent; TaskNotCancelable tolerated
                return RemoteOutcome::Stopped;
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }

        if Instant::now() >= deadline {
            return RemoteOutcome::Failed(
                "timed out; the task may still be running on the server".to_string(),
            );
        }

        let task = match transport
            .call(
                endpoint.to_string(),
                token.clone(),
                METHOD_TASKS_GET.to_string(),
                json!({ "id": task_id, "historyLength": 0 }),
            )
            .await
        {
            Ok(t) => t,
            Err(e) => return RemoteOutcome::Failed(e),
        };

        for c in task_text_chunks(&task) {
            if let Some(text) = dedup.push(&c) {
                on_output(&text);
            }
        }

        if let Some(state) = task_state(&task) {
            if let Some(outcome) = terminal_outcome(&state, task_status_message(&task)) {
                return outcome;
            }
        }
    }
}

/// Result of the streaming phase.
enum StreamPhase {
    Terminal(RemoteOutcome),
    Stopped,
    /// Stream errored mid-way but a task id is known — the caller polls instead.
    Fallback(String),
}

/// Consume `message/stream` frames, appending artifact/status text live, until a
/// `final:true` / terminal state closes it, a stop is requested, or the stream
/// errors (→ fallback poll if a task id was seen, else a failure).
async fn stream_phase<T, F>(
    transport: &T,
    endpoint: &str,
    token: &Option<String>,
    instruction: &str,
    kill_rx: &mut mpsc::UnboundedReceiver<()>,
    dedup: &mut TextDedup,
    on_output: &mut F,
) -> StreamPhase
where
    T: A2aTransport,
    F: FnMut(&str) + Send,
{
    let mut stream = match transport
        .stream(
            endpoint.to_string(),
            token.clone(),
            METHOD_MESSAGE_STREAM.to_string(),
            build_send_params(instruction),
        )
        .await
    {
        Ok(s) => s,
        Err(e) => return StreamPhase::Terminal(RemoteOutcome::Failed(e)),
    };

    let mut task_id: Option<String> = None;

    loop {
        tokio::select! {
            biased;
            _ = kill_rx.recv() => {
                if let Some(id) = &task_id {
                    let _ = transport
                        .call(
                            endpoint.to_string(),
                            token.clone(),
                            METHOD_TASKS_CANCEL.to_string(),
                            json!({ "id": id }),
                        )
                        .await;
                }
                return StreamPhase::Stopped;
            }
            frame = stream.next() => {
                match frame {
                    Some(Ok(result)) => {
                        // Track a task id for cancel / fallback.
                        if task_id.is_none() {
                            if let Some(id) = result
                                .get("taskId")
                                .or_else(|| result.get("id"))
                                .and_then(Value::as_str)
                            {
                                if !id.is_empty() {
                                    task_id = Some(id.to_string());
                                }
                            }
                        }
                        // Append any text (artifact-update carries `artifact`,
                        // status-update carries `status.message`, and a Task/Message
                        // frame carries its own parts).
                        for c in frame_text(&result) {
                            if let Some(text) = dedup.push(&c) {
                                on_output(&text);
                            }
                        }
                        // Terminal state or explicit final flag closes the stream.
                        if let Some(state) = task_state(&result) {
                            if let Some(outcome) =
                                terminal_outcome(&state, task_status_message(&result))
                            {
                                return StreamPhase::Terminal(outcome);
                            }
                        }
                        if result.get("final").and_then(Value::as_bool) == Some(true) {
                            return StreamPhase::Terminal(RemoteOutcome::Finished);
                        }
                    }
                    Some(Err(e)) => {
                        // Mid-stream error: degrade to polling if we know the task.
                        match &task_id {
                            Some(id) => return StreamPhase::Fallback(id.clone()),
                            None => {
                                return StreamPhase::Terminal(RemoteOutcome::Failed(e));
                            }
                        }
                    }
                    None => {
                        // Stream ended without a terminal frame. Treat as done if
                        // we streamed something; otherwise fall back / finish.
                        return match &task_id {
                            Some(id) => StreamPhase::Fallback(id.clone()),
                            None => StreamPhase::Terminal(RemoteOutcome::Finished),
                        };
                    }
                }
            }
        }
    }
}

/// Text carried by a single stream frame (artifact-update, status-update, or a
/// bare Task/Message frame).
fn frame_text(result: &Value) -> Vec<String> {
    let mut out = Vec::new();
    // artifact-update: { artifact: { parts: [...] } }
    if let Some(art) = result.get("artifact") {
        out.extend(parts_text(art.get("parts")));
    }
    // status-update / Task: { status: { message: { parts } } } + artifacts
    out.extend(task_text_chunks(result));
    // bare Message frame: { parts: [...] }
    if result.get("kind").and_then(Value::as_str) == Some("message") {
        out.extend(message_text(result));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- card URL derivation ------------------------------------------------

    #[test]
    fn well_known_url_derived_from_base() {
        assert_eq!(
            well_known_card_url("https://agent.example.com"),
            "https://agent.example.com/.well-known/agent-card.json"
        );
        assert_eq!(
            well_known_card_url("https://agent.example.com/"),
            "https://agent.example.com/.well-known/agent-card.json"
        );
    }

    #[test]
    fn well_known_url_accepts_full_card_url_verbatim() {
        let full = "https://x.example.com/custom/agent-card.json";
        assert_eq!(well_known_card_url(full), full);
        let wk = "https://x.example.com/.well-known/agent-card.json";
        assert_eq!(well_known_card_url(wk), wk);
    }

    // ---- card parse: both shapes -------------------------------------------

    #[test]
    fn parse_card_v03_shape() {
        let v = json!({
            "protocolVersion": "0.3.0",
            "name": "V03 Agent",
            "version": "1.0.0",
            "url": "https://a.example.com/rpc",
            "preferredTransport": "JSONRPC",
            "capabilities": { "streaming": true },
            "additionalInterfaces": [
                { "url": "https://a.example.com/grpc", "transport": "GRPC" }
            ],
            "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } },
            "skills": [ { "id": "code", "name": "Coding" } ]
        });
        let card = parse_agent_card(&v).unwrap();
        assert_eq!(card.name, "V03 Agent");
        assert_eq!(card.version, "1.0.0");
        assert!(card.streaming);
        assert_eq!(
            card.select_jsonrpc_endpoint().unwrap(),
            "https://a.example.com/rpc"
        );
        assert_eq!(card.auth_schemes, vec!["http".to_string()]);
        assert_eq!(card.skills.len(), 1);
        assert_eq!(card.skills[0].id, "code");
    }

    #[test]
    fn parse_card_v03_minimal_url_only_defaults_jsonrpc() {
        // A minimal v0.3 card with just `url` (no preferredTransport) must
        // resolve as JSONRPC per spec.
        let v = json!({ "name": "Min", "version": "0.1", "url": "https://m.example.com/a2a" });
        let card = parse_agent_card(&v).unwrap();
        assert!(!card.streaming);
        assert_eq!(
            card.select_jsonrpc_endpoint().unwrap(),
            "https://m.example.com/a2a"
        );
    }

    #[test]
    fn parse_card_v10_shape() {
        let v = json!({
            "protocolVersion": "1.0.0",
            "name": "V10 Agent",
            "version": "2.0.0",
            "capabilities": { "streaming": false },
            "supportedInterfaces": [
                { "url": "https://b.example.com/grpc", "protocolBinding": "GRPC", "protocolVersion": "1.0.0" },
                { "url": "https://b.example.com/rpc", "protocolBinding": "JSONRPC", "protocolVersion": "1.0.0" }
            ]
        });
        let card = parse_agent_card(&v).unwrap();
        assert_eq!(card.name, "V10 Agent");
        assert!(!card.streaming);
        assert_eq!(
            card.select_jsonrpc_endpoint().unwrap(),
            "https://b.example.com/rpc"
        );
    }

    // ---- endpoint selection -------------------------------------------------

    #[test]
    fn endpoint_selection_honors_preferred_order() {
        // Two JSONRPC interfaces: the first (preferred top-level url) wins.
        let v = json!({
            "name": "x", "version": "1",
            "url": "https://first.example.com/rpc",
            "preferredTransport": "JSONRPC",
            "additionalInterfaces": [
                { "url": "https://second.example.com/rpc", "transport": "JSONRPC" }
            ]
        });
        let card = parse_agent_card(&v).unwrap();
        assert_eq!(
            card.select_jsonrpc_endpoint().unwrap(),
            "https://first.example.com/rpc"
        );
    }

    #[test]
    fn endpoint_selection_errors_when_no_jsonrpc() {
        let v = json!({
            "name": "x", "version": "1",
            "supportedInterfaces": [
                { "url": "https://g.example.com", "protocolBinding": "GRPC" }
            ]
        });
        let card = parse_agent_card(&v).unwrap();
        let err = card.select_jsonrpc_endpoint().unwrap_err();
        assert!(err.contains("JSON-RPC"));
    }

    #[test]
    fn transport_token_normalizes_variants() {
        assert!(is_jsonrpc("JSONRPC"));
        assert!(is_jsonrpc("jsonrpc"));
        assert!(is_jsonrpc("JSON-RPC"));
        assert!(!is_jsonrpc("GRPC"));
        assert!(!is_jsonrpc("HTTP+JSON"));
    }

    // ---- JSON-RPC envelope --------------------------------------------------

    #[test]
    fn envelope_error_wins_over_result() {
        let v = json!({ "jsonrpc": "2.0", "id": "1",
            "error": { "code": -32001, "message": "Task not found" },
            "result": null });
        let err = parse_jsonrpc_envelope(&v).unwrap_err();
        assert_eq!(err, "-32001: Task not found");
    }

    #[test]
    fn envelope_returns_result() {
        let v = json!({ "jsonrpc": "2.0", "id": "1", "result": { "kind": "task", "id": "t1" } });
        let r = parse_jsonrpc_envelope(&v).unwrap();
        assert_eq!(r.get("id").unwrap(), "t1");
    }

    #[test]
    fn envelope_missing_both_errors() {
        let v = json!({ "jsonrpc": "2.0", "id": "1" });
        assert!(parse_jsonrpc_envelope(&v).is_err());
    }

    // ---- send-result dispatch ----------------------------------------------

    #[test]
    fn dispatch_direct_message() {
        let result = json!({
            "kind": "message",
            "role": "agent",
            "parts": [ { "kind": "text", "text": "hello there" } ]
        });
        assert_eq!(
            dispatch_send_result(&result),
            SendDispatch::DirectMessage(vec!["hello there".to_string()])
        );
    }

    #[test]
    fn dispatch_task_with_id() {
        let result = json!({
            "kind": "task",
            "id": "task-42",
            "status": { "state": "working" }
        });
        assert_eq!(
            dispatch_send_result(&result),
            SendDispatch::Task {
                id: "task-42".to_string(),
                initial_text: vec![]
            }
        );
    }

    #[test]
    fn dispatch_task_without_kind_via_structure() {
        // No `kind` but has status+id → task.
        let result = json!({ "id": "t9", "status": { "state": "submitted" } });
        assert!(matches!(
            dispatch_send_result(&result),
            SendDispatch::Task { .. }
        ));
    }

    #[test]
    fn dispatch_task_without_id_is_unusable() {
        let result = json!({ "kind": "task", "status": { "state": "working" } });
        assert_eq!(dispatch_send_result(&result), SendDispatch::UnusableTask);
    }

    // ---- part / task text ---------------------------------------------------

    #[test]
    fn non_text_parts_are_summarized() {
        let file = json!({ "kind": "file", "file": { "name": "out.txt" } });
        assert_eq!(part_to_text(&file), Some("[file: out.txt]".to_string()));
        let data = json!({ "kind": "data", "data": { "x": 1 } });
        assert_eq!(part_to_text(&data), Some("[data]".to_string()));
        let text = json!({ "kind": "text", "text": "hi" });
        assert_eq!(part_to_text(&text), Some("hi".to_string()));
    }

    #[test]
    fn task_text_gathers_status_and_artifacts() {
        let task = json!({
            "id": "t",
            "status": { "state": "working", "message": { "parts": [ { "kind": "text", "text": "step 1" } ] } },
            "artifacts": [
                { "parts": [ { "kind": "text", "text": "result A" } ] },
                { "parts": [ { "kind": "text", "text": "result B" } ] }
            ]
        });
        assert_eq!(
            task_text_chunks(&task),
            vec![
                "step 1".to_string(),
                "result A".to_string(),
                "result B".to_string()
            ]
        );
    }

    // ---- state mapping table -----------------------------------------------

    #[test]
    fn state_mapping_table() {
        assert_eq!(classify_state("submitted", None), StateClass::NonTerminal);
        assert_eq!(classify_state("working", None), StateClass::NonTerminal);
        assert_eq!(classify_state("completed", None), StateClass::Finished);
        assert_eq!(
            classify_state("failed", Some("boom".into())),
            StateClass::Failed(Some("boom".into()))
        );
        assert_eq!(classify_state("rejected", None), StateClass::Failed(None));
        assert_eq!(classify_state("canceled", None), StateClass::Stopped);
        assert_eq!(
            classify_state("input-required", None),
            StateClass::NeedsInput
        );
        assert_eq!(
            classify_state("auth-required", None),
            StateClass::NeedsInput
        );
        assert_eq!(classify_state("weird", None), StateClass::Unknown);
    }

    #[test]
    fn terminal_outcome_folds_needs_input_to_failure() {
        assert_eq!(terminal_outcome("submitted", None), None);
        assert_eq!(
            terminal_outcome("completed", None),
            Some(RemoteOutcome::Finished)
        );
        assert_eq!(
            terminal_outcome("canceled", None),
            Some(RemoteOutcome::Stopped)
        );
        match terminal_outcome("input-required", None) {
            Some(RemoteOutcome::Failed(m)) => assert!(m.contains("multi-turn")),
            other => panic!("unexpected: {other:?}"),
        }
        match terminal_outcome("failed", Some("disk full".into())) {
            Some(RemoteOutcome::Failed(m)) => assert!(m.contains("disk full")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ---- dedupe -------------------------------------------------------------

    #[test]
    fn dedup_emits_each_chunk_once() {
        let mut d = TextDedup::new();
        assert_eq!(d.push("a"), Some("a".to_string()));
        assert_eq!(d.push("a"), None);
        assert_eq!(d.push("b"), Some("b".to_string()));
        assert_eq!(d.push(""), None);
    }

    // ---- SSE parser ---------------------------------------------------------

    #[test]
    fn sse_single_frame() {
        let mut p = SseParser::new();
        let out = p.feed("data: {\"a\":1}\n\n");
        assert_eq!(out, vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn sse_multiline_data_joined_with_newline() {
        let mut p = SseParser::new();
        let out = p.feed("data: line1\ndata: line2\n\n");
        assert_eq!(out, vec!["line1\nline2".to_string()]);
    }

    #[test]
    fn sse_comments_and_crlf() {
        let mut p = SseParser::new();
        // `:` comment line is ignored; CRLF line endings are handled.
        let out = p.feed(": keep-alive\r\ndata: {\"ok\":true}\r\n\r\n");
        assert_eq!(out, vec!["{\"ok\":true}".to_string()]);
    }

    #[test]
    fn sse_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed("data: {\"par").is_empty());
        assert!(p.feed("t\":1}").is_empty());
        let out = p.feed("\n\n");
        assert_eq!(out, vec!["{\"part\":1}".to_string()]);
    }

    #[test]
    fn sse_two_frames_one_chunk() {
        let mut p = SseParser::new();
        let out = p.feed("data: a\n\ndata: b\n\n");
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn sse_garbage_is_returned_and_parses_none() {
        // The parser returns the payload even if it's not JSON — the consumer's
        // serde parse then drops it. Here we assert the raw payload comes through.
        let mut p = SseParser::new();
        let out = p.feed("data: not json\n\n");
        assert_eq!(out, vec!["not json".to_string()]);
        assert!(serde_json::from_str::<Value>(&out[0]).is_err());
    }

    #[test]
    fn request_ids_are_unique_and_shaped() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.chars().filter(|c| *c == '-').count(), 4);
    }

    // ---- driver protocol loop against a MOCK transport (no network) ---------

    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// A scriptable, no-network [`A2aTransport`] for driver-flow tests. `call`
    /// dispatches on method; `tasks/get` walks a queue (repeating its last entry
    /// so a poll loop can spin), `tasks/cancel` is counted.
    struct MockTransport {
        send_result: Mutex<Option<Result<Value, String>>>,
        get_results: Mutex<VecDeque<Result<Value, String>>>,
        stream_frames: Mutex<Option<Vec<Result<Value, String>>>>,
        cancel_calls: AtomicUsize,
        get_calls: AtomicUsize,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                send_result: Mutex::new(None),
                get_results: Mutex::new(VecDeque::new()),
                stream_frames: Mutex::new(None),
                cancel_calls: AtomicUsize::new(0),
                get_calls: AtomicUsize::new(0),
            }
        }
        fn with_send(self, r: Result<Value, String>) -> Self {
            *self.send_result.lock().unwrap() = Some(r);
            self
        }
        fn with_gets(self, rs: Vec<Result<Value, String>>) -> Self {
            *self.get_results.lock().unwrap() = rs.into();
            self
        }
        fn with_stream(self, frames: Vec<Result<Value, String>>) -> Self {
            *self.stream_frames.lock().unwrap() = Some(frames);
            self
        }
    }

    impl A2aTransport for MockTransport {
        fn fetch_card(
            &self,
            _url: String,
            _token: Option<String>,
        ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
            let card = json!({ "name": "Mock", "version": "1", "url": "https://mock/rpc" });
            async move { Ok(card) }
        }

        fn call(
            &self,
            _endpoint: String,
            _token: Option<String>,
            method: String,
            _params: Value,
        ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
            let result = if method == METHOD_MESSAGE_SEND {
                self.send_result
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or_else(|| Err("no send scripted".to_string()))
            } else if method == METHOD_TASKS_GET {
                self.get_calls.fetch_add(1, Ordering::Relaxed);
                let mut q = self.get_results.lock().unwrap();
                if q.len() > 1 {
                    q.pop_front().unwrap()
                } else {
                    q.front()
                        .cloned()
                        .unwrap_or_else(|| Err("no get scripted".to_string()))
                }
            } else if method == METHOD_TASKS_CANCEL {
                self.cancel_calls.fetch_add(1, Ordering::Relaxed);
                Ok(json!({}))
            } else {
                Err(format!("unexpected method {method}"))
            };
            async move { result }
        }

        fn stream(
            &self,
            _endpoint: String,
            _token: Option<String>,
            _method: String,
            _params: Value,
        ) -> impl std::future::Future<Output = Result<A2aStream, String>> + Send {
            let frames = self.stream_frames.lock().unwrap().clone();
            async move {
                match frames {
                    Some(fs) => {
                        let s = futures_util::stream::iter(fs);
                        Ok(Box::pin(s) as A2aStream)
                    }
                    None => Err("no stream scripted".to_string()),
                }
            }
        }
    }

    /// Run a future to completion on a current-thread runtime with the time
    /// driver enabled (the repo enables the tokio `rt`+`time` features but uses
    /// no `#[tokio::test]`, so we build the runtime explicitly).
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn tiny() -> (Duration, Duration) {
        (Duration::from_millis(1), Duration::from_secs(60))
    }

    #[test]
    fn driver_direct_message_happy_path() {
        block_on(async {
            let t = MockTransport::new().with_send(Ok(json!({
                "kind": "message",
                "parts": [ { "kind": "text", "text": "all done" } ]
            })));
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "do it",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            assert_eq!(outcome, RemoteOutcome::Finished);
            assert_eq!(out, vec!["all done".to_string()]);
        });
    }

    #[test]
    fn driver_task_poll_until_completed() {
        block_on(async {
            let t = MockTransport::new()
                .with_send(Ok(
                    json!({ "kind": "task", "id": "t1", "status": { "state": "submitted" } }),
                ))
                .with_gets(vec![
                    Ok(json!({ "id": "t1", "status": { "state": "working" } })),
                    Ok(json!({
                        "id": "t1",
                        "status": { "state": "completed" },
                        "artifacts": [ { "parts": [ { "kind": "text", "text": "final answer" } ] } ]
                    })),
                ]);
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            assert_eq!(outcome, RemoteOutcome::Finished);
            assert_eq!(out, vec!["final answer".to_string()]);
            assert!(t.get_calls.load(Ordering::Relaxed) >= 2);
        });
    }

    #[test]
    fn driver_task_failed_includes_server_message() {
        block_on(async {
            let t = MockTransport::new()
                .with_send(Ok(
                    json!({ "kind": "task", "id": "t1", "status": { "state": "working" } }),
                ))
                .with_gets(vec![Ok(json!({
                    "id": "t1",
                    "status": {
                        "state": "failed",
                        "message": { "parts": [ { "kind": "text", "text": "out of credits" } ] }
                    }
                }))]);
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            match outcome {
                RemoteOutcome::Failed(m) => assert!(m.contains("out of credits"), "got: {m}"),
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn driver_cancel_mid_poll_calls_tasks_cancel_and_stops() {
        block_on(async {
            let t = MockTransport::new()
                .with_send(Ok(
                    json!({ "kind": "task", "id": "t1", "status": { "state": "working" } }),
                ))
                // Never terminal — the poll would spin until we cancel.
                .with_gets(vec![Ok(
                    json!({ "id": "t1", "status": { "state": "working" } }),
                )]);
            let (tx, mut rx) = mpsc::unbounded_channel::<()>();
            tx.send(()).unwrap(); // request a stop before the first poll wait
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            assert_eq!(outcome, RemoteOutcome::Stopped);
            assert_eq!(t.cancel_calls.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn driver_input_required_is_actionable_failure() {
        block_on(async {
            let t = MockTransport::new()
                .with_send(Ok(
                    json!({ "kind": "task", "id": "t1", "status": { "state": "working" } }),
                ))
                .with_gets(vec![Ok(
                    json!({ "id": "t1", "status": { "state": "input-required" } }),
                )]);
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            match outcome {
                RemoteOutcome::Failed(m) => assert!(m.contains("multi-turn"), "got: {m}"),
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn driver_poll_cap_times_out() {
        block_on(async {
            let t = MockTransport::new()
                .with_send(Ok(
                    json!({ "kind": "task", "id": "t1", "status": { "state": "working" } }),
                ))
                .with_gets(vec![Ok(
                    json!({ "id": "t1", "status": { "state": "working" } }),
                )]);
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            // Zero cap: the first poll wait immediately exceeds the deadline.
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                Duration::from_millis(1),
                Duration::from_millis(0),
            )
            .await;
            match outcome {
                RemoteOutcome::Failed(m) => assert!(m.contains("timed out"), "got: {m}"),
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn driver_send_jsonrpc_error_fails() {
        block_on(async {
            let t = MockTransport::new().with_send(Err("-32600: Invalid Request".to_string()));
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                false,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            match outcome {
                RemoteOutcome::Failed(m) => assert!(m.contains("Invalid Request"), "got: {m}"),
                other => panic!("expected Failed, got {other:?}"),
            }
        });
    }

    #[test]
    fn driver_streaming_happy_path() {
        block_on(async {
            let t = MockTransport::new().with_stream(vec![
                Ok(json!({
                    "kind": "artifact-update",
                    "taskId": "t1",
                    "artifact": { "parts": [ { "kind": "text", "text": "chunk one" } ] }
                })),
                Ok(json!({
                    "kind": "status-update",
                    "taskId": "t1",
                    "status": { "state": "completed" },
                    "final": true
                })),
            ]);
            let (_tx, mut rx) = mpsc::unbounded_channel::<()>();
            let mut out: Vec<String> = Vec::new();
            let mut on = |s: &str| out.push(s.to_string());
            let (pi, pc) = tiny();
            let outcome = run_remote_protocol(
                &t,
                "https://mock/rpc",
                None,
                "go",
                true,
                &mut rx,
                &mut on,
                pi,
                pc,
            )
            .await;
            assert_eq!(outcome, RemoteOutcome::Finished);
            assert_eq!(out, vec!["chunk one".to_string()]);
        });
    }
}
