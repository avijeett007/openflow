//! Tauri commands for OpenFlow Meetings (M1): start/stop capture, list/get/delete
//! meetings, capture status, and the two meetings settings toggles + allowlist.
//! Registered in the lib.rs tauri-specta builder; bindings regenerate into
//! `src/bindings.ts`.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::managers::meeting::{
    MeetingCaptureStatus, MeetingDetail, MeetingManager, MeetingSummary,
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
