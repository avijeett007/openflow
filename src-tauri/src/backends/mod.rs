//! OpenFlow backend abstraction.
//!
//! Handy ships local-only STT (compiled-in engines) and a multi-provider
//! OpenAI-compatible cleanup client. OpenFlow adds the missing half — remote /
//! self-hosted STT over HTTP — and unifies the three model-backend modes behind
//! small, hot-swappable seams:
//!
//! - STT: `Local` keeps Handy's on-device engine; `SelfHosted` / `Remote` route
//!   recorded audio to an HTTP endpoint via [`stt_http`].
//! - Cleanup already flows through `llm_client` (OpenAI-compatible), which covers
//!   self-hosted (Ollama/OpenAI-compatible) and remote providers uniformly.
//!
//! Everything is selected from settings at call time, so switching modes in the
//! UI takes effect on the next dictation with no restart.

pub mod stt_http;

use serde::Serialize;
use specta::Type;

/// Result of a `Test` action (STT or cleanup): what came back and how long it
/// took, so the setup UI can show "it works, ~740 ms" before the user commits.
#[derive(Debug, Clone, Serialize, Type)]
pub struct BackendTestResult {
    pub ok: bool,
    pub message: String,
    /// Transcribed / cleaned sample text (empty on failure).
    pub text: String,
    pub latency_ms: u64,
    /// The backend that actually ran, e.g. "remote:groq" or "local".
    pub backend: String,
}
