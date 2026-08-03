//! Flow OS increment 2 — `AgentRunManager`: drive REAL local coding-agent CLIs
//! (Claude Code, Codex, …) as subprocesses, project-scoped, streaming their
//! output live into the app. Mirrors Agent OS's `ultracodeProcs` + `runner.ts`
//! pattern (spawn with a sanitized env, SIGTERM→SIGKILL stop, an in-process
//! registry of live runs keyed by run id).
//!
//! Runs are LONG (seconds→minutes) and must NEVER block the single-flight
//! `TranscriptionCoordinator`: `start` spawns the process + a detached streaming
//! task and returns a run id at once. Streaming lines are emitted as
//! `agent-run-output` events; the terminal status as `agent-run-status`. On
//! completion the configured output sinks (§6) run (panel is the live stream,
//! plus optional desktop notification and a written run file).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::a2a::{self, A2aTransport, HttpA2aTransport, RemoteOutcome};
use crate::acp::client::{ClientEvent, InboundRequest, PumpItem};
use crate::acp::events::{
    map_session_update, render_line, AgentRunEvent, PermissionOption, RunEvent,
};
use crate::acp::permission::{
    decide, pick_option, AcpPermissionPolicy, PermissionDecision, PolicyInput, SessionOverride,
};
use crate::acp::protocol::{
    PermissionOptionWire, PermissionOutcome, PromptResult, StopReason, ToolCallWire,
};
use crate::managers::acp_session::{AcpSessionManager, LiveSession};
use crate::settings::{
    AgentCliType, AgentDefinition, AgentKind, AgentOutputSink, CliProtocol, PromptDelivery,
};

/// Cap on the rolling per-run output buffer. Enough to keep a useful tail for
/// the panel and the written file, bounded so a chatty run can't grow memory
/// (or the persisted file) without limit.
const OUTPUT_BUFFER_CAP: usize = 1_000_000; // ~1 MiB

/// Terminal/live status of a run. Internally tagged so the TS side is a clean
/// discriminated union: `{ status: "running" } | { status: "finished", code }`
/// | `{ status: "failed", error }` | `{ status: "stopped" }`.
#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Finished { code: i32 },
    Failed { error: String },
    Stopped,
}

impl RunStatus {
    fn is_terminal(&self) -> bool {
        !matches!(self, RunStatus::Running)
    }
}

/// Emitted per output line while a run streams. Event name: `agent-run-output`.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct AgentRunOutput {
    pub run_id: String,
    pub chunk: String,
}

/// Emitted when a run reaches a terminal status. Event name: `agent-run-status`.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct AgentRunStatus {
    pub run_id: String,
    pub status: RunStatus,
}

/// A snapshot of a run for the frontend (`list_agent_runs`).
#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentRunInfo {
    pub run_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub project_path: String,
    pub status: RunStatus,
    /// RFC3339 local start time (for display).
    pub started_at: String,
    /// Epoch milliseconds — for stable sorting and elapsed-time computation.
    pub started_at_ms: i64,
    /// Rolling (capped) combined stdout+stderr buffer.
    pub output: String,
    /// The instruction (transcript) that drove the run.
    pub instruction: String,
    /// Absolute path to the written run file, once the File sink has run.
    pub output_file: Option<String>,
    /// The ACP session this run's turn belonged to; `None` for raw CLI and
    /// remote runs. ADDITIVE on purpose: one ACP turn is one run (`RunStatus`
    /// gains no variant, so no frontend `switch` breaks — DESIGN §6), and runs
    /// sharing a session id are one conversation thread in the panel.
    pub session_id: Option<String>,
}

/// Live registry entry.
struct AgentRun {
    agent_id: String,
    agent_name: String,
    project_path: String,
    status: RunStatus,
    started_at: DateTime<Local>,
    output: String,
    instruction: String,
    output_file: Option<String>,
    /// Send `()` to request a stop; `None` once the run is terminal.
    kill_tx: Option<mpsc::UnboundedSender<()>>,
    /// The ACP session this run's turn ran on; `None` for every other driver.
    session_id: Option<String>,
    /// Set by `drive_acp_run` only: the channel a user's answer to a parked
    /// permission prompt travels down to reach the turn loop. `None` once the
    /// run is terminal (nothing can be answered after that).
    permission_tx: Option<mpsc::UnboundedSender<PermissionAnswer>>,
}

impl AgentRun {
    fn to_info(&self, run_id: &str) -> AgentRunInfo {
        AgentRunInfo {
            run_id: run_id.to_string(),
            agent_id: self.agent_id.clone(),
            agent_name: self.agent_name.clone(),
            project_path: self.project_path.clone(),
            status: self.status.clone(),
            started_at: self.started_at.to_rfc3339(),
            started_at_ms: self.started_at.timestamp_millis(),
            output: self.output.clone(),
            instruction: self.instruction.clone(),
            output_file: self.output_file.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

/// In-process registry of live + recent CLI agent runs.
pub struct AgentRunManager {
    runs: Mutex<HashMap<String, AgentRun>>,
    seq: AtomicU64,
}

impl Default for AgentRunManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunManager {
    pub fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    fn next_run_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("run-{}-{}", chrono::Local::now().timestamp_millis(), n)
    }

    /// Snapshot every run, newest first.
    pub fn list_runs(&self) -> Vec<AgentRunInfo> {
        let runs = self.runs.lock().unwrap();
        let mut out: Vec<AgentRunInfo> = runs.iter().map(|(id, r)| r.to_info(id)).collect();
        out.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
        out
    }

    /// Request a stop for a running run (SIGTERM→SIGKILL, handled in the monitor
    /// task). No-op if the run is already terminal or unknown.
    pub fn stop_run(&self, run_id: &str) -> Result<(), String> {
        let runs = self.runs.lock().unwrap();
        let run = runs
            .get(run_id)
            .ok_or_else(|| format!("Run '{run_id}' not found"))?;
        match &run.kill_tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err(format!("Run '{run_id}' is not running")),
        }
    }

    /// Drop all terminal (non-running) runs from the registry.
    pub fn clear_finished(&self) {
        let mut runs = self.runs.lock().unwrap();
        runs.retain(|_, r| !r.status.is_terminal());
    }

    fn append_output(&self, run_id: &str, line: &str) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(run_id) {
            run.output.push_str(line);
            run.output.push('\n');
            // Keep only the tail once the cap is exceeded.
            if run.output.len() > OUTPUT_BUFFER_CAP {
                let start = run.output.len() - OUTPUT_BUFFER_CAP;
                // Snap to a char boundary so we never slice mid-UTF-8.
                let start = (start..run.output.len())
                    .find(|&i| run.output.is_char_boundary(i))
                    .unwrap_or(run.output.len());
                run.output = run.output[start..].to_string();
            }
        }
    }

    fn set_status(&self, run_id: &str, status: RunStatus) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status;
            run.kill_tx = None;
            // A terminal run has no turn loop left to receive an answer.
            run.permission_tx = None;
        }
    }

    /// Bind a run to the ACP session its turn ran on (ACP driver only).
    fn set_session_id(&self, run_id: &str, session_id: &str) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(run_id) {
            run.session_id = Some(session_id.to_string());
        }
    }

    /// Install the channel a user's permission answer travels down to reach
    /// this run's turn loop (ACP driver only).
    fn set_permission_sender(&self, run_id: &str, tx: mpsc::UnboundedSender<PermissionAnswer>) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(run) = runs.get_mut(run_id) {
            run.permission_tx = Some(tx);
        }
    }

    /// Hand the user's answer to a parked permission prompt to the run's turn
    /// loop, which replies to the agent. The backend half of Task 9's
    /// `respond_agent_permission` command.
    ///
    /// There is deliberately NO timeout on a parked prompt (DESIGN §8): a
    /// blocked agent is recoverable, a silent auto-deny at minute ten may leave
    /// a half-applied change. Stop is the escape hatch, and it resolves every
    /// parked prompt as `cancelled`.
    ///
    /// Exercised by this module's tests; its production caller is
    /// `commands::acp_agents::respond_agent_permission`.
    ///
    /// `option_id` is the EXACT agent-supplied option the user clicked, when
    /// the caller has one (Task 11 review, Important 5). It takes priority
    /// over `choice`-derived selection in `apply_answer`: `choice` alone only
    /// carries a kind category (allow/deny x once/always), and when an agent
    /// offers two options of the SAME kind (e.g. "Allow once" and "Allow for
    /// this directory", both `allow_once`), picking by kind alone always
    /// resolves to the first — silently answering with a different option
    /// than the one the user actually clicked. `choice` is still required: it
    /// drives the session-scoped "always" bookkeeping and the
    /// `PermissionResolved` outcome wording, and is the fallback selector if
    /// `option_id` doesn't match any of this request's current options.
    pub fn respond_permission(
        &self,
        run_id: &str,
        request_id: &str,
        choice: PermissionChoice,
        option_id: Option<String>,
    ) -> Result<(), String> {
        let runs = self.runs.lock().unwrap();
        let run = runs
            .get(run_id)
            .ok_or_else(|| format!("Run '{run_id}' not found"))?;
        let tx = run
            .permission_tx
            .as_ref()
            .ok_or_else(|| format!("Run '{run_id}' is not waiting on a permission prompt"))?;
        tx.send(PermissionAnswer {
            request_id: request_id.to_string(),
            choice,
            option_id,
        })
        .map_err(|_| format!("Run '{run_id}' is no longer running"))
    }

    /// Snapshot the current rolling output buffer for a run (for classifying a
    /// failed run's captured stderr/stdout on the run-path).
    fn current_output(&self, run_id: &str) -> String {
        let runs = self.runs.lock().unwrap();
        runs.get(run_id)
            .map(|r| r.output.clone())
            .unwrap_or_default()
    }

    /// Classify a failed run's captured output into an actionable diagnostic, or
    /// `None`. First reuses the Test-button classifier (`run_failure_diagnostic`)
    /// on the captured stderr/stdout; then, for codex specifically, falls back to
    /// the proactive static vendor check so even a failure whose text we didn't
    /// recognize still gets the actionable "reinstall Codex" guidance.
    fn classify_run_failure(
        &self,
        output: &str,
        binary: &str,
        agent: &AgentDefinition,
    ) -> Option<String> {
        if let Some(diag) = run_failure_diagnostic(output) {
            return Some(diag);
        }
        if agent.cli_type == Some(AgentCliType::Codex)
            && codex_static_vendor_hint(binary) == CodexVendorStatus::Missing
        {
            return Some(CODEX_VENDOR_MISSING_DIAGNOSTIC.to_string());
        }
        None
    }

    /// Emit a clearly-marked actionable diagnostic line to the run panel — in
    /// ADDITION to the raw output, which is never swallowed — and `log::error!`
    /// it (so handy.log records the run-path failure, which it previously did
    /// not). Used only on failure paths; the happy path never calls this.
    fn emit_diagnostic(&self, app: &AppHandle, run_id: &str, diagnostic: &str) {
        let line = format!("⚠️  {diagnostic}");
        self.append_output(run_id, &line);
        let _ = AgentRunOutput {
            run_id: run_id.to_string(),
            chunk: line,
        }
        .emit(app);
        log::error!("agent run {run_id}: {diagnostic}");
    }

    /// Spawn the agent process + a detached streaming task and return the run id
    /// immediately. Never blocks the caller (the coordinator).
    pub fn start(
        self: &Arc<Self>,
        app: &AppHandle,
        agent: AgentDefinition,
        instruction: String,
    ) -> String {
        let run_id = self.next_run_id();

        let cwd = resolve_cwd(app, &agent.project_path);
        let argv = build_argv(
            &agent.command_template,
            &cwd.to_string_lossy(),
            &instruction,
            agent.prompt_via,
        );
        let stdin_input = match agent.prompt_via {
            PromptDelivery::Stdin => Some(instruction.clone()),
            PromptDelivery::Arg => None,
        };

        let (kill_tx, kill_rx) = mpsc::unbounded_channel::<()>();

        // Register the run up front so list_runs / stop_run see it right away.
        {
            let mut runs = self.runs.lock().unwrap();
            runs.insert(
                run_id.clone(),
                AgentRun {
                    agent_id: agent.id.clone(),
                    agent_name: agent.name.clone(),
                    project_path: agent.project_path.clone(),
                    status: RunStatus::Running,
                    started_at: Local::now(),
                    output: String::new(),
                    instruction: instruction.clone(),
                    output_file: None,
                    kill_tx: Some(kill_tx),
                    // Filled in by `drive_acp_run` once its session is warm;
                    // every other driver leaves both `None`.
                    session_id: None,
                    permission_tx: None,
                },
            );
        }

        let manager = Arc::clone(self);
        let app = app.clone();
        let binary = agent.binary_path.clone();
        let run_id_task = run_id.clone();

        // Detached task: drive to completion, stream, finalize. Uses the app's
        // async runtime. The driver seam: a `Remote` (A2A) agent takes the
        // RemoteDriver; every other kind takes the CLI subprocess driver. The
        // registry entry, run_id and kill wiring above are identical for both —
        // the run panel (which only subscribes to the two events) needs nothing.
        match agent.kind {
            AgentKind::Remote => {
                tauri::async_runtime::spawn(async move {
                    manager
                        .drive_remote_run(app, run_id_task, agent, instruction, kill_rx)
                        .await;
                });
            }
            // C0: a CLI agent configured for ACP takes the session driver. A
            // third sibling behind the same seam — same registry entry, same
            // run id, same kill wiring, same two events — so the run panel
            // needs nothing. Every other agent falls through unchanged.
            _ if uses_acp_driver(&agent) => {
                tauri::async_runtime::spawn(async move {
                    manager
                        .drive_acp_run(app, run_id_task, agent, instruction, kill_rx)
                        .await;
                });
            }
            _ => {
                tauri::async_runtime::spawn(async move {
                    manager
                        .drive_run(
                            app,
                            run_id_task,
                            agent,
                            binary,
                            argv,
                            cwd,
                            stdin_input,
                            kill_rx,
                        )
                        .await;
                });
            }
        }

        run_id
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive_run(
        self: Arc<Self>,
        app: AppHandle,
        run_id: String,
        agent: AgentDefinition,
        binary: String,
        argv: Vec<String>,
        cwd: PathBuf,
        stdin_input: Option<String>,
        mut kill_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let started = std::time::Instant::now();

        // On Windows a `.cmd`/`.bat` npm shim must be launched via `cmd.exe /C`;
        // everything else spawns directly (see `spawn_plan`).
        let plan = spawn_plan(&binary, cfg!(windows));
        let mut cmd = Command::new(&plan.program);
        cmd.args(&plan.pre_args)
            .args(&argv)
            .current_dir(&cwd)
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Shared baseline env (PATH + SHELL + HOME) — identical to the Test
        // button so detection, testing, and running all agree.
        apply_baseline_env(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let err = format!("Failed to spawn '{}': {}", binary, e);
                self.append_output(&run_id, &err);
                let _ = AgentRunOutput {
                    run_id: run_id.clone(),
                    chunk: err.clone(),
                }
                .emit(&app);
                // GAP: a spawn failure otherwise dumps only the raw OS error to
                // the panel (and nothing to handy.log). Log it, and classify it
                // (plus, for codex, statically probe the vendor payload) so the
                // actionable fix reaches the panel too.
                log::error!("agent run {run_id}: {err}");
                if let Some(diag) = self.classify_run_failure(&err, &binary, &agent) {
                    self.emit_diagnostic(&app, &run_id, &diag);
                }
                self.finalize(
                    &app,
                    &run_id,
                    &agent,
                    RunStatus::Failed { error: err },
                    started,
                )
                .await;
                return;
            }
        };

        // Deliver the instruction on stdin (default), then close it so the CLI
        // knows input is done. No OS arg-length limit this way.
        if let Some(input) = stdin_input {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
        } else {
            // Still close stdin so a CLI reading it doesn't hang.
            drop(child.stdin.take());
        }

        // Line-readers for stdout + stderr, each appending to the buffer and
        // emitting `agent-run-output`.
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(tauri::async_runtime::spawn(stream_lines(
                stdout,
                Arc::clone(&self),
                app.clone(),
                run_id.clone(),
            )));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(tauri::async_runtime::spawn(stream_lines(
                stderr,
                Arc::clone(&self),
                app.clone(),
                run_id.clone(),
            )));
        }

        // Wait for exit OR a stop request (SIGTERM→SIGKILL).
        let mut stopped = false;
        let exit_code: Option<i32> = tokio::select! {
            status = child.wait() => status.ok().and_then(|s| s.code()),
            _ = kill_rx.recv() => {
                stopped = true;
                terminate_child(&mut child).await;
                None
            }
        };

        // Drain the readers so the buffer/panel has the full output.
        for r in readers {
            let _ = r.await;
        }

        let status = if stopped {
            RunStatus::Stopped
        } else {
            match exit_code {
                Some(0) => RunStatus::Finished { code: 0 },
                Some(code) => RunStatus::Finished { code },
                None => RunStatus::Failed {
                    error: "process terminated without an exit code".to_string(),
                },
            }
        };

        // GAP: a fast non-zero exit streams raw stderr to the panel without the
        // actionable classification the Test button gives, and logs nothing. On
        // a short-window failure WITH captured output, route it through the
        // classifier and surface the fix. The happy path (exit 0 / stopped) is
        // byte-for-byte unchanged — this branch never runs for it.
        let is_failure = matches!(status, RunStatus::Finished { code } if code != 0)
            || matches!(status, RunStatus::Failed { .. });
        let failed_fast = !stopped && is_failure && started.elapsed() < Duration::from_secs(10);
        if failed_fast {
            let captured = self.current_output(&run_id);
            if !captured.trim().is_empty() {
                if let Some(diag) = self.classify_run_failure(&captured, &binary, &agent) {
                    self.emit_diagnostic(&app, &run_id, &diag);
                }
            }
        }

        self.finalize(&app, &run_id, &agent, status, started).await;
    }

    /// Drive a **remote (A2A) agent** run behind the SAME run seam as
    /// `drive_run`: it reuses `append_output` / the `AgentRunOutput` event /
    /// `finalize` / the kill channel / `AgentRunStatus` verbatim, so the run
    /// panel needs zero changes. The protocol itself lives in `crate::a2a`
    /// (`run_remote_protocol`), which is unit-tested against a mock transport
    /// with no network — this method is the thin glue (endpoint resolution,
    /// header line, live-output plumbing, status mapping).
    async fn drive_remote_run(
        self: Arc<Self>,
        app: AppHandle,
        run_id: String,
        agent: AgentDefinition,
        instruction: String,
        mut kill_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let started = std::time::Instant::now();
        let transport = HttpA2aTransport::new();
        // Per-agent bearer token from the OS keyring (scope "agent", account =
        // agent id). Never in the settings store; absent for a public agent.
        let token = crate::keychain::get_api_key("agent", &agent.id);

        // 1. Resolve the JSON-RPC endpoint. Cached on the agent after a UI card
        //    fetch; if empty we attempt one fetch+resolve here, and a failure is
        //    an actionable Failed (raw English in the stream, like CLI output).
        let endpoint = match self
            .resolve_remote_endpoint(&transport, &agent, token.clone())
            .await
        {
            Ok(ep) => ep,
            Err(e) => {
                self.emit_line(&app, &run_id, &e);
                self.finalize(
                    &app,
                    &run_id,
                    &agent,
                    RunStatus::Failed { error: e },
                    started,
                )
                .await;
                return;
            }
        };

        // 2. Header line so the panel shows where this is going.
        let name = if agent.remote_card_name.trim().is_empty() {
            agent.remote_url.clone()
        } else {
            agent.remote_card_name.clone()
        };
        self.emit_line(&app, &run_id, &format!("→ {name} @ {endpoint}"));

        // 3. Run the protocol, streaming each new text chunk live into the panel.
        let manager = Arc::clone(&self);
        let app_out = app.clone();
        let run_id_out = run_id.clone();
        let mut on_output = move |chunk: &str| {
            manager.append_output(&run_id_out, chunk);
            let _ = AgentRunOutput {
                run_id: run_id_out.clone(),
                chunk: chunk.to_string(),
            }
            .emit(&app_out);
        };

        let outcome = a2a::run_remote_protocol(
            &transport,
            &endpoint,
            token,
            &instruction,
            agent.remote_streaming,
            &mut kill_rx,
            &mut on_output,
            a2a::POLL_INTERVAL,
            a2a::POLL_CAP,
        )
        .await;
        drop(on_output);

        let status = match outcome {
            RemoteOutcome::Finished => RunStatus::Finished { code: 0 },
            RemoteOutcome::Failed(error) => RunStatus::Failed { error },
            RemoteOutcome::Stopped => RunStatus::Stopped,
        };
        self.finalize(&app, &run_id, &agent, status, started).await;
    }

    /// Drive ONE ACP turn behind the SAME run seam as `drive_run` /
    /// `drive_remote_run`: same registry entry, same kill channel, same
    /// `finalize` + sinks, same two pre-existing events. A turn is a run
    /// (DESIGN §6) — when `session/prompt` returns, this run is terminal while
    /// the SESSION stays warm for the next instruction, and the two are tied
    /// together only by the additive `session_id`.
    ///
    /// The protocol turn itself lives in `run_acp_turn`, which is generic over
    /// `AcpSessionOps` and emits through a closure — the same shape
    /// `a2a::run_remote_protocol` uses — so it is unit-tested with no process,
    /// no network and no Tauri app. This method is the thin glue: session
    /// acquisition, the header line, dual emission, status mapping, and
    /// dropping a session whose child died.
    async fn drive_acp_run(
        self: Arc<Self>,
        app: AppHandle,
        run_id: String,
        agent: AgentDefinition,
        instruction: String,
        mut kill_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let started = std::time::Instant::now();

        let sessions = match app.try_state::<Arc<AcpSessionManager>>() {
            Some(state) => Arc::clone(state.inner()),
            None => {
                let err = "The ACP session manager is not initialized.".to_string();
                self.emit_line(&app, &run_id, &err);
                self.finalize(
                    &app,
                    &run_id,
                    &agent,
                    RunStatus::Failed { error: err },
                    started,
                )
                .await;
                return;
            }
        };

        // 1. The warm session — spawn + handshake on this agent's first run,
        //    a reused live child on every one after it.
        let cwd = resolve_cwd(&app, &agent.project_path);
        let session = match sessions.acquire(&agent, &cwd).await {
            Ok(s) => s,
            Err(e) => {
                self.emit_line(&app, &run_id, &e);
                log::error!("acp run {run_id}: {e}");
                if let Some(diag) = self.classify_run_failure(&e, &agent.binary_path, &agent) {
                    self.emit_diagnostic(&app, &run_id, &diag);
                }
                self.finalize(
                    &app,
                    &run_id,
                    &agent,
                    RunStatus::Failed { error: e },
                    started,
                )
                .await;
                return;
            }
        };

        // 2. Bind the run to its session + the header line (same convention as
        //    `drive_remote_run`'s `→ {name} @ {endpoint}`).
        self.set_session_id(&run_id, &session.session_id);
        self.emit_line(
            &app,
            &run_id,
            &format!("→ {} · ACP session {}", agent.name, session.session_id),
        );

        // 3. The channel Task 9's `respond_agent_permission` command uses to
        //    answer a parked prompt. Registered before the prompt is sent so no
        //    answer can arrive with nowhere to go.
        let (answer_tx, mut answer_rx) = mpsc::unbounded_channel::<PermissionAnswer>();
        self.set_permission_sender(&run_id, answer_tx);

        let manager = Arc::clone(&self);
        let app_events = app.clone();
        let run_id_events = run_id.clone();
        let mut on_event = move |event: RunEvent| {
            manager.emit_run_event(&app_events, &run_id_events, event);
        };

        let outcome = run_acp_turn(
            session.as_ref(),
            &instruction,
            agent.acp_permission_policy,
            &mut kill_rx,
            &mut answer_rx,
            &mut on_event,
            TurnTimeouts::production(),
        )
        .await;

        // 4. Terminal status + whether the session survives. The mapping itself
        //    is pure and test-asserted (`turn_result`); only the reporting side
        //    effects live here.
        match &outcome {
            TurnOutcome::Ended(_) => {}
            TurnOutcome::CancelTimedOut => self.emit_line(
                &app,
                &run_id,
                "The agent never acknowledged the cancel — ending its session.",
            ),
            TurnOutcome::Failed(error) => {
                self.emit_line(&app, &run_id, error);
                log::error!("acp run {run_id}: {error}");
            }
            TurnOutcome::Crashed(error) => {
                self.emit_line(&app, &run_id, error);
                log::error!("acp run {run_id}: {error}");
                // Same actionable-diagnostic path the raw driver uses on a
                // failure, fed the run's captured output.
                let captured = self.current_output(&run_id);
                if let Some(diag) = self.classify_run_failure(&captured, &agent.binary_path, &agent)
                {
                    self.emit_diagnostic(&app, &run_id, &diag);
                }
            }
        }
        let TurnResult {
            status,
            drop_session,
            stop_reason,
        } = turn_result(outcome);
        // ONE emission site covering every arm: a consumer of `agent-run-event`
        // alone (Task 9's structured renderer) must never see a turn that just
        // stops producing events without ever ending.
        on_event(RunEvent::TurnEnd {
            stop_reason: stop_reason.to_string(),
        });
        drop(on_event);

        // Identity-checked: we are outside `acquire`'s spawn lock, so removing
        // by agent id alone could close a healthy replacement a concurrent
        // `acquire` has already spawned — killing a live agent mid-turn.
        if drop_session {
            sessions.end_if_current(&session).await;
        }
        // Sinks (Panel/Notify/File) run unchanged — dual emission means the
        // buffer they read already contains every event's text line.
        self.finalize(&app, &run_id, &agent, status, started).await;
    }

    /// Resolve a remote agent's JSON-RPC endpoint: the cached `remote_endpoint`
    /// when present, otherwise one card fetch+resolve (with an actionable error
    /// pointing the user at the settings "Fetch card" button).
    async fn resolve_remote_endpoint<T: A2aTransport>(
        &self,
        transport: &T,
        agent: &AgentDefinition,
        token: Option<String>,
    ) -> Result<String, String> {
        if !agent.remote_endpoint.trim().is_empty() {
            return Ok(agent.remote_endpoint.clone());
        }
        if agent.remote_url.trim().is_empty() {
            return Err(
                "This remote agent has no URL. Open its settings, enter the \
                        agent URL, and Fetch the card first."
                    .to_string(),
            );
        }
        let url = a2a::well_known_card_url(&agent.remote_url);
        let card_json = transport.fetch_card(url, token).await.map_err(|e| {
            format!("Couldn't fetch the agent card ({e}). Fetch the agent card in settings first.")
        })?;
        let card = a2a::parse_agent_card(&card_json)?;
        card.select_jsonrpc_endpoint()
    }

    /// Append a plain line to the run buffer AND emit it as an `agent-run-output`
    /// event (the header line and remote-driver error lines flow through here —
    /// like CLI subprocess output, the remote stream is raw, un-i18n'd text).
    fn emit_line(&self, app: &AppHandle, run_id: &str, line: &str) {
        self.append_output(run_id, line);
        let _ = AgentRunOutput {
            run_id: run_id.to_string(),
            chunk: line.to_string(),
        }
        .emit(app);
    }

    /// Emit one ACP run event BOTH ways: the structured `agent-run-event` for
    /// the new panel rows, and its rendered text line through the existing
    /// buffer + `agent-run-output` path.
    ///
    /// The dual emission is the non-breaking guarantee, and it lives here so no
    /// call site can forget half of it: `AgentRunInfo.output`, the File sink,
    /// the Notify summary and every pre-existing panel subscriber keep working
    /// untouched because an ACP run's buffer reads exactly like a raw CLI run's.
    fn emit_run_event(&self, app: &AppHandle, run_id: &str, event: RunEvent) {
        if let Some(line) = render_line(&event) {
            self.append_output(run_id, &line);
            let _ = AgentRunOutput {
                run_id: run_id.to_string(),
                chunk: line,
            }
            .emit(app);
        }
        let _ = AgentRunEvent {
            run_id: run_id.to_string(),
            event,
        }
        .emit(app);
    }

    /// Set the terminal status, emit `agent-run-status`, and run the output sinks.
    async fn finalize(
        &self,
        app: &AppHandle,
        run_id: &str,
        agent: &AgentDefinition,
        status: RunStatus,
        started: std::time::Instant,
    ) {
        self.set_status(run_id, status.clone());

        // Notify sink: fire a desktop notification on completion.
        if agent.output_sinks.contains(&AgentOutputSink::Notify) {
            fire_notification(app, agent, &status);
        }

        // File sink: write the full instruction+output to a markdown run file.
        if agent.output_sinks.contains(&AgentOutputSink::File) {
            let (instruction, output, project) = {
                let runs = self.runs.lock().unwrap();
                match runs.get(run_id) {
                    Some(r) => (
                        r.instruction.clone(),
                        r.output.clone(),
                        r.project_path.clone(),
                    ),
                    None => (String::new(), String::new(), String::new()),
                }
            };
            let ts = Local::now();
            let dir = if project.trim().is_empty() {
                crate::portable::app_data_dir(app)
                    .map(|d| d.join("agent-runs"))
                    .unwrap_or_else(|_| PathBuf::from(".openflow/agent-runs"))
            } else {
                Path::new(&project).join(".openflow").join("agent-runs")
            };
            let path = run_file_path(&dir, &agent.id, ts);
            let contents = render_run_file(
                agent,
                &project,
                &instruction,
                &output,
                &status,
                started.elapsed(),
                ts,
            );
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, contents)) {
                Ok(()) => {
                    let path_str = path.to_string_lossy().to_string();
                    let mut runs = self.runs.lock().unwrap();
                    if let Some(r) = runs.get_mut(run_id) {
                        r.output_file = Some(path_str);
                    }
                }
                Err(e) => log::error!("Failed to write agent run file {}: {}", path.display(), e),
            }
        }

        let _ = AgentRunStatus {
            run_id: run_id.to_string(),
            status,
        }
        .emit(app);
    }
}

// ---------------------------------------------------------------------------
// C0 — the ACP turn (DESIGN-acp-agents.md §6-§8).
// ---------------------------------------------------------------------------

/// The two bounds one turn needs. Injected rather than read from the constants
/// below so both are testable in milliseconds — the `send_close_courtesy`
/// precedent, for the same reason: a timeout you can't exercise in a test is a
/// timeout nobody has checked still fires.
#[derive(Clone, Copy, Debug)]
struct TurnTimeouts {
    /// Cap on ONE write to the agent's stdin.
    ///
    /// `StdioTransport::send` does a blocking `write_all`, which this codebase
    /// already documents as never resolving if the agent is alive but has
    /// stopped draining its stdin (see `acp_session::send_close_courtesy`,
    /// which exists for exactly that). While such an await is pending, the turn
    /// loop is not polling `kill_rx` — so an unbounded write would defeat Stop,
    /// the cancel grace and the idle reaper all at once, and the run would sit
    /// at `Running` forever with no escape left.
    send: Duration,
    /// How long we wait for the agent to acknowledge a `session/cancel` with a
    /// terminal `stopReason` before giving up on it.
    ///
    /// Stop is the ONLY escape hatch from a parked permission prompt (there is
    /// no auto-deny timeout, DESIGN §8), and the idle reaper deliberately
    /// spares any session with a turn in flight — so an agent that ignores
    /// `session/cancel` would otherwise pin the turn guard, and its own child,
    /// for the life of the app. Generous, because unwinding a large in-flight
    /// edit legitimately takes a moment; bounded, because "I pressed Stop and
    /// nothing happened, ever" is not a state the user can escape.
    cancel_grace: Duration,
}

impl TurnTimeouts {
    const fn production() -> Self {
        Self {
            send: Duration::from_secs(5),
            cancel_grace: Duration::from_secs(30),
        }
    }
}

/// A write to the agent's stdin that timed out or errored ends the turn as a
/// CRASH, not a protocol failure — because it takes the SESSION down with it.
///
/// `tokio::time::timeout` drops the in-flight `write_all`, so whatever bytes the
/// pipe already accepted stay in it: the next frame written on that stream would
/// land directly after a partial one, desyncing the protocol for the life of the
/// session. `acquire` cannot detect this — `is_alive` is a `try_wait`, and a
/// wedged-but-running child passes it — so the only thing standing between a
/// half-written frame and every later run on that agent failing is this
/// returning a variant whose `turn_result` sets `drop_session`. A transport
/// error means the pipe is broken outright, which is no more reusable.
fn broken_stream(e: String) -> TurnOutcome {
    TurnOutcome::Crashed(format!(
        "{e} Its session has been dropped rather than reused — a write abandoned mid-frame \
         would desync every later turn on it. Nothing was retried, so the agent may have \
         applied some of its changes already; check the project before running it again."
    ))
}

/// Bound one write to the agent's stdin. See `TurnTimeouts::send`.
async fn bounded_send<T>(
    what: &str,
    limit: Duration,
    fut: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!(
            "The agent stopped reading its input ({what} got no further after {}s).",
            limit.as_secs()
        )),
    }
}

/// Whether an agent takes the ACP session driver. This is the actual guard
/// `start`'s match arm evaluates — not a copy of it — so routing is asserted by
/// test rather than by reading the match.
pub fn uses_acp_driver(agent: &AgentDefinition) -> bool {
    agent.kind == AgentKind::Cli && agent.cli_protocol == CliProtocol::Acp
}

/// A turn's terminal `stopReason` → the run's terminal status. `RunStatus`
/// gains NO variant (DESIGN §6): a new one would break every frontend `switch`
/// over the serde-tagged union.
pub fn stop_reason_to_status(r: StopReason) -> RunStatus {
    match r {
        StopReason::Completed => RunStatus::Finished { code: 0 },
        StopReason::Cancelled => RunStatus::Stopped,
        StopReason::MaxStepsReached => RunStatus::Failed {
            error: "The agent hit its step limit before finishing.".into(),
        },
        StopReason::RequestTimeout => RunStatus::Failed {
            error: "The agent timed out while generating a response.".into(),
        },
        StopReason::Other => RunStatus::Failed {
            error: "The agent stopped for a reason this version doesn't recognise.".into(),
        },
    }
}

/// `RunEvent::TurnEnd`'s reason when the turn ended without an ACP
/// `stopReason` at all — the child died, or the protocol round trip failed.
/// A new VALUE in an existing `String` field, not a new variant: `RunEvent` is
/// the C2 public contract and stays additive-only.
const TURN_END_FAILED: &str = "failed";

/// The wire spelling of a stop reason, for `RunEvent::TurnEnd`.
pub fn stop_reason_label(r: StopReason) -> &'static str {
    match r {
        StopReason::Completed => "completed",
        StopReason::MaxStepsReached => "max_steps_reached",
        StopReason::Cancelled => "cancelled",
        StopReason::RequestTimeout => "request_timeout",
        StopReason::Other => "other",
    }
}

/// The user's answer to a parked permission prompt. `*_always` answers are
/// recorded as a session-scoped override, not as a persistent grant to the
/// agent — see `apply_answer`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChoice {
    AllowOnce,
    AllowAlways,
    DenyOnce,
    DenyAlways,
}

impl PermissionChoice {
    fn allows(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
    fn is_persistent(self) -> bool {
        matches!(self, Self::AllowAlways | Self::DenyAlways)
    }
}

/// One answer travelling from `respond_permission` to a run's turn loop.
#[derive(Clone, Debug)]
pub struct PermissionAnswer {
    pub request_id: String,
    pub choice: PermissionChoice,
    /// The EXACT agent-supplied option the user clicked, when known. See
    /// `respond_permission`'s doc comment for why this — not `choice` alone —
    /// must be preferred when selecting which option to reply with.
    pub option_id: Option<String>,
}

/// A permission request the user still has to answer. Holding the agent's
/// JSON-RPC id here is what lets Stop resolve it as `cancelled` instead of
/// leaking a responder — an unanswered request blocks the agent's turn forever.
struct ParkedPermission {
    id: Value,
    tool_kind: String,
    options: Vec<PermissionOptionWire>,
}

/// How a turn ended.
#[derive(Debug)]
enum TurnOutcome {
    /// The agent answered `session/prompt` with a terminal `stopReason`.
    Ended(StopReason),
    /// The child exited mid-turn. NEVER retried: it may have half-applied its
    /// edits. The session is dropped and the next run spawns fresh.
    Crashed(String),
    /// The protocol round trip itself failed (couldn't send, or the result was
    /// unreadable). The session survives; `acquire`'s liveness check handles a
    /// child that turns out to be dead.
    Failed(String),
    /// Stop was pressed and the agent never acknowledged the cancelled turn.
    CancelTimedOut,
}

/// How a finished turn maps to the run's terminal state.
struct TurnResult {
    status: RunStatus,
    /// Whether the warm session must be ENDED rather than left for the next run.
    drop_session: bool,
    /// `RunEvent::TurnEnd`'s reason.
    stop_reason: &'static str,
}

/// Pure so `drop_session` is asserted by test rather than read off a match arm.
///
/// That flag is the load-bearing one: `acquire` reuses a session on
/// `should_reuse && is_alive()`, and `is_alive` is only a `try_wait` — it cannot
/// tell a healthy agent from one whose stdin we abandoned mid-frame. Anything
/// that leaves the stream or the child unusable must drop the session here, or
/// every later run on that agent inherits the damage until the idle reaper
/// collects it (up to 600s by default).
fn turn_result(outcome: TurnOutcome) -> TurnResult {
    match outcome {
        // The agent answered normally: the session is healthy and stays warm.
        TurnOutcome::Ended(reason) => TurnResult {
            status: stop_reason_to_status(reason),
            drop_session: false,
            stop_reason: stop_reason_label(reason),
        },
        // Stopped, but the agent never acknowledged it — alive and wedged.
        TurnOutcome::CancelTimedOut => TurnResult {
            status: RunStatus::Stopped,
            drop_session: true,
            stop_reason: stop_reason_label(StopReason::Cancelled),
        },
        // The child died, or a write was abandoned mid-frame (`broken_stream`).
        TurnOutcome::Crashed(error) => TurnResult {
            status: RunStatus::Failed { error },
            drop_session: true,
            stop_reason: TURN_END_FAILED,
        },
        // A protocol-level failure with the stream still intact: the agent
        // rejected the request, or its result didn't parse. Nothing was left
        // half-written, so the session is still reusable.
        TurnOutcome::Failed(error) => TurnResult {
            status: RunStatus::Failed { error },
            drop_session: false,
            stop_reason: TURN_END_FAILED,
        },
    }
}

/// The slice of a warm ACP session that one turn drives.
///
/// `LiveSession` is the production implementor. It exists as a trait because
/// `LiveSession` is concretely an `AcpClient<StdioTransport>` over a real
/// child process — the ONLY seam through which the turn loop (and, critically,
/// the turn-guard contract below) can be exercised without spawning one.
trait AcpSessionOps: Sync {
    /// RAII proof that this turn owns the session.
    type Turn<'a>: Send
    where
        Self: 'a;

    /// Take the one-turn-at-a-time guard. Its lifetime IS the contract — see
    /// `run_acp_turn`.
    fn begin_turn(&self) -> impl Future<Output = Self::Turn<'_>> + Send;
    /// `session/prompt`; the returned id correlates the terminal response.
    fn prompt(&self, text: &str) -> impl Future<Output = Result<u64, String>> + Send;
    /// The next actionable frame; `None` once the child's stdout closes.
    fn next_item(&self) -> impl Future<Output = Option<PumpItem>> + Send;
    /// `session/cancel` — ends the turn, never the session.
    fn cancel_turn(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Answer an inbound permission request.
    fn answer(
        &self,
        id: &Value,
        outcome: PermissionOutcome,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Refuse an inbound request we do not implement. Doctrine (`acp/client.rs`):
    /// an unanswered request hangs the agent's turn forever, so we ALWAYS reply.
    fn refuse(&self, id: &Value) -> impl Future<Output = Result<(), String>> + Send;
    /// The session-scoped permission override, without consuming it.
    fn permission_override(&self) -> Option<SessionOverride>;
    fn remember_override(&self, ov: SessionOverride);
}

impl AcpSessionOps for LiveSession {
    type Turn<'a> = tokio::sync::MutexGuard<'a, ()>;

    fn begin_turn(&self) -> impl Future<Output = Self::Turn<'_>> + Send {
        self.turn_guard()
    }
    fn prompt(&self, text: &str) -> impl Future<Output = Result<u64, String>> + Send {
        self.send_prompt(text)
    }
    fn next_item(&self) -> impl Future<Output = Option<PumpItem>> + Send {
        // `client_pump` (not the raw client) on purpose: it `touch`es the
        // session on every item, the second half of the guard against the idle
        // reaper killing a long-running turn.
        self.client_pump()
    }
    fn cancel_turn(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.cancel()
    }
    fn answer(
        &self,
        id: &Value,
        outcome: PermissionOutcome,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let body = permission_response_body(&outcome);
        async move { self.reply(id, body).await }
    }
    fn refuse(&self, id: &Value) -> impl Future<Output = Result<(), String>> + Send {
        self.reply_error(id, -32601, "Method not found")
    }
    fn permission_override(&self) -> Option<SessionOverride> {
        // `LiveSession` exposes only `take_override`, so read it and put it
        // straight back. Safe: the turn guard serializes turns on a session, so
        // this is the only task touching the override.
        let ov = self.take_override();
        if let Some(ov) = &ov {
            self.set_override(ov.clone());
        }
        ov
    }
    fn remember_override(&self, ov: SessionOverride) {
        self.set_override(ov);
    }
}

/// Drive ONE ACP turn: `session/prompt` → stream `session/update`s → answer
/// every inbound request → terminal `stopReason`.
///
/// # Contract 1 — the turn guard spans the WHOLE round trip
///
/// `begin_turn()`'s guard is taken before the prompt is sent and dropped only
/// when this function returns, i.e. strictly after the terminal `stopReason`
/// (or a crash/cancel outcome). `acp_session::is_reapable` spares any session
/// whose turn lock is held; releasing it early would let the idle reaper
/// SIGTERM a live agent mid-edit — the 600s default timeout against an
/// 11-minute refactor — leaving half-applied changes on the user's disk. This
/// is NOT compile-enforced; it is pinned by
/// `the_turn_guard_is_held_for_the_whole_prompt_round_trip`.
///
/// Generic over the session and emitting through a closure (the shape
/// `a2a::run_remote_protocol` established) so all of the above is testable
/// against a scripted double: no process, no network, no Tauri app.
#[allow(clippy::too_many_arguments)]
async fn run_acp_turn<S: AcpSessionOps>(
    session: &S,
    instruction: &str,
    policy: AcpPermissionPolicy,
    kill_rx: &mut mpsc::UnboundedReceiver<()>,
    answers: &mut mpsc::UnboundedReceiver<PermissionAnswer>,
    on_event: &mut (impl FnMut(RunEvent) + Send),
    timeouts: TurnTimeouts,
) -> TurnOutcome {
    // CONTRACT 1: held until this function returns. Do not drop it early.
    let _turn = session.begin_turn().await;

    // Bounded like every other write below: an unbounded `prompt` that never
    // returns would never even reach the select loop, so Stop could not be read
    // and the guard would be pinned for the life of the app.
    let prompt_id = match bounded_send(
        "sending the instruction",
        timeouts.send,
        session.prompt(instruction),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return broken_stream(e),
    };

    let mut parked: HashMap<String, ParkedPermission> = HashMap::new();
    let mut prompts_seen = 0u64;
    let mut watch_kill = true;
    let mut watch_answers = true;
    let mut cancel_deadline: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            item = session.next_item() => {
                let Some(item) = item else {
                    return TurnOutcome::Crashed(
                        "The agent exited before finishing this turn. Nothing was retried — it may \
                         have applied some of its changes already, so check the project before \
                         running it again."
                            .to_string(),
                    );
                };
                match item {
                    // The turn's own answer: the only frame that ends it.
                    PumpItem::Response { id, result } if id == prompt_id => {
                        return match result {
                            Ok(v) => match serde_json::from_value::<PromptResult>(v) {
                                Ok(r) => TurnOutcome::Ended(r.stop_reason.unwrap_or(StopReason::Other)),
                                Err(e) => TurnOutcome::Failed(format!(
                                    "The agent's session/prompt result was malformed: {e}"
                                )),
                            },
                            Err(e) => TurnOutcome::Failed(format!(
                                "The agent rejected the instruction: {} ({})",
                                e.message, e.code
                            )),
                        };
                    }
                    // A reply to some other request of ours — not this turn's.
                    PumpItem::Response { .. } => {}
                    PumpItem::Event(ClientEvent::Update(n)) => {
                        if let Some(event) = map_session_update(&n.update) {
                            on_event(event);
                        }
                    }
                    PumpItem::Event(ClientEvent::Inbound(InboundRequest::RequestPermission {
                        id,
                        params,
                    })) => {
                        prompts_seen += 1;
                        // OUR request id, not the agent's: the JSON-RPC id may be
                        // a number or a string, and this one is a stable opaque
                        // handle the frontend echoes back.
                        let request_id = format!("perm-{prompts_seen}");
                        let decision = decide(&PolicyInput {
                            policy,
                            tool_kind: params.tool_call.kind.clone(),
                            session_override: session.permission_override(),
                            options: params.options.clone(),
                        });
                        match decision {
                            // An automatic decision is still reported, so the user
                            // can always see afterwards what was allowed on their
                            // behalf (DESIGN §8).
                            PermissionDecision::Allow { option_id, automatic } => {
                                // A failed write is never reported as a resolution:
                                // the agent never received the answer, so claiming
                                // "→ allow (automatic)" while the turn stalls would
                                // be a lie about what happened.
                                if let Err(e) = bounded_send("answering a permission request", timeouts.send,
                                    session.answer(&id, PermissionOutcome::Selected { option_id })).await {
                                    return broken_stream(e);
                                }
                                on_event(RunEvent::PermissionResolved {
                                    request_id,
                                    outcome: "allow".to_string(),
                                    automatic,
                                });
                            }
                            PermissionDecision::Deny { option_id, automatic } => {
                                if let Err(e) = bounded_send("answering a permission request", timeouts.send,
                                    session.answer(&id, PermissionOutcome::Selected { option_id })).await {
                                    return broken_stream(e);
                                }
                                on_event(RunEvent::PermissionResolved {
                                    request_id,
                                    outcome: "deny".to_string(),
                                    automatic,
                                });
                            }
                            // Park it and KEEP PUMPING — updates must keep
                            // streaming while the user decides.
                            PermissionDecision::Ask => {
                                on_event(permission_request_event(&request_id, &params.tool_call, &params.options));
                                parked.insert(
                                    request_id,
                                    ParkedPermission {
                                        id,
                                        tool_kind: params.tool_call.kind,
                                        options: params.options,
                                    },
                                );
                            }
                        }
                    }
                    PumpItem::Event(ClientEvent::Inbound(InboundRequest::Unsupported {
                        id,
                        method,
                    })) => {
                        // We declared no fs/terminal capabilities, so this should
                        // not happen — but never leave it unanswered.
                        log::warn!("acp: refusing unsupported agent request '{method}'");
                        if let Err(e) = bounded_send("refusing an agent request", timeouts.send,
                            session.refuse(&id)).await {
                            return broken_stream(e);
                        }
                    }
                    PumpItem::Event(ClientEvent::Closed) => {
                        return TurnOutcome::Crashed(
                            "The agent closed the connection before finishing this turn."
                                .to_string(),
                        );
                    }
                }
            }

            answer = answers.recv(), if watch_answers => {
                match answer {
                    // The registry dropped our sender — nothing more can arrive.
                    None => watch_answers = false,
                    Some(answer) => {
                        if let Some(p) = parked.remove(&answer.request_id) {
                            if let Err(e) =
                                apply_answer(session, &answer, &p, on_event, timeouts.send).await
                            {
                                return broken_stream(e);
                            }
                        }
                    }
                }
            }

            signal = kill_rx.recv(), if watch_kill => {
                watch_kill = false;
                if signal.is_some() {
                    // Arm the grace BEFORE the writes below, not after: their own
                    // bound is what stops a wedged stdin from parking us here,
                    // and the clock the user is waiting on started when they
                    // pressed Stop, not when the agent got round to reading it.
                    cancel_deadline = Some(tokio::time::Instant::now() + timeouts.cancel_grace);
                    // Cancel ends the TURN, not the session — it stays warm for
                    // the next instruction.
                    let mut write_failed = None;
                    if let Err(e) = bounded_send("cancelling the turn", timeouts.send,
                        session.cancel_turn()).await {
                        write_failed = Some(e);
                    }
                    // Resolve every parked prompt so no responder is leaked: an
                    // unanswered permission request blocks the agent forever.
                    for (request_id, p) in parked.drain() {
                        if let Err(e) = bounded_send("cancelling a permission request", timeouts.send,
                            session.answer(&p.id, PermissionOutcome::Cancelled)).await {
                            write_failed.get_or_insert(e);
                        }
                        on_event(RunEvent::PermissionResolved {
                            request_id,
                            outcome: "cancelled".to_string(),
                            automatic: true,
                        });
                    }
                    // A write abandoned mid-frame here leaves the stream unusable
                    // (see `broken_stream`), so there is nothing left to wait
                    // for. Return DIRECTLY — do not collapse `cancel_deadline`
                    // and re-enter the select hoping the timer arm wins. A
                    // `sleep_until` whose deadline is already past still returns
                    // `Pending` on its first poll (tokio only reports readiness
                    // once the timer driver has been parked), while
                    // `next_item()` is immediately `Ready` if the agent already
                    // queued a frame — which is exactly the likely case here,
                    // since an agent that stopped draining its stdin usually
                    // keeps writing stdout. The pump arm would then take
                    // `Ended`/`Failed`, both of which KEEP the session, carrying
                    // the half-written `session/cancel` frame this branch exists
                    // to get rid of.
                    //
                    // `CancelTimedOut` (rather than `Crashed`) because the user
                    // deliberately pressed Stop: it is the Stop-shaped outcome
                    // (`RunStatus::Stopped`) that ALSO sets `drop_session`.
                    // Everything this branch owes has already happened above —
                    // every parked prompt answered and its `PermissionResolved`
                    // emitted — so returning here loses nothing.
                    if let Some(e) = write_failed {
                        log::warn!("acp: {e} — ending the session rather than reusing it");
                        return TurnOutcome::CancelTimedOut;
                    }
                }
            }

            _ = tokio::time::sleep_until(
                cancel_deadline.unwrap_or_else(|| tokio::time::Instant::now() + timeouts.cancel_grace)
            ), if cancel_deadline.is_some() => {
                return TurnOutcome::CancelTimedOut;
            }
        }
    }
}

/// The `session/request_permission` response body. ACP nests the tagged
/// outcome under an `outcome` field, so the wire shape is
/// `{"outcome":{"outcome":"selected","optionId":"…"}}` — the inner object is
/// `PermissionOutcome`'s own serialization. Serializing it cannot fail (plain
/// strings only); `Null` would be a malformed-but-present answer, which still
/// beats leaving the agent's request unanswered.
fn permission_response_body(outcome: &PermissionOutcome) -> Value {
    serde_json::json!({ "outcome": serde_json::to_value(outcome).unwrap_or(Value::Null) })
}

/// Build the `PermissionRequest` event for a prompt we're about to park.
fn permission_request_event(
    request_id: &str,
    tool_call: &ToolCallWire,
    options: &[PermissionOptionWire],
) -> RunEvent {
    RunEvent::PermissionRequest {
        request_id: request_id.to_string(),
        tool_call_id: (!tool_call.tool_call_id.is_empty()).then(|| tool_call.tool_call_id.clone()),
        title: tool_call.title.clone(),
        options: options
            .iter()
            .map(|o| PermissionOption {
                option_id: o.option_id.clone(),
                name: o.name.clone(),
                kind: o.kind.clone(),
            })
            .collect(),
    }
}

/// Apply a user's answer to a parked prompt: record any session-scoped
/// persistence, then reply to the agent with an option IT offered.
///
/// An `*_always` answer is recorded as OUR `SessionOverride` and still replies
/// with the one-shot option (`pick_option` prefers `*_once`). Keeping the
/// persistence on our side means "End session" revokes it; selecting the
/// agent's own `allow_always` would hand it a grant we can neither see nor take
/// back.
async fn apply_answer<S: AcpSessionOps>(
    session: &S,
    answer: &PermissionAnswer,
    parked: &ParkedPermission,
    on_event: &mut (impl FnMut(RunEvent) + Send),
    send_timeout: Duration,
) -> Result<(), String> {
    if answer.choice.is_persistent() {
        let mut ov = session.permission_override().unwrap_or_default();
        // PER KIND, in both directions. The user answered a question about THIS
        // tool kind; an "always allow" on a benign `read` must not silently
        // approve an `execute` or `delete` later in the same session. It never
        // sets `allow_all` — that is the separate, explicit allow-everything
        // answer.
        let list = if answer.choice.allows() {
            &mut ov.allowed_kinds
        } else {
            &mut ov.denied_kinds
        };
        if !list.contains(&parked.tool_kind) {
            list.push(parked.tool_kind.clone());
        }
        session.remember_override(ov);
    }

    // Which option id to actually reply with (Task 11 review, Important 5).
    //
    // An `*_always` click is UNCHANGED from before: it never selects the
    // agent's own persistent option (see this fn's doc comment above), so it
    // always goes through `pick_option`'s kind-based one-shot selection
    // regardless of `answer.option_id`.
    //
    // An `*_once` click prefers the EXACT option the user clicked. `choice`
    // alone is only a kind category (allow/deny x once/always) — when an
    // agent offers two options of the SAME once kind (e.g. "Allow once" and
    // "Allow for this directory", both `allow_once`), `pick_option` always
    // resolves to whichever comes first, silently answering with a DIFFERENT
    // option than the one the user actually clicked. Falls back to
    // `pick_option` if the id is missing or no longer among this request's
    // current options (e.g. a stale answer racing a resolved request).
    let selected_option_id = if answer.choice.is_persistent() {
        pick_option(&parked.options, answer.choice.allows())
    } else {
        answer
            .option_id
            .as_ref()
            .filter(|id| parked.options.iter().any(|o| &o.option_id == *id))
            .cloned()
            .or_else(|| pick_option(&parked.options, answer.choice.allows()))
    };

    // A failed write is propagated, never reported as a resolution: the agent
    // never received the answer, so emitting one would tell the user their click
    // landed while the turn quietly stalls.
    match selected_option_id {
        Some(option_id) => {
            bounded_send(
                "answering a permission request",
                send_timeout,
                session.answer(&parked.id, PermissionOutcome::Selected { option_id }),
            )
            .await?;
            on_event(RunEvent::PermissionResolved {
                request_id: answer.request_id.clone(),
                outcome: if answer.choice.allows() {
                    "allow"
                } else {
                    "deny"
                }
                .to_string(),
                automatic: false,
            });
        }
        // The agent offered nothing matching the answer. Cancelling still
        // ANSWERS the request — leaving it open would hang the turn.
        None => {
            bounded_send(
                "cancelling a permission request",
                send_timeout,
                session.answer(&parked.id, PermissionOutcome::Cancelled),
            )
            .await?;
            on_event(RunEvent::PermissionResolved {
                request_id: answer.request_id.clone(),
                outcome: "cancelled".to_string(),
                automatic: false,
            });
        }
    }
    Ok(())
}

/// Read a stream line-by-line, appending each line to the run's buffer and
/// emitting an `agent-run-output` event. Raw text streams regardless of format.
async fn stream_lines<R: AsyncRead + Unpin>(
    reader: R,
    manager: Arc<AgentRunManager>,
    app: AppHandle,
    run_id: String,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        manager.append_output(&run_id, &line);
        let _ = AgentRunOutput {
            run_id: run_id.clone(),
            chunk: line,
        }
        .emit(&app);
    }
}

/// SIGTERM, then a SIGKILL backstop after a grace period (mirrors Agent OS's
/// `killProc`). On non-unix, `start_kill` (the platform terminate) is used.
/// `pub(crate)` so `acp_session::LiveSession` reuses the exact same stop
/// ladder rather than re-implementing it for the long-lived ACP child.
pub(crate) async fn terminate_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        tokio::select! {
            _ = child.wait() => return,
            _ = tokio::time::sleep(Duration::from_millis(2500)) => {}
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Fire a desktop notification for a completed run (Notify sink).
fn fire_notification(app: &AppHandle, agent: &AgentDefinition, status: &RunStatus) {
    use tauri_plugin_notification::NotificationExt;
    let project = if agent.project_path.trim().is_empty() {
        String::new()
    } else {
        Path::new(&agent.project_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| agent.project_path.clone())
    };
    let verb = match status {
        RunStatus::Finished { code: 0 } => "finished".to_string(),
        RunStatus::Finished { code } => format!("exited ({code})"),
        RunStatus::Failed { .. } => "failed".to_string(),
        RunStatus::Stopped => "stopped".to_string(),
        RunStatus::Running => "running".to_string(),
    };
    let body = if project.is_empty() {
        verb.clone()
    } else {
        format!("{verb} · {project}")
    };
    let _ = app
        .notification()
        .builder()
        .title(format!("OpenFlow · {} {}", agent.name, verb))
        .body(body)
        .show();
}

/// Resolve the working directory for a run. Falls back to `$HOME`, then the app
/// data dir, when no project folder is configured.
fn resolve_cwd(app: &AppHandle, project_path: &str) -> PathBuf {
    if !project_path.trim().is_empty() {
        return PathBuf::from(project_path);
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    crate::portable::app_data_dir(app).unwrap_or_else(|_| PathBuf::from("."))
}

/// Build the argv (after the binary) from a command template, substituting
/// `{cwd}` everywhere and `{prompt}` per the delivery mode. With `Stdin`
/// delivery a bare `{prompt}` token is dropped (the instruction goes to stdin);
/// with `Arg` delivery it is substituted with the instruction. Pure + testable.
pub fn build_argv(
    command_template: &str,
    cwd: &str,
    prompt: &str,
    prompt_via: PromptDelivery,
) -> Vec<String> {
    let mut out = Vec::new();
    for tok in tokenize_template(command_template) {
        if prompt_via == PromptDelivery::Stdin && tok == "{prompt}" {
            continue;
        }
        let tok = tok.replace("{cwd}", cwd);
        let tok = match prompt_via {
            PromptDelivery::Arg => tok.replace("{prompt}", prompt),
            PromptDelivery::Stdin => tok.replace("{prompt}", ""),
        };
        out.push(tok);
    }
    out
}

/// Minimal shell-ish tokenizer: splits on whitespace, honoring single/double
/// quotes so a quoted placeholder value stays one argument. No shell expansion,
/// no escapes beyond the quotes — the instruction never becomes a shell string.
/// `pub` so `acp_session::build_acp_argv` can reuse it verbatim: quoting must
/// behave identically whether a template runs in raw or ACP mode.
pub fn tokenize_template(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;
    for c in s.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                has_token = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_token = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_token {
                    tokens.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(cur);
    }
    tokens
}

/// The static (installation-independent) unix tool directories a GUI-launched
/// app's stripped launchd PATH is missing. On Apple Silicon Homebrew lives under
/// `/opt/homebrew`; on Intel under `/usr/local`. Both are listed so one binary
/// works on either arch.
const STATIC_BASELINE_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// De-duplicate a list of paths, preserving first-seen order.
fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for p in items {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// Split a raw PATH-style value into directory entries, mirroring
/// `std::env::split_paths` semantics per platform: Windows splits on `;` with
/// double-quoted segments protected (quotes stripped); unix splits on `:`.
/// A hardcoded `':'` split (the pre-fix behavior) would mangle Windows entries
/// at the drive-letter colon (`C:\bin` → `C` + `\bin`). Empty entries are
/// dropped. Parameterized on `windows` (not cfg-gated) so the Windows behavior
/// is unit-testable from any host; production callers pass `cfg!(windows)`.
/// A host-parity test pins this against `std::env::split_paths`.
pub fn split_path_list(raw: &str, windows: bool) -> Vec<String> {
    if windows {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        for c in raw.chars() {
            match c {
                '"' => in_quotes = !in_quotes,
                ';' if !in_quotes => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    } else {
        raw.split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// Join directory entries into a PATH-style value (`;` on Windows, `:` on
/// unix). Inverse of `split_path_list`; parameterized for testability.
pub fn join_path_list(dirs: &[String], windows: bool) -> String {
    dirs.join(if windows { ";" } else { ":" })
}

/// Enumerate `$HOME/.nvm/versions/node/*/bin` directories, **newest version
/// first** (semver-descending). nvm installs each Node version under its own
/// tree, and global npm binaries (like `claude`) land in that version's `bin`;
/// a GUI app never inherits the shell's active-version PATH, so we add them all.
pub fn nvm_node_bin_dirs(home: &str) -> Vec<String> {
    let versions_dir = Path::new(home).join(".nvm/versions/node");
    let mut entries: Vec<(Vec<u64>, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&versions_dir) {
        for ent in rd.flatten() {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            let key = parse_semver_key(&name);
            let bin = path.join("bin");
            entries.push((key, bin.to_string_lossy().to_string()));
        }
    }
    // Newest first.
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries.into_iter().map(|(_, dir)| dir).collect()
}

/// Parse a `vMAJOR.MINOR.PATCH` (or `MAJOR.MINOR.PATCH`) directory name into a
/// numeric key for descending sort. Unparseable components sort as 0.
fn parse_semver_key(name: &str) -> Vec<u64> {
    name.trim_start_matches('v')
        .split('.')
        .map(|c| c.parse::<u64>().unwrap_or(0))
        .collect()
}

/// The ordered baseline tool directories to search **in addition to** the
/// process PATH. Unix: home-relative install dirs (native installers, bun,
/// cargo, volta, deno, nvm) then the static system/Homebrew dirs. Windows: the
/// npm global-shim dir (`%APPDATA%\npm`, where `claude.cmd`/`codex.cmd` live),
/// per-user installers (`%LOCALAPPDATA%\Programs`), bun/volta/cargo/scoop under
/// the user profile, and `%ProgramFiles%\nodejs` — all resolved from injected
/// env vars, never hardcoded drives. Pure + testable: `env` is an injected
/// lookup and the filesystem-derived `nvm` dirs come from the caller.
///
/// `.kimi-code/bin` is Kimi Code CLI's own install location — LIVE-VERIFIED
/// against its official installer (`curl -fsSL
/// https://code.kimi.com/kimi-code/install.sh | bash`), which drops a
/// self-contained native binary at `$HOME/.kimi-code/bin/kimi` and only adds it
/// to PATH via the user's shell rc file (`.bash_profile`/`.zshrc`), which a
/// GUI-launched app never sources.
///
/// `.openclaw/bin` is one of OpenClaw's own install locations — LIVE-VERIFIED:
/// its `install-cli.sh` variant writes the wrapper to `<prefix>/bin/openclaw`
/// with a default prefix of `~/.openclaw`. (A plain `npm install -g openclaw`,
/// used for this PR's live verification, instead lands in npm's global bin —
/// already covered by `.local/bin`/Homebrew/`STATIC_BASELINE_DIRS` below — but
/// the dedicated-prefix installer path is not, so it's added defensively.)
/// Hermes needs no new entry: its installer (LIVE-VERIFIED,
/// `curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash`)
/// symlinks to `~/.local/bin/hermes`, already in this list.
pub fn baseline_bin_dirs(
    windows: bool,
    env: &impl Fn(&str) -> Option<String>,
    nvm_node_bin_dirs: &[String],
) -> Vec<String> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let mut dirs: Vec<String> = Vec::new();
    if windows {
        if let Some(appdata) = get("APPDATA") {
            // npm's global prefix — `.cmd` shims for claude/codex land here.
            dirs.push(format!("{appdata}\\npm"));
        }
        if let Some(local) = get("LOCALAPPDATA") {
            // Per-user app installers.
            dirs.push(format!("{local}\\Programs"));
        }
        if let Some(profile) = get("USERPROFILE") {
            for sub in [
                ".bun\\bin",    // Bun global bins
                ".volta\\bin",  // Volta-managed node tools
                ".cargo\\bin",  // Rust/cargo
                "scoop\\shims", // Scoop package manager
            ] {
                dirs.push(format!("{profile}\\{sub}"));
            }
        }
        if let Some(pf) = get("ProgramFiles") {
            dirs.push(format!("{pf}\\nodejs"));
        }
    } else {
        if let Some(home) = get("HOME") {
            // Common per-user install locations that a stripped launchd PATH omits.
            for sub in [
                ".local/bin",       // native installers (incl. Claude Code native, Hermes)
                ".claude/local",    // Claude Code local install
                ".kimi-code/bin",   // Kimi Code CLI native install
                ".openclaw/bin",    // OpenClaw install-cli.sh default prefix
                ".bun/bin",         // Bun global bins
                ".cargo/bin",       // Rust/cargo
                ".volta/bin",       // Volta-managed node tools
                ".deno/bin",        // Deno
                ".nvm/current/bin", // nvm "current" symlink, when present
                ".npm-global/bin",  // custom npm prefix
            ] {
                dirs.push(format!("{home}/{sub}"));
            }
            // Every installed nvm node version, newest first.
            dirs.extend(nvm_node_bin_dirs.iter().cloned());
        }
        dirs.extend(STATIC_BASELINE_DIRS.iter().map(|s| s.to_string()));
    }
    dedupe(dirs)
}

/// The full ordered directory list `detect_agent_binary` scans: the process
/// PATH first (an explicitly-configured tool wins) then the baseline dirs. Pure
/// + testable; `windows`, the env lookup, and the nvm dirs are injected so no
/// platform or filesystem access is needed here.
pub fn detect_search_dirs(
    process_path: Option<&str>,
    windows: bool,
    env: &impl Fn(&str) -> Option<String>,
    nvm_node_bin_dirs: &[String],
) -> Vec<String> {
    let mut dirs = split_path_list(process_path.unwrap_or(""), windows);
    dirs.extend(baseline_bin_dirs(windows, env, nvm_node_bin_dirs));
    dedupe(dirs)
}

/// Candidate file names to probe for `name` in each directory. Unix: the bare
/// name (executables have no extension). Windows: one candidate per PATHEXT
/// extension — parsed from the given `PATHEXT` value when set, else the
/// `.exe`/`.cmd`/`.bat` default trio — because npm installs CLIs as `.cmd`
/// shims and a bare extensionless file isn't executable there. Pure +
/// parameterized so the Windows behavior is unit-testable from any host.
pub fn candidate_file_names(name: &str, windows: bool, pathext: Option<&str>) -> Vec<String> {
    if !windows {
        return vec![name.to_string()];
    }
    let exts: Vec<String> = pathext
        .map(|raw| {
            raw.split(';')
                .map(str::trim)
                .filter(|e| e.len() > 1 && e.starts_with('.'))
                .map(|e| e.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![".exe".into(), ".cmd".into(), ".bat".into()]);
    dedupe(exts.into_iter().map(|e| format!("{name}{e}")).collect())
}

/// Env lookup used by the impure wrappers.
fn std_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The nvm node bin dirs for the current user (unix only — nvm-windows lays
/// out versions differently and registers itself on PATH system-wide).
fn current_nvm_dirs() -> Vec<String> {
    if cfg!(windows) {
        Vec::new()
    } else {
        std::env::var("HOME")
            .ok()
            .map(|h| nvm_node_bin_dirs(&h))
            .unwrap_or_default()
    }
}

/// Baseline PATH: the caller's PATH plus the baseline tool dirs (Homebrew,
/// `~/.local/bin`, node version managers, cargo — or their Windows
/// equivalents), so a GUI-spawned process (which inherits a stripped launchd
/// PATH on macOS) can still resolve the CLI and any tools it shells out to.
/// Mirrors Agent OS's `agentEnv`. Shared by detect, the Test button, and the
/// run pipeline so all three see the same PATH.
pub fn baseline_path(existing: Option<&str>) -> String {
    let dirs = detect_search_dirs(existing, cfg!(windows), &std_env, &current_nvm_dirs());
    join_path_list(&dirs, cfg!(windows))
}

/// Whether `path` is an existing regular file with an execute bit (unix) / an
/// existing file (other platforms).
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ---------------------------------------------------------------------------
// Codex CLI-agent diagnostics (GAP 1 run-path classification + GAP 2 proactive
// vendor check). `@openai/codex` ships a thin Node launcher whose real native
// binary is a per-platform npm optional dependency (`@openai/codex-<os>-<arch>`)
// — a partial install (optional deps skipped offline / with `--omit=optional`)
// leaves the launcher unable to resolve it, so `codex --version` can still
// "succeed" via the JS launcher while a real run fails.
// ---------------------------------------------------------------------------

/// Actionable diagnostic emitted into the run panel + handy.log when a run's
/// failure is classified as a missing Codex native payload. This is streamed as
/// a raw run-output line (the run panel is intentionally un-i18n'd raw tool
/// output — subprocess stderr flows through it verbatim), in ADDITION to the
/// raw output, which is never swallowed. The Test button surfaces the localized
/// equivalent (`settings.agents.card.cli.binaryPath.hint.codexVendorMissing`).
pub const CODEX_VENDOR_MISSING_DIAGNOSTIC: &str = "OpenFlow diagnostic: Codex's \
native binary is missing from its install (the per-platform \
@openai/codex-<os>-<arch> package was not installed). Reinstall Codex — \
`npm i -g @openai/codex` or `brew reinstall codex` — then use the Test button \
to confirm before running again.";

/// Classify a failed run's captured stderr/stdout into an actionable
/// run-panel diagnostic, or `None` if it doesn't match a known actionable case.
/// This is GAP 1's safety net: a run's spawn failure or fast non-zero exit
/// otherwise dumps raw stderr to the panel without the actionable "reinstall
/// Codex" guidance the Test button already gives. Reuses the exact same
/// classifier the Test button uses so Test and Run agree.
pub fn run_failure_diagnostic(output: &str) -> Option<String> {
    use crate::commands::agent_runs::{classify_binary_output, AgentBinaryHint};
    // Match the concrete hint (not `Option::map`) so a future hint variant
    // forces a decision here rather than silently mapping to the codex fix.
    match classify_binary_output(output)? {
        AgentBinaryHint::CodexVendorMissing => Some(CODEX_VENDOR_MISSING_DIAGNOSTIC.to_string()),
    }
}

/// Provable state of a Codex launcher's native payload. We only ever act on
/// `Missing` — `Unknown` (self-contained native binary, unrecognized layout,
/// or any IO uncertainty) yields NO warning, so a false "reinstall" is
/// impossible. Certainty is impossible without executing the launcher, so
/// GAP 1's run-path classifier (`run_failure_diagnostic`) stays the safety net
/// for the real failures this static check can't predict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexVendorStatus {
    /// A native payload is present (optional-dep package or legacy vendor dir).
    Present,
    /// The launcher declares a per-platform native optional dependency and it
    /// is provably absent, with no legacy vendor payload either. Actionable.
    Missing,
    /// Can't prove either way — never surfaced as a warning.
    Unknown,
}

/// Whether a resolved binary is the npm Node launcher (vs. a self-contained
/// native binary). Two robust signals: the resolved path lives inside a
/// `node_modules` tree, or the file starts with a `#!...node` shebang.
pub fn is_node_launcher(path: &Path) -> bool {
    if path.components().any(|c| c.as_os_str() == "node_modules") {
        return true;
    }
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 128];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    head.lines()
        .next()
        .map(|l| l.starts_with("#!") && l.contains("node"))
        .unwrap_or(false)
}

/// Walk a resolved launcher path's ancestors to find the `@openai/codex`
/// package root — the ancestor dir named `codex` whose parent is `@openai`
/// (e.g. `.../node_modules/@openai/codex` for a `.../@openai/codex/bin/codex.js`
/// launcher). `None` if the path isn't inside such a package.
pub fn openai_codex_pkg_root(launcher: &Path) -> Option<PathBuf> {
    for anc in launcher.ancestors() {
        let is_codex = anc.file_name().and_then(|n| n.to_str()) == Some("codex");
        let parent_is_scope = anc
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("@openai");
        if is_codex && parent_is_scope {
            return Some(anc.to_path_buf());
        }
    }
    None
}

/// Read the `@openai/codex-*` keys from a package.json's `optionalDependencies`.
/// Their presence proves the per-platform-optional-dep vendor mechanism is in
/// use and gives the EXACT sibling package names to look for — so we never
/// guess a platform-name scheme that could drift between Codex versions.
pub fn read_codex_optional_deps(package_json: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(package_json) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    json.get("optionalDependencies")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.keys()
                .filter(|k| k.starts_with("@openai/codex-"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a package directory contains an executable `codex` native binary
/// (`<dir>/codex`, `<dir>/bin/codex`, or a shallow scan; `.exe` on Windows).
fn pkg_dir_has_codex_binary(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    for cand in [
        dir.join("codex"),
        dir.join("bin").join("codex"),
        dir.join("codex.exe"),
        dir.join("bin").join("codex.exe"),
    ] {
        if is_executable_file(&cand) {
            return true;
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if (name == "codex" || name == "codex.exe") && is_executable_file(&ent.path()) {
                return true;
            }
        }
    }
    false
}

/// Whether the legacy `<pkg>/vendor/<triple>/codex/codex` payload is present.
fn vendor_dir_has_codex(vendor_dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(vendor_dir) else {
        return false;
    };
    for triple in rd.flatten() {
        let tdir = triple.path();
        if !tdir.is_dir() {
            continue;
        }
        if pkg_dir_has_codex_binary(&tdir) || pkg_dir_has_codex_binary(&tdir.join("codex")) {
            return true;
        }
    }
    false
}

/// Statically determine, from a `@openai/codex` package root, whether the
/// native payload is present, provably missing, or indeterminate. Pure
/// filesystem inspection (no process execution) so it's fully unit-testable
/// against a fabricated `node_modules` layout.
pub fn codex_vendor_status_at(pkg_root: &Path) -> CodexVendorStatus {
    // Legacy layout: `<pkg>/vendor/<triple>/codex/codex`.
    if vendor_dir_has_codex(&pkg_root.join("vendor")) {
        return CodexVendorStatus::Present;
    }
    let Some(scope_dir) = pkg_root.parent() else {
        return CodexVendorStatus::Unknown;
    };
    // Modern layout: per-platform optional dependency installed as a sibling
    // `node_modules/@openai/codex-<...>`. Look up the EXACT declared names.
    let opt_deps = read_codex_optional_deps(&pkg_root.join("package.json"));
    if opt_deps.is_empty() {
        // Unrecognized/undeclared layout — absence of certainty ⇒ no warning.
        return CodexVendorStatus::Unknown;
    }
    for dep in &opt_deps {
        let Some(short) = dep.strip_prefix("@openai/") else {
            continue;
        };
        if pkg_dir_has_codex_binary(&scope_dir.join(short)) {
            return CodexVendorStatus::Present;
        }
    }
    // The launcher declares a native optional dep, none is installed with a
    // binary, and there's no legacy vendor payload ⇒ provably missing.
    CodexVendorStatus::Missing
}

/// Proactive static vendor check for a resolved `codex` binary path (GAP 2).
/// Resolves symlinks (npm's global `codex` bin is a symlink into the package),
/// confirms it's a Node launcher, finds the `@openai/codex` package root, and
/// inspects the payload. Returns `Unknown` (⇒ no warning) for anything it can't
/// prove — including a self-contained native codex.
pub fn codex_static_vendor_hint(binary_path: &str) -> CodexVendorStatus {
    let path = Path::new(binary_path);
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !is_node_launcher(&real) {
        return CodexVendorStatus::Unknown;
    }
    match openai_codex_pkg_root(&real) {
        Some(root) => codex_vendor_status_at(&root),
        None => CodexVendorStatus::Unknown,
    }
}

/// The user's login shell if `$SHELL` is an absolute path to a known shell,
/// else zsh (the macOS default). Used for the login-shell detect fallback and
/// as the `SHELL` env passed to spawned agents.
pub fn login_shell() -> String {
    match std::env::var("SHELL") {
        Ok(s) if is_known_shell(&s) => s,
        _ => "/bin/zsh".to_string(),
    }
}

/// Whether `path` is an absolute path to a recognized interactive shell.
fn is_known_shell(path: &str) -> bool {
    path.starts_with('/')
        && matches!(
            Path::new(path).file_name().and_then(|n| n.to_str()),
            Some("zsh" | "bash" | "fish" | "sh" | "dash" | "ksh")
        )
}

/// Parse the stdout of `command -v <name>`: the first non-empty, trimmed line.
/// (`command -v` prints the resolved path for an external command.) Pure +
/// testable; absoluteness/executability is validated by the caller.
pub fn parse_command_v_output(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// Shell-lookup fallback for detection. Unix: run `<login-shell> -lc 'command
/// -v <name>'` so the user's real profile PATH (rbenv/asdf/fnm/custom exports)
/// is consulted, exactly like their Terminal. Windows: `where.exe <name>`
/// (which searches PATH honoring PATHEXT). Bounded to 5s; returns the resolved
/// path only if it is an existing executable.
pub async fn login_shell_which(name: &str) -> Option<String> {
    #[cfg(not(windows))]
    let fut = {
        let shell = login_shell();
        let script = format!("command -v {name}");
        let mut cmd = Command::new(shell);
        cmd.arg("-lc").arg(script);
        cmd.env("PATH", baseline_path(std::env::var("PATH").ok().as_deref()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
    };
    #[cfg(windows)]
    let fut = {
        let mut cmd = Command::new("where.exe");
        cmd.arg(name);
        cmd.env("PATH", baseline_path(std::env::var("PATH").ok().as_deref()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
    };
    let output = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Both `command -v` and `where.exe` print the resolved path on the first
    // line (where.exe may print several matches; the first wins).
    let resolved = parse_command_v_output(&text)?;
    // Only trust an absolute path to a real executable (skips shell
    // aliases/builtins on unix; a stray relative match on Windows).
    let absolute = if cfg!(windows) {
        Path::new(&resolved).is_absolute()
    } else {
        resolved.starts_with('/')
    };
    if absolute && is_executable_file(Path::new(&resolved)) {
        Some(resolved)
    } else {
        None
    }
}

/// How to invoke a target binary: the program to exec plus any arguments that
/// must precede the agent's own argv. On Windows, `.cmd`/`.bat` scripts (npm's
/// global shims) cannot be executed by `CreateProcess` directly — they must be
/// launched via `cmd.exe /C <script> <args…>`. Everything else (and everything
/// on unix) spawns directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub program: String,
    /// Arguments to pass BEFORE the caller's argv (e.g. `/C <script>`).
    pub pre_args: Vec<String>,
}

/// Decide the spawn plan for `binary`. Pure + parameterized on `windows` so
/// the Windows decision is unit-testable from any host; production callers
/// pass `cfg!(windows)`.
pub fn spawn_plan(binary: &str, windows: bool) -> SpawnPlan {
    let is_batch_script = windows
        && Path::new(binary)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            .unwrap_or(false);
    if is_batch_script {
        SpawnPlan {
            program: "cmd.exe".to_string(),
            pre_args: vec!["/C".to_string(), binary.to_string()],
        }
    } else {
        SpawnPlan {
            program: binary.to_string(),
            pre_args: Vec::new(),
        }
    }
}

/// Apply the shared baseline spawn environment (augmented PATH, plus login
/// SHELL and HOME on unix) to a command. Used by BOTH the run pipeline and the
/// Test button so a GUI-launched app (stripped launchd PATH) resolves the same
/// binaries in both places, and a Node/shell shim the CLI wraps inherits a
/// usable PATH.
pub fn apply_baseline_env(cmd: &mut Command) {
    cmd.env("PATH", baseline_path(std::env::var("PATH").ok().as_deref()));
    #[cfg(not(windows))]
    cmd.env("SHELL", login_shell());
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
}

/// Construct the run file path: `<dir>/<YYYYMMDD-HHMMSS>-<agentId>.md`. Pure +
/// testable (timestamp is injected).
pub fn run_file_path(dir: &Path, agent_id: &str, ts: DateTime<Local>) -> PathBuf {
    let stamp = ts.format("%Y%m%d-%H%M%S");
    dir.join(format!("{stamp}-{agent_id}.md"))
}

/// Render the markdown run-file body (header + instruction + raw output).
fn render_run_file(
    agent: &AgentDefinition,
    project: &str,
    instruction: &str,
    output: &str,
    status: &RunStatus,
    duration: Duration,
    ts: DateTime<Local>,
) -> String {
    let cli = agent
        .cli_type
        .map(cli_type_label)
        .unwrap_or("custom")
        .to_string();
    let status_str = match status {
        RunStatus::Finished { code } => format!("finished (exit {code})"),
        RunStatus::Failed { error } => format!("failed: {error}"),
        RunStatus::Stopped => "stopped".to_string(),
        RunStatus::Running => "running".to_string(),
    };
    let project_disp = if project.trim().is_empty() {
        "(none)"
    } else {
        project
    };
    format!(
        "# Agent run — {name}\n\n\
         - **Agent:** {name} (`{id}`, {cli})\n\
         - **Project:** {project}\n\
         - **When:** {when}\n\
         - **Status:** {status}\n\
         - **Duration:** {dur:.1}s\n\n\
         ## Instruction\n\n{instruction}\n\n\
         ## Output\n\n```\n{output}\n```\n",
        name = agent.name,
        id = agent.id,
        cli = cli,
        project = project_disp,
        when = ts.to_rfc3339(),
        status = status_str,
        dur = duration.as_secs_f64(),
        instruction = instruction,
        output = output,
    )
}

fn cli_type_label(t: AgentCliType) -> &'static str {
    match t {
        AgentCliType::Claude => "claude",
        AgentCliType::Codex => "codex",
        AgentCliType::Openclaw => "openclaw",
        AgentCliType::Hermes => "hermes",
        AgentCliType::Kimi => "kimi",
        AgentCliType::Custom => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn build_argv_stdin_drops_prompt_token_and_substitutes_cwd() {
        // The verified claude template: instruction on stdin, so no {prompt}.
        let argv = build_argv(
            "-p --output-format stream-json --verbose --permission-mode acceptEdits",
            "/tmp/proj",
            "add a comment",
            PromptDelivery::Stdin,
        );
        assert_eq!(
            argv,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits"
            ]
        );
    }

    #[test]
    fn build_argv_arg_delivery_substitutes_prompt() {
        let argv = build_argv(
            "exec --json {prompt}",
            "/tmp/p",
            "do the thing",
            PromptDelivery::Arg,
        );
        assert_eq!(argv, vec!["exec", "--json", "do the thing"]);
    }

    #[test]
    fn build_argv_kimi_template_delivers_prompt_as_single_arg() {
        // Regression test for the exact bug diagnosed from the user's report:
        // Kimi's `-p, --prompt <prompt>` takes the instruction as ONE argv
        // element. A multi-word instruction like "list all the files" must
        // reach argv as a single element right after `-p`, never tokenized into
        // three separate args (which is what produced Kimi's
        // `option '-p, --prompt <prompt>' argument missing` — the instruction
        // had actually been delivered on stdin, per-word, not as this arg at
        // all). This exercises the real default Kimi template end to end.
        let argv = build_argv(
            "-p {prompt} --output-format text",
            "/tmp/proj",
            "list all the files",
            PromptDelivery::Arg,
        );
        assert_eq!(
            argv,
            vec!["-p", "list all the files", "--output-format", "text"]
        );
        // Explicitly: the prompt is ONE element, not split on its spaces.
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[1], "list all the files");
    }

    #[test]
    fn build_argv_openclaw_template_delivers_prompt_as_single_arg() {
        // Regression test for the verified default OpenClaw template:
        // `-m/--message` takes the instruction as ONE argv element. A
        // multi-word instruction must reach argv as a single element right
        // after `--message`, never re-split on its spaces.
        let argv = build_argv(
            "agent --local --agent main --message {prompt}",
            "/tmp/proj",
            "list all the files",
            PromptDelivery::Arg,
        );
        assert_eq!(
            argv,
            vec![
                "agent",
                "--local",
                "--agent",
                "main",
                "--message",
                "list all the files"
            ]
        );
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[5], "list all the files");
    }

    #[test]
    fn build_argv_hermes_template_delivers_prompt_as_single_arg() {
        // Regression test for the verified default Hermes template: `-z`
        // takes the instruction as ONE argv element right after it.
        let argv = build_argv(
            "-z {prompt} --yolo",
            "/tmp/proj",
            "list all the files",
            PromptDelivery::Arg,
        );
        assert_eq!(argv, vec!["-z", "list all the files", "--yolo"]);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[1], "list all the files");
    }

    #[test]
    fn build_argv_stdin_drops_bare_prompt_but_keeps_other_args() {
        let argv = build_argv("run {prompt} --flag", "/x", "hi", PromptDelivery::Stdin);
        assert_eq!(argv, vec!["run", "--flag"]);
    }

    #[test]
    fn build_argv_substitutes_cwd_placeholder() {
        let argv = build_argv(
            "--dir {cwd} run",
            "/home/me/proj",
            "",
            PromptDelivery::Stdin,
        );
        assert_eq!(argv, vec!["--dir", "/home/me/proj", "run"]);
    }

    #[test]
    fn tokenize_honors_quotes() {
        assert_eq!(
            tokenize_template("run \"two words\" --flag 'single quoted'"),
            vec!["run", "two words", "--flag", "single quoted"]
        );
    }

    #[test]
    fn tokenize_empty_quotes_produce_empty_arg() {
        assert_eq!(tokenize_template("--name \"\""), vec!["--name", ""]);
    }

    #[test]
    fn baseline_path_includes_homebrew_and_dedupes() {
        let path = baseline_path(Some("/usr/bin:/custom/bin"));
        assert!(path.contains("/opt/homebrew/bin"));
        assert!(path.contains("/usr/local/bin"));
        assert!(path.starts_with("/usr/bin:/custom/bin"));
        // /usr/bin appears once despite also being in the baseline set.
        assert_eq!(path.matches("/usr/bin").count(), 1);
    }

    #[test]
    fn baseline_path_from_empty_still_has_baseline() {
        let path = baseline_path(None);
        assert!(path.contains("/usr/local/bin"));
        assert!(path.contains("/opt/homebrew/bin"));
    }

    /// Test env lookup: HOME=/Users/me only (unix shape).
    fn unix_env(key: &str) -> Option<String> {
        match key {
            "HOME" => Some("/Users/me".to_string()),
            _ => None,
        }
    }

    /// Test env lookup: the standard Windows variables, injected (item: the
    /// Windows baseline must come from env vars, never hardcoded drives).
    fn win_env(key: &str) -> Option<String> {
        match key {
            "APPDATA" => Some(r"C:\Users\me\AppData\Roaming".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\me\AppData\Local".to_string()),
            "USERPROFILE" => Some(r"C:\Users\me".to_string()),
            "ProgramFiles" => Some(r"C:\Program Files".to_string()),
            _ => None,
        }
    }

    fn no_env(_key: &str) -> Option<String> {
        None
    }

    #[test]
    fn baseline_bin_dirs_includes_home_and_static_dirs() {
        let dirs = baseline_bin_dirs(false, &unix_env, &[]);
        // Home-relative install dirs a stripped launchd PATH omits.
        assert!(dirs.contains(&"/Users/me/.local/bin".to_string()));
        assert!(dirs.contains(&"/Users/me/.bun/bin".to_string()));
        assert!(dirs.contains(&"/Users/me/.cargo/bin".to_string()));
        assert!(dirs.contains(&"/Users/me/.volta/bin".to_string()));
        assert!(dirs.contains(&"/Users/me/.nvm/current/bin".to_string()));
        // Kimi Code CLI's own install location (its installer only updates a
        // shell rc file, which a GUI-launched app never sources).
        assert!(dirs.contains(&"/Users/me/.kimi-code/bin".to_string()));
        // OpenClaw's install-cli.sh default prefix (`~/.openclaw/bin`).
        assert!(dirs.contains(&"/Users/me/.openclaw/bin".to_string()));
        // Both arch Homebrew prefixes + system dirs.
        assert!(dirs.contains(&"/opt/homebrew/bin".to_string()));
        assert!(dirs.contains(&"/usr/local/bin".to_string()));
        assert!(dirs.contains(&"/usr/bin".to_string()));
    }

    #[test]
    fn baseline_bin_dirs_without_home_has_only_static() {
        let dirs = baseline_bin_dirs(false, &no_env, &[]);
        assert!(dirs.iter().all(|d| !d.contains(".local")));
        assert!(dirs.contains(&"/opt/homebrew/bin".to_string()));
    }

    #[test]
    fn baseline_bin_dirs_appends_injected_nvm_dirs_newest_first() {
        let nvm = vec![
            "/Users/me/.nvm/versions/node/v22.2.0/bin".to_string(),
            "/Users/me/.nvm/versions/node/v18.0.0/bin".to_string(),
        ];
        let dirs = baseline_bin_dirs(false, &unix_env, &nvm);
        let i22 = dirs.iter().position(|d| d.contains("v22.2.0")).unwrap();
        let i18 = dirs.iter().position(|d| d.contains("v18.0.0")).unwrap();
        assert!(i22 < i18, "newest node version must come first");
    }

    #[test]
    fn baseline_bin_dirs_windows_resolves_from_env_vars() {
        let dirs = baseline_bin_dirs(true, &win_env, &[]);
        // npm global shims — where claude.cmd/codex.cmd live.
        assert!(dirs.contains(&r"C:\Users\me\AppData\Roaming\npm".to_string()));
        // Per-user installers.
        assert!(dirs.contains(&r"C:\Users\me\AppData\Local\Programs".to_string()));
        // Profile-relative tool dirs.
        assert!(dirs.contains(&r"C:\Users\me\.bun\bin".to_string()));
        assert!(dirs.contains(&r"C:\Users\me\.volta\bin".to_string()));
        assert!(dirs.contains(&r"C:\Users\me\.cargo\bin".to_string()));
        assert!(dirs.contains(&r"C:\Users\me\scoop\shims".to_string()));
        assert!(dirs.contains(&r"C:\Program Files\nodejs".to_string()));
        // No unix dirs leak into the Windows baseline.
        assert!(dirs.iter().all(|d| !d.starts_with('/')));
    }

    #[test]
    fn baseline_bin_dirs_windows_skips_missing_env_vars() {
        // Only USERPROFILE present — APPDATA/LOCALAPPDATA/ProgramFiles entries
        // must be absent rather than "\npm"-style garbage.
        let env = |k: &str| match k {
            "USERPROFILE" => Some(r"C:\Users\me".to_string()),
            _ => None,
        };
        let dirs = baseline_bin_dirs(true, &env, &[]);
        assert!(dirs.iter().all(|d| d.starts_with(r"C:\Users\me")));
        assert!(dirs.contains(&r"C:\Users\me\scoop\shims".to_string()));
    }

    #[test]
    fn split_path_list_unix_splits_on_colon_and_drops_empties() {
        assert_eq!(
            split_path_list("/usr/bin::/bin:", false),
            vec!["/usr/bin".to_string(), "/bin".to_string()]
        );
    }

    #[test]
    fn split_path_list_windows_splits_on_semicolon_keeping_drive_letters() {
        // The pre-fix ':' split would mangle these at the drive-letter colon.
        assert_eq!(
            split_path_list(r"C:\Windows\system32;C:\Program Files\nodejs;", true),
            vec![
                r"C:\Windows\system32".to_string(),
                r"C:\Program Files\nodejs".to_string(),
            ]
        );
    }

    #[test]
    fn split_path_list_windows_honors_double_quotes() {
        // A quoted entry may contain ';' (std::env::split_paths semantics —
        // quotes protect the separator and are stripped from the entry).
        assert_eq!(
            split_path_list(r#""C:\odd;dir";C:\bin"#, true),
            vec![r"C:\odd;dir".to_string(), r"C:\bin".to_string()]
        );
    }

    #[test]
    fn split_path_list_matches_std_split_paths_on_host() {
        // Contract test: our parameterized splitter agrees with the std
        // implementation for the host platform's separator.
        let raw = if cfg!(windows) {
            r"C:\a;C:\b c;C:\d"
        } else {
            "/a:/b c:/d"
        };
        let ours = split_path_list(raw, cfg!(windows));
        let std_split: Vec<String> = std::env::split_paths(raw)
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(ours, std_split);
    }

    #[test]
    fn join_path_list_uses_platform_separator() {
        let dirs = vec!["/a".to_string(), "/b".to_string()];
        assert_eq!(join_path_list(&dirs, false), "/a:/b");
        let wdirs = vec![r"C:\a".to_string(), r"C:\b".to_string()];
        assert_eq!(join_path_list(&wdirs, true), r"C:\a;C:\b");
    }

    #[test]
    fn candidate_file_names_unix_is_bare_name() {
        assert_eq!(
            candidate_file_names("claude", false, None),
            vec!["claude".to_string()]
        );
        // PATHEXT is ignored on unix even if somehow set.
        assert_eq!(
            candidate_file_names("claude", false, Some(".EXE;.CMD")),
            vec!["claude".to_string()]
        );
    }

    #[test]
    fn candidate_file_names_windows_defaults_to_exe_cmd_bat() {
        assert_eq!(
            candidate_file_names("claude", true, None),
            vec![
                "claude.exe".to_string(),
                "claude.cmd".to_string(),
                "claude.bat".to_string(),
            ]
        );
    }

    #[test]
    fn candidate_file_names_windows_parses_pathext() {
        assert_eq!(
            candidate_file_names("codex", true, Some(".COM;.EXE;.BAT;.CMD")),
            vec![
                "codex.com".to_string(),
                "codex.exe".to_string(),
                "codex.bat".to_string(),
                "codex.cmd".to_string(),
            ]
        );
        // Blank/garbage PATHEXT falls back to the default trio.
        assert_eq!(
            candidate_file_names("codex", true, Some("  ;x")),
            vec![
                "codex.exe".to_string(),
                "codex.cmd".to_string(),
                "codex.bat".to_string(),
            ]
        );
    }

    #[test]
    fn spawn_plan_wraps_windows_batch_scripts_via_cmd() {
        // npm's global shims are .cmd — CreateProcess can't exec them raw.
        let plan = spawn_plan(r"C:\Users\me\AppData\Roaming\npm\claude.cmd", true);
        assert_eq!(plan.program, "cmd.exe");
        assert_eq!(
            plan.pre_args,
            vec![
                "/C".to_string(),
                r"C:\Users\me\AppData\Roaming\npm\claude.cmd".to_string(),
            ]
        );
        // Extension match is case-insensitive; .bat too.
        assert_eq!(spawn_plan(r"C:\t\x.CMD", true).program, "cmd.exe");
        assert_eq!(spawn_plan(r"C:\t\x.bat", true).program, "cmd.exe");
    }

    #[test]
    fn spawn_plan_spawns_exe_and_unix_binaries_directly() {
        let exe = spawn_plan(r"C:\Program Files\nodejs\codex.exe", true);
        assert_eq!(exe.program, r"C:\Program Files\nodejs\codex.exe");
        assert!(exe.pre_args.is_empty());
        // Extensionless (Windows) — direct.
        assert_eq!(spawn_plan(r"C:\t\codex", true).pre_args.len(), 0);
        // Unix: even a ".cmd"-suffixed path is spawned directly.
        let unix = spawn_plan("/usr/local/bin/claude.cmd", false);
        assert_eq!(unix.program, "/usr/local/bin/claude.cmd");
        assert!(unix.pre_args.is_empty());
    }

    #[test]
    fn parse_semver_key_orders_versions_numerically() {
        // 9 < 10 numerically (lexical sort would get this wrong).
        assert!(parse_semver_key("v10.0.0") > parse_semver_key("v9.9.9"));
        assert_eq!(parse_semver_key("v20.11.1"), vec![20, 11, 1]);
        assert_eq!(parse_semver_key("18.0.0"), vec![18, 0, 0]);
    }

    #[test]
    fn detect_search_dirs_puts_process_path_first_and_dedupes() {
        // /usr/local/bin is in BOTH the process PATH and the baseline set.
        let dirs = detect_search_dirs(
            Some("/usr/local/bin:/custom/tool/bin"),
            false,
            &unix_env,
            &[],
        );
        assert_eq!(dirs[0], "/usr/local/bin");
        assert_eq!(dirs[1], "/custom/tool/bin");
        assert_eq!(dirs.iter().filter(|d| *d == "/usr/local/bin").count(), 1);
        // Baseline home dir still present after the process PATH.
        assert!(dirs.contains(&"/Users/me/.local/bin".to_string()));
    }

    #[test]
    fn detect_search_dirs_windows_splits_semicolons_and_appends_baseline() {
        // A Windows PATH with drive letters must not be split at ':'.
        let dirs = detect_search_dirs(Some(r"C:\Windows\system32;C:\Windows"), true, &win_env, &[]);
        assert_eq!(dirs[0], r"C:\Windows\system32");
        assert_eq!(dirs[1], r"C:\Windows");
        assert!(dirs.contains(&r"C:\Users\me\AppData\Roaming\npm".to_string()));
    }

    /// Reproduction of the dev-vs-installed PATH split (BLOCKERS §10b bug #1):
    /// under the stripped launchd PATH a GUI app inherits, a process-PATH-only
    /// search (the OLD detect behavior) misses a CLI installed in `~/.local/bin`
    /// or a node-version-manager dir, but the new baseline-augmented search
    /// finds it.
    #[test]
    fn detect_search_dirs_finds_home_installed_cli_under_stripped_path() {
        // The PATH a Finder/Dock-launched app actually gets on macOS.
        let stripped = "/usr/bin:/bin:/usr/sbin:/sbin";
        let home = "/Users/me";
        let nvm = vec!["/Users/me/.nvm/versions/node/v22.2.0/bin".to_string()];

        // OLD logic: only the process PATH was searched.
        let old_dirs: Vec<String> = stripped.split(':').map(String::from).collect();
        assert!(!old_dirs.contains(&"/Users/me/.local/bin".to_string()));
        assert!(!old_dirs.contains(&"/Users/me/.nvm/versions/node/v22.2.0/bin".to_string()));

        // NEW logic: baseline dirs are appended, so the home install is reachable.
        let env = move |k: &str| match k {
            "HOME" => Some(home.to_string()),
            _ => None,
        };
        let new_dirs = detect_search_dirs(Some(stripped), false, &env, &nvm);
        assert!(new_dirs.contains(&"/Users/me/.local/bin".to_string()));
        assert!(new_dirs.contains(&"/Users/me/.nvm/versions/node/v22.2.0/bin".to_string()));
        assert!(new_dirs.contains(&"/opt/homebrew/bin".to_string()));
    }

    #[test]
    fn parse_command_v_output_returns_resolved_path() {
        assert_eq!(
            parse_command_v_output("/opt/homebrew/bin/claude\n"),
            Some("/opt/homebrew/bin/claude".to_string())
        );
        // Skips leading blank lines a login profile may emit.
        assert_eq!(
            parse_command_v_output("\n\n  /Users/me/.local/bin/codex  \n"),
            Some("/Users/me/.local/bin/codex".to_string())
        );
        assert_eq!(parse_command_v_output(""), None);
        assert_eq!(parse_command_v_output("   \n"), None);
    }

    #[test]
    fn is_known_shell_accepts_absolute_known_shells_only() {
        assert!(is_known_shell("/bin/zsh"));
        assert!(is_known_shell("/opt/homebrew/bin/bash"));
        assert!(is_known_shell("/usr/bin/fish"));
        assert!(!is_known_shell("zsh")); // not absolute
        assert!(!is_known_shell("/usr/bin/python3")); // not a shell
        assert!(!is_known_shell(""));
    }

    #[test]
    fn is_executable_file_detects_exec_bit() {
        // /bin/sh is a known executable on macOS/Linux CI.
        assert!(is_executable_file(Path::new("/bin/sh")));
        assert!(!is_executable_file(Path::new(
            "/definitely/not/a/real/path/xyz"
        )));
        // A directory is not an executable file.
        assert!(!is_executable_file(Path::new("/usr")));
    }

    #[test]
    fn run_file_path_uses_timestamp_and_agent_id() {
        let ts = Local.with_ymd_and_hms(2026, 7, 7, 9, 8, 7).unwrap();
        let path = run_file_path(Path::new("/proj/.openflow/agent-runs"), "coder", ts);
        assert_eq!(
            path,
            PathBuf::from("/proj/.openflow/agent-runs/20260707-090807-coder.md")
        );
    }

    #[test]
    fn registry_add_stop_and_clear() {
        let mgr = AgentRunManager::new();
        // Insert a fake running entry directly (start() would spawn a process).
        let (tx, _rx) = mpsc::unbounded_channel::<()>();
        {
            let mut runs = mgr.runs.lock().unwrap();
            runs.insert(
                "run-1".to_string(),
                AgentRun {
                    agent_id: "coder".into(),
                    agent_name: "Coder".into(),
                    project_path: "/proj".into(),
                    status: RunStatus::Running,
                    started_at: Local::now(),
                    output: String::new(),
                    instruction: "do it".into(),
                    output_file: None,
                    kill_tx: Some(tx),
                    session_id: None,
                    permission_tx: None,
                },
            );
        }
        assert_eq!(mgr.list_runs().len(), 1);

        // Stop signals the channel and leaves the entry (monitor task would flip
        // status in the real path); simulate that transition explicitly.
        assert!(mgr.stop_run("run-1").is_ok());
        mgr.set_status("run-1", RunStatus::Stopped);
        assert!(mgr.stop_run("run-1").is_err()); // no longer running

        // clear_finished drops terminal runs.
        mgr.clear_finished();
        assert!(mgr.list_runs().is_empty());
    }

    #[test]
    fn stop_unknown_run_errors() {
        let mgr = AgentRunManager::new();
        assert!(mgr.stop_run("nope").is_err());
    }

    #[test]
    fn append_output_caps_buffer() {
        let mgr = AgentRunManager::new();
        let (tx, _rx) = mpsc::unbounded_channel::<()>();
        {
            let mut runs = mgr.runs.lock().unwrap();
            runs.insert(
                "r".to_string(),
                AgentRun {
                    agent_id: "a".into(),
                    agent_name: "A".into(),
                    project_path: String::new(),
                    status: RunStatus::Running,
                    started_at: Local::now(),
                    output: String::new(),
                    instruction: String::new(),
                    output_file: None,
                    kill_tx: Some(tx),
                    session_id: None,
                    permission_tx: None,
                },
            );
        }
        let big = "x".repeat(OUTPUT_BUFFER_CAP);
        mgr.append_output("r", &big);
        mgr.append_output("r", &big);
        let info = mgr.list_runs().into_iter().next().unwrap();
        assert!(info.output.len() <= OUTPUT_BUFFER_CAP + 2);
    }

    // --- Codex run-path diagnostics (GAP 1) ------------------------------

    #[test]
    fn run_failure_diagnostic_classifies_older_launcher_raw_enoent() {
        // Canned stderr an older codex launcher streams to the run panel.
        let out = "Error: spawn /opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex ENOENT";
        let diag = run_failure_diagnostic(out).expect("should classify");
        assert_eq!(diag, CODEX_VENDOR_MISSING_DIAGNOSTIC);
    }

    #[test]
    fn run_failure_diagnostic_classifies_newer_launcher_missing_optional_dep() {
        let out = "node:internal/modules/cjs/loader: Missing optional dependency @openai/codex-darwin-arm64";
        assert_eq!(
            run_failure_diagnostic(out),
            Some(CODEX_VENDOR_MISSING_DIAGNOSTIC.to_string())
        );
    }

    #[test]
    fn run_failure_diagnostic_ignores_ordinary_output() {
        assert_eq!(run_failure_diagnostic("Applying edit to src/main.rs"), None);
        // An unrelated ENOENT (not the codex vendor payload) is not our case.
        assert_eq!(run_failure_diagnostic("Error: spawn git ENOENT"), None);
    }

    // --- Codex proactive vendor check (GAP 2) ----------------------------

    /// Create an executable file with the given contents (exec bit on unix so
    /// `is_executable_file` accepts it).
    fn write_exec(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Fabricate a `node_modules/@openai/codex` launcher package. `platform_pkg`,
    /// when set, becomes the declared optional dependency; `install_binary`
    /// controls whether that per-platform package ships a native `codex`.
    /// Returns (node_modules_root, launcher_js_path).
    fn fake_codex_install(
        root: &Path,
        platform_pkg: Option<&str>,
        install_binary: bool,
    ) -> (PathBuf, PathBuf) {
        let node_modules = root.join("node_modules");
        let pkg = node_modules.join("@openai").join("codex");
        let launcher = pkg.join("bin").join("codex.js");
        write_exec(&launcher, "#!/usr/bin/env node\nconsole.log('launcher');\n");
        let opt_deps = platform_pkg
            .map(|p| format!("\"optionalDependencies\":{{\"{p}\":\"1.0.0\"}}"))
            .unwrap_or_else(|| "\"optionalDependencies\":{}".to_string());
        std::fs::write(
            pkg.join("package.json"),
            format!("{{\"name\":\"@openai/codex\",{opt_deps}}}"),
        )
        .unwrap();
        if let (Some(p), true) = (platform_pkg, install_binary) {
            let short = p.strip_prefix("@openai/").unwrap();
            write_exec(
                &node_modules
                    .join("@openai")
                    .join(short)
                    .join("bin")
                    .join("codex"),
                "native binary",
            );
        }
        (node_modules, launcher)
    }

    #[test]
    fn openai_codex_pkg_root_found_from_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(dir.path(), None, false);
        let root = openai_codex_pkg_root(&launcher).expect("pkg root");
        assert!(root.ends_with("@openai/codex"));
        // A path outside such a package resolves to None.
        assert_eq!(
            openai_codex_pkg_root(Path::new("/usr/local/bin/codex")),
            None
        );
    }

    #[test]
    fn is_node_launcher_detects_node_modules_and_shebang() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(dir.path(), None, false);
        // Inside a node_modules tree.
        assert!(is_node_launcher(&launcher));
        // Shebang detection outside node_modules.
        let shebang = dir.path().join("codex");
        write_exec(&shebang, "#!/usr/bin/env node\n// launcher\n");
        assert!(is_node_launcher(&shebang));
        // A self-contained native binary (no node shebang, not under node_modules).
        let native = dir.path().join("native-codex");
        write_exec(&native, "\x7fELF fake binary");
        assert!(!is_node_launcher(&native));
    }

    #[test]
    fn codex_vendor_status_present_via_optional_dep() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(
            dir.path(),
            Some("@openai/codex-darwin-arm64"),
            true, // native binary IS installed
        );
        let root = openai_codex_pkg_root(&launcher).unwrap();
        assert_eq!(codex_vendor_status_at(&root), CodexVendorStatus::Present);
    }

    #[test]
    fn codex_vendor_status_missing_when_optional_dep_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(
            dir.path(),
            Some("@openai/codex-darwin-arm64"),
            false, // declared but NOT installed — the real broken-install bug
        );
        let root = openai_codex_pkg_root(&launcher).unwrap();
        assert_eq!(codex_vendor_status_at(&root), CodexVendorStatus::Missing);
    }

    #[test]
    fn codex_vendor_status_unknown_without_declared_optional_deps() {
        // No optionalDependencies declared and no vendor dir — we can't prove
        // the payload is missing (a future in-package layout?), so no warning.
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(dir.path(), None, false);
        let root = openai_codex_pkg_root(&launcher).unwrap();
        assert_eq!(codex_vendor_status_at(&root), CodexVendorStatus::Unknown);
    }

    #[test]
    fn codex_vendor_status_present_via_legacy_vendor_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) = fake_codex_install(dir.path(), None, false);
        let root = openai_codex_pkg_root(&launcher).unwrap();
        // Legacy `<pkg>/vendor/<triple>/codex/codex` payload present.
        write_exec(
            &root
                .join("vendor")
                .join("aarch64-apple-darwin")
                .join("codex")
                .join("codex"),
            "native binary",
        );
        assert_eq!(codex_vendor_status_at(&root), CodexVendorStatus::Present);
    }

    #[test]
    fn codex_static_vendor_hint_end_to_end_missing_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let (_nm, launcher) =
            fake_codex_install(dir.path(), Some("@openai/codex-darwin-arm64"), false);
        // End-to-end from the launcher path: provably missing.
        assert_eq!(
            codex_static_vendor_hint(&launcher.to_string_lossy()),
            CodexVendorStatus::Missing
        );
        // A non-launcher native binary is Unknown (never warned on).
        let native = dir.path().join("native-codex");
        write_exec(&native, "\x7fELF fake binary");
        assert_eq!(
            codex_static_vendor_hint(&native.to_string_lossy()),
            CodexVendorStatus::Unknown
        );
        // A nonexistent path is Unknown, never Missing.
        assert_eq!(
            codex_static_vendor_hint("/no/such/codex"),
            CodexVendorStatus::Unknown
        );
    }

    // -----------------------------------------------------------------------
    // C0 — the ACP driver.
    // -----------------------------------------------------------------------

    /// A CLI agent fixture. `AgentDefinition` deliberately has no `Default`
    /// impl (`enabled` defaults to true via serde, not `bool::default()`), so
    /// the ACP tests build one explicitly — same shape `acp_session.rs`'s own
    /// tests use.
    fn cli_agent() -> AgentDefinition {
        AgentDefinition {
            id: "coder".to_string(),
            name: "Coder".to_string(),
            enabled: true,
            binding_id: "agent:coder".to_string(),
            provider_id: "openrouter".to_string(),
            model: String::new(),
            system_prompt: String::new(),
            output_mode: crate::settings::AgentOutputMode::Inject,
            kind: AgentKind::Cli,
            cli_type: Some(AgentCliType::Claude),
            binary_path: "/usr/local/bin/claude".to_string(),
            command_template: String::new(),
            project_path: String::new(),
            output_sinks: vec![AgentOutputSink::Panel],
            prompt_via: PromptDelivery::Stdin,
            remote_url: String::new(),
            remote_endpoint: String::new(),
            remote_card_name: String::new(),
            remote_card_version: String::new(),
            remote_streaming: false,
            cli_protocol: CliProtocol::Raw,
            acp_command_template: String::new(),
            acp_permission_policy: AcpPermissionPolicy::Ask,
            acp_idle_timeout_secs: 600,
        }
    }

    #[test]
    fn stop_reason_maps_to_run_status() {
        assert_eq!(
            stop_reason_to_status(StopReason::Completed),
            RunStatus::Finished { code: 0 }
        );
        assert_eq!(
            stop_reason_to_status(StopReason::Cancelled),
            RunStatus::Stopped
        );
        match stop_reason_to_status(StopReason::MaxStepsReached) {
            RunStatus::Failed { error } => assert!(error.contains("step limit")),
            s => panic!("expected Failed, got {s:?}"),
        }
        match stop_reason_to_status(StopReason::RequestTimeout) {
            RunStatus::Failed { error } => assert!(error.contains("timed out")),
            s => panic!("expected Failed, got {s:?}"),
        }
        match stop_reason_to_status(StopReason::Other) {
            RunStatus::Failed { error } => assert!(!error.is_empty()),
            s => panic!("expected Failed, got {s:?}"),
        }
    }

    #[test]
    fn run_info_exposes_session_id_and_defaults_to_none_for_raw_runs() {
        let run = AgentRun {
            agent_id: "a".into(),
            agent_name: "A".into(),
            project_path: "/p".into(),
            status: RunStatus::Running,
            started_at: Local::now(),
            output: String::new(),
            instruction: "hi".into(),
            output_file: None,
            kill_tx: None,
            session_id: None,
            permission_tx: None,
        };
        assert_eq!(run.to_info("r1").session_id, None);

        let acp = AgentRun {
            session_id: Some("s1".into()),
            ..run
        };
        assert_eq!(acp.to_info("r2").session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn acp_agents_route_to_the_acp_driver_and_everything_else_does_not() {
        // `uses_acp_driver` is not a mirror of the dispatch guard — it IS the
        // guard `start`'s match arm evaluates, so this asserts the production
        // routing decision rather than a copy of it.
        let mut a = cli_agent();
        a.kind = AgentKind::Cli;
        a.cli_protocol = CliProtocol::Acp;
        assert!(uses_acp_driver(&a));

        a.cli_protocol = CliProtocol::Raw;
        assert!(
            !uses_acp_driver(&a),
            "raw CLI agents keep the existing driver"
        );

        // The `AgentKind::Remote` arm sits BEFORE this guard in `start`, so a
        // remote agent never reaches it — and the guard itself also rejects it,
        // which keeps the routing correct even if the arms are ever reordered.
        a.kind = AgentKind::Remote;
        a.cli_protocol = CliProtocol::Acp;
        assert!(!uses_acp_driver(&a), "remote agents keep the A2A driver");

        a.kind = AgentKind::Prompt;
        assert!(!uses_acp_driver(&a));
    }

    // ---- the turn loop, against a scripted session (no process, no Tauri) ----

    use crate::acp::codec::JsonRpcError;
    use crate::acp::protocol::{
        RequestPermissionParams, SessionNotification, SessionUpdate, TextContent,
    };
    use serde_json::json;
    use std::collections::VecDeque;

    /// Run a future on a current-thread runtime with the time driver enabled —
    /// this repo uses no `#[tokio::test]` (see `a2a.rs`, `acp/client.rs`).
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// Everything the turn loop sent back to the agent, in order.
    #[derive(Debug, PartialEq)]
    enum Sent {
        Prompt(String),
        Answer {
            id: Value,
            outcome: PermissionOutcome,
        },
        Refused(Value),
        Cancel,
    }

    /// A scripted ACP session.
    ///
    /// Reply-driven and therefore fully deterministic: `replies[n]` is what the
    /// agent says after the n-th thing WE send it (`replies[0]` after the
    /// prompt). An inner `None` scripts the child exiting. Once the script is
    /// exhausted the agent simply goes quiet, so a test can only end by
    /// scripting a terminal frame, a crash, or a timeout — never by falling off
    /// the end into an accidental "crash".
    struct FakeSession {
        turn_lock: tokio::sync::Mutex<()>,
        inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<Option<PumpItem>>>,
        feed: mpsc::UnboundedSender<Option<PumpItem>>,
        replies: Mutex<VecDeque<Vec<Option<PumpItem>>>>,
        sent: Mutex<Vec<Sent>>,
        /// Whether the turn guard was held at each delivery — literally
        /// `is_reapable`'s check, which is why this pins Contract 1.
        guard_held: Mutex<Vec<bool>>,
        session_override: Mutex<Option<SessionOverride>>,
        /// Writes that never resolve — the agent is alive but has stopped
        /// draining its stdin, which is exactly what `StdioTransport::send`'s
        /// blocking `write_all` does in that situation.
        wedged: Mutex<Vec<Sent>>,
    }

    /// The id `FakeSession::prompt` hands back, so scripted responses can
    /// correlate with the turn (and a different id can prove they don't).
    const FAKE_PROMPT_ID: u64 = 1;

    impl FakeSession {
        fn new(replies: Vec<Vec<Option<PumpItem>>>) -> Self {
            let (feed, rx) = mpsc::unbounded_channel();
            Self {
                turn_lock: tokio::sync::Mutex::new(()),
                inbound: tokio::sync::Mutex::new(rx),
                feed,
                replies: Mutex::new(replies.into()),
                sent: Mutex::new(Vec::new()),
                guard_held: Mutex::new(Vec::new()),
                session_override: Mutex::new(None),
                wedged: Mutex::new(Vec::new()),
            }
        }

        /// Make the given writes hang forever instead of completing.
        fn wedging(self, writes: Vec<Sent>) -> Self {
            *self.wedged.lock().unwrap() = writes;
            self
        }

        /// Record what we sent, then let the agent respond to it. Hangs forever
        /// first if this write is one of the wedged ones.
        async fn record(&self, s: Sent) {
            if self.wedged.lock().unwrap().contains(&s) {
                std::future::pending::<()>().await;
            }
            self.sent.lock().unwrap().push(s);
            if let Some(batch) = self.replies.lock().unwrap().pop_front() {
                for item in batch {
                    let _ = self.feed.send(item);
                }
            }
        }

        fn sent(&self) -> std::sync::MutexGuard<'_, Vec<Sent>> {
            self.sent.lock().unwrap()
        }
    }

    impl AcpSessionOps for FakeSession {
        type Turn<'a> = tokio::sync::MutexGuard<'a, ()>;

        fn begin_turn(&self) -> impl Future<Output = Self::Turn<'_>> + Send {
            self.turn_lock.lock()
        }
        fn prompt(&self, text: &str) -> impl Future<Output = Result<u64, String>> + Send {
            let text = text.to_string();
            async move {
                self.record(Sent::Prompt(text)).await;
                Ok(FAKE_PROMPT_ID)
            }
        }
        async fn next_item(&self) -> Option<PumpItem> {
            self.guard_held
                .lock()
                .unwrap()
                .push(self.turn_lock.try_lock().is_err());
            match self.inbound.lock().await.recv().await {
                Some(item) => item,
                // The internal sender is never dropped: an exhausted script
                // means "the agent has nothing more to say", not "it died".
                None => std::future::pending().await,
            }
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            self.record(Sent::Cancel).await;
            Ok(())
        }
        fn answer(
            &self,
            id: &Value,
            outcome: PermissionOutcome,
        ) -> impl Future<Output = Result<(), String>> + Send {
            let id = id.clone();
            async move {
                self.record(Sent::Answer { id, outcome }).await;
                Ok(())
            }
        }
        fn refuse(&self, id: &Value) -> impl Future<Output = Result<(), String>> + Send {
            let id = id.clone();
            async move {
                self.record(Sent::Refused(id)).await;
                Ok(())
            }
        }
        fn permission_override(&self) -> Option<SessionOverride> {
            self.session_override.lock().unwrap().clone()
        }
        fn remember_override(&self, ov: SessionOverride) {
            *self.session_override.lock().unwrap() = Some(ov);
        }
    }

    fn text_update(text: &str) -> Option<PumpItem> {
        Some(PumpItem::Event(ClientEvent::Update(SessionNotification {
            session_id: "s1".to_string(),
            update: SessionUpdate::AgentMessageChunk {
                content: TextContent {
                    text: text.to_string(),
                },
            },
        })))
    }

    fn stop(reason: &str) -> Option<PumpItem> {
        Some(PumpItem::Response {
            id: FAKE_PROMPT_ID,
            result: Ok(json!({ "stopReason": reason })),
        })
    }

    fn allow_deny_options() -> Vec<PermissionOptionWire> {
        vec![
            PermissionOptionWire {
                option_id: "a1".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            },
            PermissionOptionWire {
                option_id: "d1".into(),
                name: "Deny".into(),
                kind: "reject_once".into(),
            },
        ]
    }

    fn permission_request(id: Value, kind: &str) -> Option<PumpItem> {
        Some(PumpItem::Event(ClientEvent::Inbound(
            InboundRequest::RequestPermission {
                id,
                params: RequestPermissionParams {
                    session_id: "s1".into(),
                    tool_call: ToolCallWire {
                        tool_call_id: "t1".into(),
                        title: "Edit src/main.rs".into(),
                        kind: kind.into(),
                        ..Default::default()
                    },
                    options: allow_deny_options(),
                },
            },
        )))
    }

    /// The test's side of a live run: press Stop, or answer a parked prompt.
    struct Ctl {
        kill: mpsc::UnboundedSender<()>,
        answers: mpsc::UnboundedSender<PermissionAnswer>,
    }

    impl Ctl {
        fn stop(&self) {
            let _ = self.kill.send(());
        }
        fn answer(&self, request_id: &str, choice: PermissionChoice) {
            // `option_id: None` here — these tests exercise the turn loop's
            // handling of a choice generically; `apply_answer`'s
            // `pick_option` fallback reproduces exactly today's selection
            // when no exact option id is supplied. The exact-id path (Task
            // 11 review, Important 5) has its own dedicated test.
            let _ = self.answers.send(PermissionAnswer {
                request_id: request_id.to_string(),
                choice,
                option_id: None,
            });
        }
    }

    /// Drive one turn against a scripted session, collecting every emitted
    /// event. `on_step` sees each event as it is emitted and injects a Stop
    /// press or a permission answer through `Ctl` at a deterministic point (the
    /// loop parks/continues immediately after the callback returns, and nothing
    /// else is ready at that moment).
    ///
    /// The turn is wrapped in a 1s timeout, which hardens the TESTS and not just
    /// their assertions: `cargo test` has no per-test timeout, so a regression
    /// that leaves an agent request unanswered — the exact failure mode that
    /// blocks a real agent forever — must fail by assertion in ~1s rather than
    /// hang CI.
    /// Both bounds in milliseconds, so every timeout under test fires far inside
    /// `drive`'s own 1s harness rather than after the production 5s/30s.
    const TEST_TIMEOUTS: TurnTimeouts = TurnTimeouts {
        send: Duration::from_millis(20),
        cancel_grace: Duration::from_millis(40),
    };

    fn drive(
        session: &FakeSession,
        policy: AcpPermissionPolicy,
        timeouts: TurnTimeouts,
        mut on_step: impl FnMut(&RunEvent, &Ctl) + Send,
    ) -> (TurnOutcome, Vec<RunEvent>) {
        let (kill, mut kill_rx) = mpsc::unbounded_channel::<()>();
        let (answers, mut answer_rx) = mpsc::unbounded_channel::<PermissionAnswer>();
        let ctl = Ctl { kill, answers };
        let mut events: Vec<RunEvent> = Vec::new();
        let outcome = {
            let mut on_event = |e: RunEvent| {
                on_step(&e, &ctl);
                events.push(e);
            };
            // Built INSIDE the runtime: `tokio::time::timeout` needs a reactor
            // at construction, not just when polled.
            block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(1),
                    run_acp_turn(
                        session,
                        "do the thing",
                        policy,
                        &mut kill_rx,
                        &mut answer_rx,
                        &mut on_event,
                        timeouts,
                    ),
                )
                .await
            })
        };
        let outcome = outcome
            .expect("the turn must finish, not hang — an agent left waiting on us never replies");
        (outcome, events)
    }

    #[test]
    fn the_turn_guard_is_held_for_the_whole_prompt_round_trip() {
        // CONTRACT 1 (Task 7, not compile-enforced). `acp_session::is_reapable`
        // spares a session whose `turn_lock` is held; the fake performs exactly
        // that `try_lock` check on every frame it delivers. If the driver ever
        // released the guard early, an idle-expired session would be reaped
        // MID-TURN and its agent SIGTERM'd mid-edit (600s default timeout vs an
        // 11-minute refactor) — half-applied changes on the user's disk.
        let session = FakeSession::new(vec![vec![
            text_update("working"),
            text_update("still working"),
            stop("completed"),
        ]]);
        let (outcome, _events) =
            drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));

        let held = session.guard_held.lock().unwrap();
        assert!(
            !held.is_empty(),
            "the turn must have pumped at least once, or this proves nothing"
        );
        assert!(
            held.iter().all(|h| *h),
            "the turn guard must be held at EVERY frame of the round trip, including the \
             terminal stopReason — observed {held:?}"
        );
    }

    #[test]
    fn every_update_is_dual_emitted_and_the_turn_ends_with_a_stop_reason() {
        let session = FakeSession::new(vec![vec![text_update("hello"), stop("completed")]]);
        let (outcome, events) = drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], RunEvent::Text { text } if text == "hello"));
        // Dual emission's other half is `emit_run_event`; every event the loop
        // produces must have a text line or it vanishes from the buffer/File sink.
        assert!(events.iter().all(|e| render_line(e).is_some()));
        assert_eq!(*session.sent(), vec![Sent::Prompt("do the thing".into())]);
    }

    #[test]
    fn a_response_to_another_request_never_ends_this_turn() {
        // Only the prompt's own id is terminal; a stray response must be skipped.
        let session = FakeSession::new(vec![vec![
            Some(PumpItem::Response {
                id: FAKE_PROMPT_ID + 99,
                result: Ok(json!({ "stopReason": "completed" })),
            }),
            text_update("still going"),
            stop("cancelled"),
        ]]);
        let (outcome, events) = drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        assert!(
            matches!(outcome, TurnOutcome::Ended(StopReason::Cancelled)),
            "the turn must end on ITS OWN prompt response, got {outcome:?}"
        );
        assert_eq!(events.len(), 1, "the update after it must still stream");
    }

    #[test]
    fn an_auto_approved_permission_is_answered_with_an_offered_option_and_reported() {
        let session = FakeSession::new(vec![
            vec![permission_request(json!("a1"), "edit")],
            vec![stop("completed")],
        ]);
        let (outcome, events) = drive(
            &session,
            AcpPermissionPolicy::AutoEdits,
            TEST_TIMEOUTS,
            |_, _| {},
        );
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));
        match &events[0] {
            RunEvent::PermissionResolved {
                outcome, automatic, ..
            } => {
                assert_eq!(outcome, "allow");
                // The user must be able to see, after the fact, what was allowed
                // on their behalf (DESIGN §8).
                assert!(*automatic);
            }
            e => panic!("expected an automatic PermissionResolved, got {e:?}"),
        }
        assert_eq!(
            session.sent()[1],
            Sent::Answer {
                id: json!("a1"),
                // Never a persistent grant: `pick_option` prefers `allow_once`.
                outcome: PermissionOutcome::Selected {
                    option_id: "a1".into()
                },
            }
        );
    }

    #[test]
    fn an_ask_policy_parks_the_prompt_and_the_user_answer_resolves_it() {
        let session = FakeSession::new(vec![
            vec![permission_request(json!(7), "execute")],
            vec![stop("completed")],
        ]);
        // Answer the moment the prompt is surfaced — the loop parks it right
        // after the callback returns and picks the answer up next round.
        let (outcome, events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if let RunEvent::PermissionRequest { request_id, .. } = e {
                    ctl.answer(request_id, PermissionChoice::DenyOnce);
                }
            },
        );
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));

        match &events[0] {
            RunEvent::PermissionRequest {
                title,
                tool_call_id,
                options,
                ..
            } => {
                assert_eq!(title, "Edit src/main.rs");
                assert_eq!(tool_call_id.as_deref(), Some("t1"));
                assert_eq!(options.len(), 2);
            }
            e => panic!("expected PermissionRequest, got {e:?}"),
        }
        match &events[1] {
            RunEvent::PermissionResolved {
                outcome, automatic, ..
            } => {
                assert_eq!(outcome, "deny");
                assert!(!automatic, "a user answer is never automatic");
            }
            e => panic!("expected PermissionResolved, got {e:?}"),
        }
        assert_eq!(
            session.sent()[1],
            Sent::Answer {
                id: json!(7),
                outcome: PermissionOutcome::Selected {
                    option_id: "d1".into()
                },
            }
        );
    }

    #[test]
    fn an_always_answer_is_recorded_as_a_session_override_not_a_grant_to_the_agent() {
        for (choice, expect) in [
            (
                PermissionChoice::AllowAlways,
                SessionOverride {
                    // Per KIND, and never `allow_all`: the user answered a
                    // question about `execute`, not about everything.
                    allow_all: false,
                    allowed_kinds: vec!["execute".to_string()],
                    denied_kinds: vec![],
                },
            ),
            (
                PermissionChoice::DenyAlways,
                SessionOverride {
                    allow_all: false,
                    allowed_kinds: vec![],
                    denied_kinds: vec!["execute".to_string()],
                },
            ),
        ] {
            let session = FakeSession::new(vec![
                vec![permission_request(json!(7), "execute")],
                vec![stop("completed")],
            ]);
            drive(
                &session,
                AcpPermissionPolicy::Ask,
                TEST_TIMEOUTS,
                |e, ctl| {
                    if let RunEvent::PermissionRequest { request_id, .. } = e {
                        ctl.answer(request_id, choice);
                    }
                },
            );

            assert_eq!(
                session.session_override.lock().unwrap().clone(),
                Some(expect),
                "an *_always answer must persist as OUR session override for {choice:?}"
            );
            // …and the agent still gets the ONE-SHOT option, so it never holds a
            // persistent grant we can't revoke by ending the session.
            let expected_option = if choice.allows() { "a1" } else { "d1" };
            assert_eq!(
                session.sent()[1],
                Sent::Answer {
                    id: json!(7),
                    outcome: PermissionOutcome::Selected {
                        option_id: expected_option.into()
                    },
                }
            );
        }
    }

    #[test]
    fn an_always_allow_on_one_kind_never_auto_approves_a_different_kind() {
        // The escalation this design forbids, driven end to end through the
        // turn loop: the user clicks "always" on a benign `read` at minute two,
        // and the agent asks to `execute` something at minute eight. The second
        // prompt must still reach the user. If the driver recorded `allow_all`,
        // `decide` would auto-approve it with nothing but a `→ allow
        // (automatic)` line in the buffer to show for it.
        // Scripted so BOTH the correct and the escalating behaviour terminate
        // normally: the discriminator is the event list, not a timeout.
        let session = FakeSession::new(vec![
            vec![permission_request(json!(1), "read")],
            vec![permission_request(json!(2), "execute")],
            vec![stop("completed")],
        ]);
        let (outcome, events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if let RunEvent::PermissionRequest { request_id, .. } = e {
                    if request_id == "perm-1" {
                        ctl.answer(request_id, PermissionChoice::AllowAlways);
                    } else {
                        ctl.answer(request_id, PermissionChoice::DenyOnce);
                    }
                }
            },
        );
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));

        let kinds: Vec<&RunEvent> = events.iter().collect();
        assert!(
            matches!(kinds[0], RunEvent::PermissionRequest { request_id, .. } if request_id == "perm-1"),
            "the read prompt must be surfaced: {kinds:?}"
        );
        assert!(
            matches!(
                kinds[1],
                RunEvent::PermissionResolved {
                    automatic: false,
                    ..
                }
            ),
            "the user's own answer to it is never automatic: {kinds:?}"
        );
        // THE POINT: the `execute` request is ASKED, not auto-allowed. Under the
        // escalating behaviour this slot is instead a `PermissionResolved {
        // automatic: true }` the user never saw coming.
        match kinds[2] {
            RunEvent::PermissionRequest { request_id, .. } => assert_eq!(request_id, "perm-2"),
            e => panic!(
                "an always-allow for `read` must not auto-approve `execute` — expected a second \
                 PermissionRequest, got {e:?}"
            ),
        }
        assert_eq!(
            session
                .session_override
                .lock()
                .unwrap()
                .clone()
                .unwrap()
                .allowed_kinds,
            vec!["read".to_string()],
            "the override must record the kind the user actually answered for"
        );
    }

    #[test]
    fn an_unsupported_agent_request_is_always_answered() {
        // client.rs's doctrine: an unanswered request hangs the agent's turn
        // forever. We declared no fs/terminal capabilities, so this shouldn't
        // happen — but it must never be dropped silently.
        let session = FakeSession::new(vec![
            vec![Some(PumpItem::Event(ClientEvent::Inbound(
                InboundRequest::Unsupported {
                    id: json!(42),
                    method: "fs/write_text_file".to_string(),
                },
            )))],
            vec![stop("completed")],
        ]);
        let (outcome, _events) =
            drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Completed)));
        assert_eq!(session.sent()[1], Sent::Refused(json!(42)));
    }

    #[test]
    fn stopping_a_run_cancels_the_turn_and_resolves_every_parked_prompt() {
        let session = FakeSession::new(vec![
            vec![permission_request(json!("p9"), "execute")],
            vec![stop("cancelled")],
        ]);
        // Stop is the ONLY escape hatch from a parked prompt (there is no
        // auto-deny timeout — DESIGN §8).
        let (outcome, events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if matches!(e, RunEvent::PermissionRequest { .. }) {
                    ctl.stop();
                }
            },
        );
        // Cancel ends the TURN, not the session: `Stopped`, session stays warm.
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Cancelled)));
        assert_eq!(
            stop_reason_to_status(StopReason::Cancelled),
            RunStatus::Stopped
        );

        let sent = session.sent();
        assert_eq!(sent[1], Sent::Cancel);
        assert_eq!(
            sent[2],
            Sent::Answer {
                id: json!("p9"),
                outcome: PermissionOutcome::Cancelled,
            },
            "every parked prompt must be resolved as cancelled — an unanswered \
             one leaks a responder and blocks the agent forever"
        );
        assert!(matches!(
            &events[1],
            RunEvent::PermissionResolved { outcome, .. } if outcome == "cancelled"
        ));
    }

    #[test]
    fn a_cancel_the_agent_never_acknowledges_is_bounded() {
        // Stop was already pressed, so there is no second escape hatch, and the
        // idle reaper deliberately spares a session with a turn in flight — the
        // turn guard would otherwise be held for the life of the app.
        // The agent goes quiet after one line and never acknowledges the cancel.
        // `drive`'s own 1s bound is what turns a regression here into a failed
        // assertion rather than a hung CI run; `TEST_TIMEOUTS.cancel_grace` is
        // the thing actually under test and stays far inside it.
        let session = FakeSession::new(vec![vec![text_update("working")]]);
        let (outcome, _events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if matches!(e, RunEvent::Text { .. }) {
                    ctl.stop();
                }
            },
        );
        assert!(matches!(outcome, TurnOutcome::CancelTimedOut));
        assert_eq!(session.sent()[1], Sent::Cancel);
    }

    #[test]
    fn a_wedged_stdin_cannot_swallow_the_cancel_and_pin_the_turn_forever() {
        // The agent is alive but has stopped draining its stdin, so our
        // `session/cancel` write never completes (`StdioTransport::send` does a
        // blocking `write_all` — this is why `send_close_courtesy` exists). An
        // unbounded write there parks the loop OUTSIDE `select!`, so `kill_rx`
        // is no longer polled, the cancel grace never starts, the turn guard
        // stays held, the reaper spares the session, and the run sits at
        // `Running` for the life of the app with the user's one escape hatch
        // already spent.
        let session =
            FakeSession::new(vec![vec![text_update("working")]]).wedging(vec![Sent::Cancel]);
        let (outcome, _events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if matches!(e, RunEvent::Text { .. }) {
                    ctl.stop();
                }
            },
        );
        assert!(
            matches!(outcome, TurnOutcome::CancelTimedOut),
            "a write the agent never drains must not outlive its own bound, got {outcome:?}"
        );
        // The wedged write never landed — proving the bound fired rather than
        // the fake simply completing it.
        assert!(
            !session.sent().contains(&Sent::Cancel),
            "the cancel write must have been abandoned at its timeout"
        );
    }

    #[test]
    fn a_wedged_stdin_on_the_prompt_itself_fails_instead_of_hanging() {
        // Same hazard one step earlier: an unbounded `prompt` never reaches the
        // select loop at all, so Stop could not even be read.
        let session = FakeSession::new(vec![vec![stop("completed")]])
            .wedging(vec![Sent::Prompt("do the thing".to_string())]);
        let (outcome, events) = drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        match &outcome {
            TurnOutcome::Crashed(msg) => assert!(
                msg.contains("stopped reading its input"),
                "the message must name the real cause: {msg}"
            ),
            o => panic!("expected Crashed, got {o:?}"),
        }
        // THE POINT: an abandoned write takes the SESSION with it. `timeout`
        // drops the in-flight `write_all`, so bytes the pipe already accepted
        // stay there and the next run's frame would land after a partial one —
        // and `acquire`'s `is_alive` (a `try_wait`) cannot see that, so a
        // retained session would desync every later run on this agent.
        assert!(
            turn_result(outcome).drop_session,
            "a turn that abandoned a write mid-frame must NOT leave its session warm"
        );
        assert!(events.is_empty());
    }

    #[test]
    fn a_failed_cancel_write_drops_the_session_even_with_a_frame_already_in_flight() {
        // The race the previous mechanism lost every single time. Collapsing the
        // cancel deadline and re-entering `select!` does NOT reliably take the
        // timer arm: a `sleep_until` already past still returns `Pending` on its
        // first poll, while `next_item()` is immediately `Ready` if the agent has
        // queued a frame — the likely case, since an agent that has stopped
        // draining stdin usually keeps writing stdout. The pump arm then takes
        // `Ended`, which KEEPS the session, carrying the half-written
        // `session/cancel` frame that `acquire`'s `try_wait` cannot see.
        //
        // Script: a parked prompt, Stop, a wedged `Sent::Cancel`, and the
        // parked-prompt cancel-answer (which is NOT wedged) pulling the agent's
        // terminal frame into the queue — so a frame is guaranteed to be waiting
        // at the exact moment the failed write returns.
        let session = FakeSession::new(vec![
            vec![permission_request(json!("p9"), "execute")],
            vec![stop("cancelled")],
        ])
        .wedging(vec![Sent::Cancel]);
        let (outcome, _events) = drive(
            &session,
            AcpPermissionPolicy::Ask,
            TEST_TIMEOUTS,
            |e, ctl| {
                if matches!(e, RunEvent::PermissionRequest { .. }) {
                    ctl.stop();
                }
            },
        );
        assert!(
            matches!(outcome, TurnOutcome::CancelTimedOut),
            "an abandoned cancel write must end the turn there and then, not hand the \
             decision back to whatever the agent happened to queue: {outcome:?}"
        );
        let result = turn_result(outcome);
        assert!(
            result.drop_session,
            "session kept warm after an abandoned cancel write — it still holds a \
             half-written frame, and `is_alive` (a try_wait) will happily reuse it"
        );
        // …and it is still Stop-shaped: the user asked for this.
        assert_eq!(result.status, RunStatus::Stopped);
    }

    #[test]
    fn a_wedged_stdin_on_a_mid_turn_permission_answer_also_drops_the_session() {
        // The same discarded-error path, mid-turn: the agent asked, policy
        // auto-allowed, and our answer never made it out of the pipe.
        let session = FakeSession::new(vec![
            vec![permission_request(json!("a1"), "edit")],
            vec![stop("completed")],
        ])
        .wedging(vec![Sent::Answer {
            id: json!("a1"),
            outcome: PermissionOutcome::Selected {
                option_id: "a1".into(),
            },
        }]);
        let (outcome, events) = drive(
            &session,
            AcpPermissionPolicy::AutoEdits,
            TEST_TIMEOUTS,
            |_, _| {},
        );
        assert!(
            matches!(outcome, TurnOutcome::Crashed(_)),
            "an abandoned mid-turn write is a broken session, not a protocol failure: {outcome:?}"
        );
        assert!(turn_result(outcome).drop_session);
        // …and it is never reported as a resolution the agent never received.
        assert!(
            events.is_empty(),
            "a write that never landed must not emit `→ allow (automatic)`: {events:?}"
        );
    }

    #[test]
    fn only_a_turn_that_left_the_session_usable_keeps_it_warm() {
        // The whole drop/keep matrix in one place, because `is_alive` cannot
        // second-guess any of it.
        for (outcome, keep, why) in [
            (
                TurnOutcome::Ended(StopReason::Completed),
                true,
                "a completed turn leaves a healthy session — that is the point of warm sessions",
            ),
            (
                TurnOutcome::Ended(StopReason::Cancelled),
                true,
                "cancel ends the TURN, not the session",
            ),
            (
                TurnOutcome::Failed("malformed result".into()),
                true,
                "a protocol-level failure leaves the stream intact",
            ),
            (
                TurnOutcome::Crashed("the agent exited".into()),
                false,
                "a dead child, or a write abandoned mid-frame, must never be reused",
            ),
            (
                TurnOutcome::CancelTimedOut,
                false,
                "an agent that ignored session/cancel is alive and wedged",
            ),
        ] {
            let result = turn_result(outcome);
            assert_eq!(!result.drop_session, keep, "{why}");
        }
        // A stopped run still reads as Stopped even though its session is dropped.
        assert_eq!(
            turn_result(TurnOutcome::CancelTimedOut).status,
            RunStatus::Stopped
        );
    }

    #[test]
    fn a_child_that_dies_mid_turn_fails_the_run_and_is_never_retried() {
        let session = FakeSession::new(vec![vec![text_update("halfway through"), None]]);
        let (outcome, events) = drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        match outcome {
            TurnOutcome::Crashed(msg) => {
                assert!(msg.contains("exited"), "message must name the cause: {msg}");
                assert!(
                    msg.contains("check the project"),
                    "a crash may have half-applied edits — the message must say so: {msg}"
                );
            }
            o => panic!("expected Crashed, got {o:?}"),
        }
        // Exactly one prompt: a crashed turn is NEVER auto-retried.
        assert_eq!(
            session
                .sent()
                .iter()
                .filter(|s| matches!(s, Sent::Prompt(_)))
                .count(),
            1
        );
        assert_eq!(
            events.len(),
            1,
            "output before the crash is never swallowed"
        );
    }

    #[test]
    fn an_agent_error_on_the_prompt_fails_the_run_rather_than_hanging() {
        let session = FakeSession::new(vec![vec![Some(PumpItem::Response {
            id: FAKE_PROMPT_ID,
            result: Err(JsonRpcError {
                code: -32602,
                message: "Invalid params".to_string(),
            }),
        })]]);
        let (outcome, _events) =
            drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        match outcome {
            TurnOutcome::Failed(msg) => assert!(msg.contains("Invalid params")),
            o => panic!("expected Failed, got {o:?}"),
        }
    }

    #[test]
    fn a_missing_stop_reason_is_not_fatal() {
        let session = FakeSession::new(vec![vec![Some(PumpItem::Response {
            id: FAKE_PROMPT_ID,
            result: Ok(json!({})),
        })]]);
        let (outcome, _events) =
            drive(&session, AcpPermissionPolicy::Ask, TEST_TIMEOUTS, |_, _| {});
        assert!(matches!(outcome, TurnOutcome::Ended(StopReason::Other)));
    }

    #[test]
    fn permission_response_body_nests_the_outcome_the_way_acp_expects() {
        let body = permission_response_body(&PermissionOutcome::Selected {
            option_id: "a1".into(),
        });
        assert_eq!(body["outcome"]["outcome"], json!("selected"));
        assert_eq!(body["outcome"]["optionId"], json!("a1"));
        let body = permission_response_body(&PermissionOutcome::Cancelled);
        assert_eq!(body["outcome"]["outcome"], json!("cancelled"));
    }

    #[test]
    fn respond_permission_reaches_the_turn_loop_and_errors_when_it_cannot() {
        let mgr = AgentRunManager::new();
        let (tx, _rx) = mpsc::unbounded_channel::<()>();
        {
            let mut runs = mgr.runs.lock().unwrap();
            runs.insert(
                "r1".to_string(),
                AgentRun {
                    agent_id: "coder".into(),
                    agent_name: "Coder".into(),
                    project_path: String::new(),
                    status: RunStatus::Running,
                    started_at: Local::now(),
                    output: String::new(),
                    instruction: String::new(),
                    output_file: None,
                    kill_tx: Some(tx),
                    session_id: None,
                    permission_tx: None,
                },
            );
        }
        // A run with no ACP turn loop (every raw CLI / remote run) must refuse
        // rather than silently swallow the answer.
        assert!(mgr
            .respond_permission("r1", "perm-1", PermissionChoice::AllowOnce, None)
            .is_err());
        assert!(mgr
            .respond_permission("nope", "perm-1", PermissionChoice::AllowOnce, None)
            .is_err());

        let (ptx, mut prx) = mpsc::unbounded_channel::<PermissionAnswer>();
        mgr.set_permission_sender("r1", ptx);
        mgr.respond_permission(
            "r1",
            "perm-2",
            PermissionChoice::DenyAlways,
            Some("d1".to_string()),
        )
        .expect("an ACP run's answer must reach its turn loop");
        let got = prx.try_recv().expect("the answer must arrive");
        assert_eq!(got.request_id, "perm-2");
        assert_eq!(got.choice, PermissionChoice::DenyAlways);
        assert_eq!(got.option_id.as_deref(), Some("d1"));

        // Finalizing clears the channel: nothing can be answered after the run
        // is terminal.
        mgr.set_status("r1", RunStatus::Finished { code: 0 });
        assert!(mgr
            .respond_permission("r1", "perm-2", PermissionChoice::AllowOnce, None)
            .is_err());
    }

    /// Task 11 review, Important 5: two options of the SAME once-kind must
    /// not collapse to "whichever comes first" — the exact option the user
    /// clicked must be the one the agent is answered with.
    #[test]
    fn apply_answer_replies_with_the_exact_option_clicked_not_just_a_kind_match() {
        let options = vec![
            PermissionOptionWire {
                option_id: "allow-plain".into(),
                name: "Allow once".into(),
                kind: "allow_once".into(),
            },
            PermissionOptionWire {
                option_id: "allow-dir".into(),
                name: "Allow for this directory".into(),
                kind: "allow_once".into(),
            },
        ];
        let parked = ParkedPermission {
            id: json!("req-1"),
            tool_kind: "edit".into(),
            options: options.clone(),
        };
        let mut events = Vec::new();
        let mut on_event = |e: RunEvent| events.push(e);

        // The user clicked the SECOND same-kind option, not the first —
        // `pick_option` alone would always resolve to `allow-plain`.
        let answer = PermissionAnswer {
            request_id: "req-1".to_string(),
            choice: PermissionChoice::AllowOnce,
            option_id: Some("allow-dir".to_string()),
        };
        let session = FakeSession::new(vec![]);
        block_on(apply_answer(
            &session,
            &answer,
            &parked,
            &mut on_event,
            Duration::from_millis(200),
        ))
        .expect("answering must succeed");
        assert_eq!(
            session.sent().last(),
            Some(&Sent::Answer {
                id: json!("req-1"),
                outcome: PermissionOutcome::Selected {
                    option_id: "allow-dir".to_string(),
                },
            }),
            "must reply with the EXACT option the user clicked, not the first same-kind option"
        );

        // A stale/unrecognized option id (e.g. racing a resolved request)
        // falls back to the existing kind-based selection rather than
        // silently failing.
        let stale_answer = PermissionAnswer {
            request_id: "req-1".to_string(),
            choice: PermissionChoice::AllowOnce,
            option_id: Some("no-longer-exists".to_string()),
        };
        let session2 = FakeSession::new(vec![]);
        block_on(apply_answer(
            &session2,
            &stale_answer,
            &parked,
            &mut on_event,
            Duration::from_millis(200),
        ))
        .expect("answering must still succeed via the fallback");
        assert_eq!(
            session2.sent().last(),
            Some(&Sent::Answer {
                id: json!("req-1"),
                outcome: PermissionOutcome::Selected {
                    option_id: "allow-plain".to_string(),
                },
            }),
            "an unrecognized option id falls back to pick_option's kind-based selection"
        );

        // An `*_always` click is UNCHANGED: it must still reply with the
        // one-shot option, NEVER the agent's own persistent option — even
        // though `answer.option_id` names the (nonexistent, once-only) always
        // option here, proving the always-branch never even consults it.
        let always_answer = PermissionAnswer {
            request_id: "req-1".to_string(),
            choice: PermissionChoice::AllowAlways,
            option_id: Some("allow-dir".to_string()),
        };
        let session3 = FakeSession::new(vec![]);
        block_on(apply_answer(
            &session3,
            &always_answer,
            &parked,
            &mut on_event,
            Duration::from_millis(200),
        ))
        .expect("answering must succeed");
        assert_eq!(
            session3.sent().last(),
            Some(&Sent::Answer {
                id: json!("req-1"),
                outcome: PermissionOutcome::Selected {
                    option_id: "allow-plain".to_string(),
                },
            }),
            "an *_always* click must still reply with pick_option's one-shot choice, never the clicked option id verbatim"
        );
    }
}
