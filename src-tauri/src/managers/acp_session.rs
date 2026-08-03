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
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use crate::acp::client::{AcpClient, AcpTransport, ClientEvent, InboundRequest, PumpItem};
use crate::acp::codec::MAX_LINE_BYTES;
use crate::acp::permission::SessionOverride;
use crate::acp::protocol::{
    ClientCapabilities, ClientInfo, ContentBlock, InitializeParams, InitializeResult,
    NewSessionParams, NewSessionResult, PromptParams, SUPPORTED_PROTOCOL_VERSION,
};
use crate::managers::agent_run::{apply_baseline_env, spawn_plan, terminate_child};
use crate::settings::{default_acp_template, AgentDefinition};

/// Cap on receiving the FIRST response from the agent (the `initialize`
/// reply). Deliberately generous: for `npx`-launched adapters (Claude, Codex)
/// this window can include a cold `npx -y` install of the whole adapter +
/// its dependency tree on a fresh npm cache, BEFORE the process ever speaks a
/// JSON-RPC byte — plausibly tens of seconds on a slow or throttled
/// connection, not the "low single-digit seconds" a warm cache sees. Once the
/// agent has answered `initialize`, it is alive and speaking the protocol, so
/// the rest of the handshake uses the much tighter `HANDSHAKE_TIMEOUT`.
const COLD_START_TIMEOUT: Duration = Duration::from_secs(120);

/// Cap on the REST of the handshake (`session/new`) once `initialize` has
/// already answered. By then any cold install is long done and the process is
/// warm, so this only needs to cover the protocol round trip itself — a
/// misconfigured template that answers `initialize` but never finishes
/// `session/new` must still fail loudly rather than hang `acquire()` forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on waiting for the `session/close` SEND to complete (not a reply — ACP
/// defines no reply wait here) before falling through to the SIGTERM→SIGKILL
/// ladder. This is a courtesy, not a guarantee — the ladder is the real
/// backstop — but `close()` runs on the app's exit path (`shutdown_all` from
/// `RunEvent::Exit`, via `block_on` on the main thread), so an agent that
/// stops draining its stdin must not be able to hang app quit.
const CLOSE_COURTESY_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Whether the idle reaper should end this session: expired AND not currently
/// mid-turn. `turn_lock` is held by Task 8's driver for the full duration of a
/// `session/prompt` round trip, so a failed `try_lock` means a turn is in
/// flight — ending the session there would SIGTERM the child mid-edit, which
/// is exactly the "reaper kills a live turn" failure this guards against. The
/// `try_lock` is released immediately (it's a point-in-time check, not held
/// across the reap), so this is a synchronous, non-blocking check safe to
/// call from inside the `sessions` map's std `Mutex` guard.
fn is_reapable(
    last_used_ms: i64,
    now_ms: i64,
    timeout_secs: u32,
    turn_lock: &tokio::sync::Mutex<()>,
) -> bool {
    is_expired(last_used_ms, now_ms, timeout_secs) && turn_lock.try_lock().is_ok()
}

/// Current wall-clock time in epoch milliseconds — the single clock source for
/// `touch`/`is_expired` so a session's "last used" and the reaper's "now" are
/// always comparable.
fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// Resolve the binary to exec for an ACP session: the built-in per-`cli_type`
/// hint (`npx` for the official Claude/Codex ACP adapters, `kimi`'s built-in
/// `acp` subcommand) takes priority, falling back to `agent.binary_path` only
/// when no hint exists (`Openclaw`/`Hermes`/`Custom` — `default_acp_template`
/// returns `None` for all three, and `binary_path` is the only escape hatch
/// for them).
///
/// This order is NOT symmetric with the raw driver, and that's deliberate:
/// raw-CLI mode *requires* `binary_path` to be set (it's the actual CLI
/// binary, e.g. `claude`/`codex`), so any agent with a confirmed ACP hint
/// **always** arrives here with `binary_path` already populated from raw mode
/// — but that's the wrong program for ACP (Claude/Codex's ACP adapters run
/// via `npx`, not the raw `claude`/`codex` binary). Preferring `binary_path`
/// would silently spawn the interactive raw CLI as if it spoke JSON-RPC:
/// piped stdio, no TTY, and no protocol handshake ever arrives — see
/// `HANDSHAKE_TIMEOUT` for the backstop that bounds the resulting hang.
fn resolve_acp_binary(agent: &AgentDefinition) -> Result<String, String> {
    if let Some((binary, _argv)) = agent.cli_type.and_then(default_acp_template) {
        return Ok(binary);
    }
    if !agent.binary_path.trim().is_empty() {
        return Ok(agent.binary_path.clone());
    }
    Err(format!(
        "'{}' has no binary configured for ACP mode. Set a binary path in its settings.",
        agent.name
    ))
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
    /// Shared with `AcpSessionManager::pending_children` until this session is
    /// promoted into `sessions` (see `acquire`) — an `Arc` so the same child
    /// stays reachable by BOTH registries during that handoff, never by
    /// neither.
    child: Arc<tokio::sync::Mutex<Child>>,
    /// This session's key in `pending_children`, cleared once promoted.
    pending_id: u64,
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
    ///
    /// Every item received `touch`es the session: a long-running turn (e.g. an
    /// 11-minute refactor, well past the 600s default idle timeout) must not
    /// look idle to the reaper just because nothing has called `acquire`
    /// again. This is one of two guards against the reaper killing a live
    /// turn — see `is_reapable`'s `turn_lock` check for the other.
    pub async fn client_pump(&self) -> Option<PumpItem> {
        let item = self.client.pump().await;
        if item.is_some() {
            self.touch(now_ms());
        }
        item
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
    /// `terminate_child` is the actual guarantee and always runs regardless
    /// of how `send_close_courtesy` resolves — see its doc comment.
    async fn close(&self) {
        send_close_courtesy(&self.client, &self.session_id, CLOSE_COURTESY_TIMEOUT).await;
        let mut child = self.child.lock().await;
        terminate_child(&mut child).await;
    }
}

/// Send `session/close`, bounded by `timeout`. This is a courtesy, not a
/// guarantee — `StdioTransport::send` does a blocking `write_all` on the
/// child's stdin, which never resolves if the agent is alive but has stopped
/// draining it, and `close()` runs on the app's exit path (`shutdown_all`
/// from `RunEvent::Exit`, via `block_on` on the main thread) — so the send is
/// bounded and its outcome (success, error, or timeout) is ignored either
/// way; `terminate_child` right after is the real guarantee. `timeout` is a
/// parameter (production always passes `CLOSE_COURTESY_TIMEOUT`) so this is
/// testable with a short duration against a transport whose `send` never
/// resolves, without waiting out the real 2s budget or spawning a process.
async fn send_close_courtesy<T: AcpTransport>(
    client: &AcpClient<T>,
    session_id: &str,
    timeout: Duration,
) {
    let _ = tokio::time::timeout(
        timeout,
        client.send_request("session/close", json!({ "sessionId": session_id })),
    )
    .await;
}

/// A pending child's teardown action, type-erased so `pending_children`
/// doesn't need to know about `tokio::process::Child` concretely.
/// Production always closes over a real spawned child and reuses
/// `terminate_child` (the SIGTERM→SIGKILL ladder) — nothing here reinvents
/// it. Type-erasing this is what lets `shutdown_all`'s pending-drain logic be
/// tested with a dummy entry, no real process required.
type PendingKill = Box<dyn (FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>>) + Send>;

/// Registry of warm ACP sessions, keyed by `agent_id`: at most one live
/// session per agent. A project-path change or a dead child invalidates it
/// (`should_reuse`, `is_alive`) rather than silently reusing state rooted in
/// the wrong repo.
pub struct AcpSessionManager {
    sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    /// Per-agent spawn lock: serializes `acquire`'s whole
    /// check-reuse→end-stale→spawn→insert sequence so two concurrent triggers
    /// for the SAME agent (a double hotkey press, or a queued follow-up
    /// landing during a cold `npx` handshake) can't both miss the registry and
    /// each spawn their own child for it. Deliberately NOT consulted by
    /// `shutdown_all` — see `pending_children` for how orphan-at-quit is
    /// actually prevented; waiting on this lock at quit was tried and
    /// reverted (it coupled quit latency to the handshake budget, up to
    /// ~161s worst case — see task-7-report.md's round-3 section). Never
    /// removed once created: agent ids are a small, bounded set (one per
    /// configured agent), so this cannot grow unbounded.
    spawn_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Spawned children not yet promoted into `sessions` (still mid-handshake)
    /// or torn down after a failed handshake. Registered synchronously right
    /// after `cmd.spawn()` succeeds, before any `.await` — so there is no
    /// window where a spawned child is invisible to both this and `sessions`.
    /// `shutdown_all` kills everything still here directly, rather than
    /// waiting for any in-flight spawn to finish: this is what keeps quit
    /// bounded (~4.5s) regardless of how wide the handshake budget is.
    pending_children: Mutex<HashMap<u64, PendingKill>>,
    next_pending_id: AtomicU64,
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
            spawn_locks: Mutex::new(HashMap::new()),
            pending_children: Mutex::new(HashMap::new()),
            next_pending_id: AtomicU64::new(1),
        }
    }

    /// Get-or-create the per-agent spawn lock used to serialize `acquire`.
    fn spawn_lock_for(&self, agent_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.spawn_locks.lock().unwrap();
        Arc::clone(
            locks
                .entry(agent_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
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
        // Held for the ENTIRE body below: a second concurrent `acquire` for
        // this same agent id queues here rather than racing this one through
        // the check-then-spawn-then-insert window (see `spawn_locks`'s doc
        // comment for the orphan this prevents). Different agent ids get
        // different locks, so this never serializes unrelated agents.
        let spawn_lock = self.spawn_lock_for(&agent.id);
        let _spawn_guard = spawn_lock.lock().await;

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
            // Safe to remove by key here: with `_spawn_guard` held, no
            // concurrent `acquire` for this agent id can have replaced this
            // entry between the read above and this removal.
            self.end_session(&agent.id).await;
        }

        let session = self.spawn_session(agent, cwd).await?;
        self.sessions
            .lock()
            .unwrap()
            .insert(agent.id.clone(), Arc::clone(&session));
        // Fully promoted: `shutdown_all` will now find this child via
        // `sessions`. Only remove the pending marker AFTER the insert above,
        // not before — so at every instant the child is reachable via
        // `pending_children` OR `sessions`, never via neither.
        self.pending_children
            .lock()
            .unwrap()
            .remove(&session.pending_id);
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
    /// (`0` = never — see `is_expired`), UNLESS a turn is currently in flight
    /// on it (`is_reapable`). Intended to be driven by a periodic timer (60s)
    /// from the app's setup.
    pub async fn reap_idle(&self, now_ms: i64) {
        let stale: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, s)| {
                    is_reapable(s.last_used_ms(), now_ms, s.idle_timeout_secs, &s.turn_lock)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        // Concurrently: N stubborn sessions (each bounded by `close`'s own
        // courtesy timeout + terminate_child's grace period) should cost
        // roughly constant wall-clock time, not N times that.
        futures_util::future::join_all(stale.iter().map(|id| self.end_session(id))).await;
    }

    /// Close every live session AND kill every still-pending (mid-handshake)
    /// child. Called from the app's exit handler so no child is ever left
    /// running after OpenFlow quits.
    ///
    /// Deliberately does NOT wait on any spawn lock — an earlier version of
    /// this fix did, to close the same orphan window `pending_children` now
    /// closes, but that coupled quit latency to however long an in-flight
    /// spawn's handshake budget allows (worst case ~161s: see
    /// task-7-report.md's round-3 section). `pending_children` closes the
    /// same window without waiting for anything: a pending child is killed
    /// directly, not awaited to finish on its own.
    pub async fn shutdown_all(&self) {
        let pending: Vec<PendingKill> = {
            let mut p = self.pending_children.lock().unwrap();
            p.drain().map(|(_, kill)| kill).collect()
        };
        let ids: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions.keys().cloned().collect()
        };

        // This runs on the main thread via `block_on` in `RunEvent::Exit` —
        // serial teardown would visibly delay app quit by the sum of every
        // session's + every pending child's teardown; concurrently, N of
        // either cost roughly constant wall-clock time instead.
        let mut futs: Vec<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = Vec::new();
        for kill in pending {
            futs.push(kill());
        }
        for id in &ids {
            futs.push(Box::pin(self.end_session(id)));
        }
        futures_util::future::join_all(futs).await;
    }
}

/// Spawn the agent binary as an ACP server, run the handshake (`initialize`
/// then `session/new`), and wrap the result as a `LiveSession`. Any failure
/// after the child is spawned kills it before returning — a
/// partially-initialized ACP agent must never be left running unattended,
/// since nothing else holds a handle to it yet.
impl AcpSessionManager {
    /// Spawn the agent binary as an ACP server, run the handshake
    /// (`initialize` then `session/new`), and wrap the result as a
    /// `LiveSession`. Registers the spawned child in `pending_children`
    /// BEFORE the handshake — synchronously, with no `.await` in between —
    /// so `shutdown_all` can always reach it, even if the app quits
    /// mid-handshake. On a failure path here, the pending marker is removed
    /// and the child is killed directly; on success, the caller (`acquire`)
    /// removes the marker only once the session is fully promoted into
    /// `sessions`, so the child is never invisible to both registries at once.
    async fn spawn_session(
        &self,
        agent: &AgentDefinition,
        cwd: &Path,
    ) -> Result<Arc<LiveSession>, String> {
        let binary = resolve_acp_binary(agent)?;
        let argv = build_acp_argv(&agent.acp_command_template);
        let idle_timeout_secs = agent.acp_idle_timeout_secs;

        // Same plumbing as the raw driver: `spawn_plan` handles the Windows
        // `.cmd`/`.bat` npm-shim case (two of our three ACP agents launch via
        // `npx`), and `apply_baseline_env` restores the Homebrew/nvm/cargo
        // PATH a GUI-launched process otherwise lacks. Do not reinvent either.
        let plan = spawn_plan(&binary, cfg!(windows));
        let mut cmd = Command::new(&plan.program);
        cmd.args(&plan.pre_args)
            .args(&argv)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_baseline_env(&mut cmd);

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn ACP agent '{binary}': {e}"))?;
        let child = Arc::new(tokio::sync::Mutex::new(child));

        // Register BEFORE anything below can yield to another task: from
        // this instant until either promotion (`acquire`, on success) or
        // removal in the `Err` arm below, `shutdown_all` can always find and
        // kill this child.
        let pending_id = self.next_pending_id.fetch_add(1, Ordering::SeqCst);
        {
            let child_for_kill = Arc::clone(&child);
            let kill: PendingKill = Box::new(move || {
                Box::pin(async move {
                    let mut c = child_for_kill.lock().await;
                    terminate_child(&mut c).await;
                })
            });
            self.pending_children
                .lock()
                .unwrap()
                .insert(pending_id, kill);
        }

        let (stdin, stdout, stderr) = {
            let mut c = child.lock().await;
            (
                c.stdin.take().expect("stdin was piped"),
                c.stdout.take().expect("stdout was piped"),
                c.stderr.take(),
            )
        };
        // Drain stderr so a chatty agent never blocks on a full pipe. There is
        // no run yet to attribute this to (a session outlives any one run);
        // log it for post-mortem diagnosis of a crash.
        if let Some(stderr) = stderr {
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

        // Bounded, in two phases — see `run_handshake`'s doc comment for why a
        // single budget doesn't work here (an `npx` cold install can dwarf
        // the handshake itself).
        match run_handshake(&client, cwd, &agent.name).await {
            Ok(session_id) => Ok(Arc::new(LiveSession {
                session_id,
                agent_id: agent.id.clone(),
                cwd: cwd.to_string_lossy().to_string(),
                client,
                child,
                pending_id,
                last_used_ms: AtomicI64::new(now_ms()),
                idle_timeout_secs,
                turn_lock: tokio::sync::Mutex::new(()),
                session_override: Mutex::new(None),
            })),
            Err(e) => {
                // Not registered in `sessions` (never was) and no longer
                // pending — if we didn't kill it here, it would be orphaned.
                self.pending_children.lock().unwrap().remove(&pending_id);
                let mut c = child.lock().await;
                terminate_child(&mut c).await;
                Err(e)
            }
        }
    }
}

/// `initialize` then `session/new`, with a deliberately two-phase timeout
/// budget rather than one flat one: `initialize` gets the generous
/// `COLD_START_TIMEOUT` (an `npx -y`-launched adapter can spend most of that
/// on a cold package install before ever speaking a byte); `session/new` gets
/// the much tighter `HANDSHAKE_TIMEOUT` (by the time `initialize` has
/// answered, the process is warm and any install is long done). The two
/// resulting timeout errors are worded differently on purpose — "never
/// started" (check the binary/template, or just wait out a slow first
/// install) and "started but didn't finish" (it's alive and installed, but
/// something about the protocol conversation itself is wrong) call for
/// different user actions.
///
/// Generic over `T: AcpTransport` (rather than concretely `StdioTransport`)
/// so this is directly testable against a scripted in-memory transport — no
/// process spawn needed to prove the handshake's frame-handling logic.
async fn run_handshake<T: AcpTransport>(
    client: &AcpClient<T>,
    cwd: &Path,
    agent_name: &str,
) -> Result<String, String> {
    let init = match tokio::time::timeout(COLD_START_TIMEOUT, do_initialize(client)).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(format!(
                "'{agent_name}' never responded to ACP initialize within {}s. If this is its \
                 first launch, `npx` may still be installing the adapter — try again once that \
                 finishes. Otherwise, check the resolved ACP binary and command template.",
                COLD_START_TIMEOUT.as_secs()
            ));
        }
    };
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
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, do_session_new(client, cwd)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!(
            "'{agent_name}' answered ACP initialize but did not complete session/new within \
             {}s. It started and is speaking the protocol, but something about the handshake \
             itself is wrong.",
            HANDSHAKE_TIMEOUT.as_secs()
        )),
    }
}

/// Reply "method not found" to an inbound request that arrives while we're
/// waiting for a handshake response, rather than silently dropping it. No ACP
/// agent is expected to call back into the client before we've answered its
/// `initialize`/`session/new`, but `client.rs`'s own doctrine for
/// `InboundRequest` is unconditional: an unanswered request hangs the
/// agent's turn forever, so we always reply — being mid-handshake rather than
/// mid-turn is not an exception to that rule.
async fn refuse_inbound<T: AcpTransport>(client: &AcpClient<T>, req: InboundRequest) {
    let id = match &req {
        InboundRequest::RequestPermission { id, .. } => id,
        InboundRequest::Unsupported { id, .. } => id,
    };
    let _ = client.reply_error(id, -32601, "Method not found").await;
}

/// `initialize` request/response. An inbound request arriving before the
/// answer is refused (`refuse_inbound`) rather than dropped; any other stray
/// event (e.g. a `session/update`) is skipped — none of our target agents are
/// known to emit one pre-handshake, but a creative one must not wedge the
/// pump forever on the wrong frame.
async fn do_initialize<T: AcpTransport>(client: &AcpClient<T>) -> Result<InitializeResult, String> {
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
            Some(PumpItem::Event(ClientEvent::Inbound(req))) => {
                refuse_inbound(client, req).await;
            }
            Some(_) => continue,
            None => return Err("The agent exited before completing initialize.".to_string()),
        }
    }
}

/// `session/new` request/response. Same inbound-request handling as
/// `do_initialize` — see `refuse_inbound`.
async fn do_session_new<T: AcpTransport>(
    client: &AcpClient<T>,
    cwd: &Path,
) -> Result<String, String> {
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
            Some(PumpItem::Event(ClientEvent::Inbound(req))) => {
                refuse_inbound(client, req).await;
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
    fn resolve_acp_binary_prefers_the_cli_type_hint_over_a_stale_binary_path() {
        // This is the exact bug found in review: raw-CLI mode REQUIRES
        // `binary_path`, so any Claude/Codex agent that ever ran in raw mode
        // already arrives here with `binary_path` populated (e.g. the real
        // `claude` binary) — but that's the WRONG program for ACP mode, whose
        // Claude adapter runs via `npx`. The hint must win, or we'd spawn the
        // interactive raw CLI as if it spoke JSON-RPC and hang forever.
        let agent = agent_fixture(Some(AgentCliType::Claude), "/usr/local/bin/claude");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "npx");

        let agent = agent_fixture(Some(AgentCliType::Codex), "/usr/local/bin/codex");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "npx");
    }

    #[test]
    fn resolve_acp_binary_falls_back_to_the_cli_type_hint_when_unset() {
        let agent = agent_fixture(Some(AgentCliType::Codex), "");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "npx");

        let agent = agent_fixture(Some(AgentCliType::Kimi), "");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "kimi");
    }

    #[test]
    fn resolve_acp_binary_falls_back_to_binary_path_when_no_hint_exists() {
        // Openclaw/Hermes/Custom have no confirmed ACP adapter
        // (`default_acp_template` returns `None` for all three) — an
        // explicitly configured `binary_path` is their only escape hatch, and
        // must still be honored.
        let agent = agent_fixture(Some(AgentCliType::Openclaw), "/opt/custom/my-openclaw-acp");
        assert_eq!(
            resolve_acp_binary(&agent).unwrap(),
            "/opt/custom/my-openclaw-acp"
        );

        let agent = agent_fixture(Some(AgentCliType::Custom), "/opt/custom/my-agent");
        assert_eq!(resolve_acp_binary(&agent).unwrap(), "/opt/custom/my-agent");
    }

    #[test]
    fn resolve_acp_binary_errs_actionably_when_neither_is_available() {
        let agent = agent_fixture(Some(AgentCliType::Openclaw), "");
        let err = resolve_acp_binary(&agent).unwrap_err();
        assert!(err.contains("Coder"), "error should name the agent: {err}");

        assert!(resolve_acp_binary(&agent_fixture(Some(AgentCliType::Hermes), "")).is_err());
        assert!(resolve_acp_binary(&agent_fixture(Some(AgentCliType::Custom), "")).is_err());
        assert!(resolve_acp_binary(&agent_fixture(None, "")).is_err());
    }

    #[test]
    fn reaper_spares_a_session_whose_turn_lock_is_held() {
        let lock = tokio::sync::Mutex::new(());
        let start = 1_000_000i64;
        let now = start + 601_000; // past the 600s default timeout

        // Free lock, expired: reapable.
        assert!(is_reapable(start, now, 600, &lock));

        // Hold the lock — simulating a turn in flight via `turn_guard` — and
        // the SAME expired session must now be spared. This is the exact bug
        // from review: an 11-minute refactor on the default 600s timeout must
        // not get SIGTERM'd mid-edit just because it's also "expired".
        let _held = lock.try_lock().unwrap();
        assert!(
            !is_reapable(start, now, 600, &lock),
            "a session with an in-flight turn must never be reaped, even if expired"
        );
    }

    #[test]
    fn spawn_lock_for_shares_one_lock_per_agent_and_a_distinct_one_per_other_agent() {
        let mgr = AcpSessionManager::new();
        let a1 = mgr.spawn_lock_for("agent-a");
        let a2 = mgr.spawn_lock_for("agent-a");
        assert!(
            Arc::ptr_eq(&a1, &a2),
            "the same agent id must always get the same spawn lock"
        );

        let b = mgr.spawn_lock_for("agent-b");
        assert!(
            !Arc::ptr_eq(&a1, &b),
            "different agents must not share a spawn lock (that would serialize unrelated agents)"
        );
    }

    /// Run a future to completion on a current-thread runtime with the time
    /// driver enabled — mirrors `a2a.rs`'s and `acp/client.rs`'s helper; this
    /// repo uses no `#[tokio::test]`.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// Scripted transport mirroring `acp/client.rs`'s own test double: replays
    /// canned inbound lines, records what we sent. Lets the handshake's
    /// frame-handling be tested without spawning a process.
    struct FakeTransport {
        inbound: std::sync::Mutex<std::collections::VecDeque<String>>,
        sent: std::sync::Mutex<Vec<String>>,
    }

    impl FakeTransport {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                inbound: std::sync::Mutex::new(lines.iter().map(|s| s.to_string()).collect()),
                sent: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl AcpTransport for FakeTransport {
        async fn send(&self, line: String) -> Result<(), String> {
            self.sent.lock().unwrap().push(line);
            Ok(())
        }
        async fn recv(&self) -> Option<String> {
            self.inbound.lock().unwrap().pop_front()
        }
    }

    #[test]
    fn handshake_replies_to_an_inbound_request_instead_of_dropping_it() {
        block_on(async {
            let transport = FakeTransport::new(vec![
                // Arrives BEFORE the initialize response. Must not be
                // silently dropped: client.rs's own doctrine is that an
                // unanswered inbound request hangs the agent's turn forever.
                r#"{"jsonrpc":"2.0","id":"a1","method":"session/request_permission",
                    "params":{"sessionId":"s1","toolCall":{"toolCallId":"t1","title":"x",
                    "kind":"edit"},"options":[]}}"#,
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[]}}"#,
            ]);
            let client = AcpClient::new(transport);

            let result = do_initialize(&client).await;
            assert!(
                result.is_ok(),
                "the real initialize response must still be found past the inbound request: {result:?}"
            );

            let sent = client.transport().sent();
            assert_eq!(
                sent.len(),
                2,
                "expected our outgoing initialize request + one reply to the inbound request, got: {sent:?}"
            );
            assert!(
                sent[1].contains(r#""id":"a1""#) && sent[1].contains("-32601"),
                "must reply Method-not-found to the inbound request rather than drop it: {sent:?}"
            );
        });
    }

    /// A transport whose `send` never resolves — models an agent that is
    /// alive but has stopped draining its stdin (the exact scenario
    /// `send_close_courtesy`'s timeout exists for). `recv` never resolves
    /// either, which is irrelevant here: this test only ever calls `send`.
    struct HangingTransport;

    impl AcpTransport for HangingTransport {
        async fn send(&self, _line: String) -> Result<(), String> {
            std::future::pending().await
        }
        async fn recv(&self) -> Option<String> {
            std::future::pending().await
        }
    }

    #[test]
    fn close_courtesy_send_is_bounded_even_when_the_transport_never_resolves() {
        block_on(async {
            let client = AcpClient::new(HangingTransport);
            // Outer timeout hardens the TEST itself, not just the assertion:
            // `cargo test` has no per-test timeout, so if `send_close_courtesy`'s
            // own inner timeout ever regresses (e.g. someone removes it), this
            // must fail with a named assertion in ~1s rather than hang CI
            // forever. The inner 20ms timeout is what's actually under test;
            // it stays far short of the outer 1s bound so a passing run is fast.
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                send_close_courtesy(&client, "s1", Duration::from_millis(20)),
            )
            .await;
            assert!(
                result.is_ok(),
                "send_close_courtesy must return well within its own timeout, not hang the test"
            );
        });
    }

    #[test]
    fn shutdown_all_does_not_wait_on_an_in_flight_spawn_lock() {
        // Round-3 regression test: an earlier fix made `shutdown_all` wait on
        // every spawn lock before tearing down, to close an orphan-at-quit
        // window — but that coupled quit latency to the handshake budget
        // (worst case ~161s). `pending_children` closes the same window
        // without waiting on anything, so holding a spawn lock (simulating an
        // `acquire` mid-`spawn_session`) must have NO effect on `shutdown_all`'s
        // latency at all.
        block_on(async {
            let mgr = AcpSessionManager::new();
            let lock = mgr.spawn_lock_for("coder");
            let _guard = lock.lock().await; // simulates acquire() mid-spawn_session

            let result = tokio::time::timeout(Duration::from_millis(100), mgr.shutdown_all()).await;
            assert!(
                result.is_ok(),
                "shutdown_all must not block on a spawn lock held by an in-flight acquire — \
                 quit must stay bounded no matter how wide the handshake budget is"
            );
        });
    }

    #[test]
    fn shutdown_all_kills_every_pending_entry_and_clears_the_registry() {
        // Process-free stand-in for "shutdown_all reaches a child that's
        // still mid-handshake": register a dummy `PendingKill` (no real
        // `Child` — pending_children is type-erased for exactly this reason)
        // and confirm shutdown_all invokes it and removes it.
        block_on(async {
            let mgr = AcpSessionManager::new();
            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let called_in_kill = Arc::clone(&called);
            let dummy: PendingKill = Box::new(move || {
                Box::pin(async move {
                    called_in_kill.store(true, Ordering::SeqCst);
                })
            });
            mgr.pending_children.lock().unwrap().insert(1, dummy);

            mgr.shutdown_all().await;

            assert!(
                called.load(Ordering::SeqCst),
                "shutdown_all must invoke every kill action still in pending_children"
            );
            assert!(
                mgr.pending_children.lock().unwrap().is_empty(),
                "shutdown_all must clear pending_children after killing everything in it"
            );
        });
    }
}
