//! Flow OS increment 2 — CLI-agent commands: binary detection/testing and the
//! run registry (list / stop / clear). CLI-agent CRUD reuses the increment-1
//! `create_agent`/`update_agent`/`delete_agent` (they already take the whole
//! `AgentDefinition`; the new CLI fields round-trip through them).

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::managers::agent_run::{AgentRunInfo, AgentRunManager};
use crate::settings::{
    default_cli_binary_name, default_cli_template, AgentCliType, PromptDelivery,
};

/// Result of `test_agent_binary`: whether the binary ran and its version output.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentBinaryTest {
    pub ok: bool,
    pub output: String,
}

/// The prefilled config for a CLI agent type — one source of truth shared with
/// the frontend so its "Add CLI agent" form matches exactly what the backend
/// verified (esp. the live-tested `claude` template).
#[derive(Debug, Clone, Serialize, Type)]
pub struct CliAgentDefaults {
    pub command_template: String,
    pub prompt_via: PromptDelivery,
    /// Canonical binary name to auto-detect on PATH (`None` for `custom`).
    pub binary_name: Option<String>,
}

/// Return the prefilled `command_template` / `prompt_via` / binary name for a
/// CLI type. The frontend calls this when the user picks an agent type.
#[tauri::command]
#[specta::specta]
pub fn get_cli_agent_defaults(cli_type: AgentCliType) -> CliAgentDefaults {
    let (command_template, prompt_via) = default_cli_template(cli_type);
    CliAgentDefaults {
        command_template,
        prompt_via,
        binary_name: default_cli_binary_name(cli_type).map(|s| s.to_string()),
    }
}

/// Resolve a CLI agent's binary path by looking up its canonical name on PATH
/// (`which`-style). Returns the absolute path, or an error if not found. The
/// PATH searched is the baseline-augmented one (Homebrew etc.), matching how the
/// run will actually be spawned.
#[tauri::command]
#[specta::specta]
pub fn detect_agent_binary(cli_type: AgentCliType) -> Result<String, String> {
    let name = default_cli_binary_name(cli_type).ok_or_else(|| {
        "This agent type has no default binary; set the path manually".to_string()
    })?;

    let path = crate::managers::agent_run::baseline_path(std::env::var("PATH").ok().as_deref());
    for dir in path.split(':').filter(|s| !s.is_empty()) {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().to_string());
        }
    }
    Err(format!("'{name}' not found on PATH"))
}

/// Run `<binary> --version` (best-effort) and report success + captured output.
/// Never fails the command itself on a non-zero exit — the caller inspects `ok`.
#[tauri::command]
#[specta::specta]
pub async fn test_agent_binary(binary_path: String) -> Result<AgentBinaryTest, String> {
    if binary_path.trim().is_empty() {
        return Err("Binary path is empty".to_string());
    }
    let path = crate::managers::agent_run::baseline_path(std::env::var("PATH").ok().as_deref());
    let output = Command::new(&binary_path)
        .arg("--version")
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to run '{binary_path} --version': {e}"))?;

    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    Ok(AgentBinaryTest {
        ok: output.status.success(),
        output: text,
    })
}

/// Snapshot of all CLI agent runs (running + recent), newest first.
#[tauri::command]
#[specta::specta]
pub fn list_agent_runs(app: AppHandle) -> Vec<AgentRunInfo> {
    match app.try_state::<Arc<AgentRunManager>>() {
        Some(mgr) => mgr.list_runs(),
        None => Vec::new(),
    }
}

/// Request a stop (SIGTERM→SIGKILL) for a running CLI agent run.
#[tauri::command]
#[specta::specta]
pub fn stop_agent_run(app: AppHandle, run_id: String) -> Result<(), String> {
    let mgr = app
        .try_state::<Arc<AgentRunManager>>()
        .ok_or_else(|| "Agent run manager not initialized".to_string())?;
    mgr.stop_run(&run_id)
}

/// Drop all terminal (finished/failed/stopped) runs from the registry.
#[tauri::command]
#[specta::specta]
pub fn clear_finished_agent_runs(app: AppHandle) -> Result<(), String> {
    if let Some(mgr) = app.try_state::<Arc<AgentRunManager>>() {
        mgr.clear_finished();
    }
    Ok(())
}
