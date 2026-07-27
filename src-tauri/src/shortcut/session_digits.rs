//! Digit quasimode for session hotkeys.
//!
//! While an agent recording is live, a temporary listener captures `1`–`9`
//! (select a pinned session slot), `0` (force a new session), and `Esc` (clear
//! the selection). The choice is stashed in `PENDING` and resolved when the
//! transcript is dispatched to the agent run (see `actions.rs`). Teardown is
//! guarded three ways — recording stop, recording cancel, and a failsafe timer —
//! because a permanently swallowed global digit key would be a release blocker.
//!
//! No digit pressed ⇒ `PENDING` stays `None` ⇒ today's behavior (new session).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter};

use crate::settings::{get_settings, KeyboardImplementation};

/// Force teardown after this long if `stop` was somehow never delivered.
const FAILSAFE_SECS: u64 = 60;

/// Bumped on every start AND stop so a stale failsafe timer can tell it was
/// superseded and skip tearing down a newer capture.
static CAPTURE_GEN: AtomicU64 = AtomicU64::new(0);

/// The pending selection for the current recording: `Some(1..=9)` = resume that
/// slot, `Some(0)` = force a new session, `None` = no explicit choice. Only one
/// recording runs at a time (the same invariant the recorder already relies on).
static PENDING: Mutex<Option<u8>> = Mutex::new(None);

/// Emitted to the picker overlay when the selection changes.
#[derive(Clone, Serialize, Type)]
pub struct SlotSelectedEvent {
    /// `Some(1..=9)` slot, `Some(0)` new-session, `None` cleared.
    pub selection: Option<u8>,
}

/// Record a digit/Esc selection and notify the overlay. `Some(0..=9)` from a
/// digit; `None` from Esc (clear).
pub fn set_pending_session_selection(app: &AppHandle, selection: Option<u8>) {
    if let Ok(mut p) = PENDING.lock() {
        *p = selection;
    }
    let _ = app.emit("session-slot-selected", SlotSelectedEvent { selection });
}

/// Take (and clear) the pending selection at dispatch time.
pub fn take_pending_session_selection() -> Option<u8> {
    PENDING.lock().ok().and_then(|mut p| p.take())
}

/// Clear without emitting (used at capture start and on cancel).
pub fn clear_pending_session_selection() {
    if let Ok(mut p) = PENDING.lock() {
        *p = None;
    }
}

/// Begin digit capture for the current agent recording. Arms a failsafe teardown.
pub fn start_capture(app: &AppHandle) {
    clear_pending_session_selection();
    let generation = CAPTURE_GEN.fetch_add(1, Ordering::SeqCst) + 1;

    match get_settings(app).keyboard_implementation {
        KeyboardImplementation::Tauri => super::tauri_impl::start_session_digit_capture(app),
        KeyboardImplementation::HandyKeys => super::handy_keys::start_session_digit_capture(app),
    }

    // Failsafe: if no stop bumped the counter within the window, force teardown.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(FAILSAFE_SECS)).await;
        if CAPTURE_GEN.load(Ordering::SeqCst) == generation {
            log::warn!("session digit capture failsafe fired; forcing teardown");
            stop_capture(&app);
        }
    });
}

/// End digit capture. Idempotent (backends ignore unregister-of-absent).
pub fn stop_capture(app: &AppHandle) {
    // Supersede any pending failsafe timer.
    CAPTURE_GEN.fetch_add(1, Ordering::SeqCst);
    match get_settings(app).keyboard_implementation {
        KeyboardImplementation::Tauri => super::tauri_impl::stop_session_digit_capture(app),
        KeyboardImplementation::HandyKeys => super::handy_keys::stop_session_digit_capture(app),
    }
}
