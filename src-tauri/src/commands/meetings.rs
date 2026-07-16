//! Tauri commands for OpenFlow Meetings (M1): start/stop capture, list/get/delete
//! meetings, capture status, and the two meetings settings toggles + allowlist.
//! Registered in the lib.rs tauri-specta builder; bindings regenerate into
//! `src/bindings.ts`.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::managers::meeting::{
    DiarizationStatus, MeetingCaptureStatus, MeetingDetail, MeetingManager, MeetingSpeakerRecord,
    MeetingSummary,
};
use crate::settings::{get_settings, write_settings};

fn manager(app: &AppHandle) -> Result<Arc<MeetingManager>, String> {
    app.try_state::<Arc<MeetingManager>>()
        .map(|m| m.inner().clone())
        .ok_or_else(|| "Meeting manager not initialized".to_string())
}

/// Start capturing a meeting. `app_bundle_id` (when known) targets the system
/// audio tap at that app's PID; capture degrades to mic-only on any tap failure.
#[tauri::command]
#[specta::specta]
pub fn start_meeting_capture(app: AppHandle, app_bundle_id: Option<String>) -> Result<i64, String> {
    manager(&app)?.start_capture(app_bundle_id)
}

/// Stop the active meeting capture and finalize it.
#[tauri::command]
#[specta::specta]
pub fn stop_meeting_capture(app: AppHandle) -> Result<(), String> {
    manager(&app)?.stop_capture()
}

/// Whether a capture is running (and whether system audio degraded to mic-only).
#[tauri::command]
#[specta::specta]
pub fn get_meeting_capture_status(app: AppHandle) -> Result<MeetingCaptureStatus, String> {
    Ok(manager(&app)?.capture_status())
}

/// All meetings, newest first.
#[tauri::command]
#[specta::specta]
pub fn list_meetings(app: AppHandle) -> Result<Vec<MeetingSummary>, String> {
    manager(&app)?.list_meetings()
}

/// A single meeting with its transcript segments.
#[tauri::command]
#[specta::specta]
pub fn get_meeting(app: AppHandle, meeting_id: i64) -> Result<Option<MeetingDetail>, String> {
    manager(&app)?.get_meeting(meeting_id)
}

/// Delete a meeting, its segments, and its WAVs.
#[tauri::command]
#[specta::specta]
pub fn delete_meeting(app: AppHandle, meeting_id: i64) -> Result<(), String> {
    manager(&app)?.delete_meeting(meeting_id)
}

/// Toggle the meetings feature master switch.
#[tauri::command]
#[specta::specta]
pub fn set_meetings_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meetings_enabled = enabled;
    write_settings(&app, settings);
    Ok(())
}

/// Toggle meeting auto-detection (the record prompt). Capture start stays
/// user-confirmed regardless.
#[tauri::command]
#[specta::specta]
pub fn set_meeting_auto_detect(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meeting_auto_detect = enabled;
    write_settings(&app, settings);
    Ok(())
}

/* ─────────────────────────  M2 — diarization  ─────────────────────────── */

/// Rename a per-meeting speaker cluster ("Speaker 1" → "Alice"). An empty name
/// clears the custom label back to the default. Scoped to this meeting only.
#[tauri::command]
#[specta::specta]
pub fn rename_meeting_speaker(
    app: AppHandle,
    meeting_id: i64,
    local_speaker: i64,
    name: String,
) -> Result<(), String> {
    manager(&app)?.rename_speaker(meeting_id, local_speaker, name)
}

/// The per-meeting speaker display names.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_speakers(
    app: AppHandle,
    meeting_id: i64,
) -> Result<Vec<MeetingSpeakerRecord>, String> {
    manager(&app)?.get_speakers(meeting_id)
}

/// Diarization availability + effective mode (for the status chip + settings).
#[tauri::command]
#[specta::specta]
pub fn get_diarization_status(app: AppHandle) -> Result<DiarizationStatus, String> {
    Ok(manager(&app)?.diarization_status())
}

/// Toggle the diarization master switch. Enabling it does NOT download models —
/// the frontend calls `download_diarization_models` explicitly so the ~34 MB
/// fetch is always a deliberate, progress-tracked action.
#[tauri::command]
#[specta::specta]
pub fn set_meetings_diarization(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meetings_diarization = enabled;
    write_settings(&app, settings);
    Ok(())
}

/// Toggle live provisional labels (opt-in; off by default on slow machines).
#[tauri::command]
#[specta::specta]
pub fn set_meetings_diarization_provisional(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.meetings_diarization_provisional = enabled;
    write_settings(&app, settings);
    Ok(())
}

/// Whether the diarization models are installed + their total download size.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct DiarizationModelsStatus {
    pub installed: bool,
    pub size_mb: u64,
}

#[tauri::command]
#[specta::specta]
pub fn get_diarization_models_status(app: AppHandle) -> Result<DiarizationModelsStatus, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(DiarizationModelsStatus {
            installed: crate::meeting::diar_models::models_installed(&app),
            size_mb: crate::meeting::diar_models::total_size_mb(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(DiarizationModelsStatus {
            installed: false,
            size_mb: 0,
        })
    }
}

/// Download + verify the diarization models (progress via
/// `diarization-model-progress`). Only ever called on explicit user action.
#[tauri::command]
#[specta::specta]
pub async fn download_diarization_models(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::meeting::diar_models::download_all(&app)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("Diarization is only available on macOS".to_string())
    }
}
