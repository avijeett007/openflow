//! AI Mode commands: create / update / delete / test AI modes.
//!
//! An "AI Mode" is a named dictation profile (see `settings::AiMode`) with its
//! own optional global hotkey and optional per-app auto-selection rules. These
//! commands manage the additive `settings.ai_modes` list and the seeded
//! `mode:<id>` shortcut bindings — mirroring the agents CRUD (`commands::agents`).
//!
//! Deliberately NOT re-implemented here (reused instead):
//! - **Hotkey editing** goes through the existing `change_binding("mode:<id>",
//!   ...)`. It works because `create_ai_mode` seeds a `ShortcutBinding` into
//!   `settings.bindings` (the generic command rejects ids absent from
//!   settings+defaults).
//! - **API keys** are inherited from the mode's cleanup provider (scope
//!   `"cleanup"`), so there is no per-mode key to manage.

use std::time::Instant;

use serde::Serialize;
use specta::Type;
use tauri::AppHandle;

use crate::commands::agents::is_valid_agent_slug;
use crate::settings::{self, AiMode, AppSettings, ShortcutBinding};
use crate::shortcut;

/// Result of the AI mode "Test" action: the transformed output plus how long it
/// took. Mirrors `AgentTestResult` / `test_agent`.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AiModeTestResult {
    pub output: String,
    pub latency_ms: u64,
}

/// List currently-running apps for the mode card's "Activate when using" picker.
/// Best-effort — an empty list just means the user types bundle ids / names by
/// hand.
#[tauri::command]
#[specta::specta]
pub fn get_running_apps() -> Vec<crate::active_app::RunningApp> {
    crate::active_app::running_apps()
}

/// Pure core of `create_ai_mode`: validate the slug, reject duplicates, force the
/// `mode:<id>` binding id, seed a `ShortcutBinding` so the generic
/// `change_binding` accepts hotkey edits, and push the mode. No I/O.
fn apply_create_ai_mode(settings: &mut AppSettings, mut mode: AiMode) -> Result<(), String> {
    if !is_valid_agent_slug(&mode.id) {
        return Err(format!(
            "Invalid mode id '{}': must match ^[a-z0-9_-]{{1,48}}$",
            mode.id
        ));
    }
    if settings.ai_modes.iter().any(|m| m.id == mode.id) {
        return Err(format!("Mode '{}' already exists", mode.id));
    }

    let binding_id = format!("mode:{}", mode.id);
    mode.binding_id = binding_id.clone();

    // Seed an (unbound) ShortcutBinding so the existing change_binding command —
    // which rejects ids absent from settings+defaults — will accept hotkey edits.
    let binding = ShortcutBinding {
        id: binding_id.clone(),
        name: mode.name.clone(),
        description: format!("AI Mode: {}", mode.name),
        default_binding: String::new(),
        current_binding: String::new(),
    };
    settings.bindings.entry(binding_id).or_insert(binding);
    settings.ai_modes.push(mode);
    Ok(())
}

/// Pure core of `update_ai_mode`: replace the mode by id, keeping `binding_id`
/// immutable. Returns `(was_enabled, now_enabled)` so the wrapper can
/// (un)register the hotkey when the enabled flag flips.
fn apply_update_ai_mode(
    settings: &mut AppSettings,
    mut mode: AiMode,
) -> Result<(bool, bool), String> {
    let idx = settings
        .ai_modes
        .iter()
        .position(|m| m.id == mode.id)
        .ok_or_else(|| format!("Mode '{}' not found", mode.id))?;

    // binding_id is always derived from the id and never mutated by the client.
    mode.binding_id = format!("mode:{}", mode.id);
    let was_enabled = settings.ai_modes[idx].enabled;
    let now_enabled = mode.enabled;
    settings.ai_modes[idx] = mode;
    Ok((was_enabled, now_enabled))
}

/// Pure core of `delete_ai_mode`: remove the mode and its seeded binding. Returns
/// the removed binding (if any) so the wrapper can unregister its hotkey.
fn apply_delete_ai_mode(
    settings: &mut AppSettings,
    id: &str,
) -> Result<Option<ShortcutBinding>, String> {
    if !settings.ai_modes.iter().any(|m| m.id == id) {
        return Err(format!("Mode '{}' not found", id));
    }
    let binding_id = format!("mode:{}", id);
    let removed_binding = settings.bindings.remove(&binding_id);
    settings.ai_modes.retain(|m| m.id != id);
    Ok(removed_binding)
}

#[tauri::command]
#[specta::specta]
pub fn create_ai_mode(app: AppHandle, mode: AiMode) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    apply_create_ai_mode(&mut settings, mode)?;
    settings::write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_ai_mode(app: AppHandle, mode: AiMode) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let binding_id = format!("mode:{}", mode.id);
    let (was_enabled, now_enabled) = apply_update_ai_mode(&mut settings, mode)?;
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
pub fn delete_ai_mode(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let removed_binding = apply_delete_ai_mode(&mut settings, &id)?;
    settings::write_settings(&app, settings);

    // Unregister its hotkey (best-effort).
    if let Some(binding) = removed_binding {
        if !binding.current_binding.trim().is_empty() {
            let _ = shortcut::unregister_shortcut(&app, binding);
        }
    }
    Ok(())
}

/// Run the mode's transform over `sample_text` and return the output + latency.
/// Powers the mode card's "Test" button (mirrors `test_agent`). For `Direct`
/// modes this returns the sample text unchanged (no LLM call).
#[tauri::command]
#[specta::specta]
pub async fn test_ai_mode(
    app: AppHandle,
    id: String,
    sample_text: String,
) -> Result<AiModeTestResult, String> {
    let settings = settings::get_settings(&app);
    let mode = settings
        .ai_modes
        .iter()
        .find(|m| m.id == id)
        .cloned()
        .ok_or_else(|| format!("Mode '{}' not found", id))?;

    let started = Instant::now();
    let output = crate::actions::run_ai_mode_transform(&app, &mode, &sample_text).await?;
    Ok(AiModeTestResult {
        output,
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{get_default_settings, AiModeKind};

    fn mode(id: &str) -> AiMode {
        AiMode {
            id: id.to_string(),
            name: "Translate".to_string(),
            kind: AiModeKind::Rewrite,
            enabled: true,
            binding_id: String::new(), // create/update force this to mode:<id>
            prompt: "Translate to French.".to_string(),
            provider_id: None,
            model: None,
            app_rules: vec![],
        }
    }

    #[test]
    fn create_seeds_binding_and_forces_binding_id() {
        let mut settings = get_default_settings();
        apply_create_ai_mode(&mut settings, mode("translate")).unwrap();

        let stored = settings
            .ai_modes
            .iter()
            .find(|m| m.id == "translate")
            .unwrap();
        assert_eq!(stored.binding_id, "mode:translate");

        let binding = settings
            .bindings
            .get("mode:translate")
            .expect("binding seeded");
        assert_eq!(binding.id, "mode:translate");
        assert_eq!(binding.description, "AI Mode: Translate");
        assert!(binding.current_binding.is_empty());
        assert!(binding.default_binding.is_empty());
    }

    #[test]
    fn create_rejects_invalid_slug() {
        let mut settings = get_default_settings();
        let err = apply_create_ai_mode(&mut settings, mode("Bad Id")).unwrap_err();
        assert!(err.contains("Invalid mode id"));
        assert!(settings.ai_modes.is_empty());
        assert!(!settings.bindings.contains_key("mode:Bad Id"));
    }

    #[test]
    fn create_rejects_duplicate() {
        let mut settings = get_default_settings();
        apply_create_ai_mode(&mut settings, mode("translate")).unwrap();
        let err = apply_create_ai_mode(&mut settings, mode("translate")).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(settings.ai_modes.len(), 1);
    }

    #[test]
    fn create_preserves_kind_and_app_rules() {
        let mut settings = get_default_settings();
        let mut cmd = mode("command");
        cmd.kind = AiModeKind::Command;
        cmd.app_rules = vec!["com.apple.Terminal".to_string(), "iterm".to_string()];
        cmd.provider_id = Some("openrouter".to_string());
        cmd.model = Some("gpt-4o-mini".to_string());
        apply_create_ai_mode(&mut settings, cmd).unwrap();

        let stored = settings
            .ai_modes
            .iter()
            .find(|m| m.id == "command")
            .unwrap();
        assert_eq!(stored.kind, AiModeKind::Command);
        assert_eq!(stored.app_rules.len(), 2);
        assert_eq!(stored.provider_id.as_deref(), Some("openrouter"));
        assert_eq!(stored.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(stored.binding_id, "mode:command");
    }

    #[test]
    fn update_reports_enabled_flip_and_keeps_binding_id() {
        let mut settings = get_default_settings();
        apply_create_ai_mode(&mut settings, mode("translate")).unwrap();

        let mut updated = mode("translate");
        updated.enabled = false;
        updated.binding_id = "mode:hacked".to_string(); // client tampering ignored
        let (was, now) = apply_update_ai_mode(&mut settings, updated).unwrap();
        assert!(was);
        assert!(!now);
        let stored = settings
            .ai_modes
            .iter()
            .find(|m| m.id == "translate")
            .unwrap();
        assert_eq!(stored.binding_id, "mode:translate");
        assert!(!stored.enabled);
    }

    #[test]
    fn update_missing_mode_errors() {
        let mut settings = get_default_settings();
        let err = apply_update_ai_mode(&mut settings, mode("ghost")).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn delete_removes_mode_and_binding() {
        let mut settings = get_default_settings();
        apply_create_ai_mode(&mut settings, mode("translate")).unwrap();
        assert!(settings.bindings.contains_key("mode:translate"));

        let removed = apply_delete_ai_mode(&mut settings, "translate").unwrap();
        assert!(removed.is_some());
        assert!(settings.ai_modes.iter().all(|m| m.id != "translate"));
        assert!(!settings.bindings.contains_key("mode:translate"));

        // The default bindings are untouched by the delete.
        assert!(settings.bindings.contains_key("transcribe"));
    }

    #[test]
    fn delete_missing_mode_errors() {
        let mut settings = get_default_settings();
        let err = apply_delete_ai_mode(&mut settings, "ghost").unwrap_err();
        assert!(err.contains("not found"));
    }
}
