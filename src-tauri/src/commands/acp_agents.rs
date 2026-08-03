//! C0 — the three commands the ACP run panel needs that the raw-CLI and
//! remote-agent commands don't already cover: answering a parked permission
//! prompt, a safe reachability test for an ACP agent, and ending its warm
//! session on demand.
//!
//! `respond_agent_permission` routes into `AgentRunManager::respond_permission`
//! (Task 8's existing per-run permission channel) — there is exactly ONE path
//! from a UI click to a parked prompt, and this is it. `test_acp_agent` mirrors
//! `commands::remote_agents::test_remote_agent`'s deliberate restraint: it never
//! starts a session or sends a prompt, because a real turn may cost money or
//! trigger real work on the user's machine.

use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};

use crate::managers::acp_session::AcpSessionManager;
use crate::managers::agent_run::{uses_acp_driver, AgentRunManager, PermissionChoice};
use crate::settings::{self, AgentDefinition};

/// Result of `test_acp_agent`: what the agent said during `initialize`, and
/// nothing more — no session, no prompt, no cost.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AcpAgentTest {
    pub ok: bool,
    pub agent_name: String,
    pub agent_version: String,
    pub protocol_version: u16,
}

fn find_agent(app: &AppHandle, id: &str) -> Result<AgentDefinition, String> {
    settings::get_settings(app)
        .agents
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("Agent '{id}' not found"))
}

/// The parsed shape of a UI permission answer: whether it allows, without yet
/// deciding once-vs-always (that's the second element `parse_outcome` returns).
struct ParsedOutcome {
    allow: bool,
}

/// Parse the frontend's `outcome` string (`allow_once | allow_always |
/// deny_once | deny_always`) into `(allow?, persistent?)`. The `*_always`
/// forms are the ones that set a session-scoped override — see
/// `managers::agent_run::apply_answer`, which records the override PER TOOL
/// KIND (not a blanket grant): an always-allow on a file read must never
/// silently authorize a later shell command.
fn parse_outcome(outcome: &str) -> Result<(ParsedOutcome, bool), String> {
    match outcome {
        "allow_once" => Ok((ParsedOutcome { allow: true }, false)),
        "allow_always" => Ok((ParsedOutcome { allow: true }, true)),
        "deny_once" => Ok((ParsedOutcome { allow: false }, false)),
        "deny_always" => Ok((ParsedOutcome { allow: false }, true)),
        other => Err(format!("Unknown permission outcome '{other}'")),
    }
}

/// Map a parsed outcome to the wire enum the driver's turn loop actually
/// consumes (`managers::agent_run::apply_answer`). Kept separate from
/// `parse_outcome` so the string-parsing and the enum-selection are each
/// independently testable.
fn to_permission_choice(parsed: ParsedOutcome, persistent: bool) -> PermissionChoice {
    match (parsed.allow, persistent) {
        (true, false) => PermissionChoice::AllowOnce,
        (true, true) => PermissionChoice::AllowAlways,
        (false, false) => PermissionChoice::DenyOnce,
        (false, true) => PermissionChoice::DenyAlways,
    }
}

/// Answer a parked `session/request_permission` prompt. Routes into Task 8's
/// existing per-run channel (`AgentRunManager::respond_permission`) — there is
/// no second path into the driver's turn loop.
///
/// `option_id` is the EXACT agent-supplied option the UI button the user
/// clicked corresponds to (Task 11 review, Important 5). `outcome` still
/// drives the once/always + allow/deny bookkeeping (`parse_outcome`); it is
/// no longer solely responsible for selecting which option gets sent back —
/// see `AgentRunManager::respond_permission`'s doc comment for why `outcome`
/// alone made two same-kind options indistinguishable.
#[tauri::command]
#[specta::specta]
pub fn respond_agent_permission(
    app: AppHandle,
    run_id: String,
    request_id: String,
    outcome: String,
    option_id: String,
) -> Result<(), String> {
    let (parsed, persistent) = parse_outcome(&outcome)?;
    let choice = to_permission_choice(parsed, persistent);
    let manager = app
        .try_state::<Arc<AgentRunManager>>()
        .ok_or_else(|| "Agent run manager not initialized".to_string())?;
    manager.respond_permission(&run_id, &request_id, choice, Some(option_id))
}

/// Lightweight reachability test for an ACP agent: spawn it, send
/// `initialize`, report what it said, then close it. Deliberately does NOT
/// call `session/new` or send a prompt — same restraint as
/// `commands::remote_agents::test_remote_agent`, since a real turn may cost
/// money or trigger real work.
#[tauri::command]
#[specta::specta]
pub async fn test_acp_agent(app: AppHandle, agent_id: String) -> Result<AcpAgentTest, String> {
    let agent = find_agent(&app, &agent_id)?;
    if !uses_acp_driver(&agent) {
        return Err(format!("'{}' is not configured for ACP mode.", agent.name));
    }
    let sessions = app
        .try_state::<Arc<AcpSessionManager>>()
        .ok_or_else(|| "ACP session manager not initialized".to_string())?;

    let init = sessions.test_handshake(&agent).await?;
    let info = init.agent_info.unwrap_or_default();
    Ok(AcpAgentTest {
        ok: true,
        agent_name: info.name,
        agent_version: info.version,
        protocol_version: init.protocol_version,
    })
}

/// End an agent's warm ACP session on demand (the run panel's "End session"
/// action). A no-op if the agent has no live session.
#[tauri::command]
#[specta::specta]
pub async fn end_acp_session(app: AppHandle, agent_id: String) -> Result<(), String> {
    let sessions = app
        .try_state::<Arc<AcpSessionManager>>()
        .ok_or_else(|| "ACP session manager not initialized".to_string())?;
    sessions.end_session(&agent_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_outcome_strings_map_to_wire_and_overrides() {
        // allow_always / deny_always set a session-scoped override; the *_once
        // forms do not.
        let (out, ov) = parse_outcome("allow_once").unwrap();
        assert!(out.allow && !ov);
        let (out, ov) = parse_outcome("allow_always").unwrap();
        assert!(out.allow && ov);
        let (out, ov) = parse_outcome("deny_once").unwrap();
        assert!(!out.allow && !ov);
        let (out, ov) = parse_outcome("deny_always").unwrap();
        assert!(!out.allow && ov);
        assert!(parse_outcome("nonsense").is_err());
    }

    #[test]
    fn parsed_outcomes_map_to_the_matching_permission_choice() {
        let (out, ov) = parse_outcome("allow_once").unwrap();
        assert_eq!(to_permission_choice(out, ov), PermissionChoice::AllowOnce);
        let (out, ov) = parse_outcome("allow_always").unwrap();
        assert_eq!(to_permission_choice(out, ov), PermissionChoice::AllowAlways);
        let (out, ov) = parse_outcome("deny_once").unwrap();
        assert_eq!(to_permission_choice(out, ov), PermissionChoice::DenyOnce);
        let (out, ov) = parse_outcome("deny_always").unwrap();
        assert_eq!(to_permission_choice(out, ov), PermissionChoice::DenyAlways);
    }
}
