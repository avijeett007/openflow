//! Meeting detection: a debounced fusion of two weak signals (DESIGN-meetings.md
//! §3) — (1) a known meeting app is running (`NSWorkspace.runningApplications`
//! bundle-id match) AND (2) the microphone is in use somewhere
//! (`kAudioDevicePropertyDeviceIsRunningSomewhere`). Neither is a real "a call is
//! active" API; together, debounced, they are the honest ceiling of
//! auto-detection. A manual "Start capture" always exists as the reliable path.
//!
//! Two traps are designed around explicitly:
//! - **Self-suppression:** OpenFlow's own dictation / wake-word / always-on mic
//!   opens the input device, which would light up signal (2). Detection is
//!   suppressed whenever our recorder is active, and while a meeting capture is
//!   already running.
//! - **Debounce:** both signals must hold for a few consecutive polls before a
//!   single `meeting-detected` is emitted, and it is not re-emitted until the
//!   condition clears (so one Zoom call prompts once).

use std::sync::Arc;
use std::time::Duration;

use log::{debug, info};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::managers::audio::AudioRecordingManager;
use crate::managers::meeting::{MeetingDetected, MeetingManager};
use crate::settings::get_settings;

/// Poll cadence. Debounce is expressed in ticks (`DEBOUNCE_TICKS`).
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Consecutive positive polls required before emitting (≈3 s, per the design).
const DEBOUNCE_TICKS: u32 = 3;

/// A running application matched against the meeting allowlist.
#[derive(Clone, Debug)]
pub struct RunningMeetingApp {
    pub bundle_id: String,
    pub app_name: String,
    pub pid: i32,
}

/// Start the background detection loop. Started once at app init (like the
/// wake-word manager); it reads settings live so toggling detection on/off takes
/// effect without a restart.
pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("meeting-detector".into())
        .spawn(move || detector_loop(app))
        .expect("failed to spawn meeting detector thread");
}

/// Pure debounce + self-suppression state machine, extracted from the poll loop
/// so it can be unit-tested without an app handle or audio devices. One `observe`
/// per poll tick; it returns the bundle id to prompt for, or `None`.
#[derive(Default)]
pub(crate) struct DebounceState {
    consecutive: u32,
    /// Bundle id we've already prompted for this "session"; cleared when the app
    /// is no longer running so the next call can prompt again.
    prompted_for: Option<String>,
}

impl DebounceState {
    /// One tick. `running_bundle` is the matched meeting app (if any),
    /// `mic_in_use` is the device-level signal, `suppressed` is true while our own
    /// recorder/monitor or a meeting capture is active. Returns the bundle id to
    /// emit `meeting-detected` for, exactly once per detected call.
    pub(crate) fn observe(
        &mut self,
        running_bundle: Option<&str>,
        mic_in_use: bool,
        suppressed: bool,
    ) -> Option<String> {
        if suppressed {
            // Our own mic use — don't count it toward detection.
            self.consecutive = 0;
            return None;
        }

        let Some(bundle) = running_bundle else {
            // App gone: reset the debounce and the prompt latch.
            self.consecutive = 0;
            self.prompted_for = None;
            return None;
        };

        if !mic_in_use {
            // App running but no call audio yet — hold the latch, reset the count.
            self.consecutive = 0;
            return None;
        }

        self.consecutive = self.consecutive.saturating_add(1);
        let already = self.prompted_for.as_deref() == Some(bundle);
        if self.consecutive >= DEBOUNCE_TICKS && !already {
            self.prompted_for = Some(bundle.to_string());
            Some(bundle.to_string())
        } else {
            None
        }
    }
}

fn detector_loop(app: AppHandle) {
    let mut state = DebounceState::default();

    loop {
        std::thread::sleep(POLL_INTERVAL);

        let settings = get_settings(&app);
        if !settings.meetings_enabled || !settings.meeting_auto_detect {
            state = DebounceState::default();
            continue;
        }

        let suppressed = recorder_active(&app) || meeting_active(&app);
        let running = running_meeting_app(&settings.meeting_app_allowlist);
        let mic_in_use = super::capture::any_input_device_running();

        if let Some(bundle_id) = state.observe(
            running.as_ref().map(|a| a.bundle_id.as_str()),
            mic_in_use,
            suppressed,
        ) {
            let app_name = running
                .as_ref()
                .map(|a| a.app_name.clone())
                .unwrap_or_else(|| bundle_id.clone());
            info!("meeting detected: {app_name} ({bundle_id}) — mic in use, offering capture");
            let _ = MeetingDetected {
                bundle_id,
                app_name,
            }
            .emit(&app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_once_after_debounce() {
        let mut s = DebounceState::default();
        // Two ticks aren't enough (DEBOUNCE_TICKS = 3).
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        // Third tick fires exactly once.
        assert_eq!(
            s.observe(Some("us.zoom.xos"), true, false).as_deref(),
            Some("us.zoom.xos")
        );
        // Subsequent ticks for the same app do NOT re-prompt.
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
    }

    #[test]
    fn self_suppression_blocks_and_resets() {
        let mut s = DebounceState::default();
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        // Our own recorder becomes active on the tick that would have emitted —
        // suppressed, and the count resets so no false positive.
        assert_eq!(s.observe(Some("us.zoom.xos"), true, true), None);
        // After suppression clears it must debounce again from zero.
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(
            s.observe(Some("us.zoom.xos"), true, false).as_deref(),
            Some("us.zoom.xos")
        );
    }

    #[test]
    fn requires_both_signals() {
        let mut s = DebounceState::default();
        // App running but mic not in use → never counts up.
        for _ in 0..5 {
            assert_eq!(s.observe(Some("us.zoom.xos"), false, false), None);
        }
        // Mic in use but no known app → nothing.
        for _ in 0..5 {
            assert_eq!(s.observe(None, true, false), None);
        }
    }

    #[test]
    fn app_quit_relatches_for_next_call() {
        let mut s = DebounceState::default();
        for _ in 0..3 {
            s.observe(Some("us.zoom.xos"), true, false);
        }
        // App quits (latch clears), then a new call to the same app re-prompts.
        assert_eq!(s.observe(None, false, false), None);
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(s.observe(Some("us.zoom.xos"), true, false), None);
        assert_eq!(
            s.observe(Some("us.zoom.xos"), true, false).as_deref(),
            Some("us.zoom.xos")
        );
    }
}

fn recorder_active(app: &AppHandle) -> bool {
    app.try_state::<Arc<AudioRecordingManager>>()
        .map(|rm| rm.is_recording() || rm.is_monitoring())
        .unwrap_or(false)
}

fn meeting_active(app: &AppHandle) -> bool {
    app.try_state::<Arc<MeetingManager>>()
        .map(|mm| mm.is_active())
        .unwrap_or(false)
}

/// Return the first running application whose bundle id is in `allowlist`.
pub fn running_meeting_app(allowlist: &[String]) -> Option<RunningMeetingApp> {
    #[cfg(target_os = "macos")]
    {
        running_meeting_app_macos(allowlist)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = allowlist;
        None
    }
}

/// Resolve a running app's PID by bundle id (used to target the process tap).
pub fn pid_for_bundle_id(bundle_id: &str) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        running_meeting_app_macos(std::slice::from_ref(&bundle_id.to_string())).map(|a| a.pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_id;
        None
    }
}

#[cfg(target_os = "macos")]
fn running_meeting_app_macos(allowlist: &[String]) -> Option<RunningMeetingApp> {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    // Safe read-only accessors; return retained NSRunningApplication objects.
    let apps = workspace.runningApplications();
    let count = apps.count();
    for i in 0..count {
        let running = apps.objectAtIndex(i);
        let bundle = running.bundleIdentifier();
        let Some(bundle) = bundle else { continue };
        let bundle = bundle.to_string();
        if allowlist.iter().any(|b| b == &bundle) {
            let name = running
                .localizedName()
                .map(|n| n.to_string())
                .unwrap_or_else(|| bundle.clone());
            let pid = running.processIdentifier();
            debug!("meeting app running: {name} ({bundle}) pid {pid}");
            return Some(RunningMeetingApp {
                bundle_id: bundle,
                app_name: name,
                pid,
            });
        }
    }
    None
}
