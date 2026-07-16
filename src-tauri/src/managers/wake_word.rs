//! Hands-free / wake-word listener (v2 — continuous, always-open detection).
//!
//! When enabled, [`WakeWordManager`] puts the shared [`AudioRecordingManager`]
//! into always-open **monitor mode** (see `recorder.rs`) and runs two background
//! threads:
//!
//! 1. A **segmenter** thread that owns the wake-frame channel. Monitor mode
//!    forwards only VAD-passed (voiced) 16 kHz frames, so the segmenter simply
//!    accumulates a rolling buffer and ships it as one utterance whenever it sees
//!    a ≥700 ms voiced gap (no frame for that long) or hits a 6 s cap. Delivery
//!    to the worker is non-blocking (`try_send`) — if the worker is busy the
//!    segmenter keeps accumulating (bounded by the cap) rather than blocking
//!    capture. The buffer is reset after every shipped segment and whenever a
//!    manual dictation is in progress (monitor delivery is auto-suppressed then).
//! 2. A **transcribe worker** thread (serial) that transcribes each segment with
//!    the local [`TranscriptionManager`] and fuzzy-matches the user-configurable
//!    wake phrase (default "hey flow"). On a match it feeds the following speech
//!    through the exact same clean → inject → log path a manual hotkey dictation
//!    uses ([`crate::actions::finish_dictation`]).
//!
//! This replaces v1's fixed-window "record 4s → transcribe → sleep" polling loop.
//! Detection is now driven by the utterance boundaries themselves; the mic is
//! never re-opened per cycle. (A dedicated lightweight wake-word model —
//! openWakeWord / Porcupine — would cut CPU further and is the obvious Phase-2
//! optimization; still out of scope to avoid a new ML dependency.)
//!
//! Once the wake phrase is matched, the command that follows is captured with
//! *smart, VAD-driven* timing ([`capture_until_silence`]): it waits up to a
//! user-configurable grace window (`wake_word_listen_seconds`) for the user to
//! start talking, then keeps the mic open for as long as speech keeps arriving,
//! stopping a configurable silence gap (`wake_word_silence_timeout_ms`) after they
//! finish — so short commands submit promptly and long ones are never cut off
//! mid-thought.
//!
//! Failure handling: wake-word detection ALWAYS needs a local STT model (even for
//! users who dictate via a remote backend). When the selected local model is
//! missing/unloadable the worker would otherwise fail on every segment, so we
//! surface the problem: on the first failure of a streak we emit a
//! `hands-free-error` Tauri event (payload `"model"` / `"transcription"` /
//! `"microphone"`) the UI turns into a banner, and after a few consecutive
//! failures we back off hard ([`PERSISTENT_ERROR_SLEEP`]). A `"microphone"` error
//! is surfaced when the stream can't be opened at monitor start.

use crate::actions::finish_dictation;
use crate::audio_feedback::{play_voice_cue, play_voice_cue_blocking, VoiceCue};
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::get_settings;
use log::{debug, error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Synthetic binding id used when the wake-word worker drives the shared recorder
/// for command capture, so it never collides with a real user hotkey binding.
const WAKE_BINDING_ID: &str = "__wake_word__";

/// How often the smart command capture polls the live voiced-sample counter to
/// decide whether the user is still speaking.
const POLL_MS: u64 = 100;

/// Absolute safety cap added on top of the user's minimum window, so a noisy room
/// (VAD seeing "speech" indefinitely) can never hold the mic open forever.
const HARD_MAX_EXTRA_SECS: u64 = 180;

/// Voiced-gap that ends an utterance: if no voiced frame arrives for this long the
/// segmenter ships whatever it has buffered. Monitor mode only forwards
/// VAD-passed frames, so "no frame" == silence.
const SEGMENT_GAP: Duration = Duration::from_millis(700);

/// Hard cap on one detection segment. Bounds segmenter memory (6 s @ 16 kHz f32 ≈
/// 384 KB) and guarantees a very long monologue still gets transcribed for wake
/// matching rather than growing without bound.
const SEGMENT_CAP_SAMPLES: usize = (WHISPER_SAMPLE_RATE as usize) * 6;

/// How long the segmenter blocks waiting for the next voiced frame before
/// re-evaluating the gap/cap boundary and the running flag. Keeps the thread
/// responsive to `stop()` (worst-case exit latency).
const SEGMENTER_RECV_TIMEOUT: Duration = Duration::from_millis(100);

/// How long the worker blocks waiting for the next segment before re-checking the
/// running flag, so it exits promptly on disable.
const WORKER_RECV_TIMEOUT: Duration = Duration::from_millis(200);

/// Defensive suppression window at the very start of command capture: ignore
/// voiced-count growth for this long so the tail of the "Got it" ack (or output
/// device latency) can never be mistaken for the user starting to speak.
const CAPTURE_SUPPRESSION_PREFIX: Duration = Duration::from_millis(150);

/// Longer backoff after an error (e.g. no local model loaded) to avoid log spam.
const ERROR_SLEEP: Duration = Duration::from_millis(1500);

/// Number of consecutive per-cycle failures that flips the loop from the normal
/// [`ERROR_SLEEP`] back-off to the much longer [`PERSISTENT_ERROR_SLEEP`].
const PERSISTENT_ERROR_THRESHOLD: u32 = 3;

/// Back-off once a failure looks persistent (e.g. the selected local model is
/// missing and every `transcribe` fails). Far longer than [`ERROR_SLEEP`] so a
/// hopeless loop stops reopening the mic every ~5s forever until the user fixes
/// the underlying problem (which restarts the listener fresh).
const PERSISTENT_ERROR_SLEEP: Duration = Duration::from_secs(30);

/// Emit the `hands-free-error` event the settings UI turns into a banner. `kind`
/// is one of `"model"`, `"transcription"`, or `"microphone"`. Matches the plain
/// string-payload emit pattern used elsewhere (e.g. `actions.rs`
/// `"transcription-error"`).
fn emit_hands_free_error(app: &AppHandle, kind: &str) {
    let _ = app.emit("hands-free-error", kind);
}

/// Classify a transcription error string into the `hands-free-error` payload the
/// UI maps to a message. A missing/unloadable local model surfaces as `"model"`
/// (the common hands-free failure — detection always needs a local model); any
/// other failure is a generic `"transcription"` error.
fn classify_wake_error(err: &str) -> &'static str {
    if err.to_lowercase().contains("model") {
        "model"
    } else {
        "transcription"
    }
}

pub struct WakeWordManager {
    app: AppHandle,
    /// Source-of-truth run flag shared with the segmenter + worker threads. Set
    /// false on `stop()`; both threads observe it within their recv-timeout.
    running: Arc<AtomicBool>,
    /// Join handle for the supervisor thread (which owns the segmenter loop and
    /// joins the worker). Held so `stop()` can join it and guarantee a clean
    /// teardown — no leaked threads across repeated toggles.
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl WakeWordManager {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            running: Arc::new(AtomicBool::new(false)),
            supervisor: Mutex::new(None),
        }
    }

    #[allow(dead_code)] // Exposed for callers/diagnostics; not yet wired to a command.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Start always-open monitoring + the segmenter/worker threads. Idempotent: a
    /// second call while already running is a no-op.
    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            debug!("Wake-word listener already running");
            return;
        }
        let app = self.app.clone();
        let running = Arc::clone(&self.running);
        let handle = std::thread::spawn(move || {
            info!("Wake-word listener started");
            // Fully defensive; any error inside logs and the loop continues/exits.
            run_hands_free(app, running);
            info!("Wake-word listener stopped");
        });
        *self.supervisor.lock().unwrap() = Some(handle);
    }

    /// Signal the threads to stop and block until they have fully wound down (both
    /// the segmenter and worker exit within their recv timeouts), so repeated
    /// enable/disable toggles never leak a thread. Idempotent.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.supervisor.lock().unwrap().take() {
            let _ = handle.join();
        }
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

/// Supervisor: bring up always-open monitoring, spawn the transcribe worker, run
/// the segmenter inline, then tear everything down cleanly on stop.
fn run_hands_free(app: AppHandle, running: Arc<AtomicBool>) {
    let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());

    // 1. Put the recorder into always-open monitor mode. A failure here is almost
    //    always the microphone stream not opening, so surface it as "microphone".
    if let Err(e) = rm.start_monitoring() {
        error!("Hands-free could not start monitoring: {}", e);
        emit_hands_free_error(&app, "microphone");
        running.store(false, Ordering::SeqCst);
        return;
    }

    // 2. Take the wake-frame receiver for the segmenter. If it's missing (already
    //    taken), abort cleanly.
    let wake_rx = match rm.take_wake_receiver() {
        Some(rx) => rx,
        None => {
            error!("Hands-free wake receiver unavailable; aborting");
            rm.stop_monitoring();
            running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // 3. Segment channel: capacity 1 so the segmenter can non-blockingly hand off
    //    at most one pending utterance while the worker transcribes the previous.
    let (seg_tx, seg_rx) = mpsc::sync_channel::<Vec<f32>>(1);

    // 4. Transcribe worker (serial). Owns the segment receiver.
    let worker = {
        let app = app.clone();
        let running = Arc::clone(&running);
        std::thread::spawn(move || transcribe_worker(app, running, seg_rx))
    };

    // 5. Run the segmenter on this thread until stopped; it returns the receiver.
    let wake_rx = segmenter_loop(&app, &rm, &running, wake_rx, &seg_tx);

    // 6. Teardown: dropping seg_tx lets the worker's recv disconnect; join it,
    //    stop monitoring, and hand the receiver back for a future re-enable.
    drop(seg_tx);
    let _ = worker.join();
    rm.stop_monitoring();
    rm.return_wake_receiver(wake_rx);
    running.store(false, Ordering::SeqCst);
}

/// Pure segment-boundary decision, extracted for unit testing. Ship the buffered
/// utterance when there is something buffered AND either the user has gone quiet
/// for `gap` (no voiced frame that long) or the buffer has reached `cap`.
fn should_close_segment(
    buffered_samples: usize,
    since_last_voiced: Duration,
    gap: Duration,
    cap_samples: usize,
) -> bool {
    buffered_samples > 0 && (since_last_voiced >= gap || buffered_samples >= cap_samples)
}

/// Segmenter thread body. Owns the wake-frame receiver, accumulates a rolling
/// buffer of VAD-passed frames, and ships one segment per utterance. Returns the
/// receiver on exit so it can be reused after a re-enable.
fn segmenter_loop(
    app: &AppHandle,
    rm: &Arc<AudioRecordingManager>,
    running: &Arc<AtomicBool>,
    wake_rx: mpsc::Receiver<Vec<f32>>,
    seg_tx: &mpsc::SyncSender<Vec<f32>>,
) -> mpsc::Receiver<Vec<f32>> {
    let mut buf: Vec<f32> = Vec::new();
    // Time of the last voiced frame; `None` when the buffer is empty.
    let mut last_voiced_at: Option<Instant> = None;

    while running.load(Ordering::SeqCst) {
        // Honor an out-of-band disable (setting flipped elsewhere).
        if !get_settings(app).hands_free_enabled {
            break;
        }

        // While a manual dictation is in progress the recorder suppresses monitor
        // delivery, so drop any partial buffer to avoid stitching a pre-recording
        // fragment onto post-recording audio.
        if rm.is_recording() && !buf.is_empty() {
            buf.clear();
            last_voiced_at = None;
        }

        match wake_rx.recv_timeout(SEGMENTER_RECV_TIMEOUT) {
            Ok(frame) => {
                // Accumulate up to the cap; past it we stop growing but keep the
                // buffer so the cap boundary below can ship it.
                if buf.len() < SEGMENT_CAP_SAMPLES {
                    buf.extend_from_slice(&frame);
                }
                last_voiced_at = Some(Instant::now());
            }
            Err(RecvTimeoutError::Timeout) => { /* no voiced frame — gap grows */ }
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let since_last_voiced = last_voiced_at.map(|t| t.elapsed()).unwrap_or_default();
        if should_close_segment(
            buf.len(),
            since_last_voiced,
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES,
        ) {
            match seg_tx.try_send(buf.clone()) {
                Ok(()) => {
                    // Shipped — reset for the next utterance.
                    buf.clear();
                    last_voiced_at = None;
                }
                Err(TrySendError::Full(_)) => {
                    // Worker still busy with the previous segment. Keep the buffer
                    // and retry next iteration (bounded by the cap so it can't
                    // grow without limit). Reset the gap clock so we don't spin on
                    // an already-elapsed gap while waiting for the worker.
                    last_voiced_at = Some(Instant::now());
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    }

    wake_rx
}

/// Transcribe worker: serially transcribe each shipped segment, fuzzy-match the
/// wake phrase, and on a hit run the shared wake handler. Owns the failure-streak
/// state + `hands-free-error` emission (moved here from the v1 loop).
fn transcribe_worker(app: AppHandle, running: Arc<AtomicBool>, seg_rx: mpsc::Receiver<Vec<f32>>) {
    // Failure-streak state. Fresh per worker start, so re-enabling hands-free
    // begins with a clean slate. `*_emitted` de-dupes the event to once per streak.
    let mut transcription_failures: u32 = 0;
    let mut transcription_error_emitted = false;

    while running.load(Ordering::SeqCst) {
        let samples = match seg_rx.recv_timeout(WORKER_RECV_TIMEOUT) {
            Ok(s) => s,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if samples.is_empty() {
            continue;
        }

        let settings = get_settings(&app);
        if !settings.hands_free_enabled {
            break;
        }

        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());

        // Ensure the local STT model is loading/loaded; transcribe() waits for an
        // in-flight load. Wake-word detection requires a local model regardless of
        // the STT backend used for the actual dictation.
        tm.initiate_model_load();

        let transcript = match tm.transcribe(samples.clone()) {
            Ok(t) => {
                transcription_failures = 0;
                transcription_error_emitted = false;
                t
            }
            Err(e) => {
                // The common cause is a missing/unloadable local model (detection
                // always needs one, even for remote-STT users). Surface it once per
                // streak and back off harder once it looks persistent.
                transcription_failures = transcription_failures.saturating_add(1);
                if !transcription_error_emitted {
                    emit_hands_free_error(&app, classify_wake_error(&e.to_string()));
                    transcription_error_emitted = true;
                }
                debug!("Wake-word segment transcription failed: {}", e);
                let backoff = if transcription_failures >= PERSISTENT_ERROR_THRESHOLD {
                    PERSISTENT_ERROR_SLEEP
                } else {
                    ERROR_SLEEP
                };
                std::thread::sleep(backoff);
                continue;
            }
        };

        if transcript.trim().is_empty() {
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
    }
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
            // Always absorb the delta so a burst during the suppression window
            // isn't counted later, but only treat growth as real speech once the
            // ~150ms suppression prefix has elapsed. This guards against the tail
            // of the blocking "Got it" ack (or output-device latency) being
            // mistaken for the user starting to talk. (hands-free v2 addition)
            last_voiced_count = count;
            if start.elapsed() >= CAPTURE_SUPPRESSION_PREFIX {
                last_voiced_at = Instant::now();
                saw_speech = true;
            }
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
        // Voice cue right before injection (nothing is capturing here, so async is
        // fine and won't self-capture). (hands-free v2 addition)
        play_voice_cue(&app, VoiceCue::Typing);
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
            None,
            None,
            None,
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
    // Blocking "Got it" ack BEFORE the command mic opens. Because it blocks, the
    // cue has fully finished playing before capture_until_silence starts the
    // recorder, so it physically cannot leak into the capture (speakers are live
    // during wake capture). It also gives the user a clear cue to start speaking.
    // (hands-free v2 addition)
    play_voice_cue_blocking(app, VoiceCue::Ack);
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
    // Voice cue right before injection; capture has already ended so async is fine.
    // (hands-free v2 addition)
    play_voice_cue(&app, VoiceCue::Typing);
    tauri::async_runtime::block_on(finish_dictation(
        &app,
        text,
        post_process,
        samples_len,
        0,
        None,
        cancel_generation,
        None,
        None,
        None,
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
            Ok(outcome) => {
                // Remote STT bypassed dictionary correction entirely until now —
                // run the same post-processing hook the local engine applies
                // internally in `TranscriptionManager::transcribe`/`finalize_stream`.
                // `outcome.prompted` mirrors whisper's local `initial_prompt` gate:
                // when the dictionary words were already sent to the engine as a
                // biasing hint, skip the redundant fuzzy pass but still run
                // deterministic aliases.
                return Ok(
                    crate::managers::transcription::post_process_transcription_text(
                        outcome.text,
                        &settings,
                        outcome.prompted,
                    ),
                );
            }
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

    #[test]
    fn classify_wake_error_flags_model_failures() {
        // The real-world missing-model failure string.
        assert_eq!(
            classify_wake_error("model load failed: gguf load error (status 4)"),
            "model"
        );
        // Case-insensitive.
        assert_eq!(classify_wake_error("MODEL not found"), "model");
        assert_eq!(classify_wake_error("Failed to load Model file"), "model");
    }

    #[test]
    fn classify_wake_error_defaults_to_transcription() {
        assert_eq!(
            classify_wake_error("engine returned empty output"),
            "transcription"
        );
        assert_eq!(classify_wake_error(""), "transcription");
    }

    #[test]
    fn empty_buffer_never_closes() {
        // Nothing buffered — never ship, regardless of gap/cap.
        assert!(!should_close_segment(
            0,
            Duration::from_secs(10),
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES
        ));
    }

    #[test]
    fn closes_on_voiced_gap() {
        // Buffered audio + silence >= gap => ship.
        assert!(should_close_segment(
            1000,
            SEGMENT_GAP,
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES
        ));
        // Just under the gap => keep waiting.
        assert!(!should_close_segment(
            1000,
            SEGMENT_GAP - Duration::from_millis(1),
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES
        ));
    }

    #[test]
    fn closes_on_cap_even_without_gap() {
        // At the cap, ship immediately even though speech is still arriving
        // (since_last_voiced == 0).
        assert!(should_close_segment(
            SEGMENT_CAP_SAMPLES,
            Duration::from_millis(0),
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES
        ));
    }

    #[test]
    fn keeps_accumulating_below_cap_while_speaking() {
        // Below cap and still speaking (no gap) => keep accumulating.
        assert!(!should_close_segment(
            SEGMENT_CAP_SAMPLES - 1,
            Duration::from_millis(0),
            SEGMENT_GAP,
            SEGMENT_CAP_SAMPLES
        ));
    }
}
