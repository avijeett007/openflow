//! OpenFlow Meetings (M1) — capture + on-device transcription of meetings.
//!
//! A second, parallel pipeline alongside push-to-talk dictation. It captures two
//! independent local streams — the user's microphone ("You") and the meeting
//! app's output audio ("Them", via a macOS CoreAudio process tap) — VAD-chunks
//! each, transcribes the chunks through the existing local STT engine, and
//! persists/streams the resulting turns.
//!
//! Module layout (mirrors DESIGN-meetings.md §4.1 so M2–M4 slot in later):
//! - [`capture`]  — `MeetingCapture` trait + macOS mic (cpal) & system-audio
//!   (CoreAudio process tap) implementations. All FFI is contained here.
//! - [`detector`] — `MeetingDetector`: known-bundle-id + mic-in-use fusion that
//!   emits `meeting-detected`, debounced and self-suppressed while OpenFlow's own
//!   recorder is active.
//! - [`segmenter`] — `MeetingSegmenter`: VAD-boundary chunking that turns a raw
//!   per-channel audio stream into timed speech segments (the STT unit).
//!
//! The session state machine (`MeetingManager`) lives in `managers/meeting.rs`,
//! next to the other managers, and is a *sibling* — never a client — of the
//! single-flight `TranscriptionCoordinator`.

pub mod capture;
pub mod detector;
pub mod segmenter;

use serde::{Deserialize, Serialize};
use specta::Type;

/// Which of a meeting's two independent streams a segment came from. `Mic` is the
/// local user ("You"); `System` is the tapped meeting-app output ("Them"). Stored
/// verbatim in `meeting_segments.channel` as `mic` / `system` so M2 diarization
/// can keep the mic channel as "You" by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum MeetingChannel {
    Mic,
    System,
}

impl MeetingChannel {
    /// Stable lowercase tag persisted in SQLite (`meeting_segments.channel`).
    pub fn as_str(self) -> &'static str {
        match self {
            MeetingChannel::Mic => "mic",
            MeetingChannel::System => "system",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "mic" => Some(MeetingChannel::Mic),
            "system" => Some(MeetingChannel::System),
            _ => None,
        }
    }
}
