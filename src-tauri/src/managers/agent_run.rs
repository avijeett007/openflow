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

        let mut cmd = Command::new(&binary);
        cmd.args(&argv)
            .current_dir(&cwd)
            .env("PATH", baseline_path(std::env::var("PATH").ok().as_deref()))
            .env(
                "SHELL",
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
            )
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }

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

/// Baseline PATH: the caller's PATH plus the standard macOS bin dirs + Homebrew,
/// so a GUI-spawned detached process (which can inherit a stripped PATH) can
/// still resolve tools the agent shells out to. Mirrors Agent OS's `agentEnv`.
pub fn baseline_path(existing: Option<&str>) -> String {
    let baseline = [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ];
    let mut parts: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in existing
        .unwrap_or("")
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .chain(baseline.iter().map(|s| s.to_string()))
    {
        if seen.insert(p.clone()) {
            parts.push(p);
        }
    }
    parts.join(":")
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
}
