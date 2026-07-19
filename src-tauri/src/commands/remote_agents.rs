//! Flow OS increment 3 — commands for **remote (A2A) agents**.
//!
//! These manage the A2A-specific lifecycle that the generic agent commands
//! (`create_agent`/`update_agent`/`delete_agent` in `commands::agents`) don't
//! cover: fetching + resolving an agent card, and a lightweight reachability
//! test. Everything else (create/update/delete, hotkey binding, the per-agent
//! keyring token via `set_api_key("agent", id, …)`) is reused unchanged.
//!
//! The token lives in the OS keyring (scope `"agent"`, account = agent id) and
//! is never written to the settings store; `delete_agent` already cleans it up.

use tauri::AppHandle;

use crate::a2a::{self, A2aTransport, AgentCardSummary, HttpA2aTransport};
use crate::settings::{self, AgentDefinition};

/// Map a raw fetch/transport error into a friendly, user-facing message
/// (mirrors the `service.rs` status-mapping pattern). The `no-JSON-RPC` and
/// `not-A2A` cases already carry clear messages and pass through.
fn friendly_card_error(e: &str) -> String {
    if e.contains("HTTP 401") {
        "This agent needs a token. Add one in the token field below, then fetch again.".to_string()
    } else if e.contains("HTTP 404") {
        "No agent card was found at that URL. Check the address.".to_string()
    } else if e.starts_with("Could not reach the agent")
        || e.contains("JSON-RPC")
        || e.contains("not a JSON object")
        || e.contains("valid JSON")
    {
        // These already carry a clear, user-facing message — surface verbatim.
        e.to_string()
    } else {
        format!("Couldn't read the agent card: {e}")
    }
}

/// Shared core: fetch the card (with 401→token retry inside the transport),
/// parse it, resolve the JSON-RPC endpoint, and return the parsed card + endpoint.
async fn fetch_and_resolve<T: A2aTransport>(
    transport: &T,
    agent: &AgentDefinition,
    token: Option<String>,
) -> Result<(a2a::AgentCard, String), String> {
    if agent.remote_url.trim().is_empty() {
        return Err("Enter the agent's URL first.".to_string());
    }
    let url = a2a::well_known_card_url(&agent.remote_url);
    let card_json = transport
        .fetch_card(url, token)
        .await
        .map_err(|e| friendly_card_error(&e))?;
    let card = a2a::parse_agent_card(&card_json).map_err(|e| friendly_card_error(&e))?;
    let endpoint = card.select_jsonrpc_endpoint()?;
    Ok((card, endpoint))
}

fn find_agent(app: &AppHandle, id: &str) -> Result<AgentDefinition, String> {
    settings::get_settings(app)
        .agents
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("Agent '{id}' not found"))
}

/// Fetch and resolve a remote agent's card, PERSIST the resolved endpoint +
/// display metadata onto the agent, and return a summary for the UI. This is the
/// settings "Fetch card" button.
#[tauri::command]
#[specta::specta]
pub async fn fetch_remote_agent_card(
    app: AppHandle,
    agent_id: String,
) -> Result<AgentCardSummary, String> {
    let agent = find_agent(&app, &agent_id)?;
    let token = crate::keychain::get_api_key("agent", &agent_id);
    let transport = HttpA2aTransport::new();

    let (card, endpoint) = fetch_and_resolve(&transport, &agent, token).await?;
    let summary = card.summary(endpoint.clone());

    // Persist the resolved endpoint + cached display metadata onto the agent so
    // a run doesn't have to re-fetch. Re-read to avoid clobbering a concurrent
    // edit to a different field.
    let mut current = settings::get_settings(&app);
    if let Some(a) = current.agents.iter_mut().find(|a| a.id == agent_id) {
        a.remote_endpoint = endpoint;
        a.remote_card_name = card.name.clone();
        a.remote_card_version = card.version.clone();
        a.remote_streaming = card.streaming;
        settings::write_settings(&app, current);
    }

    Ok(summary)
}

/// Lightweight reachability test: re-fetch the card (with the 401→token retry).
/// Deliberately does NOT send a real message — that could cost money or trigger
/// real work on the user's server. Returns the resolved endpoint on success.
#[tauri::command]
#[specta::specta]
pub async fn test_remote_agent(app: AppHandle, agent_id: String) -> Result<String, String> {
    let agent = find_agent(&app, &agent_id)?;
    let token = crate::keychain::get_api_key("agent", &agent_id);
    let transport = HttpA2aTransport::new();
    let (_card, endpoint) = fetch_and_resolve(&transport, &agent, token).await?;
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_error_maps_401_to_token_hint() {
        let msg = friendly_card_error("HTTP 401");
        assert!(msg.contains("token"));
    }

    #[test]
    fn friendly_error_maps_404() {
        assert!(friendly_card_error("HTTP 404").contains("Check the address"));
    }

    #[test]
    fn friendly_error_passes_through_no_jsonrpc() {
        let raw = "This agent doesn't offer a JSON-RPC interface. OpenFlow only speaks the A2A JSON-RPC binding.";
        assert_eq!(friendly_card_error(raw), raw);
    }

    #[test]
    fn friendly_error_passes_through_unreachable() {
        let raw = "Could not reach the agent: connection refused";
        assert_eq!(friendly_card_error(raw), raw);
    }

    #[test]
    fn friendly_error_wraps_unknown() {
        assert!(friendly_card_error("weird thing").starts_with("Couldn't read the agent card"));
    }
}
