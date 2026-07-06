//! Flow OS agent commands: create / update / delete / test agents.
//!
//! An "agent" is dictation routed through a persona LLM before injection (see
//! `actions::finish_dictation`). These commands manage the additive
//! `settings.agents` list and the seeded `agent:<id>` shortcut bindings.
//!
//! Deliberately NOT re-implemented here (reused instead):
//! - **Hotkey editing** goes through the existing `change_binding("agent:<id>",
//!   ...)`. It works because `create_agent` seeds a `ShortcutBinding` into
//!   `settings.bindings` (the generic command rejects ids absent from
//!   settings+defaults).
//! - **API-key storage** goes through the existing generic
//!   `set_api_key("agent", id, key)` / `has_api_key` / `delete_api_key`.

use std::time::Instant;

use serde::Serialize;
use specta::Type;
use tauri::AppHandle;

use crate::settings::{self, AgentDefinition, AppSettings, ShortcutBinding};
use crate::shortcut;

/// Result of the agent "Test" action: the LLM's output plus how long it took.
/// Mirrors `BackendTestResult` / `test_cleanup_backend`.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AgentTestResult {
    pub output: String,
    pub latency_ms: u64,
}

/// Validate an agent id against `^[a-z0-9_-]{1,48}$`. Kept pure so it is unit
/// testable and reused by the (also-pure) `apply_*` helpers below.
pub(crate) fn is_valid_agent_slug(id: &str) -> bool {
    let len = id.chars().count();
    (1..=48).contains(&len)
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Pure core of `create_agent`: validate the slug, reject duplicates, force the
/// `agent:<id>` binding id, seed a `ShortcutBinding` so the generic
/// `change_binding` accepts hotkey edits, and push the agent. No I/O — the
/// command wrapper handles load/persist.
fn apply_create_agent(
    settings: &mut AppSettings,
    mut agent: AgentDefinition,
) -> Result<(), String> {
    if !is_valid_agent_slug(&agent.id) {
        return Err(format!(
            "Invalid agent id '{}': must match ^[a-z0-9_-]{{1,48}}$",
            agent.id
        ));
    }
    if settings.agents.iter().any(|a| a.id == agent.id) {
        return Err(format!("Agent '{}' already exists", agent.id));
    }

    let binding_id = format!("agent:{}", agent.id);
    agent.binding_id = binding_id.clone();

    // Seed an (unbound) ShortcutBinding so the existing change_binding command —
    // which rejects ids absent from settings+defaults — will accept hotkey edits.
    let binding = ShortcutBinding {
        id: binding_id.clone(),
        name: agent.name.clone(),
        description: format!("Agent: {}", agent.name),
        default_binding: String::new(),
        current_binding: String::new(),
    };
    settings.bindings.entry(binding_id).or_insert(binding);
    settings.agents.push(agent);
    Ok(())
}

/// Pure core of `update_agent`: replace the agent by id, keeping `binding_id`
/// immutable. Returns `(was_enabled, now_enabled)` so the wrapper can (un)register
/// the hotkey when the enabled flag flips.
fn apply_update_agent(
    settings: &mut AppSettings,
    mut agent: AgentDefinition,
) -> Result<(bool, bool), String> {
    let idx = settings
        .agents
        .iter()
        .position(|a| a.id == agent.id)
        .ok_or_else(|| format!("Agent '{}' not found", agent.id))?;

    // binding_id is always derived from the id and never mutated by the client.
    agent.binding_id = format!("agent:{}", agent.id);
    let was_enabled = settings.agents[idx].enabled;
    let now_enabled = agent.enabled;
    settings.agents[idx] = agent;
    Ok((was_enabled, now_enabled))
}

/// Pure core of `delete_agent`: remove the agent and its seeded binding. Returns
/// the removed binding (if any) so the wrapper can unregister its hotkey.
fn apply_delete_agent(
    settings: &mut AppSettings,
    id: &str,
) -> Result<Option<ShortcutBinding>, String> {
    if !settings.agents.iter().any(|a| a.id == id) {
        return Err(format!("Agent '{}' not found", id));
    }
    let binding_id = format!("agent:{}", id);
    let removed_binding = settings.bindings.remove(&binding_id);
    settings.agents.retain(|a| a.id != id);
    Ok(removed_binding)
}

#[tauri::command]
#[specta::specta]
pub fn create_agent(app: AppHandle, agent: AgentDefinition) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    apply_create_agent(&mut settings, agent)?;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_agent(app: AppHandle, agent: AgentDefinition) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let binding_id = format!("agent:{}", agent.id);
    let (was_enabled, now_enabled) = apply_update_agent(&mut settings, agent)?;
    let binding = settings.bindings.get(&binding_id).cloned();
    settings::write_settings(&app, settings);

    // Only touch the OS hotkey registration when the enabled flag actually flips,
    // and only when a hotkey is set. Errors are non-fatal (duplicate/absent).
    if was_enabled != now_enabled {
        if let Some(binding) = binding {
            if !binding.current_binding.trim().is_empty() {
                if now_enabled {
                    let _ = shortcut::register_shortcut(&app, binding);
                } else {
                    let _ = shortcut::unregister_shortcut(&app, binding);
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn delete_agent(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let removed_binding = apply_delete_agent(&mut settings, &id)?;
    settings::write_settings(&app, settings);

    // Unregister its hotkey (best-effort) and drop any per-agent API key.
    if let Some(binding) = removed_binding {
        if !binding.current_binding.trim().is_empty() {
            let _ = shortcut::unregister_shortcut(&app, binding);
        }
    }
    let _ = crate::keychain::delete_api_key("agent", &id);
    Ok(())
}

/// Run the agent's persona LLM over `sample_text` and return the output +
/// latency. Powers the agent card's "Test" button (mirrors `test_cleanup_backend`).
#[tauri::command]
#[specta::specta]
pub async fn test_agent(
    app: AppHandle,
    id: String,
    sample_text: String,
) -> Result<AgentTestResult, String> {
    let settings = settings::get_settings(&app);
    let agent = settings
        .agents
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or_else(|| format!("Agent '{}' not found", id))?;

    let started = Instant::now();
    let output = crate::actions::run_agent_transform(&app, &agent, &sample_text).await?;
    Ok(AgentTestResult {
        output,
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{get_default_settings, AgentOutputMode};

    fn agent(id: &str) -> AgentDefinition {
        AgentDefinition {
            id: id.to_string(),
            name: "Coder".to_string(),
            enabled: true,
            binding_id: String::new(), // create/update force this to agent:<id>
            provider_id: "openrouter".to_string(),
            model: "gpt-4o-mini".to_string(),
            system_prompt: "You are a coder.".to_string(),
            output_mode: AgentOutputMode::Inject,
        }
    }

    #[test]
    fn valid_slugs_are_accepted() {
        assert!(is_valid_agent_slug("coder"));
        assert!(is_valid_agent_slug("commit-msg"));
        assert!(is_valid_agent_slug("agent_1"));
        assert!(is_valid_agent_slug("a"));
        assert!(is_valid_agent_slug(&"a".repeat(48)));
    }

    #[test]
    fn invalid_slugs_are_rejected() {
        assert!(!is_valid_agent_slug("")); // empty
        assert!(!is_valid_agent_slug(&"a".repeat(49))); // too long
        assert!(!is_valid_agent_slug("Coder")); // uppercase
        assert!(!is_valid_agent_slug("has space"));
        assert!(!is_valid_agent_slug("emoji😀"));
        assert!(!is_valid_agent_slug("dots.not.allowed"));
        assert!(!is_valid_agent_slug("agent:coder")); // colon not allowed
    }

    #[test]
    fn create_agent_seeds_binding_and_forces_binding_id() {
        let mut settings = get_default_settings();
        apply_create_agent(&mut settings, agent("coder")).unwrap();

        // Agent stored with the derived binding id.
        let stored = settings.agents.iter().find(|a| a.id == "coder").unwrap();
        assert_eq!(stored.binding_id, "agent:coder");

        // A ShortcutBinding was seeded so change_binding will accept edits.
        let binding = settings
            .bindings
            .get("agent:coder")
            .expect("binding seeded");
        assert_eq!(binding.id, "agent:coder");
        assert_eq!(binding.description, "Agent: Coder");
        assert!(binding.current_binding.is_empty());
        assert!(binding.default_binding.is_empty());
    }

    #[test]
    fn create_agent_rejects_invalid_slug() {
        let mut settings = get_default_settings();
        let err = apply_create_agent(&mut settings, agent("Bad Id")).unwrap_err();
        assert!(err.contains("Invalid agent id"));
        assert!(settings.agents.is_empty());
        assert!(!settings.bindings.contains_key("agent:Bad Id"));
    }

    #[test]
    fn create_agent_rejects_duplicate() {
        let mut settings = get_default_settings();
        apply_create_agent(&mut settings, agent("coder")).unwrap();
        let err = apply_create_agent(&mut settings, agent("coder")).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(settings.agents.len(), 1);
    }

    #[test]
    fn update_agent_reports_enabled_flip_and_keeps_binding_id() {
        let mut settings = get_default_settings();
        apply_create_agent(&mut settings, agent("coder")).unwrap();

        let mut updated = agent("coder");
        updated.enabled = false;
        updated.binding_id = "agent:hacked".to_string(); // client tampering ignored
        let (was, now) = apply_update_agent(&mut settings, updated).unwrap();
        assert!(was);
        assert!(!now);
        let stored = settings.agents.iter().find(|a| a.id == "coder").unwrap();
        assert_eq!(stored.binding_id, "agent:coder");
        assert!(!stored.enabled);
    }

    #[test]
    fn update_missing_agent_errors() {
        let mut settings = get_default_settings();
        let err = apply_update_agent(&mut settings, agent("ghost")).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn delete_agent_removes_agent_and_binding() {
        let mut settings = get_default_settings();
        apply_create_agent(&mut settings, agent("coder")).unwrap();
        assert!(settings.bindings.contains_key("agent:coder"));

        let removed = apply_delete_agent(&mut settings, "coder").unwrap();
        assert!(removed.is_some());
        assert!(settings.agents.iter().all(|a| a.id != "coder"));
        assert!(!settings.bindings.contains_key("agent:coder"));

        // The default bindings are untouched by the delete.
        assert!(settings.bindings.contains_key("transcribe"));
    }

    #[test]
    fn delete_missing_agent_errors() {
        let mut settings = get_default_settings();
        let err = apply_delete_agent(&mut settings, "ghost").unwrap_err();
        assert!(err.contains("not found"));
    }
}
