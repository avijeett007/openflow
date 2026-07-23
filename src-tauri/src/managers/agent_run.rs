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
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::settings::{AgentCliType, AgentDefinition, AgentOutputSink, PromptDelivery};

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
        }
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
                },
            );
        }

        let manager = Arc::clone(self);
        let app = app.clone();
        let binary = agent.binary_path.clone();
        let run_id_task = run_id.clone();

        // Detached task: spawn, stream, finalize. Uses the app's async runtime.
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
async fn terminate_child(child: &mut tokio::process::Child) {
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
    for tok in tokenize(command_template) {
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
fn tokenize(s: &str) -> Vec<String> {
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
            tokenize("run \"two words\" --flag 'single quoted'"),
            vec!["run", "two words", "--flag", "single quoted"]
        );
    }

    #[test]
    fn tokenize_empty_quotes_produce_empty_arg() {
        assert_eq!(tokenize("--name \"\""), vec!["--name", ""]);
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
}
