use log::{debug, warn};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::fmt;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub const APPLE_INTELLIGENCE_PROVIDER_ID: &str = "apple_intelligence";
pub const APPLE_INTELLIGENCE_DEFAULT_MODEL_ID: &str = "Apple Intelligence";

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

// Custom deserializer to handle both old numeric format (1-5) and new string format ("trace", "debug", etc.)
impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LogLevelVisitor;

        impl<'de> Visitor<'de> for LogLevelVisitor {
            type Value = LogLevel;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or integer representing log level")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<LogLevel, E> {
                match value.to_lowercase().as_str() {
                    "trace" => Ok(LogLevel::Trace),
                    "debug" => Ok(LogLevel::Debug),
                    "info" => Ok(LogLevel::Info),
                    "warn" => Ok(LogLevel::Warn),
                    "error" => Ok(LogLevel::Error),
                    _ => Err(E::unknown_variant(
                        value,
                        &["trace", "debug", "info", "warn", "error"],
                    )),
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<LogLevel, E> {
                match value {
                    1 => Ok(LogLevel::Trace),
                    2 => Ok(LogLevel::Debug),
                    3 => Ok(LogLevel::Info),
                    4 => Ok(LogLevel::Warn),
                    5 => Ok(LogLevel::Error),
                    _ => Err(E::invalid_value(de::Unexpected::Unsigned(value), &"1-5")),
                }
            }
        }

        deserializer.deserialize_any(LogLevelVisitor)
    }
}

impl From<LogLevel> for tauri_plugin_log::LogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => tauri_plugin_log::LogLevel::Trace,
            LogLevel::Debug => tauri_plugin_log::LogLevel::Debug,
            LogLevel::Info => tauri_plugin_log::LogLevel::Info,
            LogLevel::Warn => tauri_plugin_log::LogLevel::Warn,
            LogLevel::Error => tauri_plugin_log::LogLevel::Error,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ShortcutBinding {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_binding: String,
    pub current_binding: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LLMPrompt {
    pub id: String,
    pub name: String,
    pub prompt: String,
}

/// What an agent does with its LLM response. `Inject` pastes it at the cursor
/// exactly like a normal dictation; `Clipboard` only copies it (no paste, no
/// auto-submit) and shows a brief confirmation. Defaults to `Inject`.
#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputMode {
    Inject,
    Clipboard,
}

impl Default for AgentOutputMode {
    fn default() -> Self {
        AgentOutputMode::Inject
    }
}

/// Flow OS increment 2 — what KIND of agent this is. `Prompt` is the increment-1
/// behavior (dictation routed through a persona LLM before injection). `Cli`
/// drives a REAL local coding-agent binary (Claude Code, Codex, …) as a
/// subprocess in a chosen project folder. Defaults to `Prompt` so every agent
/// stored before this field existed stays a prompt agent, byte-for-byte.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Prompt,
    Cli,
}

impl Default for AgentKind {
    fn default() -> Self {
        AgentKind::Prompt
    }
}

/// Which local coding-agent CLI a `Cli` agent drives. Selects the prefilled
/// invocation template (see `default_command_template_for`). `Custom` is a
/// fully user-defined template for any other binary.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentCliType {
    Claude,
    Codex,
    Openclaw,
    Hermes,
    Custom,
}

/// Where a CLI agent run's output goes (multi-select). `Panel` is the live
/// streamed in-app view (always effectively on for a running view). `Notify`
/// fires a desktop notification on completion. `File` writes the full
/// instruction+output to a markdown file in the project.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputSink {
    Panel,
    Notify,
    File,
}

/// How a CLI agent receives the instruction (transcript). `Stdin` (default)
/// writes it to the process stdin — no OS arg-length limit, mirroring Agent OS.
/// `Arg` substitutes it into the `{prompt}` placeholder in the command template.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    Stdin,
    Arg,
}

impl Default for PromptDelivery {
    fn default() -> Self {
        PromptDelivery::Stdin
    }
}

/// `#[serde(default)]` helper: a CLI agent's output sinks default to just the
/// live panel when the stored value is absent.
fn default_output_sinks() -> Vec<AgentOutputSink> {
    vec![AgentOutputSink::Panel]
}

/// Default binary name for a CLI type — what `detect_agent_binary` looks up on
/// PATH. `Custom` has no canonical binary (the user supplies the path).
pub fn default_cli_binary_name(cli_type: AgentCliType) -> Option<&'static str> {
    match cli_type {
        AgentCliType::Claude => Some("claude"),
        AgentCliType::Codex => Some("codex"),
        AgentCliType::Openclaw => Some("openclaw"),
        AgentCliType::Hermes => Some("hermes"),
        AgentCliType::Custom => None,
    }
}

/// Prefilled `(command_template, prompt_via)` per CLI type. Claude Code's flags
/// are LIVE-VERIFIED against `claude` 2.1.x: `-p --output-format stream-json
/// --verbose` runs headless and streams line-delimited JSON; `acceptEdits`
/// auto-accepts file edits so the agent can actually modify the repo without an
/// interactive permission prompt (git is the safety net, per DESIGN §9). The
/// instruction is delivered on stdin (no arg-length limit). codex/openclaw/
/// hermes are best-effort (binaries not installed here — see BLOCKERS).
pub fn default_cli_template(cli_type: AgentCliType) -> (String, PromptDelivery) {
    match cli_type {
        AgentCliType::Claude => (
            "-p --output-format stream-json --verbose --permission-mode acceptEdits".to_string(),
            PromptDelivery::Stdin,
        ),
        // Codex CLI: `codex exec` is the non-interactive mode; `--json` streams
        // JSONL. The prompt is passed as the trailing positional arg.
        AgentCliType::Codex => ("exec --json {prompt}".to_string(), PromptDelivery::Arg),
        // Best-effort placeholders until the binaries are available to verify.
        AgentCliType::Openclaw => ("run {prompt}".to_string(), PromptDelivery::Arg),
        AgentCliType::Hermes => ("run {prompt}".to_string(), PromptDelivery::Arg),
        AgentCliType::Custom => (String::new(), PromptDelivery::Stdin),
    }
}

/// A Flow OS agent: dictation routed through a persona LLM before injection.
/// Each agent has its own global hotkey (via a seeded `ShortcutBinding` keyed
/// `agent:<id>`). All optional fields are `#[serde(default)]` so an old settings
/// store (with no `agents` key, or partial entries) always deserializes cleanly —
/// the store wipes to defaults on any parse failure.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct AgentDefinition {
    /// Stable slug, e.g. "coder"; unique. Matches `^[a-z0-9_-]{1,48}$`.
    pub id: String,
    /// Display name shown in the UI.
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// ALWAYS `"agent:<id>"` — the join key into `AppSettings.bindings`.
    pub binding_id: String,
    /// References an existing `post_process_providers` entry.
    pub provider_id: String,
    #[serde(default)]
    pub model: String,
    /// The persona used as the LLM system prompt.
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub output_mode: AgentOutputMode,

    // ---- Flow OS increment 2: CLI agents ----
    /// Discriminator. `Prompt` (default) → increment-1 persona-LLM behavior,
    /// unchanged. `Cli` → drive a real coding-agent subprocess (fields below).
    #[serde(default)]
    pub kind: AgentKind,
    /// Which coding CLI this agent drives (only meaningful when `kind == Cli`).
    #[serde(default)]
    pub cli_type: Option<AgentCliType>,
    /// Resolved/overridable path to the agent binary (e.g. `/usr/local/bin/claude`).
    #[serde(default)]
    pub binary_path: String,
    /// Argv template appended after the binary; supports `{cwd}`/`{prompt}`
    /// placeholders (see `AgentRunManager::build_argv`). Prefilled per `cli_type`.
    #[serde(default)]
    pub command_template: String,
    /// Project folder the agent runs in (`cwd`). `""` = no fixed project (a
    /// sensible default dir is used at run time).
    #[serde(default)]
    pub project_path: String,
    /// Where run output goes. Defaults to `[Panel]` (live in-app stream only).
    #[serde(default = "default_output_sinks")]
    pub output_sinks: Vec<AgentOutputSink>,
    /// How the instruction reaches the CLI. `Stdin` (default) or `Arg`.
    #[serde(default)]
    pub prompt_via: PromptDelivery,
}

/// A user dictionary entry: a canonical spelling plus optional "sounds like"
/// aliases (misheard/alternate forms) that are rewritten to the canonical word.
/// Supersedes the flat `custom_words` list (legacy entries migrate to one entry
/// each with no aliases). See `audio_toolkit::text::apply_dictionary`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct DictionaryEntry {
    /// Canonical spelling used in the output. Spaces are allowed (phrases).
    pub word: String,
    /// Aliases / misheard forms that are replaced by `word`. Matched exactly
    /// first (deterministic, threshold-independent) then fuzzily.
    #[serde(default)]
    pub sounds_like: Vec<String>,
    /// When true, only deterministic alias replacement runs for this entry — the
    /// fuzzy pass never matches against `word` (or its aliases).
    #[serde(default)]
    pub replace_exact: bool,
    /// When true, `word`'s exact casing is emitted verbatim (bypasses the
    /// case-pattern preservation that otherwise mirrors the input token's case).
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PostProcessProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub allow_base_url_edit: bool,
    #[serde(default)]
    pub models_endpoint: Option<String>,
    #[serde(default)]
    pub supports_structured_output: bool,
}

/// Where speech-to-text runs. `Local` uses the bundled on-device engine
/// (OpenFlow's Parakeet/Whisper). `SelfHosted` and `Remote` both POST audio to an
/// HTTP endpoint — the only difference is UX (a user-typed URL vs. a named
/// provider) and where the key comes from.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum SttBackendMode {
    #[default]
    Local,
    SelfHosted,
    Remote,
}

/// The wire protocol an HTTP STT endpoint speaks. Most providers and
/// self-hosted servers (OpenAI, Groq, Speaches, whisper-server, LocalAI) use the
/// OpenAI `/audio/transcriptions` multipart shape; Deepgram is different enough
/// to need its own adapter.
/// How much of each dictation the analytics backend persists. `Full` stores
/// everything; `KeywordsOnly` keeps derived keywords but nulls raw/cleaned text
/// and window titles; `Off` skips logging entirely.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsPrivacy {
    #[default]
    Full,
    KeywordsOnly,
    Off,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum SttApiStyle {
    #[default]
    OpenaiCompatible,
    Deepgram,
}

/// A remote STT provider entry (Mode C) or the template for a self-hosted one.
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct SttProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub allow_base_url_edit: bool,
    #[serde(default)]
    pub api_style: SttApiStyle,
    #[serde(default)]
    pub default_model: String,
    /// Optional `/models` listing endpoint (OpenAI-compatible only).
    #[serde(default)]
    pub models_endpoint: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPosition {
    Top,
    // `none` is retired: overlay visibility is owned by `OverlayStyle` now. The
    // alias keeps legacy stores (`"overlay_position": "none"`) deserializing
    // instead of failing the whole load; the one-time overlay migration reads the
    // raw stored string to recover the old "hidden" intent as `OverlayStyle::None`.
    #[serde(alias = "none")]
    Bottom,
}

/// Which recording overlay to display. `Minimal` and `Live` share one base
/// (the pill); `Live` grows into the panel that shows live transcription text.
/// `None` hides the overlay entirely. Decoupled from whether the model runs in
/// streaming mode (that is driven purely by model capability).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum OverlayStyle {
    None,
    Minimal,
    Live,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelUnloadTimeout {
    Never,
    Immediately,
    Min2,
    #[default]
    Min5,
    Min10,
    Min15,
    Hour1,
    Sec15, // Debug mode only
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    CtrlV,
    Direct,
    None,
    ShiftInsert,
    CtrlShiftV,
    ExternalScript,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardHandling {
    #[default]
    DontModify,
    CopyToClipboard,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoSubmitKey {
    #[default]
    Enter,
    CtrlEnter,
    CmdEnter,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRetentionPeriod {
    Never,
    PreserveLimit,
    Days3,
    Weeks2,
    Months3,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardImplementation {
    Tauri,
    HandyKeys,
}

impl Default for KeyboardImplementation {
    fn default() -> Self {
        #[cfg(target_os = "linux")]
        return KeyboardImplementation::Tauri;
        #[cfg(not(target_os = "linux"))]
        return KeyboardImplementation::HandyKeys;
    }
}

impl Default for PasteMethod {
    fn default() -> Self {
        // Default to CtrlV for macOS and Windows, Direct for Linux
        #[cfg(target_os = "linux")]
        return PasteMethod::Direct;
        #[cfg(not(target_os = "linux"))]
        return PasteMethod::CtrlV;
    }
}

impl ModelUnloadTimeout {
    pub fn to_minutes(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Min2 => Some(2),
            ModelUnloadTimeout::Min5 => Some(5),
            ModelUnloadTimeout::Min10 => Some(10),
            ModelUnloadTimeout::Min15 => Some(15),
            ModelUnloadTimeout::Hour1 => Some(60),
            ModelUnloadTimeout::Sec15 => Some(0), // Special case for debug - handled separately
        }
    }

    pub fn to_seconds(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Sec15 => Some(15),
            _ => self.to_minutes().map(|m| m * 60),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum SoundTheme {
    Marimba,
    Pop,
    Custom,
}

impl SoundTheme {
    fn as_str(&self) -> &'static str {
        match self {
            SoundTheme::Marimba => "marimba",
            SoundTheme::Pop => "pop",
            SoundTheme::Custom => "custom",
        }
    }

    pub fn to_start_path(self) -> String {
        format!("resources/{}_start.wav", self.as_str())
    }

    pub fn to_stop_path(self) -> String {
        format!("resources/{}_stop.wav", self.as_str())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum TypingTool {
    #[default]
    Auto,
    Wtype,
    Kwtype,
    Dotool,
    Ydotool,
    Xdotool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum TranscribeAcceleratorSetting {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrtAcceleratorSetting {
    #[default]
    Auto,
    Cpu,
    Cuda,
    #[serde(rename = "directml")]
    DirectMl,
    Rocm,
}

#[derive(Clone, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub(crate) struct SecretMap(HashMap<String, String>);

impl fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted: HashMap<&String, &str> = self
            .0
            .iter()
            .map(|(k, v)| (k, if v.is_empty() { "" } else { "[REDACTED]" }))
            .collect();
        redacted.fmt(f)
    }
}

impl std::ops::Deref for SecretMap {
    type Target = HashMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SecretMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/* still handy for composing the initial JSON in the store ------------- */
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct AppSettings {
    /// Internal settings schema marker for one-time migrations. Fresh installs
    /// start at the current version; existing stores missing this key are
    /// treated as version 0 and migrated forward.
    #[serde(default = "default_settings_schema_version")]
    pub settings_schema_version: u32,
    pub bindings: HashMap<String, ShortcutBinding>,
    pub push_to_talk: bool,
    pub audio_feedback: bool,
    #[serde(default = "default_audio_feedback_volume")]
    pub audio_feedback_volume: f32,
    #[serde(default = "default_sound_theme")]
    pub sound_theme: SoundTheme,
    #[serde(default = "default_start_hidden")]
    pub start_hidden: bool,
    #[serde(default = "default_autostart_enabled")]
    pub autostart_enabled: bool,
    #[serde(default = "default_update_checks_enabled")]
    pub update_checks_enabled: bool,
    #[serde(default = "default_show_whats_new_on_update")]
    pub show_whats_new_on_update: bool,
    /// The app version whose What's New the user has already seen. Fresh installs
    /// default to the current version (nothing is "new" to them). Existing users
    /// upgrading from before this key existed are blanked by the migration so they
    /// see the current release's notes — see `apply_settings_migrations`.
    #[serde(default = "default_whats_new_last_seen_version")]
    pub whats_new_last_seen_version: String,
    #[serde(default = "default_model")]
    pub selected_model: String,
    #[serde(default)]
    pub onboarding_completed: bool,
    #[serde(default = "default_always_on_microphone")]
    pub always_on_microphone: bool,
    #[serde(default)]
    pub selected_microphone: Option<String>,
    #[serde(default)]
    pub clamshell_microphone: Option<String>,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_translate_to_english")]
    pub translate_to_english: bool,
    #[serde(default = "default_selected_language")]
    pub selected_language: String,
    #[serde(default = "default_overlay_position")]
    pub overlay_position: OverlayPosition,
    #[serde(default = "default_debug_mode")]
    pub debug_mode: bool,
    /// Basic vs. Advanced UI mode. When false (default), the settings sidebar
    /// shows only the core speech-to-text sections; when true, every section is
    /// revealed. Purely a UI gate — it never disables any underlying feature.
    #[serde(default = "default_advanced_mode")]
    pub advanced_mode: bool,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    /// Legacy flat custom-word list. Kept deserializable for back-compat and
    /// migrated into `dictionary` on load; `dictionary` is the source of truth.
    #[serde(default)]
    pub custom_words: Vec<String>,
    /// User dictionary: canonical spellings + "sounds like" alias rules.
    #[serde(default)]
    pub dictionary: Vec<DictionaryEntry>,
    /// One-shot marker for the legacy `custom_words` → `dictionary` migration
    /// (see `apply_settings_migrations`). Missing/`false` means the migration
    /// hasn't run yet; once it runs (whether or not there was anything to
    /// migrate) this is set `true` forever, so a user who later deletes every
    /// dictionary entry doesn't get `custom_words` silently re-migrated back in
    /// on the next settings read. Fresh installs start `true` since there is
    /// nothing to migrate.
    #[serde(default)]
    pub dictionary_migrated: bool,
    #[serde(default)]
    pub model_unload_timeout: ModelUnloadTimeout,
    #[serde(default = "default_word_correction_threshold")]
    pub word_correction_threshold: f64,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    #[serde(default = "default_recording_retention_period")]
    pub recording_retention_period: RecordingRetentionPeriod,
    #[serde(default)]
    pub paste_method: PasteMethod,
    #[serde(default)]
    pub clipboard_handling: ClipboardHandling,
    #[serde(default = "default_auto_submit")]
    pub auto_submit: bool,
    #[serde(default)]
    pub auto_submit_key: AutoSubmitKey,
    #[serde(default = "default_post_process_enabled")]
    pub post_process_enabled: bool,
    #[serde(default = "default_post_process_provider_id")]
    pub post_process_provider_id: String,
    #[serde(default = "default_post_process_providers")]
    pub post_process_providers: Vec<PostProcessProvider>,
    #[serde(default = "default_post_process_api_keys")]
    pub post_process_api_keys: SecretMap,
    #[serde(default = "default_post_process_models")]
    pub post_process_models: HashMap<String, String>,
    #[serde(default = "default_post_process_prompts")]
    pub post_process_prompts: Vec<LLMPrompt>,
    #[serde(default = "default_post_process_selected_prompt_id")]
    pub post_process_selected_prompt_id: Option<String>,
    /// Per-app cleanup prompt overrides: active-app name (as returned by
    /// `active_app::current().app_name`, e.g. "Slack", "Visual Studio Code",
    /// "Mail") → a cleanup prompt/instruction string. When the frontmost app
    /// matches an entry, its prompt is used instead of the selected default.
    #[serde(default)]
    pub per_app_prompts: HashMap<String, String>,
    #[serde(default)]
    pub mute_while_recording: bool,
    #[serde(default)]
    pub append_trailing_space: bool,
    #[serde(default = "default_app_language")]
    pub app_language: String,
    #[serde(default)]
    pub experimental_enabled: bool,
    #[serde(default)]
    pub lazy_stream_close: bool,
    #[serde(default)]
    pub keyboard_implementation: KeyboardImplementation,
    #[serde(default = "default_show_tray_icon")]
    pub show_tray_icon: bool,
    #[serde(default = "default_paste_delay_ms")]
    pub paste_delay_ms: u64,
    #[serde(default = "default_typing_tool")]
    pub typing_tool: TypingTool,
    pub external_script_path: Option<String>,
    #[serde(default)]
    pub custom_filler_words: Option<Vec<String>>,
    #[serde(default)]
    pub transcribe_accelerator: TranscribeAcceleratorSetting,
    #[serde(default)]
    pub ort_accelerator: OrtAcceleratorSetting,
    #[serde(default = "default_transcribe_gpu_device")]
    pub transcribe_gpu_device: i32,
    #[serde(default)]
    pub extra_recording_buffer_ms: u64,
    #[serde(default = "default_vad_enabled")]
    pub vad_enabled: bool,
    /// Which recording overlay to show: None / Minimal / Live. Streaming mode is
    /// not gated on this — that follows model capability. Migrated from the old
    /// `overlay_position` (position `none` → style `None`).
    #[serde(default = "default_overlay_style")]
    pub overlay_style: OverlayStyle,

    // ---- OpenFlow: speech-to-text backend selection (Modes A/B/C) ----
    /// Where STT runs: on-device (`Local`), a user-typed endpoint (`SelfHosted`),
    /// or a named cloud provider (`Remote`). Independent of the cleanup backend.
    #[serde(default)]
    pub stt_backend_mode: SttBackendMode,
    /// Selected remote provider id (indexes `stt_providers`).
    #[serde(default = "default_stt_provider_id")]
    pub stt_provider_id: String,
    #[serde(default = "default_stt_providers")]
    pub stt_providers: Vec<SttProvider>,
    /// Per-provider chosen model name (provider id → model).
    #[serde(default)]
    pub stt_models: HashMap<String, String>,
    /// Self-hosted (Mode B) endpoint URL, e.g. a Speaches / whisper-server base.
    #[serde(default = "default_stt_selfhosted_url")]
    pub stt_selfhosted_url: String,
    #[serde(default)]
    pub stt_selfhosted_model: String,
    #[serde(default)]
    pub stt_selfhosted_api_style: SttApiStyle,

    // ---- OpenFlow: analytics (M4) ----
    /// How much of each dictation the usage-analytics backend persists.
    #[serde(default)]
    pub analytics_privacy: AnalyticsPrivacy,

    // ---- OpenFlow: hands-free / wake-word ----
    /// Master switch for the always-on wake-word listener. When true, the
    /// `WakeWordManager` runs a background loop that listens for the wake phrase
    /// and starts a voice-triggered dictation (no hotkey required).
    #[serde(default)]
    pub hands_free_enabled: bool,
    /// The wake phrase the listener matches at the start of an utterance.
    #[serde(default = "default_wake_word")]
    pub wake_word: String,
    /// Similarity threshold (0.0..=1.0) for the fuzzy wake-word match. Higher is
    /// stricter; the default is a good balance for the "hey flow" default phrase.
    #[serde(default = "default_wake_word_sensitivity")]
    pub wake_word_sensitivity: f32,
    /// Pre-speech grace window (seconds): after the wake word, how long to wait
    /// for the user to START speaking before giving up. A pause after the wake
    /// word (while gathering a thought) never cuts them off. Once they DO speak,
    /// capture keeps extending while speech continues and ends shortly after they
    /// go quiet (see `wake_word_silence_timeout_ms`) — so a short command submits
    /// promptly rather than waiting out this whole window.
    #[serde(default = "default_wake_word_listen_seconds")]
    pub wake_word_listen_seconds: u64,
    /// Once the minimum window has elapsed AND the user has spoken, how long
    /// (milliseconds) of continuous silence ends the command capture. This is what
    /// makes hands-free "smart": short commands finish quickly, long ones keep the
    /// mic open as long as speech continues. Clamped to a sane range at the command.
    #[serde(default = "default_wake_word_silence_timeout_ms")]
    pub wake_word_silence_timeout_ms: u64,
    /// When true, hands-free plays short spoken acknowledgment cues: "Got it"
    /// after a wake match (before the command mic opens) and "Now typing" just
    /// before the transcribed text is injected. Reuses the audio-feedback volume
    /// and selected output device. Default on.
    #[serde(default = "default_hands_free_voice_feedback")]
    pub hands_free_voice_feedback: bool,

    // ---- Flow OS: agents (per-agent voice hotkeys) ----
    /// User-defined agents. Empty by default. Purely additive — an old store
    /// with no `agents` key deserializes to an empty list, so behavior with no
    /// agents configured is byte-for-byte identical to before this field existed.
    #[serde(default)]
    pub agents: Vec<AgentDefinition>,

    // ---- OpenFlow Meetings (M1) ----
    /// Master switch for the meetings feature (capture + on-device transcription).
    /// Additive & fully defaultable; when false the detector never runs and manual
    /// capture is refused. Default on so the shipped feature is usable.
    #[serde(default = "default_true")]
    pub meetings_enabled: bool,
    /// Auto-detect meetings (known bundle id + mic-in-use fusion) and offer to
    /// capture. Default on; capture start is always user-confirmed regardless.
    #[serde(default = "default_true")]
    pub meeting_auto_detect: bool,
    /// Bundle ids treated as meeting apps for auto-detection. User-extensible
    /// (Webex/Slack huddles etc.); defaults to Zoom, Teams (classic + new), FaceTime.
    #[serde(default = "default_meeting_app_allowlist")]
    pub meeting_app_allowlist: Vec<String>,
}

fn default_meeting_app_allowlist() -> Vec<String> {
    vec![
        "us.zoom.xos".to_string(),
        "com.microsoft.teams".to_string(),
        "com.microsoft.teams2".to_string(),
        "com.apple.FaceTime".to_string(),
    ]
}

fn default_model() -> String {
    "".to_string()
}

const CURRENT_SETTINGS_SCHEMA_VERSION: u32 = 1;

fn default_settings_schema_version() -> u32 {
    CURRENT_SETTINGS_SCHEMA_VERSION
}

fn default_always_on_microphone() -> bool {
    false
}

fn default_translate_to_english() -> bool {
    false
}

fn default_start_hidden() -> bool {
    false
}

fn default_autostart_enabled() -> bool {
    false
}

fn default_update_checks_enabled() -> bool {
    true
}

fn default_show_whats_new_on_update() -> bool {
    true
}

fn default_whats_new_last_seen_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_selected_language() -> String {
    "auto".to_string()
}

fn default_overlay_position() -> OverlayPosition {
    // Position only matters when the overlay is shown; whether it shows at all is
    // `overlay_style` (Linux defaults that to None). So a single default suffices.
    OverlayPosition::Bottom
}

fn default_overlay_style() -> OverlayStyle {
    // Linux hides the overlay by default; other platforms show the live overlay.
    // Position is independent and only selects top vs. bottom placement.
    #[cfg(target_os = "linux")]
    return OverlayStyle::None;
    #[cfg(not(target_os = "linux"))]
    return OverlayStyle::Live;
}

fn default_vad_enabled() -> bool {
    true
}

fn default_debug_mode() -> bool {
    false
}

fn default_advanced_mode() -> bool {
    false
}

fn default_log_level() -> LogLevel {
    LogLevel::Debug
}

fn default_word_correction_threshold() -> f64 {
    0.18
}

fn default_paste_delay_ms() -> u64 {
    60
}

fn default_auto_submit() -> bool {
    false
}

fn default_history_limit() -> usize {
    5
}

fn default_recording_retention_period() -> RecordingRetentionPeriod {
    RecordingRetentionPeriod::PreserveLimit
}

fn default_audio_feedback_volume() -> f32 {
    1.0
}

fn default_sound_theme() -> SoundTheme {
    SoundTheme::Marimba
}

fn default_post_process_enabled() -> bool {
    false
}

fn default_app_language() -> String {
    tauri_plugin_os::locale()
        .map(|l| l.replace('_', "-"))
        .unwrap_or_else(|| "en".to_string())
}

fn default_show_tray_icon() -> bool {
    true
}

fn default_post_process_provider_id() -> String {
    "openai".to_string()
}

/// Select the built-in "Improve Transcriptions" prompt by default so that once a
/// user enables cleanup (in Model Setup), the main dictation hotkey actually
/// runs it — otherwise post-processing silently skips with "no prompt selected".
fn default_post_process_selected_prompt_id() -> Option<String> {
    Some("default_improve_transcriptions".to_string())
}

fn default_stt_provider_id() -> String {
    "groq".to_string()
}

fn default_stt_selfhosted_url() -> String {
    "http://localhost:8000/v1".to_string()
}

/// Remote STT providers (Mode C). Groq is the cheap low-friction default;
/// OpenAI and Deepgram round out the set the goal calls for.
pub fn default_stt_providers() -> Vec<SttProvider> {
    vec![
        SttProvider {
            id: "groq".to_string(),
            label: "Groq".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            allow_base_url_edit: false,
            api_style: SttApiStyle::OpenaiCompatible,
            default_model: "whisper-large-v3-turbo".to_string(),
            models_endpoint: Some("/models".to_string()),
        },
        SttProvider {
            id: "openai".to_string(),
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            allow_base_url_edit: false,
            api_style: SttApiStyle::OpenaiCompatible,
            default_model: "gpt-4o-transcribe".to_string(),
            models_endpoint: Some("/models".to_string()),
        },
        SttProvider {
            id: "deepgram".to_string(),
            label: "Deepgram".to_string(),
            base_url: "https://api.deepgram.com/v1".to_string(),
            allow_base_url_edit: false,
            api_style: SttApiStyle::Deepgram,
            default_model: "nova-2".to_string(),
            models_endpoint: None,
        },
    ]
}

fn default_post_process_providers() -> Vec<PostProcessProvider> {
    let mut providers = vec![
        PostProcessProvider {
            id: "openai".to_string(),
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "anthropic".to_string(),
            label: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "groq".to_string(),
            label: "Groq".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "cerebras".to_string(),
            label: "Cerebras".to_string(),
            base_url: "https://api.cerebras.ai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
    ];

    // Note: We always include Apple Intelligence on macOS ARM64 without checking availability
    // at startup. The availability check is deferred to when the user actually tries to use it
    // (in actions.rs). This prevents crashes on macOS 26.x beta where accessing
    // SystemLanguageModel.default during early app initialization causes SIGABRT.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        providers.push(PostProcessProvider {
            id: APPLE_INTELLIGENCE_PROVIDER_ID.to_string(),
            label: "Apple Intelligence".to_string(),
            base_url: "apple-intelligence://local".to_string(),
            allow_base_url_edit: false,
            models_endpoint: None,
            supports_structured_output: true,
        });
    }

    // AWS Bedrock via Mantle (OpenAI-compatible endpoint)
    providers.push(PostProcessProvider {
        id: "bedrock_mantle".to_string(),
        label: "AWS Bedrock (Mantle)".to_string(),
        base_url: "https://bedrock-mantle.us-east-1.api.aws/v1".to_string(),
        allow_base_url_edit: false,
        models_endpoint: Some("/models".to_string()),
        supports_structured_output: true,
    });

    // Custom provider always comes last
    providers.push(PostProcessProvider {
        id: "custom".to_string(),
        label: "Custom".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        allow_base_url_edit: true,
        models_endpoint: Some("/models".to_string()),
        supports_structured_output: false,
    });

    providers
}

fn default_post_process_api_keys() -> SecretMap {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(provider.id, String::new());
    }
    SecretMap(map)
}

fn default_model_for_provider(provider_id: &str) -> String {
    if provider_id == APPLE_INTELLIGENCE_PROVIDER_ID {
        return APPLE_INTELLIGENCE_DEFAULT_MODEL_ID.to_string();
    }
    String::new()
}

fn default_post_process_models() -> HashMap<String, String> {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(
            provider.id.clone(),
            default_model_for_provider(&provider.id),
        );
    }
    map
}

fn default_post_process_prompts() -> Vec<LLMPrompt> {
    vec![LLMPrompt {
        id: "default_improve_transcriptions".to_string(),
        name: "Improve Transcriptions".to_string(),
        prompt: "Clean this transcript:\n1. Fix spelling, capitalization, and punctuation errors\n2. Convert number words to digits (twenty-five → 25, ten percent → 10%, five dollars → $5)\n3. Replace spoken punctuation with symbols (period → ., comma → ,, question mark → ?)\n4. Remove filler words (um, uh, like as filler)\n5. Keep the language in the original version (if it was french, keep it in french for example)\n\nPreserve exact meaning and word order. Do not paraphrase or reorder content.\n\nReturn only the cleaned transcript.\n\nTranscript:\n${output}".to_string(),
    }]
}

fn default_transcribe_gpu_device() -> i32 {
    -1 // auto
}

fn default_typing_tool() -> TypingTool {
    TypingTool::Auto
}

fn default_wake_word() -> String {
    "hey flow".to_string()
}

fn default_wake_word_listen_seconds() -> u64 {
    // Minimum guaranteed listen window after the wake word. Short enough that a
    // quick command doesn't feel laggy, long enough to survive a brief pause
    // before the user starts talking; smart VAD extends it while speech continues.
    10
}

fn default_wake_word_silence_timeout_ms() -> u64 {
    // End the command after ~2.5s of silence once the user has spoken. Comfortably
    // longer than the VAD hangover tail so natural mid-sentence pauses don't cut off.
    2500
}

fn default_wake_word_sensitivity() -> f32 {
    0.8
}

fn default_hands_free_voice_feedback() -> bool {
    true
}

fn ensure_post_process_defaults(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    for provider in default_post_process_providers() {
        // Use match to do a single lookup - either sync existing or add new
        match settings
            .post_process_providers
            .iter_mut()
            .find(|p| p.id == provider.id)
        {
            Some(existing) => {
                // Sync supports_structured_output field for existing providers (migration)
                if existing.supports_structured_output != provider.supports_structured_output {
                    debug!(
                        "Updating supports_structured_output for provider '{}' from {} to {}",
                        provider.id,
                        existing.supports_structured_output,
                        provider.supports_structured_output
                    );
                    existing.supports_structured_output = provider.supports_structured_output;
                    changed = true;
                }
            }
            None => {
                // Provider doesn't exist, add it
                settings.post_process_providers.push(provider.clone());
                changed = true;
            }
        }

        if !settings.post_process_api_keys.contains_key(&provider.id) {
            settings
                .post_process_api_keys
                .insert(provider.id.clone(), String::new());
            changed = true;
        }

        let default_model = default_model_for_provider(&provider.id);
        match settings.post_process_models.get_mut(&provider.id) {
            Some(existing) => {
                if existing.is_empty() && !default_model.is_empty() {
                    *existing = default_model.clone();
                    changed = true;
                }
            }
            None => {
                settings
                    .post_process_models
                    .insert(provider.id.clone(), default_model);
                changed = true;
            }
        }
    }

    changed
}

pub const SETTINGS_STORE_PATH: &str = "settings_store.json";

pub fn get_default_settings() -> AppSettings {
    #[cfg(target_os = "windows")]
    let default_shortcut = "ctrl+space";
    #[cfg(target_os = "macos")]
    let default_shortcut = "option+space";
    #[cfg(target_os = "linux")]
    let default_shortcut = "ctrl+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_shortcut = "alt+space";

    let mut bindings = HashMap::new();
    bindings.insert(
        "transcribe".to_string(),
        ShortcutBinding {
            id: "transcribe".to_string(),
            name: "Transcribe".to_string(),
            description: "Converts your speech into text.".to_string(),
            default_binding: default_shortcut.to_string(),
            current_binding: default_shortcut.to_string(),
        },
    );
    #[cfg(target_os = "windows")]
    let default_post_process_shortcut = "ctrl+shift+space";
    #[cfg(target_os = "macos")]
    let default_post_process_shortcut = "option+shift+space";
    #[cfg(target_os = "linux")]
    let default_post_process_shortcut = "ctrl+shift+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_post_process_shortcut = "alt+shift+space";

    bindings.insert(
        "transcribe_with_post_process".to_string(),
        ShortcutBinding {
            id: "transcribe_with_post_process".to_string(),
            name: "Transcribe with Post-Processing".to_string(),
            description: "Converts your speech into text and applies AI post-processing."
                .to_string(),
            default_binding: default_post_process_shortcut.to_string(),
            current_binding: default_post_process_shortcut.to_string(),
        },
    );
    bindings.insert(
        "cancel".to_string(),
        ShortcutBinding {
            id: "cancel".to_string(),
            name: "Cancel".to_string(),
            description: "Cancels the current recording.".to_string(),
            default_binding: "escape".to_string(),
            current_binding: "escape".to_string(),
        },
    );

    // OpenFlow Meetings: a global hotkey to start/stop meeting capture without
    // opening the app. Seeded UNBOUND (empty) — Google Meet runs in a browser, so
    // there is no sensible cross-app default we could pick that wouldn't collide;
    // the user assigns it in Settings → Meetings. Empty bindings are skipped by
    // every registration loop, exactly like the seeded `agent:<id>` hotkeys.
    bindings.insert(
        "meeting_capture".to_string(),
        ShortcutBinding {
            id: "meeting_capture".to_string(),
            name: "Meeting Capture".to_string(),
            description: "Start or stop capturing the current meeting.".to_string(),
            default_binding: String::new(),
            current_binding: String::new(),
        },
    );

    AppSettings {
        settings_schema_version: default_settings_schema_version(),
        bindings,
        push_to_talk: true,
        audio_feedback: false,
        audio_feedback_volume: default_audio_feedback_volume(),
        sound_theme: default_sound_theme(),
        start_hidden: default_start_hidden(),
        autostart_enabled: default_autostart_enabled(),
        update_checks_enabled: default_update_checks_enabled(),
        show_whats_new_on_update: default_show_whats_new_on_update(),
        whats_new_last_seen_version: default_whats_new_last_seen_version(),
        selected_model: "".to_string(),
        onboarding_completed: false,
        always_on_microphone: false,
        selected_microphone: None,
        clamshell_microphone: None,
        selected_output_device: None,
        translate_to_english: false,
        selected_language: "auto".to_string(),
        overlay_position: default_overlay_position(),
        debug_mode: false,
        advanced_mode: default_advanced_mode(),
        log_level: default_log_level(),
        custom_words: Vec::new(),
        dictionary: Vec::new(),
        // Nothing to migrate on a fresh install.
        dictionary_migrated: true,
        model_unload_timeout: ModelUnloadTimeout::default(),
        word_correction_threshold: default_word_correction_threshold(),
        history_limit: default_history_limit(),
        recording_retention_period: default_recording_retention_period(),
        paste_method: PasteMethod::default(),
        clipboard_handling: ClipboardHandling::default(),
        auto_submit: default_auto_submit(),
        auto_submit_key: AutoSubmitKey::default(),
        post_process_enabled: default_post_process_enabled(),
        post_process_provider_id: default_post_process_provider_id(),
        post_process_providers: default_post_process_providers(),
        post_process_api_keys: default_post_process_api_keys(),
        post_process_models: default_post_process_models(),
        post_process_prompts: default_post_process_prompts(),
        post_process_selected_prompt_id: default_post_process_selected_prompt_id(),
        per_app_prompts: HashMap::new(),
        mute_while_recording: false,
        append_trailing_space: false,
        app_language: default_app_language(),
        experimental_enabled: false,
        lazy_stream_close: false,
        keyboard_implementation: KeyboardImplementation::default(),
        show_tray_icon: default_show_tray_icon(),
        paste_delay_ms: default_paste_delay_ms(),
        typing_tool: default_typing_tool(),
        external_script_path: None,
        custom_filler_words: None,
        transcribe_accelerator: TranscribeAcceleratorSetting::default(),
        ort_accelerator: OrtAcceleratorSetting::default(),
        transcribe_gpu_device: default_transcribe_gpu_device(),
        extra_recording_buffer_ms: 0,
        vad_enabled: default_vad_enabled(),
        overlay_style: default_overlay_style(),
        stt_backend_mode: SttBackendMode::default(),
        stt_provider_id: default_stt_provider_id(),
        stt_providers: default_stt_providers(),
        stt_models: HashMap::new(),
        stt_selfhosted_url: default_stt_selfhosted_url(),
        stt_selfhosted_model: String::new(),
        stt_selfhosted_api_style: SttApiStyle::default(),
        analytics_privacy: AnalyticsPrivacy::default(),
        hands_free_enabled: false,
        wake_word: default_wake_word(),
        wake_word_sensitivity: default_wake_word_sensitivity(),
        wake_word_listen_seconds: default_wake_word_listen_seconds(),
        wake_word_silence_timeout_ms: default_wake_word_silence_timeout_ms(),
        hands_free_voice_feedback: default_hands_free_voice_feedback(),
        agents: Vec::new(),
        meetings_enabled: true,
        meeting_auto_detect: true,
        meeting_app_allowlist: default_meeting_app_allowlist(),
    }
}

/// `#[serde(default)]` helper for boolean fields that should default to `true`.
fn default_true() -> bool {
    true
}

impl AppSettings {
    pub fn active_post_process_provider(&self) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == self.post_process_provider_id)
    }

    pub fn post_process_provider(&self, provider_id: &str) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }

    pub fn post_process_provider_mut(
        &mut self,
        provider_id: &str,
    ) -> Option<&mut PostProcessProvider> {
        self.post_process_providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
    }
}

pub fn load_or_create_app_settings(app: &AppHandle) -> AppSettings {
    // Initialize store
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    let mut settings = if let Some(settings_value) = store.get("settings") {
        // Parse the entire settings object
        match serde_json::from_value::<AppSettings>(settings_value.clone()) {
            Ok(mut settings) => {
                debug!("Found existing settings: {:?}", settings);
                let default_settings = get_default_settings();
                let mut updated = apply_settings_migrations(&mut settings, &settings_value);

                // Merge default bindings into existing settings
                for (key, value) in default_settings.bindings {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        settings.bindings.entry(key)
                    {
                        debug!("Adding missing binding: {}", entry.key());
                        entry.insert(value);
                        updated = true;
                    }
                }

                if updated {
                    debug!("Settings updated with defaults/migrations");
                    store.set("settings", serde_json::to_value(&settings).unwrap());
                }

                settings
            }
            Err(e) => {
                warn!("Failed to parse settings: {}", e);
                // Fall back to default settings if parsing fails
                let default_settings = get_default_settings();
                store.set("settings", serde_json::to_value(&default_settings).unwrap());
                default_settings
            }
        }
    } else {
        let default_settings = get_default_settings();
        store.set("settings", serde_json::to_value(&default_settings).unwrap());
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        store.set("settings", serde_json::to_value(&settings).unwrap());
    }

    settings
}

pub fn get_settings(app: &AppHandle) -> AppSettings {
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    // Settings reads also persist one-time migrations. Migration helpers are
    // idempotent, so this converges after the first read of an older store.
    let mut settings = if let Some(settings_value) = store.get("settings") {
        match serde_json::from_value::<AppSettings>(settings_value.clone()) {
            Ok(mut settings) => {
                if apply_settings_migrations(&mut settings, &settings_value) {
                    store.set("settings", serde_json::to_value(&settings).unwrap());
                }
                settings
            }
            Err(_) => {
                let default_settings = get_default_settings();
                store.set("settings", serde_json::to_value(&default_settings).unwrap());
                default_settings
            }
        }
    } else {
        let default_settings = get_default_settings();
        store.set("settings", serde_json::to_value(&default_settings).unwrap());
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        store.set("settings", serde_json::to_value(&settings).unwrap());
    }

    settings
}

fn apply_settings_migrations(
    settings: &mut AppSettings,
    settings_value: &serde_json::Value,
) -> bool {
    let mut updated = false;

    // One-time onboarding migration: users with an explicit selected model have
    // already made it through model selection. Users who merely have compatible
    // files on disk should still see onboarding.
    if settings_value.get("onboarding_completed").is_none() {
        settings.onboarding_completed = !settings.selected_model.is_empty();
        updated = true;
    }

    // One-time What's New migration: migrations only run on an existing store
    // (fresh installs stamp the current version via get_default_settings). A
    // missing key here means a user upgrading from before it existed — blank it
    // so they see the current release's What's New, mirroring the onboarding
    // migration's explicit first-run-vs-upgrade decision.
    if settings_value.get("whats_new_last_seen_version").is_none() {
        settings.whats_new_last_seen_version = String::new();
        updated = true;
    }

    let stored_schema_version = settings_value
        .get("settings_schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if stored_schema_version < 1 {
        // `transcribe_gpu_device` used to be a UI ordinal; it is now a
        // transcribe.cpp registry index. A positive legacy value can point at a
        // different GPU after CPU/accelerator/backend devices are included in
        // the registry, so reset ambiguous explicit selections to Auto once.
        if settings.transcribe_gpu_device > 0 {
            settings.transcribe_accelerator = TranscribeAcceleratorSetting::Auto;
            settings.transcribe_gpu_device = default_transcribe_gpu_device();
        }
        settings.settings_schema_version = CURRENT_SETTINGS_SCHEMA_VERSION;
        updated = true;
    }

    // Dictionary migration: fold the legacy flat `custom_words` list into the
    // richer `dictionary` (one entry per word, no aliases) so existing users keep
    // their vocabulary. One-shot via `dictionary_migrated`, NOT re-derived from
    // `settings.dictionary.is_empty()` — a user who migrates and then deletes
    // every dictionary entry must stay empty, not have custom_words resurrected
    // on the next read. `custom_words` is left intact for back-compat; `dictionary`
    // is the source of truth everywhere else.
    if !settings.dictionary_migrated {
        if settings.dictionary.is_empty() && !settings.custom_words.is_empty() {
            settings.dictionary = settings
                .custom_words
                .iter()
                .map(|word| DictionaryEntry {
                    word: word.clone(),
                    sounds_like: Vec::new(),
                    replace_exact: false,
                    case_sensitive: false,
                })
                .collect();
        }
        settings.dictionary_migrated = true;
        updated = true;
    }

    // One-time overlay migration (only while the new key is absent): the retired
    // overlay_position `none` meant "hide the overlay" → OverlayStyle::None; any
    // other position had it visible → Live. The position enum no longer has a
    // `none` variant (legacy "none" deserializes to Bottom via a serde alias), so
    // read the raw stored string to recover the old intent.
    if settings_value.get("overlay_style").is_none() {
        let was_hidden = settings_value
            .get("overlay_position")
            .and_then(|v| v.as_str())
            == Some("none");
        settings.overlay_style = if was_hidden {
            OverlayStyle::None
        } else {
            OverlayStyle::Live
        };
        updated = true;
    }

    updated
}

pub fn write_settings(app: &AppHandle, settings: AppSettings) {
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    store.set("settings", serde_json::to_value(&settings).unwrap());
}

pub fn get_bindings(app: &AppHandle) -> HashMap<String, ShortcutBinding> {
    let settings = get_settings(app);

    settings.bindings
}

pub fn get_stored_binding(app: &AppHandle, id: &str) -> ShortcutBinding {
    let bindings = get_bindings(app);

    let binding = bindings.get(id).unwrap().clone();

    binding
}

pub fn get_history_limit(app: &AppHandle) -> usize {
    let settings = get_settings(app);
    settings.history_limit
}

pub fn get_recording_retention_period(app: &AppHandle) -> RecordingRetentionPeriod {
    let settings = get_settings(app);
    settings.recording_retention_period
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_disable_auto_submit() {
        let settings = get_default_settings();
        assert!(!settings.auto_submit);
        assert_eq!(settings.auto_submit_key, AutoSubmitKey::Enter);
        assert_eq!(
            settings.settings_schema_version,
            CURRENT_SETTINGS_SCHEMA_VERSION
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn default_overlay_style_is_live_when_overlay_defaults_on() {
        let settings = get_default_settings();
        assert_eq!(settings.overlay_style, OverlayStyle::Live);
    }

    #[test]
    fn overlay_migration_keeps_disabled_overlay_off() {
        let mut settings = get_default_settings();

        // Legacy store: overlay was hidden via the retired position "none".
        let raw = serde_json::json!({
            "selected_model": "",
            "overlay_position": "none"
        });

        assert!(apply_settings_migrations(&mut settings, &raw));
        assert_eq!(settings.overlay_style, OverlayStyle::None);
    }

    #[test]
    fn legacy_none_overlay_position_deserializes_to_bottom() {
        // A persisted "none" must not fail the whole settings load; the serde
        // alias folds it onto Bottom (visibility is owned by overlay_style).
        let raw = serde_json::json!({ "overlay_position": "none" });
        let position: OverlayPosition =
            serde_json::from_value(raw.get("overlay_position").unwrap().clone())
                .expect("legacy \"none\" should deserialize, not error");
        assert_eq!(position, OverlayPosition::Bottom);
    }

    #[test]
    fn overlay_migration_promotes_enabled_overlay_to_live() {
        let mut settings = get_default_settings();
        settings.overlay_position = OverlayPosition::Top;
        settings.overlay_style = OverlayStyle::Minimal;

        let raw = serde_json::json!({
            "selected_model": "",
            "overlay_position": "top"
        });

        assert!(apply_settings_migrations(&mut settings, &raw));
        assert_eq!(settings.overlay_style, OverlayStyle::Live);
        assert_eq!(settings.overlay_position, OverlayPosition::Top);
    }

    #[test]
    fn gpu_device_migration_resets_legacy_positive_selection_to_auto() {
        let mut settings = get_default_settings();
        settings.transcribe_accelerator = TranscribeAcceleratorSetting::Gpu;
        settings.transcribe_gpu_device = 2;

        let raw = serde_json::json!({
            "transcribe_accelerator": "gpu",
            "transcribe_gpu_device": 2
        });

        assert!(apply_settings_migrations(&mut settings, &raw));
        assert_eq!(
            settings.transcribe_accelerator,
            TranscribeAcceleratorSetting::Auto
        );
        assert_eq!(
            settings.transcribe_gpu_device,
            default_transcribe_gpu_device()
        );
        assert_eq!(
            settings.settings_schema_version,
            CURRENT_SETTINGS_SCHEMA_VERSION
        );
    }

    #[test]
    fn gpu_device_migration_keeps_current_schema_positive_selection() {
        let mut settings = get_default_settings();
        settings.transcribe_accelerator = TranscribeAcceleratorSetting::Gpu;
        settings.transcribe_gpu_device = 2;

        let raw = serde_json::json!({
            "settings_schema_version": CURRENT_SETTINGS_SCHEMA_VERSION,
            "onboarding_completed": false,
            "whats_new_last_seen_version": default_whats_new_last_seen_version(),
            "overlay_style": "live",
            "transcribe_accelerator": "gpu",
            "transcribe_gpu_device": 2
        });

        assert!(!apply_settings_migrations(&mut settings, &raw));
        assert_eq!(
            settings.transcribe_accelerator,
            TranscribeAcceleratorSetting::Gpu
        );
        assert_eq!(settings.transcribe_gpu_device, 2);
    }

    #[test]
    fn custom_words_migrate_into_dictionary() {
        let mut settings = get_default_settings();
        settings.custom_words = vec!["ChargeBee".to_string(), "OpenFlow".to_string()];
        settings.dictionary = Vec::new();
        // Simulate a legacy store that predates the one-shot marker.
        settings.dictionary_migrated = false;

        let raw = serde_json::json!({
            "settings_schema_version": CURRENT_SETTINGS_SCHEMA_VERSION,
            "onboarding_completed": false,
            "whats_new_last_seen_version": default_whats_new_last_seen_version(),
            "overlay_style": "live",
            "custom_words": ["ChargeBee", "OpenFlow"]
        });

        assert!(apply_settings_migrations(&mut settings, &raw));
        assert_eq!(settings.dictionary.len(), 2);
        assert_eq!(settings.dictionary[0].word, "ChargeBee");
        assert!(settings.dictionary[0].sounds_like.is_empty());
        assert!(!settings.dictionary[0].replace_exact);
        assert!(!settings.dictionary[0].case_sensitive);
        assert_eq!(settings.dictionary[1].word, "OpenFlow");
        // Legacy list is preserved for back-compat.
        assert_eq!(settings.custom_words.len(), 2);
        // The one-shot marker is now set so this never runs again.
        assert!(settings.dictionary_migrated);
    }

    #[test]
    fn dictionary_migration_runs_only_once_via_marker() {
        // A second read of the same (already-migrated) settings must not touch
        // dictionary/custom_words again, and must report no further update.
        let mut settings = get_default_settings();
        settings.custom_words = vec!["ChargeBee".to_string()];
        settings.dictionary = vec![DictionaryEntry {
            word: "ChargeBee".to_string(),
            sounds_like: Vec::new(),
            replace_exact: false,
            case_sensitive: false,
        }];
        settings.dictionary_migrated = true;

        let raw = serde_json::json!({
            "settings_schema_version": CURRENT_SETTINGS_SCHEMA_VERSION,
            "onboarding_completed": false,
            "whats_new_last_seen_version": default_whats_new_last_seen_version(),
            "overlay_style": "live",
            "custom_words": ["ChargeBee"],
            "dictionary": [{ "word": "ChargeBee" }],
            "dictionary_migrated": true
        });

        assert!(!apply_settings_migrations(&mut settings, &raw));
        assert_eq!(settings.dictionary.len(), 1);
    }

    #[test]
    fn deleting_all_dictionary_entries_does_not_resurrect_custom_words() {
        // FIX 2 regression test: a user who already migrated (marker set) and
        // then deleted every dictionary entry must stay empty across a re-read,
        // even though the legacy custom_words list still has old data sitting
        // around for back-compat.
        let mut settings = get_default_settings();
        settings.custom_words = vec!["ChargeBee".to_string(), "OpenFlow".to_string()];
        settings.dictionary = Vec::new();
        settings.dictionary_migrated = true;

        let raw = serde_json::json!({
            "settings_schema_version": CURRENT_SETTINGS_SCHEMA_VERSION,
            "onboarding_completed": false,
            "whats_new_last_seen_version": default_whats_new_last_seen_version(),
            "overlay_style": "live",
            "custom_words": ["ChargeBee", "OpenFlow"],
            "dictionary": [],
            "dictionary_migrated": true
        });

        assert!(!apply_settings_migrations(&mut settings, &raw));
        assert!(settings.dictionary.is_empty());
        assert!(settings.dictionary_migrated);
    }

    #[test]
    fn dictionary_migrated_flag_serializes() {
        // Fresh installs persist `dictionary_migrated: true` (nothing to migrate).
        let default_settings = get_default_settings();
        let value = serde_json::to_value(&default_settings).unwrap();
        assert_eq!(
            value.get("dictionary_migrated"),
            Some(&serde_json::json!(true))
        );

        // A legacy store missing the key entirely deserializes it as `false`,
        // so the one-shot migration still fires exactly once for it.
        let legacy = serde_json::json!({
            "bindings": {},
            "push_to_talk": true,
            "audio_feedback": false,
            "external_script_path": null
        });
        let parsed: AppSettings = serde_json::from_value(legacy).unwrap();
        assert!(!parsed.dictionary_migrated);
    }

    #[test]
    fn dictionary_migration_is_idempotent_when_already_populated() {
        let mut settings = get_default_settings();
        settings.custom_words = vec!["ChargeBee".to_string()];
        settings.dictionary = vec![DictionaryEntry {
            word: "Something".to_string(),
            sounds_like: vec!["some thing".to_string()],
            replace_exact: false,
            case_sensitive: false,
        }];

        let raw = serde_json::json!({
            "settings_schema_version": CURRENT_SETTINGS_SCHEMA_VERSION,
            "onboarding_completed": false,
            "whats_new_last_seen_version": default_whats_new_last_seen_version(),
            "overlay_style": "live",
            "custom_words": ["ChargeBee"],
            "dictionary": [{ "word": "Something", "sounds_like": ["some thing"] }]
        });

        // No migration needed: dictionary is already the source of truth.
        assert!(!apply_settings_migrations(&mut settings, &raw));
        assert_eq!(settings.dictionary.len(), 1);
        assert_eq!(settings.dictionary[0].word, "Something");
    }

    #[test]
    fn debug_output_redacts_api_keys() {
        let mut settings = get_default_settings();
        settings
            .post_process_api_keys
            .insert("openai".to_string(), "sk-proj-secret-key-12345".to_string());
        settings.post_process_api_keys.insert(
            "anthropic".to_string(),
            "sk-ant-secret-key-67890".to_string(),
        );
        settings
            .post_process_api_keys
            .insert("empty_provider".to_string(), "".to_string());

        let debug_output = format!("{:?}", settings);

        assert!(!debug_output.contains("sk-proj-secret-key-12345"));
        assert!(!debug_output.contains("sk-ant-secret-key-67890"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn legacy_agent_without_cli_fields_defaults_to_prompt_kind() {
        // An increment-1 stored agent has none of the CLI fields. It MUST
        // deserialize cleanly (the store wipes to defaults on any parse
        // failure) and default to `kind == Prompt`, so the finish_dictation
        // dispatch keeps routing it through the increment-1 persona-LLM
        // transform — byte-for-byte unchanged.
        let raw = serde_json::json!({
            "id": "coder",
            "name": "Coder",
            "enabled": true,
            "binding_id": "agent:coder",
            "provider_id": "openrouter",
            "model": "gpt-4o-mini",
            "system_prompt": "You are a coder.",
            "output_mode": "inject"
        });
        let agent: AgentDefinition = serde_json::from_value(raw).unwrap();
        assert_eq!(agent.kind, AgentKind::Prompt);
        assert!(agent.cli_type.is_none());
        assert!(agent.binary_path.is_empty());
        assert!(agent.command_template.is_empty());
        assert!(agent.project_path.is_empty());
        assert_eq!(agent.output_sinks, vec![AgentOutputSink::Panel]);
        assert_eq!(agent.prompt_via, PromptDelivery::Stdin);
    }

    #[test]
    fn cli_agent_round_trips_through_serde() {
        // A CLI agent's new fields must survive a serialize→deserialize cycle
        // (this is what create_agent/update_agent persist through).
        let raw = serde_json::json!({
            "id": "claude",
            "name": "Claude Code",
            "enabled": true,
            "binding_id": "agent:claude",
            "provider_id": "",
            "kind": "cli",
            "cli_type": "claude",
            "binary_path": "/usr/local/bin/claude",
            "command_template": "-p --output-format stream-json --verbose",
            "project_path": "/tmp/proj",
            "output_sinks": ["panel", "notify", "file"],
            "prompt_via": "stdin"
        });
        let agent: AgentDefinition = serde_json::from_value(raw).unwrap();
        assert_eq!(agent.kind, AgentKind::Cli);
        assert_eq!(agent.cli_type, Some(AgentCliType::Claude));
        assert_eq!(agent.binary_path, "/usr/local/bin/claude");
        assert_eq!(agent.project_path, "/tmp/proj");
        assert_eq!(
            agent.output_sinks,
            vec![
                AgentOutputSink::Panel,
                AgentOutputSink::Notify,
                AgentOutputSink::File
            ]
        );

        // Re-serialize and back to confirm stability.
        let value = serde_json::to_value(&agent).unwrap();
        let again: AgentDefinition = serde_json::from_value(value).unwrap();
        assert_eq!(again.kind, AgentKind::Cli);
        assert_eq!(again.command_template, agent.command_template);
    }

    #[test]
    fn claude_default_template_matches_verified_flags() {
        let (template, via) = default_cli_template(AgentCliType::Claude);
        assert_eq!(
            template,
            "-p --output-format stream-json --verbose --permission-mode acceptEdits"
        );
        assert_eq!(via, PromptDelivery::Stdin);
        assert_eq!(
            default_cli_binary_name(AgentCliType::Claude),
            Some("claude")
        );
        assert_eq!(default_cli_binary_name(AgentCliType::Custom), None);
    }

    #[test]
    fn secret_map_debug_redacts_values() {
        let map = SecretMap(HashMap::from([("key".into(), "secret".into())]));
        let out = format!("{:?}", map);
        assert!(!out.contains("secret"));
        assert!(out.contains("[REDACTED]"));
    }
}
