//! Hands-free / wake-word listener.
//!
//! When enabled, [`WakeWordManager`] runs a background loop that repeatedly
//! captures a short listening window (via the shared [`AudioRecordingManager`]
//! with VAD Offline), transcribes it with the local [`TranscriptionManager`],
//! and fuzzy-matches a user-configurable wake phrase (default "hey flow") at the
//! start of the utterance. On a match it feeds the following speech through the
//! exact same clean → inject → log path a manual hotkey dictation uses
//! ([`crate::actions::finish_dictation`]).
//!
//! v1 NOTE: this reuses the general-purpose local STT model to detect the wake
//! word, which is CPU-heavy. The listening cadence is utterance-gated (a fixed
//! short window per cycle plus a small inter-cycle sleep), NOT a tight
//! transcribe-everything loop, which keeps CPU reasonable. A dedicated
//! lightweight wake-word model (openWakeWord / Porcupine) would be far cheaper
//! and is the obvious future optimization — left out here to avoid adding a new
//! ML model/dependency.
//!
//! Once the wake phrase is matched, the command that follows is captured with
//! *smart, VAD-driven* timing ([`capture_until_silence`]): it waits up to a
//! user-configurable grace window (`wake_word_listen_seconds`) for the user to
//! start talking, then keeps the mic open for as long as speech keeps arriving,
//! stopping a configurable silence gap (`wake_word_silence_timeout_ms`) after they
//! finish — so short commands submit promptly and long ones are never cut off
//! mid-thought. This replaces the old fixed-window capture. Note the *detection*
//! windows are still fixed short cycles — a single always-open rolling stream for
//! detection is the next refinement.

use crate::actions::finish_dictation;
use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::get_settings;
use log::{debug, error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Synthetic binding id used when the wake-word loop drives the shared recorder,
/// so it never collides with a real user hotkey binding.
const WAKE_BINDING_ID: &str = "__wake_word__";

/// How long each wake-phrase listening window records before we stop and
/// transcribe it. Long enough to catch "hey flow ..." with a short trailing
/// command; VAD Offline trims surrounding silence.
const LISTEN_WINDOW_MS: u64 = 4000;

/// How often the smart command capture polls the live voiced-sample counter to
/// decide whether the user is still speaking.
const POLL_MS: u64 = 100;

/// Absolute safety cap added on top of the user's minimum window, so a noisy room
/// (VAD seeing "speech" indefinitely) can never hold the mic open forever.
const HARD_MAX_EXTRA_SECS: u64 = 180;

/// Inter-cycle sleep to avoid busy-spin and give the CPU a break between windows.
const IDLE_SLEEP: Duration = Duration::from_millis(400);

/// Longer backoff after an error (e.g. no local model loaded) to avoid log spam.
const ERROR_SLEEP: Duration = Duration::from_millis(1500);

pub struct WakeWordManager {
    app: AppHandle,
    running: Arc<AtomicBool>,
}

impl WakeWordManager {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(dead_code)] // Exposed for callers/diagnostics; not yet wired to a command.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Start the background listening loop. Idempotent: a second call while
    /// already running is a no-op.
    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            debug!("Wake-word listener already running");
            return;
        }
        let app = self.app.clone();
        let running = Arc::clone(&self.running);
        std::thread::spawn(move || {
            info!("Wake-word listener started");
            // The loop is fully defensive; any error inside logs and continues.
            wake_word_loop(app, running);
            info!("Wake-word listener stopped");
        });
    }

    /// Signal the background loop to stop. It exits after finishing the current
    /// window (loop checks the flag between cycles).
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Toggle at runtime when the `hands_free_enabled` setting changes.
    pub fn set_enabled(&self, enabled: bool) {
        if enabled {
            self.start();
        } else {
            self.stop();
        }
    }
}

fn wake_word_loop(app: AppHandle, running: Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        let settings = get_settings(&app);
        // The setting is the source of truth; honor an out-of-band disable.
        if !settings.hands_free_enabled {
            break;
        }

        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());

        // Never contend with a manual dictation: only listen when the recorder is
        // otherwise idle.
        if rm.is_recording() {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }

        // Ensure the local STT model is loading/loaded; transcribe() waits for an
        // in-flight load. Wake-word detection requires a local model regardless of
        // the STT backend used for the actual dictation.
        tm.initiate_model_load();

        // Capture one listening window and transcribe it.
        let samples = match capture_window(&rm, &running, LISTEN_WINDOW_MS) {
            Some(s) if !s.is_empty() => s,
            _ => {
                std::thread::sleep(IDLE_SLEEP);
                continue;
            }
        };

        let transcript = match tm.transcribe(samples.clone()) {
            Ok(t) => t,
            Err(e) => {
                // Non-fatal: log and back off. Most common cause is no local model
                // loaded (e.g. user only configured a remote STT backend).
                debug!("Wake-word listen transcription failed: {}", e);
                std::thread::sleep(ERROR_SLEEP);
                continue;
            }
        };

        if transcript.trim().is_empty() {
            std::thread::sleep(IDLE_SLEEP);
            continue;
        }

        match match_wake_word(
            &transcript,
            &settings.wake_word,
            settings.wake_word_sensitivity,
        ) {
            Some(command) => {
                debug!(
                    "Wake word matched in '{}' (phrase '{}'); command tail: '{}'",
                    transcript, settings.wake_word, command
                );
                handle_wake(&app, &rm, &tm, command, samples.len(), &running);
            }
            None => {
                debug!("No wake word in '{}'", transcript);
            }
        }

        std::thread::sleep(IDLE_SLEEP);
    }

    running.store(false, Ordering::SeqCst);
}

/// Drive the shared recorder for a fixed window with VAD Offline, then return the
/// captured (silence-trimmed) samples. Returns `None` if the recorder could not
/// be started or produced nothing.
fn capture_window(
    rm: &Arc<AudioRecordingManager>,
    running: &Arc<AtomicBool>,
    window_ms: u64,
) -> Option<Vec<f32>> {
    let cancel_generation = rm.cancel_generation();
    if let Err(e) = rm.try_start_recording(WAKE_BINDING_ID, VadPolicy::Offline) {
        // Typically means a manual dictation grabbed the recorder first; skip.
        debug!("Wake-word could not start listening window: {}", e);
        return None;
    }

    let start = Instant::now();
    let window = Duration::from_millis(window_ms);
    while start.elapsed() < window {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    rm.stop_recording(WAKE_BINDING_ID, cancel_generation)
}

/// Smart command capture: drive the shared recorder with VAD Offline and keep it
/// open based on live speech activity rather than a fixed window.
///
/// Rules:
/// 1. **Before the user starts speaking:** keep the mic open, waiting up to
///    `min_open_ms` for speech to begin. This is the pre-speech grace window —
///    a pause after the wake word (while the user gathers their thought) never
///    cuts them off. If nothing is spoken within it, capture ends (empty).
/// 2. **Once the user has spoken:** keep extending for as long as speech keeps
///    arriving, and end `silence_timeout_ms` after they go quiet — *regardless of
///    the grace window*. So a short command submits promptly (it does not wait out
///    the whole grace window) and a long one stays open as long as they talk.
/// 3. An absolute cap (`min_open_ms` + [`HARD_MAX_EXTRA_SECS`]) guards against a
///    noisy room (VAD false-positives) holding the mic open forever.
///
/// End-of-speech is detected by polling [`AudioRecordingManager::voiced_sample_count`],
/// which advances only while VAD-passed (voiced) audio is arriving.
fn capture_until_silence(
    rm: &Arc<AudioRecordingManager>,
    running: &Arc<AtomicBool>,
    min_open_ms: u64,
    silence_timeout_ms: u64,
) -> Option<Vec<f32>> {
    let cancel_generation = rm.cancel_generation();
    if let Err(e) = rm.try_start_recording(WAKE_BINDING_ID, VadPolicy::Offline) {
        debug!("Wake-word could not start command window: {}", e);
        return None;
    }

    let start = Instant::now();
    let min_open = Duration::from_millis(min_open_ms);
    let silence_timeout = Duration::from_millis(silence_timeout_ms);
    let hard_max = min_open + Duration::from_secs(HARD_MAX_EXTRA_SECS);

    let mut last_voiced_count = rm.voiced_sample_count();
    let mut last_voiced_at = Instant::now();
    let mut saw_speech = false;

    loop {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        if !running.load(Ordering::SeqCst) {
            break;
        }

        // Speech is arriving whenever the voiced-sample counter advances.
        let count = rm.voiced_sample_count();
        if count > last_voiced_count {
            last_voiced_count = count;
            last_voiced_at = Instant::now();
            saw_speech = true;
        }

        let elapsed = start.elapsed();
        // Absolute safety cap first, so a noisy room can never hold the mic open.
        if elapsed >= hard_max {
            debug!("Wake-word command hit hard-max window ({:?})", hard_max);
            break;
        }

        if saw_speech {
            // The user has spoken — end shortly after they go quiet, even if the
            // grace window hasn't fully elapsed, so short commands submit promptly.
            if last_voiced_at.elapsed() >= silence_timeout {
                debug!(
                    "Wake-word command ended after {:?} of silence",
                    silence_timeout
                );
                break;
            }
        } else {
            // No speech yet — keep waiting through the pre-speech grace window,
            // then give up if the user still hasn't said anything.
            if elapsed >= min_open {
                debug!(
                    "Wake-word command: no speech within {:?}; giving up",
                    min_open
                );
                break;
            }
        }
    }

    rm.stop_recording(WAKE_BINDING_ID, cancel_generation)
}

/// On a wake-word hit, either dictate the text that already followed the phrase in
/// the same utterance, or capture the NEXT utterance and dictate that. Runs the
/// shared finish tail so cleanup/injection/analytics match the hotkey path.
fn handle_wake(
    app: &AppHandle,
    rm: &Arc<AudioRecordingManager>,
    tm: &Arc<TranscriptionManager>,
    command: String,
    wake_samples_len: usize,
    running: &Arc<AtomicBool>,
) {
    let post_process = get_settings(app).post_process_enabled;
    let cancel_generation = rm.cancel_generation();

    let trimmed = command.trim();
    if !trimmed.is_empty() {
        // Substantive text after the wake phrase in the same utterance — inject it.
        let app = app.clone();
        let text = trimmed.to_string();
        // Block so the loop stays serial and never contends with itself over the
        // transcription engine while a paste is in flight.
        tauri::async_runtime::block_on(finish_dictation(
            &app,
            text,
            post_process,
            wake_samples_len,
            0,
            None,
            cancel_generation,
        ));
        return;
    }

    // Nothing meaningful after the wake word: keep the mic open for the command
    // using smart, VAD-driven capture. It waits up to `wake_word_listen_seconds`
    // for the user to start talking (so a thinking pause never cuts them off),
    // then keeps extending for as long as speech keeps arriving, and stops once
    // the user has been silent for `wake_word_silence_timeout_ms`. This fixes the
    // old "cut off after a few seconds" / fixed-window feel.
    let settings = get_settings(app);
    let min_open_ms = settings
        .wake_word_listen_seconds
        .clamp(3, 120)
        .saturating_mul(1000);
    let silence_ms = settings.wake_word_silence_timeout_ms.clamp(1000, 15000);
    let samples = match capture_until_silence(rm, running, min_open_ms, silence_ms) {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };

    // Transcribe the command through the user's CONFIGURED STT backend, not the
    // local model. Wake-word *detection* must be local (continuous listening),
    // but the actual command benefits from a fast/accurate remote or self-hosted
    // backend when one is configured — so hands-free honors "local wake word +
    // remote STT". Falls back to the local engine in Local mode.
    let text = match tauri::async_runtime::block_on(transcribe_command(app, tm, samples.clone())) {
        Ok(t) => t,
        Err(e) => {
            error!("Wake-word command transcription failed: {}", e);
            return;
        }
    };

    if text.trim().is_empty() {
        return;
    }

    let app = app.clone();
    let samples_len = samples.len();
    let cancel_generation = rm.cancel_generation();
    tauri::async_runtime::block_on(finish_dictation(
        &app,
        text,
        post_process,
        samples_len,
        0,
        None,
        cancel_generation,
    ));
}

/// Transcribe a captured command through the user's configured STT backend.
/// Wake-word detection always runs on the local model, but the command itself
/// goes to whatever STT the user picked — so a "local wake word + remote STT"
/// (or self-hosted) combination works. Falls back to the local engine in Local
/// mode or if the remote call fails.
async fn transcribe_command(
    app: &AppHandle,
    tm: &Arc<TranscriptionManager>,
    samples: Vec<f32>,
) -> anyhow::Result<String> {
    let settings = get_settings(app);
    if settings.stt_backend_mode != crate::settings::SttBackendMode::Local {
        match crate::backends::stt_http::transcribe(&settings, &samples).await {
            Ok(outcome) => return Ok(outcome.text),
            Err(e) => {
                // Non-fatal: fall back to the local engine so the command is not lost.
                debug!("Wake-word remote STT failed ({e}); falling back to local");
            }
        }
    }
    tm.transcribe(samples)
}

/// Normalize text for matching: lowercase, drop punctuation, collapse whitespace.
fn normalize(s: &str) -> String {
    let cleaned: String = s
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fuzzy-match the wake phrase at/near the start of `transcript`. Returns the
/// text AFTER the wake phrase (possibly empty) on a match, or `None` otherwise.
///
/// Strategy: (1) a cheap exact "contains near the start" check that also lets us
/// slice the true command tail, then (2) a Jaro-Winkler similarity check on the
/// leading token window of the same length as the wake phrase, gated by the
/// sensitivity threshold.
fn match_wake_word(transcript: &str, wake_word: &str, sensitivity: f32) -> Option<String> {
    let norm_w = normalize(wake_word);
    let norm_t = normalize(transcript);
    if norm_w.is_empty() || norm_t.is_empty() {
        return None;
    }

    // 1. Exact substring at/near the start.
    if let Some(pos) = norm_t.find(&norm_w) {
        if pos <= 3 {
            let after = norm_t[pos + norm_w.len()..].trim().to_string();
            return Some(after);
        }
    }

    // 2. Fuzzy match over the leading window (same token count as the phrase).
    let wake_tokens: Vec<&str> = norm_w.split(' ').collect();
    let t_tokens: Vec<&str> = norm_t.split(' ').collect();
    let n = wake_tokens.len().min(t_tokens.len());
    if n == 0 {
        return None;
    }
    let candidate = t_tokens[..n].join(" ");
    let sim = strsim::jaro_winkler(&candidate, &norm_w);
    if sim >= sensitivity as f64 {
        let after = t_tokens[n..].join(" ");
        return Some(after);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_wake_word_with_command() {
        let cmd = match_wake_word("Hey Flow, open the door.", "hey flow", 0.8);
        assert_eq!(cmd, Some("open the door".to_string()));
    }

    #[test]
    fn matches_wake_word_only() {
        let cmd = match_wake_word("hey flow", "hey flow", 0.8);
        assert_eq!(cmd, Some(String::new()));
    }

    #[test]
    fn matches_fuzzy_misrecognition() {
        // Whisper mishears the phrase slightly; Jaro-Winkler should still accept.
        let cmd = match_wake_word("hey flo take a note", "hey flow", 0.8);
        assert!(cmd.is_some());
        assert_eq!(cmd.unwrap(), "take a note".to_string());
    }

    #[test]
    fn rejects_unrelated_speech() {
        assert!(match_wake_word("the weather is nice today", "hey flow", 0.8).is_none());
    }

    #[test]
    fn empty_inputs_do_not_match() {
        assert!(match_wake_word("", "hey flow", 0.8).is_none());
        assert!(match_wake_word("hey flow", "", 0.8).is_none());
    }
}
