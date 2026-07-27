//! Session-hotkeys commands: read/clear pinned slots, toggle the picker.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::managers::session_slots::{SessionSlot, SessionSlotState};
use crate::settings::{get_settings, write_settings};

/// Pinned slots for an agent (sorted by slot number). Empty if none.
#[tauri::command]
#[specta::specta]
pub fn get_session_slots(app: AppHandle, agent_id: String) -> Result<Vec<SessionSlot>, String> {
    let state = app
        .try_state::<Arc<SessionSlotState>>()
        .ok_or("session slot store not initialized")?;
    let store = state
        .store
        .lock()
        .map_err(|_| "failed to lock session slots")?;
    Ok(store.slots_for(&agent_id))
}

/// Forget a pinned slot, freeing its number for reuse.
#[tauri::command]
#[specta::specta]
pub fn clear_session_slot(app: AppHandle, agent_id: String, slot: u8) -> Result<(), String> {
    let state = app
        .try_state::<Arc<SessionSlotState>>()
        .ok_or("session slot store not initialized")?;
    let mut store = state
        .store
        .lock()
        .map_err(|_| "failed to lock session slots")?;
    store.clear(&agent_id, slot);
    store.save(&state.path)?;
    Ok(())
}

/// Toggle the session picker feature (persisted in settings).
#[tauri::command]
#[specta::specta]
pub fn set_session_picker_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.session_picker_enabled = enabled;
    write_settings(&app, settings);
    Ok(())
}
