# OpenFlow — build plan (scope-first, per GOAL-fable5.md §How to work)

Fork base: **Handy 0.9.0** (MIT, Tauri 2.10 + Rust + React/TS/Tailwind). Upstream remote kept as `upstream` for attribution and future merges. What Handy already gives us (verified by code map):

- Global hotkey (handy-keys/rdev) with hold-to-talk **and** toggle (`push_to_talk` bool), 30ms-debounced FSM coordinator (`transcription_coordinator.rs`).
- cpal capture → rubato resample → 16 kHz mono f32; Silero VAD (ONNX) with smoothing (`audio_toolkit/`).
- Local STT: transcribe-cpp (whisper-family GGUF, Metal/Vulkan) + transcribe-rs (Parakeet et al., ONNX int8) behind `EngineType`/`LoadedEngine` enums; 65-model catalog + hf-hub downloads with sha256 + progress events.
- LLM post-processing: `llm_client.rs` — generic OpenAI-compatible chat client, providers openai/anthropic/openrouter/groq/zai/cerebras/custom(Ollama), structured output, per-provider headers.
- Injection: clipboard save→write→Cmd/Ctrl+V→restore (`clipboard.rs::paste_via_clipboard`) + enigo direct typing + auto-submit; macOS permission plugin + onboarding screens.
- SQLite history (`history.db`: transcription_history), rusqlite_migration.
- Tauri packaging: ad-hoc macOS signing ("-") already configured, NSIS/Windows, updater plugin, i18n (20 locales).

## What OpenFlow adds/changes (the delta = our milestones)

### 1. Backend trait abstraction (M2 core)

New module `src-tauri/src/backends/`:

```rust
#[async_trait]
pub trait SttBackend: Send + Sync {
    fn id(&self) -> &str;                       // "local", "selfhosted", "remote:groq", ...
    async fn transcribe(&self, wav16k: &[f32], lang: Option<&str>) -> Result<SttResult>;
    async fn health_check(&self) -> Result<BackendHealth>;   // powers the "Test" button
}
pub struct SttResult { pub text: String, pub latency_ms: u64, pub model: String }

#[async_trait]
pub trait CleanupBackend: Send + Sync {
    fn id(&self) -> &str;
    async fn clean(&self, raw: &str, ctx: &AppContext) -> Result<CleanupResult>; // ctx = active app/window → tone
    async fn health_check(&self) -> Result<BackendHealth>;
    async fn list_models(&self) -> Result<Vec<String>>;
}
```

Implementations:

- `LocalStt` — wraps Handy's existing `TranscriptionManager` (enum engines stay as-is inside).
- `HttpStt` — OpenAI-compatible `POST /v1/audio/transcriptions` (multipart WAV). Covers Mode B (Speaches/whisper-server/self-hosted) AND Mode C (OpenAI, Groq; Deepgram gets its own small adapter, different API shape).
- `HttpCleanup` — wraps existing `llm_client.rs` (Mode B Ollama/OpenAI-compatible + Mode C openai/anthropic/openrouter/groq/gemini).
- `LocalCleanup` — llama.cpp **server** sidecar (`llama-server` binary + Qwen2.5-3B-Instruct Q4_K_M GGUF, downloaded on demand) exposed as OpenAI-compatible localhost → reuses `HttpCleanup` internally. One HTTP code path for everything; the sidecar is the only new process.

Config: `stt_backend_mode` ∈ {local, selfhosted, remote} × `cleanup_backend_mode` ∈ {local, selfhosted, remote, off} — independent, hot-swappable (a `BackendRegistry` rebuilds the active backend on settings change; no restart).

### 2. Keychain (M2)

`keyring` crate (macOS Keychain / Windows Credential Manager). API keys move out of settings JSON; settings store only provider names + non-secret config. Migration shim reads legacy `SecretMap` once and moves values.

### 3. Model Setup UI (M2)

New settings section "Model Setup": two cards (Speech-to-Text / Text Cleanup), each with Local | My Endpoint | Remote segmented toggle, provider/model pickers, endpoint URL + validate (lists models), keychain-backed key entry, and a **Test** button (records 2 s from mic OR uses a bundled sample WAV → runs the card's backend → shows result + measured latency). First-run wizard gains: hardware detection (RAM/CPU cores/GPU) → mode recommendation → per-mode setup → end-to-end test dictation before finish.

### 4. Cleanup layer default-on (M3)

Make post-processing part of the main `transcribe` binding when a cleanup backend is configured (Handy gates it behind a second hotkey; we make cleanup the default path with per-app tone prompts). Per-app prompt map keyed by active app bundle id/exe name (detected at injection time), editable in settings.

### 5. Analytics (M4)

New table `dictation_events` in history.db (kept separate from transcription_history):
`id, ts, duration_ms, audio_ms, raw_text?, cleaned_text?, word_count, wpm, active_app, window_title?, detected_project?, language, stt_backend, stt_model, cleanup_backend?, cleanup_model?, stt_latency_ms, cleanup_latency_ms, total_latency_ms, injected_ok`
Privacy mode `analytics_privacy` ∈ {full, keywords_only, off}: keywords_only stores only extracted keywords (`keywords` column, JSON) and nulls raw/cleaned text. Keyword extraction: Rust-side stopword-filtered tokenization (no extra model).
Dashboard: new "Dashboard" sidebar section, React + recharts: totals/streaks, dictations+words over time, WPM + time-saved (vs 40 WPM typing baseline), by-app bars, by-project bars (project inferred from window title heuristics: git-repo-like tokens, "— ProjectName" suffixes), top keywords + trend.

### 6. Rebrand + packaging (M5)

productName OpenFlow, identifier `care.hexai.openflow`, new icon (simple generated), macOS ad-hoc signing (already "-"), Windows: drop Azure signCommand → plain unsigned NSIS. GitHub Actions matrix (macos-13 x64, macos-14 arm64, windows-latest) building .dmg + .exe, uploading artifacts + Release. Updater endpoint stubbed to our repo's releases latest.json (keys generated, private key stored as Actions secret — documented). README: install + per-OS unblock + three modes + add-a-provider guide.

## M0 spike approach (risk first)

1. Build + launch Handy fork debug app on this Mac (permissions inherited from VSCode).
2. Route audio: set default input to BlackHole 2ch, play `say`-generated speech into it → app hears "mic" audio. (switchaudio-osx to flip devices; restore after.)
3. Trigger dictation via synthetic hotkey (CGEvent option+space) with a target app focused.
4. Verify injected text lands in 3 apps: TextEdit (editor), Safari (browser textarea), Notes (chat-like compose). Measure E2E latency from logs.
5. Windows path: code-verified + CI build only (no Windows hardware here); live check deferred to user checklist.

## Verification method (every milestone)

Screenshots via `screencapture` into `verification/mN-*/`, checked with vision; fresh-context verifier subagent drives the running app against the milestone's success criteria before I mark it done.
