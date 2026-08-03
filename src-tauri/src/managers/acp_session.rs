//! Warm ACP session manager: the child-process lifecycle for `CliProtocol::Acp`
//! agents. Spawns an agent binary as a long-lived ACP server (Task 1's wire
//! types + Task 2's codec + Task 6's `AcpClient`/`AcpTransport`), keeps it warm
//! between instructions so follow-ups retain context, reaps it when idle, and
//! never leaks an orphan child.
//!
//! The turn loop itself (streaming `session/update` into `RunEvent`s, the
//! permission dance) is Task 8's driver — it owns the event emission and calls
//! back into `LiveSession`. This module owns only: spawn, handshake
//! (`initialize` + `session/new`), warm reuse, idle reaping, and shutdown.
//!
//! `AcpSessionManager` is constructed and managed as Tauri state, and its
//! idle-reaper/shutdown paths are already wired up in `lib.rs` — but nothing
//! calls `acquire()` yet: that seam (`AgentKind::Cli` + `CliProtocol::Acp` in
//! `agent_run.rs::start`) is Task 8's `drive_acp_run`. Silence dead-code on the
//! acquire→spawn→handshake→`LiveSession` subtree until it is wired up, same as
//! `acp/protocol.rs`, `acp/codec.rs`, `acp/permission.rs` and `acp/client.rs`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use crate::acp::client::{AcpClient, AcpTransport, PumpItem};
use crate::acp::codec::MAX_LINE_BYTES;
use crate::acp::permission::SessionOverride;
use crate::acp::protocol::{
    ClientCapabilities, ClientInfo, ContentBlock, InitializeParams, InitializeResult,
    NewSessionParams, NewSessionResult, PromptParams, SUPPORTED_PROTOCOL_VERSION,
};
use crate::managers::agent_run::{apply_baseline_env, spawn_plan, terminate_child};
use crate::settings::{default_acp_template, AgentDefinition};

/// A warm session may only be reused for the exact cwd it was created with: the
/// agent's entire context is rooted there, so reusing across a project change
/// would silently operate on the wrong repo.
pub fn should_reuse(existing_cwd: &str, wanted_cwd: &str) -> bool {
    !existing_cwd.is_empty() && existing_cwd == wanted_cwd
}

/// `timeout_secs == 0` means never expire. A clock that moves backwards must
/// not expire a live session.
pub fn is_expired(last_used_ms: i64, now_ms: i64, timeout_secs: u32) -> bool {
    if timeout_secs == 0 {
        return false;
    }
    now_ms.saturating_sub(last_used_ms) > (timeout_secs as i64) * 1000
}

/// Split an ACP argv template. Reuses the same tokenizer the raw driver uses so
/// quoting behaves identically in both modes.
pub fn build_acp_argv(template: &str) -> Vec<String> {
    crate::managers::agent_run::tokenize_template(template)
}

/// Current wall-clock time in epoch milliseconds — the single clock source for
/// `touch`/`is_expired` so a session's "last used" and the reaper's "now" are
/// always comparable.
fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// Resolve the binary to exec for an ACP session. `acp_command_template` is
/// argv-only (§4.1 of DESIGN-acp-agents.md — deliberately separate from the
/// raw `command_template` so toggling protocol never destroys the other mode's
/// config), so the program itself is expected in the same `binary_path` field
/// the raw driver already reads regardless of protocol. Falls back to the
/// built-in per-`cli_type` hint (`npx` for the official Claude/Codex ACP
/// adapters, `kimi`'s built-in `acp` subcommand) when `binary_path` is still
/// empty — e.g. before an agent has been through an ACP-aware editor flow.
fn resolve_acp_binary(agent: &AgentDefinition) -> Result<String, String> {
    if !agent.binary_path.trim().is_empty() {
        return Ok(agent.binary_path.clone());
    }
    match agent.cli_type.and_then(default_acp_template) {
        Some((binary, _argv)) => Ok(binary),
        None => Err(format!(
            "'{}' has no binary configured for ACP mode. Set a binary path in its settings.",
            agent.name
        )),
    }
}

/// Production `AcpTransport`: the agent child's stdin/stdout, framed as
/// newline-delimited JSON-RPC. A background task drains stdout line-by-line
/// into an unbounded channel so `recv` never blocks on the writer side; the
/// task ends (closing the channel, so `recv` yields `None`) when the agent's
/// stdout closes — the same signal `AcpClient::pump` already treats as "the
/// child exited".
pub struct StdioTransport {
    stdin: tokio::sync::Mutex<ChildStdin>,
    inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
}

impl StdioTransport {
    /// Takes ownership of both pipes and spawns the stdout-draining task.
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        // Bounded by MAX_LINE_BYTES: a legitimate ACP frame can
                        // be large (tool output, file content), but an agent
                        // that never emits a newline must not be able to grow
                        // memory downstream without bound. Drop and log rather
                        // than forwarding an oversized line.
                        if line.len() > MAX_LINE_BYTES {
                            log::warn!(
                                "acp: dropped an inbound line of {} bytes (over the {}-byte cap)",
                                line.len(),
                                MAX_LINE_BYTES
                            );
                            continue;
                        }
                        if tx.send(line).is_err() {
                            break; // the transport (receiver) is gone
                        }
                    }
                    Ok(None) => break, // agent closed stdout
                    Err(e) => {
                        log::warn!("acp: error reading agent stdout: {e}");
                        break;
                    }
                }
            }
        });
        Self {
            stdin: tokio::sync::Mutex::new(stdin),
            inbound: tokio::sync::Mutex::new(rx),
        }
    }
}

impl AcpTransport for StdioTransport {
    async fn send(&self, line: String) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn recv(&self) -> Option<String> {
        self.inbound.lock().await.recv().await
    }
}

/// A warm ACP session: the spawned child, its `AcpClient`, and enough state to
/// decide reuse (`should_reuse`)/expiry (`is_expired`)/turn-serialization. The
/// turn loop itself — correlating `session/prompt`'s response, streaming
/// `session/update` into `RunEvent`s, answering permission requests — lives in
/// Task 8's driver; this only exposes the primitives it needs.
pub struct LiveSession {
    pub session_id: String,
    pub agent_id: String,
    pub cwd: String,
    client: AcpClient<StdioTransport>,
    child: tokio::sync::Mutex<Child>,
    last_used_ms: AtomicI64,
    idle_timeout_secs: u32,
    /// Serializes turns: ACP does not guarantee concurrent prompts on one
    /// session, so a second trigger waits here rather than racing the first.
    /// Task 8 holds this guard for the full duration of one `session/prompt`
    /// round trip.
    turn_lock: tokio::sync::Mutex<()>,
    session_override: Mutex<Option<SessionOverride>>,
}

impl LiveSession {
    fn last_used_ms(&self) -> i64 {
        self.last_used_ms.load(Ordering::SeqCst)
    }

    /// Record that the session was just used, resetting its idle clock.
    pub fn touch(&self, now_ms: i64) {
        self.last_used_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Read the next actionable item from the agent (a response to our
    /// request, a `session/update`, or an inbound request). `None` once the
    /// child's stdout closes — the caller must treat that as a crash: never
    /// auto-retry the in-flight turn (it may have half-applied edits), drop
    /// the session, surface the failure, and let the next `acquire` respawn.
    pub async fn client_pump(&self) -> Option<PumpItem> {
        self.client.pump().await
    }

    /// Send `session/prompt`; returns the JSON-RPC request id so the caller
    /// can correlate the eventual `PumpItem::Response` yielded by
    /// `client_pump`.
    pub async fn send_prompt(&self, text: &str) -> Result<u64, String> {
        let params = PromptParams {
            session_id: self.session_id.clone(),
            prompt: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        };
        let value = serde_json::to_value(&params).map_err(|e| e.to_string())?;
        self.client.send_request("session/prompt", value).await
    }

    /// `session/cancel` — ends the in-flight turn only; the session itself
    /// stays warm (cancel is not close).
    pub async fn cancel(&self) -> Result<(), String> {
        self.client
            .send_notification("session/cancel", json!({ "sessionId": self.session_id }))
            .await
    }

    pub fn set_override(&self, ov: SessionOverride) {
        *self.session_override.lock().unwrap() = Some(ov);
    }

    pub fn take_override(&self) -> Option<SessionOverride> {
        self.session_override.lock().unwrap().take()
    }

    /// Acquire the one-turn-at-a-time lock. Hold it for the full duration of a
    /// `session/prompt` round trip; a second trigger blocks here instead of
    /// racing the first (concurrent turns on one session are not an ACP
    /// guarantee).
    pub async fn turn_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.turn_lock.lock().await
    }

    async fn is_alive(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    /// `session/close`, then the existing SIGTERM→SIGKILL stop ladder.
    /// `terminate_child` has its own grace-period backstop, so this never
    /// blocks forever even if the agent never answers `session/close`.
    async fn close(&self) {
        let _ = self
            .client
            .send_request("session/close", json!({ "sessionId": self.session_id }))
            .await;
        let mut child = self.child.lock().await;
        terminate_child(&mut child).await;
    }
}

/// Registry of warm ACP sessions, keyed by `agent_id`: at most one live
/// session per agent. A project-path change or a dead child invalidates it
/// (`should_reuse`, `is_alive`) rather than silently reusing state rooted in
/// the wrong repo.
pub struct AcpSessionManager {
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
}

impl Default for AcpSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Return the warm session for `agent.id`, reusing it only when its `cwd`
    /// still matches `cwd` and its child is still alive; otherwise end any
    /// stale session and spawn a fresh one (spawn → `initialize` →
    /// `session/new`).
    pub async fn acquire(
        &self,
        agent: &AgentDefinition,
        cwd: &Path,
    ) -> Result<Arc<LiveSession>, String> {
        let wanted_cwd = cwd.to_string_lossy().to_string();

        let existing = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(&agent.id).cloned()
        };
        if let Some(session) = existing {
            if should_reuse(&session.cwd, &wanted_cwd) && session.is_alive().await {
                session.touch(now_ms());
                return Ok(session);
            }
            // Wrong cwd, or the child died under us — never silently reuse.
            self.end_session(&agent.id).await;
        }

        let session = spawn_session(agent, cwd).await?;
        self.sessions
            .lock()
            .unwrap()
            .insert(agent.id.clone(), Arc::clone(&session));
        Ok(session)
    }

    /// `session/close` + the SIGTERM→SIGKILL ladder, then drop the registry
    /// entry. A no-op if `agent_id` has no live session.
    pub async fn end_session(&self, agent_id: &str) {
        let session = self.sessions.lock().unwrap().remove(agent_id);
        if let Some(session) = session {
            session.close().await;
        }
    }

    /// Close every session idle beyond its own `acp_idle_timeout_secs`
    /// (`0` = never — see `is_expired`). Intended to be driven by a periodic
    /// timer (60s) from the app's setup.
    pub async fn reap_idle(&self, now_ms: i64) {
        let stale: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, s)| is_expired(s.last_used_ms(), now_ms, s.idle_timeout_secs))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in stale {
            self.end_session(&id).await;
        }
    }

    /// Close every live session. Called from the app's exit handler so no
    /// child is ever left running after OpenFlow quits.
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions.keys().cloned().collect()
        };
        for id in ids {
            self.end_session(&id).await;
        }
    }
}

/// Spawn the agent binary as an ACP server, run the handshake (`initialize`
/// then `session/new`), and wrap the result as a `LiveSession`. Any failure
/// after the child is spawned kills it before returning — a
/// partially-initialized ACP agent must never be left running unattended,
/// since nothing else holds a handle to it yet.
async fn spawn_session(agent: &AgentDefinition, cwd: &Path) -> Result<Arc<LiveSession>, String> {
    let binary = resolve_acp_binary(agent)?;
    let argv = build_acp_argv(&agent.acp_command_template);
    let idle_timeout_secs = agent.acp_idle_timeout_secs;

    // Same plumbing as the raw driver: `spawn_plan` handles the Windows
    // `.cmd`/`.bat` npm-shim case (two of our three ACP agents launch via
    // `npx`), and `apply_baseline_env` restores the Homebrew/nvm/cargo PATH a
    // GUI-launched process otherwise lacks. Do not reinvent either.
    let plan = spawn_plan(&binary, cfg!(windows));
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.pre_args)
        .args(&argv)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_baseline_env(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ACP agent '{binary}': {e}"))?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    // Drain stderr so a chatty agent never blocks on a full pipe. There is no
    // run yet to attribute this to (a session outlives any one run); log it
    // for post-mortem diagnosis of a crash.
    if let Some(stderr) = child.stderr.take() {
        let agent_id = agent.id.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("acp[{agent_id}] stderr: {line}");
            }
        });
    }

    let transport = StdioTransport::new(stdin, stdout);
    let client = AcpClient::new(transport);

    match handshake(&client, cwd).await {
        Ok(session_id) => Ok(Arc::new(LiveSession {
            session_id,
            agent_id: agent.id.clone(),
            cwd: cwd.to_string_lossy().to_string(),
            client,
            child: tokio::sync::Mutex::new(child),
            last_used_ms: AtomicI64::new(now_ms()),
            idle_timeout_secs,
            turn_lock: tokio::sync::Mutex::new(()),
            session_override: Mutex::new(None),
        })),
        Err(e) => {
            // The child isn't registered in `sessions` yet — if we return
            // without killing it here, it is orphaned forever.
            terminate_child(&mut child).await;
            Err(e)
        }
    }
}

/// `initialize` then `session/new`, in one place so `spawn_session` has a
/// single fallible step to wrap in orphan-cleanup.
async fn handshake(client: &AcpClient<StdioTransport>, cwd: &Path) -> Result<String, String> {
    let init = do_initialize(client).await?;
    if init.protocol_version != SUPPORTED_PROTOCOL_VERSION {
        return Err(format!(
            "This agent speaks ACP protocol version {}, but OpenFlow only supports version {SUPPORTED_PROTOCOL_VERSION}.",
            init.protocol_version
        ));
    }
    if !init.auth_methods.is_empty() {
        return Err(
            "This agent needs to be logged in first — run it once in a terminal.".to_string(),
        );
    }
    do_session_new(client, cwd).await
}

/// `initialize` request/response. Any stray event arriving before the answer
/// is skipped rather than treated as a protocol violation — none of our
/// target agents are known to emit anything pre-handshake, but a creative one
/// must not wedge the pump forever on the wrong frame.
async fn do_initialize(client: &AcpClient<StdioTransport>) -> Result<InitializeResult, String> {
    let params = InitializeParams {
        protocol_version: SUPPORTED_PROTOCOL_VERSION,
        client_capabilities: ClientCapabilities::v1_defaults(),
        client_info: ClientInfo {
            name: "OpenFlow".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };
    let value = serde_json::to_value(&params).map_err(|e| e.to_string())?;
    let id = client.send_request("initialize", value).await?;
    loop {
        match client.pump().await {
            Some(PumpItem::Response { id: rid, result }) if rid == id => {
                let v =
                    result.map_err(|e| format!("initialize failed: {} ({})", e.message, e.code))?;
                return serde_json::from_value(v)
                    .map_err(|e| format!("Malformed initialize result: {e}"));
            }
            Some(_) => continue,
            None => return Err("The agent exited before completing initialize.".to_string()),
        }
    }
}

/// `session/new` request/response.
async fn do_session_new(client: &AcpClient<StdioTransport>, cwd: &Path) -> Result<String, String> {
    let params = NewSessionParams {
        cwd: cwd.to_string_lossy().to_string(),
        mcp_servers: Vec::new(),
    };
    let value = serde_json::to_value(&params).map_err(|e| e.to_string())?;
    let id = client.send_request("session/new", value).await?;
    loop {
        match client.pump().await {
            Some(PumpItem::Response { id: rid, result }) if rid == id => {
                let v = result
                    .map_err(|e| format!("session/new failed: {} ({})", e.message, e.code))?;
                let parsed: NewSessionResult = serde_json::from_value(v)
                    .map_err(|e| format!("Malformed session/new result: {e}"))?;
                return Ok(parsed.session_id);
            }
            Some(_) => continue,
            None => return Err("The agent exited before completing session/new.".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        AcpPermissionPolicy, AgentCliType, AgentKind, AgentOutputMode, AgentOutputSink,
        CliProtocol, PromptDelivery,
    };

    #[test]
    fn session_is_reused_only_for_the_same_cwd() {
        assert!(should_reuse("/repo/a", "/repo/a"));
        // A project-path change must invalidate: the agent's whole context is
        // rooted at cwd, so reusing it would silently work in the wrong repo.
        assert!(!should_reuse("/repo/a", "/repo/b"));
        assert!(!should_reuse("", "/repo/a"));
    }

    #[test]
    fn idle_expiry_respects_timeout_and_zero_means_never() {
        let start = 1_000_000i64;
        assert!(!is_expired(start, start + 599_000, 600));
        assert!(is_expired(start, start + 601_000, 600));
        // 0 = never expire.
        assert!(!is_expired(start, start + 999_999_999, 0));
    }

    #[test]
    fn idle_expiry_tolerates_clock_going_backwards() {
        let start = 1_000_000i64;
        assert!(!is_expired(start, start - 50_000, 600));
    }

    #[test]
    fn acp_argv_is_built_from_the_acp_template_not_the_raw_one() {
        let argv = build_acp_argv("-y @agentclientprotocol/codex-acp");
        assert_eq!(argv, vec!["-y", "@agentclientprotocol/codex-acp"]);
        assert!(build_acp_argv("").is_empty());
        // Quoted segments hold together (a Custom template may contain a path).
        assert_eq!(
            build_acp_argv(r#"acp --root "/my dir""#),
            vec!["acp", "--root", "/my dir"]
        );
    }

    fn agent_fixture(cli_type: Option<AgentCliType>, binary_path: &str) -> AgentDefinition {
        AgentDefinition {
            id: "coder".to_string(),
            name: "Coder".to_string(),
            enabled: true,
            binding_id: "agent:coder".to_string(),
            provider_id: "openrouter".to_string(),
            model: String::new(),
            system_prompt: String::new(),
            output_mode: AgentOutputMode::Inject,
            kind: AgentKind::Cli,
            cli_type,
            binary_path: binary_path.to_string(),
            command_template: String::new(),
            project_path: String::new(),
            output_sinks: vec![AgentOutputSink::Panel],
            prompt_via: PromptDelivery::Stdin,
            remote_url: String::new(),
            remote_endpoint: String::new(),
            remote_card_name: String::new(),
            remote_card_version: String::new(),
            remote_streaming: false,
            cli_protocol: CliProtocol::Acp,
            acp_command_template: String::new(),
            acp_permission_policy: AcpPermissionPolicy::Ask,
            acp_idle_timeout_secs: 600,
        }
    }

    #[test]
    fn resolve_acp_binary_prefers_an_explicitly_configured_binary_path() {
        // Kimi's raw AND acp binary happen to be the same name, but Claude's
        // ACP adapter runs via `npx`, not the `claude` binary — an explicit
        // `binary_path` must win over any cli_type-derived guess.
        let agent = agent_fixture(Some(AgentCliType::Claude), "/opt/custom/my-claude-acp");
        assert_eq!(
            resolve_acp_binary(&agent).unwrap(),
            "/opt/custom/my-claude-acp"
        );
    }

    #[test]
    fn resolve_acp_binary_falls_back_to_the_cli_type_hint_when_unset() {
        let agent = agent_fixture(Some(AgentCliType::Codex), "");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "npx");

        let agent = agent_fixture(Some(AgentCliType::Kimi), "");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "kimi");
    }

    #[test]
    fn resolve_acp_binary_errs_actionably_when_neither_is_available() {
        // Openclaw/Hermes have no confirmed ACP adapter (`default_acp_template`
        // returns None for them), and Custom has no built-in hint either.
        let agent = agent_fixture(Some(AgentCliType::Openclaw), "");
        let err = resolve_acp_binary(&agent).unwrap_err();
        assert!(err.contains("Coder"), "error should name the agent: {err}");

        assert!(resolve_acp_binary(&agent_fixture(Some(AgentCliType::Hermes), "")).is_err());
        assert!(resolve_acp_binary(&agent_fixture(Some(AgentCliType::Custom), "")).is_err());
        assert!(resolve_acp_binary(&agent_fixture(None, "")).is_err());
    }
}
