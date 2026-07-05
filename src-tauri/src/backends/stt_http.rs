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

use crate::settings::{AppSettings, DictionaryEntry, SttApiStyle, SttBackendMode, SttProvider};

/// Outcome of a remote/self-hosted transcription.
pub struct SttOutcome {
    pub text: String,
    pub model: String,
    pub latency_ms: u64,
    /// Human label of the backend that ran, e.g. `"remote:groq"`.
    pub backend: String,
    /// True when dictionary words were sent to the engine as a biasing hint
    /// (OpenAI-compatible `prompt`, or Deepgram `keyterm`/`keywords`). Callers
    /// use this to decide whether the post-STT fuzzy correction pass would be
    /// redundant — same rationale as whisper's local `initial_prompt` path
    /// (see `post_process_transcription_text`). `false` whenever the
    /// dictionary was empty or the request otherwise carried no hint.
    pub prompted: bool,
}

/// Conservative cap on the OpenAI-compatible `prompt` field, in characters.
/// Whisper's real limit is ~224 *tokens*; we don't have a tokenizer handy at
/// this layer, so we cap the joined string length instead. ~800 chars is a
/// safely conservative stand-in (average English word ~5 chars + separator,
/// so ~800 chars is comfortably under 224 tokens even for multi-token words).
const OPENAI_PROMPT_MAX_CHARS: usize = 800;

/// Conservative cap on the number of Deepgram `keyterm` params. Deepgram's
/// documented limit is 500 *tokens* total across all keyterms; we approximate
/// with a per-term cap since keyterms may be multi-word phrases.
const DEEPGRAM_KEYTERM_MAX_COUNT: usize = 500;

/// Deepgram's documented cap on the number of `keywords` params (legacy
/// models).
const DEEPGRAM_KEYWORDS_MAX_COUNT: usize = 100;

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
    transcribe_with(
        &provider,
        &model,
        &api_key,
        samples,
        &language,
        &settings.dictionary,
    )
    .await
}

/// Core transcription against an explicit provider/model/key — shared by the
/// dictation path and the `Test` button. `dictionary` is used for engine-side
/// biasing (OpenAI-compatible `prompt`, Deepgram `keyterm`/`keywords`); pass an
/// empty slice to send none.
pub async fn transcribe_with(
    provider: &SttProvider,
    model: &str,
    api_key: &str,
    samples: &[f32],
    language: &str,
    dictionary: &[DictionaryEntry],
) -> Result<SttOutcome, String> {
    if model.trim().is_empty() {
        return Err("No STT model selected for this backend".to_string());
    }
    let wav = encode_wav_16k_mono(samples)?;
    let started = Instant::now();
    let base = provider.base_url.trim_end_matches('/');

    let words: Vec<&str> = dictionary
        .iter()
        .map(|e| e.word.as_str())
        .filter(|w| !w.trim().is_empty())
        .collect();

    let (text, prompted) = match provider.api_style {
        SttApiStyle::OpenaiCompatible => {
            openai_transcribe(base, model, api_key, wav, language, &words).await?
        }
        SttApiStyle::Deepgram => {
            deepgram_transcribe(base, model, api_key, wav, &words, dictionary).await?
        }
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
        prompted,
    })
}

/// Build a biasing prompt string from dictionary words, joined with `", "`.
/// Whisper-family decoders effectively only attend to the *tail* of an
/// overlong prompt, so when the joined string would exceed `max_chars` we drop
/// whole words from the FRONT (the earliest-added, so presumptively
/// least-important, entries) and keep as many trailing words as fit — rather
/// than mid-word char-truncating the joined string, which could hand the
/// decoder a garbled fragment. Returns `None` for an empty/all-blank word
/// list; callers must send no prompt param at all in that case.
pub(crate) fn build_prompt_string(words: &[&str], max_chars: usize) -> Option<String> {
    let words: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| !w.trim().is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    let full = words.join(", ");
    if full.len() <= max_chars {
        return Some(full);
    }

    // Keep trailing whole words until adding another would exceed max_chars.
    let mut kept: Vec<&str> = Vec::new();
    let mut len = 0usize;
    for w in words.iter().rev() {
        let sep_len = if kept.is_empty() { 0 } else { 2 }; // ", "
        let candidate_len = len + sep_len + w.len();
        if candidate_len > max_chars {
            break;
        }
        len = candidate_len;
        kept.push(w);
    }
    if kept.is_empty() {
        // Even the single most-important word doesn't fit; send it anyway —
        // the engine will truncate internally, which beats sending nothing.
        return words.last().map(|w| w.to_string());
    }
    kept.reverse();
    Some(kept.join(", "))
}

async fn openai_transcribe(
    base: &str,
    model: &str,
    api_key: &str,
    wav: Vec<u8>,
    language: &str,
    dictionary_words: &[&str],
) -> Result<(String, bool), String> {
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
    // Vocabulary biasing: whisper-1, gpt-4o-transcribe, Groq whisper-large-v3(-turbo)
    // and OpenAI-compatible self-hosted servers all accept a free-text `prompt`
    // field; it's harmless if the server ignores it.
    let prompt = build_prompt_string(dictionary_words, OPENAI_PROMPT_MAX_CHARS);
    let prompted = prompt.is_some();
    if let Some(prompt) = prompt {
        form = form.text("prompt", prompt);
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
        .map(|s| (s.to_string(), prompted))
        .ok_or_else(|| format!("response had no 'text' field: {}", truncate(&body, 200)))
}

/// Which Deepgram vocabulary-biasing param to send, chosen by model name.
/// `keyterm` is Nova-3/Flux-only (Deepgram docs: "Keyterm Prompting" is
/// supported by Nova-3 and Flux models); every older/legacy model (Nova-2 and
/// earlier, plus Whisper-Cloud-on-Deepgram) only understands the legacy
/// `keywords` param.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeepgramBiasingStyle {
    /// Repeated `keyterm` params. Multi-word phrases OK, ≤500 tokens total.
    Keyterm,
    /// Repeated `keywords` params, `word` or `word:boost` format. Single
    /// words only, ≤100 total.
    Keywords,
}

pub(crate) fn deepgram_biasing_style(model: &str) -> DeepgramBiasingStyle {
    let m = model.trim().to_lowercase();
    if m.contains("nova-3") || m.contains("flux") {
        DeepgramBiasingStyle::Keyterm
    } else {
        DeepgramBiasingStyle::Keywords
    }
}

/// Words considered for Deepgram `keyterm` biasing: canonical `word`s *plus*
/// their `sounds_like` aliases. Unlike the cleanup-prompt vocabulary block (see
/// `actions::dictionary_vocabulary_block`) or the legacy `keywords`/OpenAI
/// `prompt` paths, `keyterm` biases the ASR's own acoustic matching rather than
/// asking a downstream text model to prefer a spelling — so surfacing the
/// misheard/alternate forms here increases the odds Deepgram actually emits
/// something close to one of them, which the alias-exact/fuzzy correction pass
/// can then resolve to the canonical spelling. Blank words/aliases are dropped.
fn deepgram_keyterm_words(dictionary: &[DictionaryEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in dictionary {
        if !entry.word.trim().is_empty() {
            out.push(entry.word.clone());
        }
        for alias in &entry.sounds_like {
            if !alias.trim().is_empty() {
                out.push(alias.clone());
            }
        }
    }
    out
}

async fn deepgram_transcribe(
    base: &str,
    model: &str,
    api_key: &str,
    wav: Vec<u8>,
    dictionary_words: &[&str],
    dictionary: &[DictionaryEntry],
) -> Result<(String, bool), String> {
    if api_key.is_empty() {
        return Err("Deepgram requires an API key".to_string());
    }
    let url = format!("{base}/listen");
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .query(&[
            ("model", model),
            ("punctuate", "true"),
            ("smart_format", "true"),
        ])
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", "audio/wav");

    let mut prompted = false;
    match deepgram_biasing_style(model) {
        DeepgramBiasingStyle::Keyterm => {
            // keyterm-only: canonical words AND sounds_like aliases (see
            // `deepgram_keyterm_words`). Legacy `keywords` below stays
            // canonical-words-only, matching the OpenAI-compatible `prompt`.
            let keyterm_words = deepgram_keyterm_words(dictionary);
            let pairs: Vec<(&str, &str)> = keyterm_words
                .iter()
                .map(|w| w.as_str())
                .filter(|w| !w.trim().is_empty())
                .take(DEEPGRAM_KEYTERM_MAX_COUNT)
                .map(|w| ("keyterm", w))
                .collect();
            if !pairs.is_empty() {
                prompted = true;
                req = req.query(&pairs);
            }
        }
        DeepgramBiasingStyle::Keywords => {
            // Legacy `keywords` only supports single words — multi-word
            // dictionary entries (phrases) are silently skipped rather than
            // sent malformed.
            let pairs: Vec<(&str, &str)> = dictionary_words
                .iter()
                .filter(|w| !w.trim().is_empty() && !w.trim().contains(char::is_whitespace))
                .take(DEEPGRAM_KEYWORDS_MAX_COUNT)
                .map(|w| ("keywords", *w))
                .collect();
            if !pairs.is_empty() {
                prompted = true;
                req = req.query(&pairs);
            }
        }
    }

    let resp = req
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
        .map(|s| (s.to_string(), prompted))
        .ok_or_else(|| format!("unexpected Deepgram response: {}", truncate(&body, 200)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_string_empty_dictionary_is_none() {
        assert_eq!(build_prompt_string(&[], 800), None);
        // Blank/whitespace-only entries count as empty too.
        assert_eq!(build_prompt_string(&["", "   "], 800), None);
    }

    #[test]
    fn build_prompt_string_joins_with_comma_space() {
        let words = ["ChargeBee", "Kubernetes", "OpenFlow"];
        assert_eq!(
            build_prompt_string(&words, 800).as_deref(),
            Some("ChargeBee, Kubernetes, OpenFlow")
        );
    }

    #[test]
    fn build_prompt_string_ignores_blank_entries_among_real_ones() {
        let words = ["ChargeBee", "", "OpenFlow"];
        assert_eq!(
            build_prompt_string(&words, 800).as_deref(),
            Some("ChargeBee, OpenFlow")
        );
    }

    #[test]
    fn build_prompt_string_truncates_keeping_the_tail() {
        // Each word is 5 chars; with ", " separators a 3-word budget is 19
        // chars ("aaaaa, bbbbb, ccccc"). Cap at 13 chars should keep only the
        // last whole word or two, dropping from the FRONT (whisper attends to
        // the prompt's tail, so the most-recently-added/most-important words
        // must survive truncation).
        let words = ["aaaaa", "bbbbb", "ccccc"];
        let result = build_prompt_string(&words, 13).unwrap();
        // "bbbbb, ccccc" is exactly 12 chars <= 13; adding "aaaaa, " would blow
        // the budget, so only the trailing two words are kept, in original order.
        assert_eq!(result, "bbbbb, ccccc");
        assert!(!result.contains("aaaaa"));
    }

    #[test]
    fn build_prompt_string_single_oversized_word_still_sent() {
        // A single word longer than max_chars can't be dropped to fit (nothing
        // else to drop) — send it anyway rather than sending nothing.
        let words = ["a-very-long-single-dictionary-word-phrase"];
        let result = build_prompt_string(&words, 5).unwrap();
        assert_eq!(result, words[0]);
    }

    #[test]
    fn build_prompt_string_no_truncation_when_under_budget() {
        let words = ["short", "list"];
        assert_eq!(
            build_prompt_string(&words, 800).as_deref(),
            Some("short, list")
        );
    }

    #[test]
    fn deepgram_biasing_style_nova3_and_flux_use_keyterm() {
        assert_eq!(
            deepgram_biasing_style("nova-3"),
            DeepgramBiasingStyle::Keyterm
        );
        assert_eq!(
            deepgram_biasing_style("nova-3-general"),
            DeepgramBiasingStyle::Keyterm
        );
        assert_eq!(
            deepgram_biasing_style("flux-general-en"),
            DeepgramBiasingStyle::Keyterm
        );
        // Case-insensitive.
        assert_eq!(
            deepgram_biasing_style("Nova-3-Medical"),
            DeepgramBiasingStyle::Keyterm
        );
    }

    fn dict_entry(word: &str, sounds_like: &[&str]) -> DictionaryEntry {
        DictionaryEntry {
            word: word.to_string(),
            sounds_like: sounds_like.iter().map(|s| s.to_string()).collect(),
            replace_exact: false,
            case_sensitive: false,
        }
    }

    #[test]
    fn deepgram_keyterm_words_includes_canonical_and_aliases() {
        // Keyterm-only: unlike the cleanup vocabulary block, sounds_like
        // aliases are wanted here since keyterm biases the ASR's acoustic
        // matching rather than a text model's word choice.
        let dict = vec![
            dict_entry("ChargeBee", &["charge bee", "charge b"]),
            dict_entry("Kubernetes", &[]),
        ];
        let words = deepgram_keyterm_words(&dict);
        assert_eq!(
            words,
            vec!["ChargeBee", "charge bee", "charge b", "Kubernetes"]
        );
    }

    #[test]
    fn deepgram_keyterm_words_drops_blank_entries() {
        let dict = vec![dict_entry("", &["", "  "]), dict_entry("OK", &[""])];
        assert_eq!(deepgram_keyterm_words(&dict), vec!["OK".to_string()]);
    }

    #[test]
    fn deepgram_keyterm_words_empty_for_empty_dictionary() {
        assert!(deepgram_keyterm_words(&[]).is_empty());
    }

    #[test]
    fn deepgram_biasing_style_legacy_models_use_keywords() {
        assert_eq!(
            deepgram_biasing_style("nova-2"),
            DeepgramBiasingStyle::Keywords
        );
        assert_eq!(
            deepgram_biasing_style("nova-2-general"),
            DeepgramBiasingStyle::Keywords
        );
        assert_eq!(
            deepgram_biasing_style("base"),
            DeepgramBiasingStyle::Keywords
        );
        assert_eq!(
            deepgram_biasing_style("whisper-large"),
            DeepgramBiasingStyle::Keywords
        );
    }
}
