use crate::active_app;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error, VadPolicy};
use crate::managers::analytics::{AnalyticsManager, DictationEvent};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::StreamWorkKind;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, AppSettings, OverlayStyle, APPLE_INTELLIGENCE_PROVIDER_ID};
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
use tauri::{AppHandle, Emitter};

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Drop guard that notifies the [`TranscriptionCoordinator`] when the
/// transcription pipeline finishes — whether it completes normally or panics.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
    }
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction {
    post_process: bool,
}

/// Field name for structured output JSON schema
const TRANSCRIPTION_FIELD: &str = "transcription";

/// Strip invisible Unicode characters that some LLMs may insert
fn strip_invisible_chars(s: &str) -> String {
    s.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Build a system prompt from the user's prompt template.
/// Removes `${output}` placeholder since the transcription is sent as the user message.
fn build_system_prompt(prompt_template: &str) -> String {
    prompt_template.replace("${output}", "").trim().to_string()
}

/// Returns `true` when a transcription has no meaningful content to
/// post-process (empty or whitespace-only). Used to skip the post-processing
/// LLM call when nothing was actually transcribed, which would otherwise make
/// the model reply with an error message such as "you need to provide the
/// transcription".
fn is_blank_transcription(transcription: &str) -> bool {
    transcription.trim().is_empty()
}

/// Resolve a per-app cleanup prompt for the given active-app name. Matches
/// case-insensitively, either exactly or by containment in either direction
/// (so "Code" matches "Visual Studio Code" and vice versa). Empty prompt values
/// and an "unknown"/empty app name are ignored. Returns `None` when nothing
/// matches, so callers fall back to the default selected prompt.
fn resolve_per_app_prompt(settings: &AppSettings, app_name: &str) -> Option<String> {
    let name = app_name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("unknown") {
        return None;
    }
    let lower = name.to_lowercase();
    settings
        .per_app_prompts
        .iter()
        .filter(|(_, prompt)| !prompt.trim().is_empty())
        .find(|(key, _)| {
            let key_lower = key.trim().to_lowercase();
            !key_lower.is_empty()
                && (lower == key_lower || lower.contains(&key_lower) || key_lower.contains(&lower))
        })
        .map(|(_, prompt)| prompt.clone())
}

async fn post_process_transcription(settings: &AppSettings, transcription: &str) -> Option<String> {
    if is_blank_transcription(transcription) {
        debug!("Post-processing skipped because the transcription is empty");
        return None;
    }

    let provider = match settings.active_post_process_provider().cloned() {
        Some(provider) => provider,
        None => {
            debug!("Post-processing enabled but no provider is selected");
            return None;
        }
    };

    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    if model.trim().is_empty() {
        debug!(
            "Post-processing skipped because provider '{}' has no model configured",
            provider.id
        );
        return None;
    }

    // Per-app tone override: if the frontmost app matches a configured
    // per_app_prompts entry, use that prompt instead of the selected default.
    // Defensive — an "unknown" or unmatched app falls through to the default.
    let per_app_prompt = {
        let active = crate::active_app::current();
        resolve_per_app_prompt(settings, &active.app_name)
    };

    let prompt = if let Some(prompt) = per_app_prompt {
        debug!("Using per-app cleanup prompt override for the active application");
        prompt
    } else {
        let selected_prompt_id = match &settings.post_process_selected_prompt_id {
            Some(id) => id.clone(),
            None => {
                debug!("Post-processing skipped because no prompt is selected");
                return None;
            }
        };

        match settings
            .post_process_prompts
            .iter()
            .find(|prompt| prompt.id == selected_prompt_id)
        {
            Some(prompt) => prompt.prompt.clone(),
            None => {
                debug!(
                    "Post-processing skipped because prompt '{}' was not found",
                    selected_prompt_id
                );
                return None;
            }
        }
    };

    if prompt.trim().is_empty() {
        debug!("Post-processing skipped because the selected prompt is empty");
        return None;
    }

    debug!(
        "Starting LLM post-processing with provider '{}' (model: {})",
        provider.id, model
    );

    // OpenFlow: prefer the keychain; fall back to any legacy plaintext value.
    let api_key = crate::keychain::get_api_key("cleanup", &provider.id)
        .filter(|k| !k.is_empty())
        .or_else(|| settings.post_process_api_keys.get(&provider.id).cloned())
        .unwrap_or_default();

    // Disable reasoning for providers where post-processing rarely benefits from it.
    // - custom: top-level reasoning_effort (works for local OpenAI-compat servers)
    // - openrouter: nested reasoning object; exclude:true also keeps reasoning text
    //   out of the response so it can't pollute structured-output JSON parsing
    let (reasoning_effort, reasoning) = match provider.id.as_str() {
        "custom" => (Some("none".to_string()), None),
        "openrouter" => (
            None,
            Some(crate::llm_client::ReasoningConfig {
                effort: Some("none".to_string()),
                exclude: Some(true),
            }),
        ),
        _ => (None, None),
    };

    if provider.supports_structured_output {
        debug!("Using structured outputs for provider '{}'", provider.id);

        let system_prompt = build_system_prompt(&prompt);
        let user_content = transcription.to_string();

        // Handle Apple Intelligence separately since it uses native Swift APIs
        if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                if !apple_intelligence::check_apple_intelligence_availability() {
                    debug!(
                        "Apple Intelligence selected but not currently available on this device"
                    );
                    return None;
                }

                let token_limit = model.trim().parse::<i32>().unwrap_or(0);
                return match apple_intelligence::process_text_with_system_prompt(
                    &system_prompt,
                    &user_content,
                    token_limit,
                ) {
                    Ok(result) => {
                        if result.trim().is_empty() {
                            debug!("Apple Intelligence returned an empty response");
                            None
                        } else {
                            let result = strip_invisible_chars(&result);
                            debug!(
                                "Apple Intelligence post-processing succeeded. Output length: {} chars",
                                result.len()
                            );
                            Some(result)
                        }
                    }
                    Err(err) => {
                        error!("Apple Intelligence post-processing failed: {}", err);
                        None
                    }
                };
            }

            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                debug!("Apple Intelligence provider selected on unsupported platform");
                return None;
            }
        }

        // Define JSON schema for transcription output
        let json_schema = serde_json::json!({
            "type": "object",
            "properties": {
                (TRANSCRIPTION_FIELD): {
                    "type": "string",
                    "description": "The cleaned and processed transcription text"
                }
            },
            "required": [TRANSCRIPTION_FIELD],
            "additionalProperties": false
        });

        match crate::llm_client::send_chat_completion_with_schema(
            &provider,
            api_key.clone(),
            &model,
            user_content,
            Some(system_prompt),
            Some(json_schema),
            reasoning_effort.clone(),
            reasoning.clone(),
        )
        .await
        {
            Ok(Some(content)) => {
                // Parse the JSON response to extract the transcription field
                match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(json) => {
                        if let Some(transcription_value) =
                            json.get(TRANSCRIPTION_FIELD).and_then(|t| t.as_str())
                        {
                            let result = strip_invisible_chars(transcription_value);
                            debug!(
                                "Structured output post-processing succeeded for provider '{}'. Output length: {} chars",
                                provider.id,
                                result.len()
                            );
                            return Some(result);
                        } else {
                            error!("Structured output response missing 'transcription' field");
                            return Some(strip_invisible_chars(&content));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to parse structured output JSON: {}. Returning raw content.",
                            e
                        );
                        return Some(strip_invisible_chars(&content));
                    }
                }
            }
            Ok(None) => {
                error!("LLM API response has no content");
                return None;
            }
            Err(e) => {
                warn!(
                    "Structured output failed for provider '{}': {}. Falling back to legacy mode.",
                    provider.id, e
                );
                // Fall through to legacy mode below
            }
        }
    }

    // Legacy mode: Replace ${output} variable in the prompt with the actual text
    let processed_prompt = prompt.replace("${output}", transcription);
    debug!("Processed prompt length: {} chars", processed_prompt.len());

    match crate::llm_client::send_chat_completion(
        &provider,
        api_key,
        &model,
        processed_prompt,
        reasoning_effort,
        reasoning,
    )
    .await
    {
        Ok(Some(content)) => {
            let content = strip_invisible_chars(&content);
            debug!(
                "LLM post-processing succeeded for provider '{}'. Output length: {} chars",
                provider.id,
                content.len()
            );
            Some(content)
        }
        Ok(None) => {
            error!("LLM API response has no content");
            None
        }
        Err(e) => {
            error!(
                "LLM post-processing failed for provider '{}': {}. Falling back to original transcription.",
                provider.id,
                e
            );
            None
        }
    }
}

async fn maybe_convert_chinese_variant(
    effective_language: &str,
    transcription: &str,
) -> Option<String> {
    // Gate on the language the model actually transcribed in (the effective
    // language), not the persisted intent. A leftover zh-Hans/zh-Hant intent
    // from a previously selected model must not run OpenCC S2T/T2S over output a
    // non-Chinese model produced — that would silently rewrite any shared CJK
    // characters (e.g. Japanese kanji) in the result.
    let is_simplified = effective_language == "zh-Hans";
    let is_traditional = effective_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("effective language is not Simplified or Traditional Chinese; skipping conversion");
        return None;
    }

    debug!(
        "Starting Chinese variant conversion using OpenCC for language: {}",
        effective_language
    );

    // Use OpenCC to convert based on selected language
    let config = if is_simplified {
        // Convert Traditional Chinese to Simplified Chinese
        BuiltinConfig::Tw2sp
    } else {
        // Convert Simplified Chinese to Traditional Chinese
        BuiltinConfig::S2tw
    };

    match OpenCC::from_config(config) {
        Ok(converter) => {
            let converted = converter.convert(transcription);
            debug!(
                "OpenCC translation completed. Input length: {}, Output length: {}",
                transcription.len(),
                converted.len()
            );
            Some(converted)
        }
        Err(e) => {
            error!("Failed to initialize OpenCC converter: {}. Falling back to original transcription.", e);
            None
        }
    }
}

pub(crate) struct ProcessedTranscription {
    pub final_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
}

/// Resolve the persisted language *intent* into the language the currently-loaded
/// model will actually use — the same capability-aware coercion the transcription
/// paths apply (see [`crate::managers::model::effective_language`]). Post-processing
/// resolves it independently so it agrees with the language the transcription ran
/// in, without threading a value through the pipeline.
fn resolve_effective_language(app: &AppHandle, settings: &AppSettings) -> String {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let model_manager = app.state::<Arc<ModelManager>>();
    let active_model = tm
        .get_current_model()
        .unwrap_or_else(|| settings.selected_model.clone());
    match model_manager.get_model_info(&active_model) {
        Some(info) => crate::managers::model::effective_language(
            &settings.selected_language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => settings.selected_language.clone(),
    }
}

pub(crate) async fn process_transcription_output(
    app: &AppHandle,
    transcription: &str,
    post_process: bool,
) -> ProcessedTranscription {
    let settings = get_settings(app);
    let mut final_text = transcription.to_string();
    let mut post_processed_text: Option<String> = None;
    let mut post_process_prompt: Option<String> = None;

    // Resolve the language the transcription actually ran in (the persisted
    // intent coerced against the loaded model's capabilities) so OpenCC keys off
    // the effective language rather than a possibly-stale intent.
    let effective_language = resolve_effective_language(app, &settings);
    if let Some(converted_text) =
        maybe_convert_chinese_variant(&effective_language, transcription).await
    {
        final_text = converted_text;
    }

    if post_process {
        if let Some(processed_text) = post_process_transcription(&settings, &final_text).await {
            post_processed_text = Some(processed_text.clone());
            final_text = processed_text;

            if let Some(prompt_id) = &settings.post_process_selected_prompt_id {
                if let Some(prompt) = settings
                    .post_process_prompts
                    .iter()
                    .find(|prompt| &prompt.id == prompt_id)
                {
                    post_process_prompt = Some(prompt.prompt.clone());
                }
            }
        }
    } else if final_text != transcription {
        post_processed_text = Some(final_text.clone());
    }

    ProcessedTranscription {
        final_text,
        post_processed_text,
        post_process_prompt,
    }
}

/// Resolve the STT backend label + model string for analytics from settings.
fn stt_backend_fields(settings: &AppSettings) -> (String, String) {
    use crate::settings::SttBackendMode;
    match settings.stt_backend_mode {
        SttBackendMode::Local => ("local".to_string(), settings.selected_model.clone()),
        SttBackendMode::SelfHosted => (
            "selfhosted".to_string(),
            settings.stt_selfhosted_model.clone(),
        ),
        SttBackendMode::Remote => {
            let model = settings
                .stt_models
                .get(&settings.stt_provider_id)
                .cloned()
                .filter(|m| !m.is_empty())
                .or_else(|| {
                    settings
                        .stt_providers
                        .iter()
                        .find(|p| p.id == settings.stt_provider_id)
                        .map(|p| p.default_model.clone())
                })
                .unwrap_or_default();
            (format!("remote:{}", settings.stt_provider_id), model)
        }
    }
}

/// Resolve the cleanup backend label + model for analytics. Returns
/// (`"none"`, "") when this dictation did not run post-processing.
fn cleanup_backend_fields(settings: &AppSettings, post_process: bool) -> (String, String) {
    if !post_process {
        return ("none".to_string(), String::new());
    }
    match settings.active_post_process_provider() {
        Some(provider) => {
            let model = settings
                .post_process_models
                .get(&provider.id)
                .cloned()
                .unwrap_or_default();
            (format!("cleanup:{}", provider.id), model)
        }
        None => ("none".to_string(), String::new()),
    }
}

/// Build a [`DictationEvent`] from the pipeline outputs. `injected_ok` starts
/// `false`; the caller flips it to `true` in the paste success branch.
#[allow(clippy::too_many_arguments)]
fn build_dictation_event(
    settings: &AppSettings,
    raw_text: String,
    cleaned_text: Option<String>,
    final_text: &str,
    audio_ms: i64,
    stt_latency_ms: i64,
    cleanup_latency_ms: i64,
    active: active_app::ActiveApp,
    post_process: bool,
) -> DictationEvent {
    let word_count = final_text.split_whitespace().count() as i64;
    let wpm = if audio_ms > 0 {
        (word_count as f64) / (audio_ms as f64 / 60000.0)
    } else {
        0.0
    };
    let (stt_backend, stt_model) = stt_backend_fields(settings);
    let (cleanup_backend, cleanup_model) = cleanup_backend_fields(settings, post_process);
    let total_latency_ms = stt_latency_ms + cleanup_latency_ms;

    DictationEvent {
        ts: chrono::Utc::now().timestamp(),
        duration_ms: total_latency_ms,
        audio_ms,
        word_count,
        wpm,
        raw_text: Some(raw_text),
        cleaned_text,
        active_app: active.app_name,
        window_title: active.window_title,
        detected_project: active.project,
        language: settings.selected_language.clone(),
        stt_backend,
        stt_model,
        cleanup_backend,
        cleanup_model,
        stt_latency_ms,
        cleanup_latency_ms,
        total_latency_ms,
        injected_ok: false,
    }
}

/// Shared "finish a dictation" tail: clean the raw transcript, optionally persist
/// a history entry, inject the text into the active app, and log a dictation
/// analytics event. This is the single code path used by BOTH the manual hotkey
/// stop ([`TranscribeAction::stop`]) and the hands-free wake-word path
/// ([`crate::managers::wake_word`]), so cleanup, the per-app tone override,
/// injection, and analytics all apply identically no matter how the dictation was
/// triggered.
///
/// - `raw_text` is the raw transcription (pre-cleanup).
/// - `post_process` forces/enables the LLM cleanup step.
/// - `samples_len` is the captured audio sample count (16 kHz mono) used to derive
///   `audio_ms` for analytics; pass `0` when unknown.
/// - `stt_latency_ms` is the measured transcription latency for analytics.
/// - `history_file_name` is `Some(name)` when a WAV was saved and a history entry
///   should reference it; `None` skips the history save (e.g. wake-word, which
///   does not persist a recording).
/// - `cancel_generation` is the recorder cancel token captured before this
///   dictation, so a mid-flight cancel still aborts the paste.
pub(crate) async fn finish_dictation(
    app: &AppHandle,
    raw_text: String,
    post_process: bool,
    samples_len: usize,
    stt_latency_ms: i64,
    history_file_name: Option<String>,
    cancel_generation: u64,
) {
    let ah = app.clone();
    let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
    let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
    let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());
    let am = Arc::clone(&app.state::<Arc<AnalyticsManager>>());
    let style = get_settings(&ah).overlay_style;

    if post_process {
        if style == OverlayStyle::Live {
            tm.emit_stream_working(StreamWorkKind::Polishing);
        } else {
            show_processing_overlay(&ah);
        }
    }

    let cleanup_start = Instant::now();
    let processed = process_transcription_output(&ah, &raw_text, post_process).await;
    let cleanup_latency_ms = if post_process {
        cleanup_start.elapsed().as_millis() as i64
    } else {
        0
    };
    let cleaned_for_analytics = processed.post_processed_text.clone();

    if rm.was_cancelled_since(cancel_generation) {
        debug!("Transcription operation cancelled before paste");
        utils::hide_recording_overlay(&ah);
        change_tray_icon(&ah, TrayIconState::Idle);
        return;
    }

    // Save to history if a recording file was persisted for this dictation.
    if let Some(file_name) = history_file_name {
        if let Err(err) = hm.save_entry(
            file_name,
            raw_text.clone(),
            post_process,
            processed.post_processed_text.clone(),
            processed.post_process_prompt.clone(),
        ) {
            error!("Failed to save history entry: {}", err);
        }
    }

    if processed.final_text.is_empty() {
        utils::hide_recording_overlay(&ah);
        change_tray_icon(&ah, TrayIconState::Idle);
        return;
    }

    let ah_clone = ah.clone();
    let paste_time = Instant::now();
    let final_text = processed.final_text;
    let rm_for_paste = Arc::clone(&rm);

    // Build the analytics event on this async task thread. The active-app lookup
    // can shell out to osascript, so it must never run on the main (paste)
    // thread. injected_ok is flipped to true only when the paste succeeds. Off
    // mode short-circuits inside log_event.
    let settings_for_analytics = get_settings(&ah);
    let privacy = settings_for_analytics.analytics_privacy;
    let audio_ms = (samples_len as i64) * 1000 / 16_000;
    let active = active_app::current();
    let dictation_event = build_dictation_event(
        &settings_for_analytics,
        raw_text,
        cleaned_for_analytics,
        &final_text,
        audio_ms,
        stt_latency_ms,
        cleanup_latency_ms,
        active,
        post_process,
    );
    let am_for_paste = Arc::clone(&am);

    ah.run_on_main_thread(move || {
        if rm_for_paste.was_cancelled_since(cancel_generation) {
            debug!("Transcription operation cancelled before paste");
            utils::hide_recording_overlay(&ah_clone);
            change_tray_icon(&ah_clone, TrayIconState::Idle);
            return;
        }

        match utils::paste(final_text, ah_clone.clone()) {
            Ok(()) => {
                debug!("Text pasted successfully in {:?}", paste_time.elapsed());
                // Non-fatal analytics logging: never panics or blocks; errors are
                // logged inside log_event.
                let mut ev = dictation_event;
                ev.injected_ok = true;
                am_for_paste.log_event(ev, privacy);
            }
            Err(e) => {
                error!("Failed to paste transcription: {}", e);
                let _ = ah_clone.emit("paste-error", ());
            }
        }
        utils::hide_recording_overlay(&ah_clone);
        change_tray_icon(&ah_clone, TrayIconState::Idle);
    })
    .unwrap_or_else(|e| {
        error!("Failed to run paste on main thread: {:?}", e);
        utils::hide_recording_overlay(&ah);
        change_tray_icon(&ah, TrayIconState::Idle);
    });
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel. Skip local-model load when STT
        // runs over HTTP — there may be no local model downloaded at all, and we
        // don't want to pay the load for audio we'll send to a remote endpoint.
        if get_settings(app).stt_backend_mode == crate::settings::SttBackendMode::Local {
            tm.initiate_model_load();
        }
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });

        let binding_id = binding_id.to_string();
        change_tray_icon(app, TrayIconState::Recording);

        // Get the microphone mode to determine audio feedback timing
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;

        let selected_model_info = app
            .state::<Arc<ModelManager>>()
            .get_model_info(&settings.selected_model);

        // Use the app-facing model capability as the single pre-recording source
        // for live streaming decisions. Unknown support is represented as false
        // until the model registry is updated by discovery or runtime load.
        // OpenFlow: only the on-device engine streams. For remote/self-hosted STT
        // we buffer the whole utterance and POST it on release, so streaming stays
        // off regardless of the model's advertised capability.
        let stt_is_local = settings.stt_backend_mode == crate::settings::SttBackendMode::Local;
        let model_supports_streaming = stt_is_local
            && selected_model_info
                .as_ref()
                .map(|m| m.supports_streaming)
                .unwrap_or(false);
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        if model_supports_streaming {
            tm.start_stream();
        }

        // Sizing the overlay follows the same advertised capability. A model that
        // doesn't stream (or whose capability is not known yet) gets the compact
        // pill instead of an oversized transparent live window.
        match settings.overlay_style {
            OverlayStyle::Live if model_supports_streaming => utils::show_streaming_overlay(app),
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            OverlayStyle::None => {} // show_overlay_state no-ops on None anyway
        }
        debug!("Microphone mode - always_on: {}", is_always_on);

        let mut recording_error: Option<String> = None;
        if is_always_on {
            // Always-on mode: Play audio feedback immediately, then apply mute after sound finishes
            debug!("Always-on mode: Playing audio feedback immediately");
            let rm_clone = Arc::clone(&rm);
            let app_clone = app.clone();
            // The blocking helper exits immediately if audio feedback is disabled,
            // so we can always reuse this thread to ensure mute happens right after playback.
            std::thread::spawn(move || {
                play_feedback_sound_blocking(&app_clone, SoundType::Start);
                rm_clone.apply_mute();
            });

            if let Err(e) = rm.try_start_recording(&binding_id, vad_policy) {
                debug!("Recording failed: {}", e);
                recording_error = Some(e);
            }
        } else {
            // On-demand mode: Start recording first, then play audio feedback, then apply mute
            // This allows the microphone to be activated before playing the sound
            debug!("On-demand mode: Starting recording first, then audio feedback");
            let recording_start_time = Instant::now();
            match rm.try_start_recording(&binding_id, vad_policy) {
                Ok(()) => {
                    debug!("Recording started in {:?}", recording_start_time.elapsed());
                    // Small delay to ensure microphone stream is active
                    let app_clone = app.clone();
                    let rm_clone = Arc::clone(&rm);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        debug!("Handling delayed audio feedback/mute sequence");
                        // Helper handles disabled audio feedback by returning early, so we reuse it
                        // to keep mute sequencing consistent in every mode.
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                        rm_clone.apply_mute();
                    });
                }
                Err(e) => {
                    debug!("Failed to start recording: {}", e);
                    recording_error = Some(e);
                }
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else if is_no_input_device_error(&err) {
                    "no_input_device"
                } else {
                    "unknown"
                };
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(err),
                    },
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        change_tray_icon(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        match (style, tm.is_streaming()) {
            (OverlayStyle::Live, true) => {
                tm.emit_stream_working(StreamWorkKind::Transcribing);
            }
            _ => show_transcribing_overlay(app),
        }

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
                                                 // Cleanup is applied when either the dedicated post-processing binding was
                                                 // used (`self.post_process` forces it, even if the toggle is off) or the
                                                 // user has enabled cleanup globally for the main transcribe hotkey. The
                                                 // effective value drives the overlay "Polishing" state, the actual cleanup
                                                 // call, and the history entry's post_process flag alike.
        let post_process = self.post_process || get_settings(app).post_process_enabled;
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Transcription operation cancelled after recording stop");
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                    return;
                }

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping persistence");
                    // Tear down any streaming worker so its channel doesn't leak
                    // and block the next start_stream.
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                } else {
                    // Save WAV concurrently with transcription
                    let sample_count = samples.len();
                    let file_name = format!("handy-{}.wav", chrono::Utc::now().timestamp());
                    let wav_path = hm.recordings_dir().join(&file_name);
                    let wav_path_for_verify = wav_path.clone();
                    let samples_for_wav = samples.clone();
                    let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                        crate::audio_toolkit::save_wav_file(&wav_path, &samples_for_wav)
                    });

                    // Transcribe concurrently with WAV save. OpenFlow: when the STT
                    // backend is remote/self-hosted, POST the audio to the HTTP
                    // endpoint instead of running the local engine (streaming is not
                    // started for those modes, so there is nothing to finalize). Local
                    // mode keeps OpenFlow's stream-finalize-then-batch fallback.
                    let transcription_time = Instant::now();
                    let stt_mode = get_settings(&ah).stt_backend_mode;
                    let transcription_result = if stt_mode != crate::settings::SttBackendMode::Local
                    {
                        let settings = get_settings(&ah);
                        match crate::backends::stt_http::transcribe(&settings, &samples).await {
                            Ok(outcome) => {
                                debug!(
                                    "Remote STT ({}) returned {} chars in {}ms",
                                    outcome.backend,
                                    outcome.text.len(),
                                    outcome.latency_ms
                                );
                                Ok(outcome.text)
                            }
                            Err(e) => Err(anyhow::anyhow!("Remote STT failed: {e}")),
                        }
                    } else {
                        // Language selection (item M3.4): the local engine honors the
                        // persisted `selected_language` setting, coerced against the
                        // loaded model's capabilities in `TranscriptionManager`
                        // (see `effective_language_for_model`). Whisper-family models
                        // are multilingual and transcribe/translate in that language;
                        // Parakeet is English-only and ignores non-English selections.
                        match tm.finalize_stream() {
                            // A finalized stream with usable text wins. An empty result
                            // (no active stream, produced nothing, or a finalize error
                            // after the engine was returned) falls back to a full batch
                            // transcription of the same audio. A finalize timeout is
                            // surfaced instead — the worker may still hold the engine,
                            // so a batch fallback would contend with it.
                            Ok(Some(text)) if !text.trim().is_empty() => Ok(text),
                            Ok(_) => tm.transcribe(samples),
                            Err(err) => Err(err),
                        }
                    };

                    // Await WAV save and verify
                    let wav_saved = match wav_handle.await {
                        Ok(Ok(())) => {
                            match crate::audio_toolkit::verify_wav_file(
                                &wav_path_for_verify,
                                sample_count,
                            ) {
                                Ok(()) => true,
                                Err(e) => {
                                    error!("WAV verification failed: {}", e);
                                    false
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            error!("Failed to save WAV file: {}", e);
                            false
                        }
                        Err(e) => {
                            error!("WAV save task panicked: {}", e);
                            false
                        }
                    };

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled before output handling");
                        utils::hide_recording_overlay(&ah);
                        change_tray_icon(&ah, TrayIconState::Idle);
                        return;
                    }

                    match transcription_result {
                        Ok(transcription) => {
                            debug!(
                                "Transcription completed in {:?}: '{}'",
                                transcription_time.elapsed(),
                                transcription
                            );

                            // Hand off to the shared finish tail (clean → history
                            // → paste → analytics), which the wake-word path also
                            // uses so injection/cleanup/logging are identical.
                            let stt_latency_ms = transcription_time.elapsed().as_millis() as i64;
                            let history_file_name = if wav_saved { Some(file_name) } else { None };
                            finish_dictation(
                                &ah,
                                transcription,
                                post_process,
                                sample_count,
                                stt_latency_ms,
                                history_file_name,
                                cancel_generation,
                            )
                            .await;
                        }
                        Err(err) => {
                            if rm.was_cancelled_since(cancel_generation) {
                                debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                                return;
                            }

                            error!("Transcription failed: {}", err);
                            // Surface the failure to the UI (toast). The full
                            // message is also in handy.log via the line above.
                            let _ = ah.emit("transcription-error", err.to_string());
                            // Save entry with empty text so user can retry
                            if wav_saved {
                                if let Err(save_err) = hm.save_entry(
                                    file_name,
                                    String::new(),
                                    post_process,
                                    None,
                                    None,
                                ) {
                                    error!("Failed to save failed history entry: {}", save_err);
                                }
                            }
                            utils::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // Tear down any streaming worker so its channel doesn't leak.
                tm.cancel_stream();
                utils::hide_recording_overlay(&ah);
                change_tray_icon(&ah, TrayIconState::Idle);
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Test Action
struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})", // Changed "Pressed" to "Started" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})", // Changed "Released" to "Stopped" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction {
            post_process: false,
        }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_post_process".to_string(),
        Arc::new(TranscribeAction { post_process: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});

#[cfg(test)]
mod tests {
    use super::is_blank_transcription;

    #[test]
    fn blank_transcription_is_detected() {
        assert!(is_blank_transcription(""));
        assert!(is_blank_transcription("   "));
        assert!(is_blank_transcription("\t\n  \r\n"));
    }

    #[test]
    fn non_blank_transcription_is_kept() {
        assert!(!is_blank_transcription("hello"));
        assert!(!is_blank_transcription("  hello  "));
    }
}
