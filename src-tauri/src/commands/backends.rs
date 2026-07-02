//! OpenFlow backend-mode commands: STT backend selection, keychain-backed API
//! keys, model listing, and the per-card "Test" actions used by the Model Setup
//! UI. Cleanup keys route through the keychain too (see `set_api_key`).

use std::sync::Arc;
use std::time::Instant;

use tauri::{AppHandle, Manager};

use crate::backends::{stt_http, BackendTestResult};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{self, SttApiStyle, SttBackendMode};

// ---------------- STT backend settings ----------------

#[tauri::command]
#[specta::specta]
pub fn set_stt_backend_mode(app: AppHandle, mode: SttBackendMode) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.stt_backend_mode = mode;
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_stt_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    if !s.stt_providers.iter().any(|p| p.id == provider_id) {
        return Err(format!("Unknown STT provider '{provider_id}'"));
    }
    s.stt_provider_id = provider_id;
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_stt_model_setting(
    app: AppHandle,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.stt_models.insert(provider_id, model);
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_stt_selfhosted_url_setting(app: AppHandle, url: String) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.stt_selfhosted_url = url;
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_stt_selfhosted_model_setting(app: AppHandle, model: String) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.stt_selfhosted_model = model;
    settings::write_settings(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_stt_selfhosted_api_style(app: AppHandle, style: SttApiStyle) -> Result<(), String> {
    let mut s = settings::get_settings(&app);
    s.stt_selfhosted_api_style = style;
    settings::write_settings(&app, s);
    Ok(())
}

// ---------------- Keychain-backed API keys ----------------

/// Store an API key in the OS keychain. `scope` is "stt" or "cleanup",
/// `provider` is the provider id (e.g. "groq"). Empty key deletes the entry.
#[tauri::command]
#[specta::specta]
pub fn set_api_key(scope: String, provider: String, key: String) -> Result<(), String> {
    crate::keychain::set_api_key(&scope, &provider, &key).map_err(|e| e.to_string())
}

/// Whether a key exists for (scope, provider). We never return the key itself.
#[tauri::command]
#[specta::specta]
pub fn has_api_key(scope: String, provider: String) -> Result<bool, String> {
    Ok(crate::keychain::has_api_key(&scope, &provider))
}

#[tauri::command]
#[specta::specta]
pub fn delete_api_key(scope: String, provider: String) -> Result<(), String> {
    crate::keychain::delete_api_key(&scope, &provider).map_err(|e| e.to_string())
}

// ---------------- Model listing ----------------

/// List models for an OpenAI-compatible STT endpoint (self-hosted or a provider
/// with a `/models` endpoint). Deepgram has no such endpoint → returns a curated
/// static list.
#[tauri::command]
#[specta::specta]
pub async fn list_stt_models(app: AppHandle, provider_id: String) -> Result<Vec<String>, String> {
    let s = settings::get_settings(&app);

    // Resolve (base_url, api_style, key, models_endpoint) for the target.
    let (base_url, style, key, models_endpoint) = if provider_id == "selfhosted" {
        (
            s.stt_selfhosted_url.trim_end_matches('/').to_string(),
            s.stt_selfhosted_api_style,
            crate::keychain::get_api_key("stt", "selfhosted").unwrap_or_default(),
            Some("/models".to_string()),
        )
    } else {
        let p = s
            .stt_providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| format!("Unknown STT provider '{provider_id}'"))?;
        (
            p.base_url.trim_end_matches('/').to_string(),
            p.api_style,
            crate::keychain::get_api_key("stt", &provider_id).unwrap_or_default(),
            p.models_endpoint.clone(),
        )
    };

    if let SttApiStyle::Deepgram = style {
        return Ok(vec![
            "nova-3".to_string(),
            "nova-2".to_string(),
            "nova".to_string(),
            "whisper-large".to_string(),
        ]);
    }

    let endpoint = models_endpoint.unwrap_or_else(|| "/models".to_string());
    let url = format!("{base_url}{endpoint}");
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if !key.is_empty() {
        req = req.bearer_auth(&key);
    }
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} listing models", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    // OpenAI shape: { data: [ { id }, ... ] }
    let ids = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ids)
}

// ---------------- Test buttons ----------------

/// Record ~2 s from the mic and transcribe it through the *currently selected*
/// STT backend, returning the text + measured latency. Powers the STT card's
/// "Test" button.
#[tauri::command]
#[specta::specta]
pub async fn test_stt_backend(app: AppHandle) -> Result<BackendTestResult, String> {
    let settings = settings::get_settings(&app);
    let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());

    if rm.is_recording() {
        return Err("A dictation is already in progress; try again in a moment".to_string());
    }

    // Capture a short sample (VAD disabled so we keep exactly what's spoken).
    let gen = rm.cancel_generation();
    rm.try_start_recording("openflow_test", crate::audio_toolkit::VadPolicy::Disabled)
        .map_err(|e| format!("Could not start microphone: {e}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
    let samples = rm
        .stop_recording("openflow_test", gen)
        .unwrap_or_default();

    if samples.is_empty() {
        return Ok(BackendTestResult {
            ok: false,
            message: "No audio captured — is the microphone permitted?".to_string(),
            text: String::new(),
            latency_ms: 0,
            backend: backend_label(&settings),
        });
    }

    let started = Instant::now();
    match settings.stt_backend_mode {
        SttBackendMode::Local => {
            let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
            match tokio::task::spawn_blocking(move || tm.transcribe(samples)).await {
                Ok(Ok(text)) => Ok(BackendTestResult {
                    ok: true,
                    message: "Local transcription succeeded".to_string(),
                    text: text.trim().to_string(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    backend: "local".to_string(),
                }),
                Ok(Err(e)) => Err(format!("Local transcription failed: {e}")),
                Err(e) => Err(format!("Local transcription task failed: {e}")),
            }
        }
        _ => match stt_http::transcribe(&settings, &samples).await {
            Ok(outcome) => Ok(BackendTestResult {
                ok: true,
                message: format!("{} responded", outcome.backend),
                text: outcome.text,
                latency_ms: outcome.latency_ms,
                backend: outcome.backend,
            }),
            Err(e) => Ok(BackendTestResult {
                ok: false,
                message: e,
                text: String::new(),
                latency_ms: started.elapsed().as_millis() as u64,
                backend: backend_label(&settings),
            }),
        },
    }
}

/// Run a fixed noisy sample through the *currently selected* cleanup backend and
/// return the polished text + latency. Powers the Cleanup card's "Test" button.
#[tauri::command]
#[specta::specta]
pub async fn test_cleanup_backend(app: AppHandle) -> Result<BackendTestResult, String> {
    let settings = settings::get_settings(&app);
    const SAMPLE: &str = "um so like i think we should uh ship the the feature on friday you know";

    let provider = settings
        .active_post_process_provider()
        .cloned()
        .ok_or_else(|| "No cleanup provider selected".to_string())?;
    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() {
        return Err(format!("No model set for cleanup provider '{}'", provider.label));
    }
    let api_key = crate::keychain::get_api_key("cleanup", &provider.id)
        .or_else(|| settings.post_process_api_keys.get(&provider.id).cloned())
        .unwrap_or_default();

    let prompt = format!(
        "Clean up this dictated transcript: fix punctuation and remove filler words. \
         Return only the cleaned text.\n\n{SAMPLE}"
    );

    let started = Instant::now();
    match crate::llm_client::send_chat_completion(&provider, api_key, &model, prompt, None, None)
        .await
    {
        Ok(Some(text)) => Ok(BackendTestResult {
            ok: true,
            message: format!("{} responded", provider.label),
            text: text.trim().to_string(),
            latency_ms: started.elapsed().as_millis() as u64,
            backend: format!("cleanup:{}", provider.id),
        }),
        Ok(None) => Ok(BackendTestResult {
            ok: false,
            message: "Cleanup provider returned no content".to_string(),
            text: String::new(),
            latency_ms: started.elapsed().as_millis() as u64,
            backend: format!("cleanup:{}", provider.id),
        }),
        Err(e) => Ok(BackendTestResult {
            ok: false,
            message: e,
            text: String::new(),
            latency_ms: started.elapsed().as_millis() as u64,
            backend: format!("cleanup:{}", provider.id),
        }),
    }
}

fn backend_label(s: &settings::AppSettings) -> String {
    match s.stt_backend_mode {
        SttBackendMode::Local => "local".to_string(),
        SttBackendMode::SelfHosted => "selfhosted".to_string(),
        SttBackendMode::Remote => format!("remote:{}", s.stt_provider_id),
    }
}
