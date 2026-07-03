//! HTTP speech-to-text: Modes B (self-hosted URL) and C (remote provider).
//!
//! Two wire shapes are supported (see [`SttApiStyle`]):
//! - **OpenAI-compatible** — `multipart/form-data POST {base}/audio/transcriptions`
//!   with a WAV `file`, `model`, optional `language`. Covers OpenAI, Groq,
//!   Speaches, whisper-server, LocalAI, vLLM, etc.
//! - **Deepgram** — `POST {base}/listen?model=..&punctuate=true` with a raw WAV
//!   body and `Authorization: Token <key>`.
//!
//! Recorded audio arrives as 16 kHz mono f32 (the pipeline's native format); we
//! encode it to an in-memory 16-bit WAV and send that.

use std::io::Cursor;
use std::time::Instant;

use crate::settings::{AppSettings, SttApiStyle, SttBackendMode, SttProvider};

/// Outcome of a remote/self-hosted transcription.
pub struct SttOutcome {
    pub text: String,
    pub model: String,
    pub latency_ms: u64,
    /// Human label of the backend that ran, e.g. `"remote:groq"`.
    pub backend: String,
}

/// Encode 16 kHz mono f32 samples to a 16-bit PCM WAV in memory.
fn encode_wav_16k_mono(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut buf, spec).map_err(|e| format!("wav init: {e}"))?;
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * i16::MAX as f32) as i16;
            writer
                .write_sample(v)
                .map_err(|e| format!("wav write: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("wav finalize: {e}"))?;
    }
    Ok(buf.into_inner())
}

/// Which STT endpoint the current settings point at, and with what key.
/// Returns `(provider, model, api_key)`. For `SelfHosted` the provider is a
/// synthetic entry built from the user's URL.
pub fn resolve_active_stt(settings: &AppSettings) -> Result<(SttProvider, String, String), String> {
    match settings.stt_backend_mode {
        SttBackendMode::Local => {
            Err("STT backend is Local; no HTTP endpoint to resolve".to_string())
        }
        SttBackendMode::SelfHosted => {
            let provider = SttProvider {
                id: "selfhosted".to_string(),
                label: "My Endpoint".to_string(),
                base_url: settings
                    .stt_selfhosted_url
                    .trim_end_matches('/')
                    .to_string(),
                allow_base_url_edit: true,
                api_style: settings.stt_selfhosted_api_style,
                default_model: String::new(),
                models_endpoint: Some("/models".to_string()),
            };
            let model = settings.stt_selfhosted_model.clone();
            // Self-hosted servers are usually open, but honor a key if the user set one.
            let key = crate::keychain::get_api_key("stt", "selfhosted").unwrap_or_default();
            Ok((provider, model, key))
        }
        SttBackendMode::Remote => {
            let provider = settings
                .stt_providers
                .iter()
                .find(|p| p.id == settings.stt_provider_id)
                .cloned()
                .ok_or_else(|| format!("Unknown STT provider '{}'", settings.stt_provider_id))?;
            let model = settings
                .stt_models
                .get(&provider.id)
                .cloned()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| provider.default_model.clone());
            let key = crate::keychain::get_api_key("stt", &provider.id).unwrap_or_default();
            Ok((provider, model, key))
        }
    }
}

/// Transcribe recorded samples through the active HTTP STT backend.
pub async fn transcribe(settings: &AppSettings, samples: &[f32]) -> Result<SttOutcome, String> {
    let (provider, model, api_key) = resolve_active_stt(settings)?;
    let language = settings.selected_language.clone();
    transcribe_with(&provider, &model, &api_key, samples, &language).await
}

/// Core transcription against an explicit provider/model/key — shared by the
/// dictation path and the `Test` button.
pub async fn transcribe_with(
    provider: &SttProvider,
    model: &str,
    api_key: &str,
    samples: &[f32],
    language: &str,
) -> Result<SttOutcome, String> {
    if model.trim().is_empty() {
        return Err("No STT model selected for this backend".to_string());
    }
    let wav = encode_wav_16k_mono(samples)?;
    let started = Instant::now();
    let base = provider.base_url.trim_end_matches('/');

    let text = match provider.api_style {
        SttApiStyle::OpenaiCompatible => {
            openai_transcribe(base, model, api_key, wav, language).await?
        }
        SttApiStyle::Deepgram => deepgram_transcribe(base, model, api_key, wav).await?,
    };

    let backend = if provider.id == "selfhosted" {
        "selfhosted".to_string()
    } else {
        format!("remote:{}", provider.id)
    };
    Ok(SttOutcome {
        text: text.trim().to_string(),
        model: model.to_string(),
        latency_ms: started.elapsed().as_millis() as u64,
        backend,
    })
}

async fn openai_transcribe(
    base: &str,
    model: &str,
    api_key: &str,
    wav: Vec<u8>,
    language: &str,
) -> Result<String, String> {
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("multipart: {e}"))?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", "json".to_string());
    // "auto" is our sentinel for "let the model decide"; only send a concrete code.
    if !language.is_empty() && language != "auto" {
        form = form.text("language", language.to_string());
    }

    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{base}/audio/transcriptions"))
        .multipart(form);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", truncate(&body, 300)));
    }
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse json: {e} (body: {})", truncate(&body, 200)))?;
    json.get("text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("response had no 'text' field: {}", truncate(&body, 200)))
}

async fn deepgram_transcribe(
    base: &str,
    model: &str,
    api_key: &str,
    wav: Vec<u8>,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("Deepgram requires an API key".to_string());
    }
    let url = format!("{base}/listen?model={model}&punctuate=true&smart_format=true");
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", "audio/wav")
        .body(wav)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {}", truncate(&body, 300)));
    }
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse json: {e}"))?;
    json.pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("unexpected Deepgram response: {}", truncate(&body, 200)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
