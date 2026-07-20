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
    /// A machine-readable hint the frontend maps to an actionable, localized
    /// message (e.g. a broken Codex install). `None` for ordinary output.
    pub hint: Option<AgentBinaryHint>,
}

/// Classified, actionable failure modes surfaced by `test_agent_binary`. The
/// frontend renders a localized fix for each instead of a raw spawn stack.
#[derive(Debug, Clone, Copy, Serialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentBinaryHint {
    /// Codex's Node launcher can't find its vendored native binary — the
    /// per-platform optional dependency (`@openai/codex-<os>-<arch>`) is
    /// missing/partial. Fix: reinstall Codex.
    CodexVendorMissing,
}

/// Classify a Test run's combined output for a known, actionable failure. The
/// `@openai/codex` npm package is a thin Node launcher; the real native binary
/// ships in a separate per-platform optional-dependency package. On a partial
/// install (npm skips optional deps offline / with `--omit=optional`, or a
/// Homebrew node hiccup) the launcher fails to resolve it — surfacing either a
/// raw `spawn <…>/vendor/<triple>/…/codex ENOENT` (older launcher) or a
/// `Missing optional dependency @openai/codex-<os>-<arch>` error (newer one).
/// Both mean the same thing and are NOT fixable from our spawn env.
pub fn classify_binary_output(output: &str) -> Option<AgentBinaryHint> {
    let lower = output.to_ascii_lowercase();
    let enoent_on_vendored_codex =
        lower.contains("enoent") && lower.contains("codex") && lower.contains("vendor");
    let missing_optional_codex =
        lower.contains("missing optional dependency") && lower.contains("codex");
    if enoent_on_vendored_codex || missing_optional_codex {
        Some(AgentBinaryHint::CodexVendorMissing)
    } else {
        None
    }
}

/// Log a warning when a resolved `codex` binary's native vendor payload is
/// provably missing (GAP 2), so handy.log records it at detect time. Returns
/// the path UNCHANGED — detect still succeeds because the launcher path IS
/// resolved; the actionable fix is surfaced to the user by the Test button
/// (`test_agent_binary`), and a real run's classifier is the final safety net.
/// Non-codex types and any indeterminate result are a silent no-op.
fn note_codex_vendor(cli_type: AgentCliType, path: String) -> String {
    use crate::managers::agent_run::{codex_static_vendor_hint, CodexVendorStatus};
    if cli_type == AgentCliType::Codex
        && codex_static_vendor_hint(&path) == CodexVendorStatus::Missing
    {
        log::warn!(
            "detect_agent_binary: resolved codex at '{path}' but its native vendor \
             payload (@openai/codex-<os>-<arch>) is provably missing — reinstall Codex"
        );
    }
    path
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

/// Resolve a CLI agent's binary path (`which`-style). Searches, in order: the
/// process PATH, then an explicit baseline of tool dirs that a GUI-launched
/// app's stripped PATH omits — on macOS Homebrew on either arch, `~/.local/bin`,
/// bun/cargo/volta/deno, every nvm node version (this is why detection found
/// nothing on the user's installed app while `which` worked in Terminal); on
/// Windows `%APPDATA%\npm`, `%LOCALAPPDATA%\Programs`, bun/volta/cargo/scoop
/// and `%ProgramFiles%\nodejs`, probing PATHEXT-style candidate names
/// (`claude.cmd` etc.). As a final fallback it consults the user's login shell
/// (`<shell> -lc 'command -v <name>'`; `where.exe` on Windows) so custom
/// profile PATHs (rbenv/asdf/fnm) resolve exactly as in their Terminal.
/// Returns the absolute path, or an error if not found.
#[tauri::command]
#[specta::specta]
pub async fn detect_agent_binary(cli_type: AgentCliType) -> Result<String, String> {
    use crate::managers::agent_run;

    let name = default_cli_binary_name(cli_type).ok_or_else(|| {
        "This agent type has no default binary; set the path manually".to_string()
    })?;

    let windows = cfg!(windows);
    let env_get = |k: &str| std::env::var(k).ok();
    let nvm = if windows {
        Vec::new()
    } else {
        std::env::var("HOME")
            .ok()
            .map(|h| agent_run::nvm_node_bin_dirs(&h))
            .unwrap_or_default()
    };
    let dirs = agent_run::detect_search_dirs(
        std::env::var("PATH").ok().as_deref(),
        windows,
        &env_get,
        &nvm,
    );
    let file_names =
        agent_run::candidate_file_names(name, windows, std::env::var("PATHEXT").ok().as_deref());
    for dir in &dirs {
        for file_name in &file_names {
            let candidate = Path::new(dir).join(file_name);
            if agent_run::is_executable_file(&candidate) {
                return Ok(note_codex_vendor(
                    cli_type,
                    candidate.to_string_lossy().to_string(),
                ));
            }
        }
    }

    // Nothing in the known dirs — ask the user's login shell (where.exe on
    // Windows), which sees any custom PATH the user's profile exports.
    if let Some(path) = agent_run::login_shell_which(name).await {
        return Ok(note_codex_vendor(cli_type, path));
    }

    Err(format!(
        "'{name}' not found on PATH or in the standard tool directories"
    ))
}

/// Run `<binary> --version` (best-effort) and report success + captured output.
/// Never fails the command itself on a non-zero exit — the caller inspects `ok`.
#[tauri::command]
#[specta::specta]
pub async fn test_agent_binary(binary_path: String) -> Result<AgentBinaryTest, String> {
    if binary_path.trim().is_empty() {
        return Err("Binary path is empty".to_string());
    }
    // On Windows a `.cmd`/`.bat` npm shim must be launched via `cmd.exe /C`
    // (CreateProcess can't exec batch scripts) — the same plan the run
    // pipeline uses, so Test and Run agree.
    let plan = crate::managers::agent_run::spawn_plan(&binary_path, cfg!(windows));
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.pre_args)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Same baseline env the run pipeline uses, so a Node/shell shim the CLI
    // wraps (e.g. `codex`) resolves its internals identically to a real run.
    crate::managers::agent_run::apply_baseline_env(&mut cmd);
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run '{binary_path} --version': {e}"))?;

    // Classify from the combined streams — a shim prints its resolution failure
    // to stderr while still exiting non-zero.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut hint = classify_binary_output(&format!("{stdout}\n{stderr}"));

    // GAP 1: `codex --version` can succeed via the JS launcher WITHOUT ever
    // touching the vendored native binary — so Test would report OK while a
    // real run fails. Proactively (and statically) prove the native payload is
    // present; only a PROVABLY-missing payload downgrades the result (an
    // `Unknown` result never fabricates a warning — see `codex_static_vendor_hint`).
    if hint.is_none()
        && crate::managers::agent_run::codex_static_vendor_hint(&binary_path)
            == crate::managers::agent_run::CodexVendorStatus::Missing
    {
        hint = Some(AgentBinaryHint::CodexVendorMissing);
    }

    let mut text = stdout.trim().to_string();
    if text.is_empty() {
        text = stderr.trim().to_string();
    }
    Ok(AgentBinaryTest {
        // A proven-missing vendor payload flips Test to not-OK even when the
        // launcher's `--version` exited 0, so the UI shows the actionable fix.
        ok: output.status.success() && hint.is_none(),
        output: text,
        hint,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_older_launcher_raw_enoent() {
        // The exact shape from BLOCKERS §10b bug #2 (older codex launcher).
        let out = "Error: spawn /opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex ENOENT";
        assert_eq!(
            classify_binary_output(out),
            Some(AgentBinaryHint::CodexVendorMissing)
        );
    }

    #[test]
    fn classifies_newer_launcher_missing_optional_dep() {
        // The newer launcher's own error (reproduced by removing the platform pkg).
        let out = "Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall Codex: npm install -g @openai/codex@latest";
        assert_eq!(
            classify_binary_output(out),
            Some(AgentBinaryHint::CodexVendorMissing)
        );
    }

    #[test]
    fn does_not_misclassify_normal_version_output() {
        assert_eq!(classify_binary_output("codex-cli 0.144.5"), None);
        assert_eq!(classify_binary_output("claude 2.1.108"), None);
        // An unrelated ENOENT (not codex/vendor) is not our actionable case.
        assert_eq!(classify_binary_output("Error: spawn foo ENOENT"), None);
    }
}
