//! VAD-boundary chunking for a single meeting channel.
//!
//! Meeting transcription reuses the existing local STT engine unchanged
//! (`TranscriptionManager::transcribe(Vec<f32>)`), so the meeting pipeline must
//! decide *what* to hand it and *when*. `MeetingSegmenter` consumes a raw audio
//! stream (any sample rate, mono), resamples it to the 16 kHz the engine expects,
//! runs Voice Activity Detection to find speech boundaries, and emits one
//! [`MeetingSegment`] per detected utterance with `[t_start_ms, t_end_ms]`
//! measured in the meeting's own timeline (DESIGN-meetings.md §4.2 — chunk timing
//! comes from the meeting pipeline's own VAD, not the STT engine's timestamps).
//!
//! The VAD is injected as a `Box<dyn VoiceActivityDetector>` so the segmentation
//! logic (boundary detection, timing, channel tagging) is unit-testable with a
//! deterministic fake, exactly like the audio recorder's `handle_frame` tests.

use crate::audio_toolkit::audio::FrameResampler;
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::vad::VoiceActivityDetector;
use crate::meeting::MeetingChannel;
use std::time::Duration;

/// Silero frame size at 16 kHz (30 ms). The resampler emits frames of this size.
const FRAME_SAMPLES: usize = (WHISPER_SAMPLE_RATE as usize) * 30 / 1000; // 480

/// Post-speech silence (in 30 ms frames) that ends a segment. ~450 ms — long
/// enough to bridge natural intra-sentence pauses, short enough to keep live
/// turns responsive.
const HANGOVER_FRAMES: u32 = 15;

/// Hard cap on a single segment (30 s). A speaker who never pauses would
/// otherwise defer transcription indefinitely; forcing a boundary here bounds
/// live-transcript latency (DESIGN-meetings.md §5.4 chunk cadence) and keeps each
/// `transcribe()` call a reasonable size.
const MAX_SEGMENT_SAMPLES: u64 = (WHISPER_SAMPLE_RATE as u64) * 30;

/// One finalized speech chunk, ready to transcribe. Timing is in the meeting
/// timeline (0 = capture start) so segments from both channels share a clock.
#[derive(Clone, Debug)]
pub struct MeetingSegment {
    pub channel: MeetingChannel,
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    /// 16 kHz mono samples for `TranscriptionManager::transcribe`.
    pub samples: Vec<f32>,
}

/// Converts one channel's raw audio into timed [`MeetingSegment`]s at VAD
/// boundaries. Not `Send`-shared: owned by a single per-channel worker thread.
pub struct MeetingSegmenter {
    channel: MeetingChannel,
    resampler: FrameResampler,
    vad: Box<dyn VoiceActivityDetector>,
    /// 16 kHz samples seen so far (the meeting clock for this channel).
    cursor: u64,
    in_speech: bool,
    seg_start: u64,
    seg_buf: Vec<f32>,
    /// Consecutive non-voiced frames since the last voiced frame while in speech.
    trailing_silence: u32,
    /// Optional full-stream 16 kHz capture for writing the channel WAV on stop.
    record_all: bool,
    all_samples: Vec<f32>,
}

impl MeetingSegmenter {
    /// `in_hz` is the source stream's sample rate; frames are resampled to 16 kHz
    /// internally. When `record_all` is set, every resampled sample is retained
    /// so the caller can write a `mic.wav` / `system.wav` at meeting end.
    pub fn new(
        channel: MeetingChannel,
        in_hz: u32,
        vad: Box<dyn VoiceActivityDetector>,
        record_all: bool,
    ) -> Self {
        Self {
            channel,
            resampler: FrameResampler::new(
                in_hz as usize,
                WHISPER_SAMPLE_RATE as usize,
                Duration::from_millis(30),
            ),
            vad,
            cursor: 0,
            in_speech: false,
            seg_start: 0,
            seg_buf: Vec::new(),
            trailing_silence: 0,
            record_all,
            all_samples: Vec::new(),
        }
    }

    fn ms(samples: u64) -> u64 {
        samples * 1000 / (WHISPER_SAMPLE_RATE as u64)
    }

    /// Feed a raw (native-rate, mono) chunk. Any finalized segments are handed to
    /// `emit` in order. Cheap when there is no speech.
    pub fn push(&mut self, raw: &[f32], mut emit: impl FnMut(MeetingSegment)) {
        // Collect resampled 16 kHz frames first; the VAD borrows `self` mutably,
        // so we can't call it inside the resampler's borrow of `self.resampler`.
        let mut frames: Vec<f32> = Vec::new();
        self.resampler
            .push(raw, |frame| frames.extend_from_slice(frame));
        for frame in frames.chunks(FRAME_SAMPLES) {
            self.on_frame(frame, &mut emit);
        }
    }

    /// Flush the resampler tail and finalize any open segment. Call once when the
    /// stream ends.
    pub fn finish(&mut self, mut emit: impl FnMut(MeetingSegment)) {
        let mut frames: Vec<f32> = Vec::new();
        self.resampler
            .finish(|frame| frames.extend_from_slice(frame));
        for frame in frames.chunks(FRAME_SAMPLES) {
            self.on_frame(frame, &mut emit);
        }
        if self.in_speech {
            self.finalize(self.cursor, &mut emit);
        }
    }

    fn on_frame(&mut self, frame: &[f32], emit: &mut impl FnMut(MeetingSegment)) {
        // The resampler always emits full 30 ms frames except possibly a short
        // final one from `finish`; Silero requires an exact frame, so pad short.
        let mut owned;
        let frame: &[f32] = if frame.len() == FRAME_SAMPLES {
            frame
        } else {
            owned = frame.to_vec();
            owned.resize(FRAME_SAMPLES, 0.0);
            &owned
        };

        self.cursor += FRAME_SAMPLES as u64;
        if self.record_all {
            self.all_samples.extend_from_slice(frame);
        }

        let voiced = self.vad.is_voice(frame).unwrap_or(true);

        if !self.in_speech {
            if voiced {
                self.in_speech = true;
                self.seg_start = self.cursor - FRAME_SAMPLES as u64;
                self.seg_buf.clear();
                self.seg_buf.extend_from_slice(frame);
                self.trailing_silence = 0;
            }
            return;
        }

        // In speech: keep accumulating (including short pauses) until the
        // hangover elapses or the hard cap forces a boundary.
        self.seg_buf.extend_from_slice(frame);
        if voiced {
            self.trailing_silence = 0;
        } else {
            self.trailing_silence += 1;
        }

        let seg_len = self.cursor - self.seg_start;
        if self.trailing_silence >= HANGOVER_FRAMES || seg_len >= MAX_SEGMENT_SAMPLES {
            self.finalize(self.cursor, emit);
        }
    }

    fn finalize(&mut self, end_cursor: u64, emit: &mut impl FnMut(MeetingSegment)) {
        // Trim the trailing-silence tail from both the buffer and the end time so
        // the persisted turn ends where the speaker actually stopped.
        let trim = (self.trailing_silence as usize) * FRAME_SAMPLES;
        let keep = self.seg_buf.len().saturating_sub(trim);
        let samples: Vec<f32> = self.seg_buf.drain(..keep).collect();
        let t_end =
            end_cursor.saturating_sub((self.trailing_silence as u64) * FRAME_SAMPLES as u64);

        self.in_speech = false;
        self.trailing_silence = 0;
        self.seg_buf.clear();

        if !samples.is_empty() {
            emit(MeetingSegment {
                channel: self.channel,
                t_start_ms: Self::ms(self.seg_start),
                t_end_ms: Self::ms(t_end.max(self.seg_start)),
                samples,
            });
        }
    }

    /// Take the full 16 kHz recording captured so far (only populated when
    /// `record_all` was set). Used to write the channel WAV on stop.
    pub fn take_recording(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.all_samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::vad::VadFrame;
    use anyhow::Result;

    /// Deterministic VAD: a frame is Speech iff its first sample is non-zero
    /// (mirrors the recorder's `TestVad`). Lets segmentation be tested without an
    /// audio device or the ONNX model.
    struct TestVad;
    impl VoiceActivityDetector for TestVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            if frame.first().copied().unwrap_or(0.0) != 0.0 {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }
    }

    fn voiced() -> Vec<f32> {
        vec![0.5; FRAME_SAMPLES]
    }
    fn silent() -> Vec<f32> {
        vec![0.0; FRAME_SAMPLES]
    }

    #[test]
    fn tags_channel_and_emits_one_segment_after_hangover() {
        let mut seg =
            MeetingSegmenter::new(MeetingChannel::System, 16000, Box::new(TestVad), false);
        let mut out = Vec::new();

        // 10 voiced frames, then enough silence to trip the hangover.
        for _ in 0..10 {
            seg.push(&voiced(), |s| out.push(s));
        }
        for _ in 0..HANGOVER_FRAMES {
            seg.push(&silent(), |s| out.push(s));
        }

        assert_eq!(
            out.len(),
            1,
            "one segment should finalize after the hangover"
        );
        let s = &out[0];
        assert_eq!(s.channel, MeetingChannel::System);
        assert_eq!(s.t_start_ms, 0, "speech starts at frame 0 → t=0ms");
        // 10 voiced frames * 30ms = 300ms of retained speech (silence trimmed).
        assert_eq!(s.t_end_ms, 300);
        assert_eq!(s.samples.len(), 10 * FRAME_SAMPLES);
    }

    #[test]
    fn mic_channel_tagging_is_independent() {
        let mut seg = MeetingSegmenter::new(MeetingChannel::Mic, 16000, Box::new(TestVad), false);
        let mut out = Vec::new();
        for _ in 0..3 {
            seg.push(&voiced(), |s| out.push(s));
        }
        seg.finish(|s| out.push(s));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel, MeetingChannel::Mic);
    }

    #[test]
    fn finish_flushes_open_segment() {
        let mut seg = MeetingSegmenter::new(MeetingChannel::Mic, 16000, Box::new(TestVad), false);
        let mut out = Vec::new();
        for _ in 0..5 {
            seg.push(&voiced(), |s| out.push(s));
        }
        assert!(out.is_empty(), "no boundary yet — speech is ongoing");
        seg.finish(|s| out.push(s));
        assert_eq!(
            out.len(),
            1,
            "finish() must flush the trailing open segment"
        );
    }

    #[test]
    fn pure_silence_emits_nothing() {
        let mut seg =
            MeetingSegmenter::new(MeetingChannel::System, 16000, Box::new(TestVad), false);
        let mut out = Vec::new();
        for _ in 0..50 {
            seg.push(&silent(), |s| out.push(s));
        }
        seg.finish(|s| out.push(s));
        assert!(out.is_empty());
    }

    #[test]
    fn two_utterances_produce_two_segments() {
        let mut seg =
            MeetingSegmenter::new(MeetingChannel::System, 16000, Box::new(TestVad), false);
        let mut out = Vec::new();
        for _ in 0..4 {
            seg.push(&voiced(), |s| out.push(s));
        }
        for _ in 0..HANGOVER_FRAMES {
            seg.push(&silent(), |s| out.push(s));
        }
        for _ in 0..4 {
            seg.push(&voiced(), |s| out.push(s));
        }
        seg.finish(|s| out.push(s));
        assert_eq!(out.len(), 2);
        assert!(
            out[1].t_start_ms > out[0].t_end_ms,
            "segments are monotonic"
        );
    }

    #[test]
    fn record_all_retains_full_stream() {
        let mut seg = MeetingSegmenter::new(MeetingChannel::Mic, 16000, Box::new(TestVad), true);
        for _ in 0..5 {
            seg.push(&voiced(), |_| {});
        }
        assert_eq!(seg.take_recording().len(), 5 * FRAME_SAMPLES);
    }
}
