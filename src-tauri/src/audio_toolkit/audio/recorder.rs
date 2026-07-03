use std::{
    io::Error,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Sample, SizedSample,
};

use crate::audio_toolkit::{
    audio::{AudioVisualiser, FrameResampler},
    constants,
    vad::{self, VadFrame},
    VoiceActivityDetector,
};

enum Cmd {
    Start(VadPolicy),
    Stop(mpsc::Sender<Vec<f32>>),
    /// Begin always-open wake-word monitoring (orthogonal to a recording
    /// session). VAD-passed frames are forwarded to `wake_cb` WITHOUT being
    /// accumulated into `processed_samples`.
    StartMonitor(VadPolicy),
    /// Stop wake-word monitoring. A manual recording session is unaffected.
    StopMonitor,
    Shutdown,
}

enum AudioChunk {
    Samples(Vec<f32>),
    EndOfStream,
}

/// How 16 kHz mono frames should be filtered for one recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadPolicy {
    /// Bypass VAD and forward every frame.
    Disabled,
    /// Current offline-tuned VAD profile.
    Offline,
    /// VAD profile with a longer post-speech tail for streaming-capable models.
    Streaming,
}

/// A single VAD engine plus the two hangover-tail lengths its smoothing wrapper
/// should use. The offline and streaming policies are never active
/// concurrently, so one detector is reconfigured per session (see `Cmd::Start`)
/// rather than kept as two resident engines.
#[derive(Clone)]
struct VadConfig {
    detector: Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>,
    offline_hangover_frames: usize,
    streaming_hangover_frames: usize,
}

impl VadConfig {
    /// Post-speech hangover tail (in 30 ms frames) for the given policy.
    /// `Disabled` never reaches the detector, so it maps to the offline value.
    fn hangover_for(&self, policy: VadPolicy) -> usize {
        match policy {
            VadPolicy::Streaming => self.streaming_hangover_frames,
            VadPolicy::Offline | VadPolicy::Disabled => self.offline_hangover_frames,
        }
    }
}

/// Callback invoked with each 16 kHz mono frame that passes the active capture
/// policy while recording. Used to feed a live streaming transcription as audio arrives.
pub type AudioFrameCallback = Arc<dyn Fn(&[f32]) + Send + Sync + 'static>;

pub struct AudioRecorder {
    device: Option<Device>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
    vad: Option<VadConfig>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    audio_cb: Option<AudioFrameCallback>,
    /// Callback for always-open wake-word monitoring. Receives VAD-passed frames
    /// while monitoring is active and no manual recording session is in progress.
    wake_cb: Option<AudioFrameCallback>,
    /// Whether always-open wake-word monitoring is active. Orthogonal to a manual
    /// recording session (which always takes per-frame precedence). Shared with the
    /// consumer thread so `start_monitor`/`stop_monitor` gate frame delivery.
    monitor: Arc<AtomicBool>,
    /// Monotonic count of VAD-passed (voiced) samples emitted since this recorder
    /// was created. It only advances while speech is being captured, so a caller
    /// polling it can detect end-of-speech in real time (the counter goes flat
    /// once the speaker stops and the VAD hangover tail drains). Never reset; use
    /// deltas rather than absolute values.
    voiced_samples: Arc<AtomicU64>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioRecorder {
            device: None,
            cmd_tx: None,
            worker_handle: None,
            vad: None,
            level_cb: None,
            audio_cb: None,
            wake_cb: None,
            monitor: Arc::new(AtomicBool::new(false)),
            voiced_samples: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Current count of voiced (VAD-passed) samples captured so far. Advances only
    /// while the speaker is talking; goes flat during silence. Used by the
    /// hands-free listener to keep the mic open while speech continues and stop
    /// after a configurable silence gap. Delta-based — the absolute value is not
    /// meaningful across recordings.
    pub fn voiced_sample_count(&self) -> u64 {
        self.voiced_samples.load(Ordering::Relaxed)
    }

    /// Attach a single VAD engine, reconfigured per session for the offline vs
    /// streaming hangover tail. The two policies are mutually exclusive within a
    /// recording, so one engine covers both instead of two resident instances.
    pub fn with_vad(
        mut self,
        detector: Box<dyn VoiceActivityDetector>,
        offline_hangover_frames: usize,
        streaming_hangover_frames: usize,
    ) -> Self {
        self.vad = Some(VadConfig {
            detector: Arc::new(Mutex::new(detector)),
            offline_hangover_frames,
            streaming_hangover_frames,
        });
        self
    }

    pub fn with_level_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        self.level_cb = Some(Arc::new(cb));
        self
    }

    /// Register a callback that receives real-time 16 kHz frames after the active
    /// VAD policy has been applied. Frames arrive in real time, in order, on the
    /// recorder's consumer thread — keep the callback cheap (e.g. forward to a
    /// channel) so it never stalls capture.
    pub fn with_audio_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(&[f32]) + Send + Sync + 'static,
    {
        self.audio_cb = Some(Arc::new(cb));
        self
    }

    /// Register a callback that receives real-time 16 kHz VAD-passed frames while
    /// always-open wake-word monitoring is active (see `start_monitor`). Frames
    /// arrive on the recorder's consumer thread; keep the callback cheap (forward
    /// to a channel). Suppressed while a manual recording session is in progress.
    pub fn with_wake_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(&[f32]) + Send + Sync + 'static,
    {
        self.wake_cb = Some(Arc::new(cb));
        self
    }

    pub fn open(&mut self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker_handle.is_some() {
            return Ok(()); // already open
        }

        let (sample_tx, sample_rx) = mpsc::channel::<AudioChunk>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let host = crate::audio_toolkit::get_cpal_host();
        let device = match device {
            Some(dev) => dev,
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };

        let thread_device = device.clone();
        let vad = self.vad.clone();
        // Move the optional level callback into the worker thread
        let level_cb = self.level_cb.clone();
        // Move the optional real-time audio frame callback into the worker thread
        let audio_cb = self.audio_cb.clone();
        // Move the optional wake-word monitor callback + its enable flag into the
        // worker thread so always-open monitoring can forward frames.
        let wake_cb = self.wake_cb.clone();
        let monitor = self.monitor.clone();
        // Share the voiced-sample counter with the consumer thread so callers can
        // poll live end-of-speech state.
        let voiced_samples = self.voiced_samples.clone();

        let worker = std::thread::spawn(move || {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_flag_for_stream = stop_flag.clone();
            let init_result = (|| -> Result<(cpal::Stream, u32), String> {
                let config = AudioRecorder::get_preferred_config(&thread_device)
                    .map_err(|e| format!("Failed to fetch preferred config: {e}"))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as usize;

                log::info!(
                    "Using device: {:?}\nSample rate: {}\nChannels: {}\nFormat: {:?}",
                    thread_device.name(),
                    sample_rate,
                    channels,
                    config.sample_format()
                );

                let stream = match config.sample_format() {
                    cpal::SampleFormat::U8 => AudioRecorder::build_stream::<u8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I8 => AudioRecorder::build_stream::<i8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I16 => AudioRecorder::build_stream::<i16>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I32 => AudioRecorder::build_stream::<i32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::F32 => AudioRecorder::build_stream::<f32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    sample_format => {
                        return Err(format!("Unsupported sample format: {sample_format:?}"));
                    }
                };

                stream
                    .play()
                    .map_err(|e| format!("Failed to start microphone stream: {e}"))?;

                Ok((stream, sample_rate))
            })();

            match init_result {
                Ok((stream, sample_rate)) => {
                    let _ = init_tx.send(Ok(()));
                    // Keep the stream alive while we process samples.
                    run_consumer(
                        sample_rate,
                        vad,
                        sample_rx,
                        cmd_rx,
                        level_cb,
                        audio_cb,
                        wake_cb,
                        monitor,
                        stop_flag,
                        voiced_samples,
                    );
                    drop(stream);
                }
                Err(error_message) => {
                    log::error!("{error_message}");
                    let _ = init_tx.send(Err(error_message));
                }
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => {
                self.device = Some(device);
                self.cmd_tx = Some(cmd_tx);
                self.worker_handle = Some(worker);
                Ok(())
            }
            Ok(Err(error_message)) => {
                let _ = worker.join();
                let kind = if is_microphone_access_denied(&error_message) {
                    std::io::ErrorKind::PermissionDenied
                } else {
                    std::io::ErrorKind::Other
                };
                Err(Box::new(Error::new(kind, error_message)))
            }
            Err(recv_error) => {
                let _ = worker.join();
                Err(Box::new(Error::other(format!(
                    "Failed to initialize microphone worker: {recv_error}"
                ))))
            }
        }
    }

    pub fn start(&self, vad_policy: VadPolicy) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Start(vad_policy))?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let (resp_tx, resp_rx) = mpsc::channel();
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Stop(resp_tx))?;
        }
        Ok(resp_rx.recv()?) // wait for the samples
    }

    /// Begin always-open wake-word monitoring. Orthogonal to `start()`: a manual
    /// recording session (if any) always takes per-frame precedence, so monitor
    /// delivery is auto-suppressed while recording. Monitored frames are
    /// VAD-passed and forwarded to `wake_cb` WITHOUT being accumulated.
    pub fn start_monitor(&self, vad_policy: VadPolicy) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::StartMonitor(vad_policy))?;
        }
        Ok(())
    }

    /// Stop always-open wake-word monitoring. Any manual recording session is
    /// unaffected.
    pub fn stop_monitor(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::StopMonitor)?;
        }
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
        self.device = None;
        Ok(())
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        sample_tx: mpsc::Sender<AudioChunk>,
        channels: usize,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: Sample + SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let mut output_buffer = Vec::new();
        let mut eos_sent = false;

        let stream_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
            if stop_flag.load(Ordering::Relaxed) {
                if !eos_sent {
                    let _ = sample_tx.send(AudioChunk::EndOfStream);
                    eos_sent = true;
                }
                return;
            }
            eos_sent = false;

            output_buffer.clear();

            if channels == 1 {
                output_buffer.extend(data.iter().map(|&sample| sample.to_sample::<f32>()));
            } else {
                let frame_count = data.len() / channels;
                output_buffer.reserve(frame_count);

                for frame in data.chunks_exact(channels) {
                    let mono_sample = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    output_buffer.push(mono_sample);
                }
            }

            if sample_tx
                .send(AudioChunk::Samples(output_buffer.clone()))
                .is_err()
            {
                log::error!("Failed to send samples");
            }
        };

        device.build_input_stream(
            &config.clone().into(),
            stream_cb,
            |err| log::error!("Stream error: {}", err),
            None,
        )
    }

    fn get_preferred_config(
        device: &cpal::Device,
    ) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
        // Use the device's native/default sample rate and let the FrameResampler
        // in run_consumer() downsample to 16kHz. This avoids forcing hardware into
        // a non-native rate which can cause issues on some devices (Bluetooth
        // codecs, certain ALSA drivers, etc.).
        let default_config = device.default_input_config()?;
        let target_rate = default_config.sample_rate();

        // Try to find the best sample format at the device's default rate
        let supported_configs = match device.supported_input_configs() {
            Ok(configs) => configs,
            Err(e) => {
                log::warn!("Could not enumerate input configs ({e}), using device default");
                return Ok(default_config);
            }
        };
        let mut best_config: Option<cpal::SupportedStreamConfigRange> = None;

        for config_range in supported_configs {
            if config_range.min_sample_rate() <= target_rate
                && config_range.max_sample_rate() >= target_rate
            {
                match best_config {
                    None => best_config = Some(config_range),
                    Some(ref current) => {
                        // Prioritize F32 > I16 > I32 > others
                        let score = |fmt: cpal::SampleFormat| match fmt {
                            cpal::SampleFormat::F32 => 4,
                            cpal::SampleFormat::I16 => 3,
                            cpal::SampleFormat::I32 => 2,
                            _ => 1,
                        };

                        if score(config_range.sample_format()) > score(current.sample_format()) {
                            best_config = Some(config_range);
                        }
                    }
                }
            }
        }

        if let Some(config) = best_config {
            return Ok(config.with_sample_rate(target_rate));
        }

        // Fall back to device default if no config matched (exotic/virtual devices)
        log::warn!(
            "No supported config matched device default rate {:?}, using default config",
            target_rate
        );
        Ok(default_config)
    }
}

pub fn is_microphone_access_denied(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("access is denied")
        || normalized.contains("permission denied")
        || normalized.contains("0x80070005")
}

pub fn is_no_input_device_error(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("no input device found")
        || (normalized.contains("failed to fetch preferred config")
            && normalized.contains("coreaudio"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{is_microphone_access_denied, is_no_input_device_error};
    use crate::audio_toolkit::vad::{
        VadFrame, VoiceActivityDetector, VAD_OFFLINE_HANGOVER_FRAMES, VAD_STREAMING_HANGOVER_FRAMES,
    };
    use anyhow::Result;
    use std::sync::atomic::AtomicUsize;

    /// Test VAD that classifies a frame as Speech when its first sample is
    /// non-zero, Noise otherwise. Lets a unit test drive `handle_frame`'s VAD
    /// branch deterministically without an audio device.
    struct TestVad {
        pushes: AtomicUsize,
    }

    impl TestVad {
        fn new() -> Self {
            Self {
                pushes: AtomicUsize::new(0),
            }
        }
    }

    impl VoiceActivityDetector for TestVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            self.pushes.fetch_add(1, Ordering::Relaxed);
            if frame.first().copied().unwrap_or(0.0) != 0.0 {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }
    }

    fn test_vad_config() -> VadConfig {
        VadConfig {
            detector: Arc::new(Mutex::new(Box::new(TestVad::new()))),
            offline_hangover_frames: VAD_OFFLINE_HANGOVER_FRAMES,
            streaming_hangover_frames: VAD_STREAMING_HANGOVER_FRAMES,
        }
    }

    /// Monitor mode: a voiced frame advances `voiced_samples` and reaches the
    /// wake callback, but MUST NOT be accumulated into `processed_samples`.
    #[test]
    fn monitor_frame_forwards_to_wake_cb_without_accumulating() {
        let vad = Some(test_vad_config());
        let voiced = Arc::new(AtomicU64::new(0));
        let mut out_buf = Vec::<f32>::new();

        let wake_hits = Arc::new(AtomicU64::new(0));
        let wake_cb: Option<AudioFrameCallback> = {
            let wake_hits = wake_hits.clone();
            Some(Arc::new(move |buf: &[f32]| {
                wake_hits.fetch_add(buf.len() as u64, Ordering::Relaxed);
            }))
        };

        let voiced_frame = vec![0.5f32; 480]; // 30ms @ 16kHz, first sample != 0
                                              // Simulate a monitor period: several voiced frames, no recording.
        for _ in 0..10 {
            handle_frame(
                &voiced_frame,
                false, // recording
                true,  // monitoring
                VadPolicy::Offline,
                &vad,
                &None, // audio_cb
                &wake_cb,
                &mut out_buf,
                &voiced,
            );
        }

        assert!(
            out_buf.is_empty(),
            "monitor mode must never append to processed_samples"
        );
        assert_eq!(
            voiced.load(Ordering::Relaxed),
            4800,
            "voiced_samples must advance on monitored voiced frames"
        );
        assert_eq!(
            wake_hits.load(Ordering::Relaxed),
            4800,
            "wake_cb must receive every voiced frame"
        );
    }

    /// Monitor mode drops non-voiced frames (VAD Noise): no counter/cb/buffer.
    #[test]
    fn monitor_drops_silence_frames() {
        let vad = Some(test_vad_config());
        let voiced = Arc::new(AtomicU64::new(0));
        let mut out_buf = Vec::<f32>::new();
        let wake_hits = Arc::new(AtomicU64::new(0));
        let wake_cb: Option<AudioFrameCallback> = {
            let wake_hits = wake_hits.clone();
            Some(Arc::new(move |buf: &[f32]| {
                wake_hits.fetch_add(buf.len() as u64, Ordering::Relaxed);
            }))
        };

        let silence_frame = vec![0.0f32; 480]; // first sample == 0 => Noise
        for _ in 0..5 {
            handle_frame(
                &silence_frame,
                false,
                true,
                VadPolicy::Offline,
                &vad,
                &None,
                &wake_cb,
                &mut out_buf,
                &voiced,
            );
        }

        assert!(out_buf.is_empty());
        assert_eq!(voiced.load(Ordering::Relaxed), 0);
        assert_eq!(wake_hits.load(Ordering::Relaxed), 0);
    }

    /// Recording mode is unchanged: a voiced frame accumulates into the output
    /// buffer AND advances the voiced counter (the verbatim manual path).
    #[test]
    fn recording_frame_still_accumulates() {
        let vad = Some(test_vad_config());
        let voiced = Arc::new(AtomicU64::new(0));
        let mut out_buf = Vec::<f32>::new();

        let voiced_frame = vec![0.5f32; 480];
        handle_frame(
            &voiced_frame,
            true,  // recording
            false, // monitoring
            VadPolicy::Offline,
            &vad,
            &None,
            &None,
            &mut out_buf,
            &voiced,
        );

        assert_eq!(
            out_buf.len(),
            480,
            "recording must accumulate voiced frames"
        );
        assert_eq!(voiced.load(Ordering::Relaxed), 480);
    }

    /// Recording takes precedence over monitoring when both flags are set, so a
    /// manual dictation is never starved by the monitor branch.
    #[test]
    fn recording_takes_precedence_over_monitoring() {
        let vad = Some(test_vad_config());
        let voiced = Arc::new(AtomicU64::new(0));
        let mut out_buf = Vec::<f32>::new();
        let wake_hits = Arc::new(AtomicU64::new(0));
        let wake_cb: Option<AudioFrameCallback> = {
            let wake_hits = wake_hits.clone();
            Some(Arc::new(move |buf: &[f32]| {
                wake_hits.fetch_add(buf.len() as u64, Ordering::Relaxed);
            }))
        };

        let voiced_frame = vec![0.5f32; 480];
        handle_frame(
            &voiced_frame,
            true, // recording
            true, // monitoring (should be ignored)
            VadPolicy::Offline,
            &vad,
            &None,
            &wake_cb,
            &mut out_buf,
            &voiced,
        );

        assert_eq!(out_buf.len(), 480, "recording path must run");
        assert_eq!(
            wake_hits.load(Ordering::Relaxed),
            0,
            "wake_cb must not fire while recording"
        );
    }

    #[test]
    fn detects_access_is_denied() {
        assert!(is_microphone_access_denied("Access is denied"));
    }

    #[test]
    fn detects_permission_denied() {
        assert!(is_microphone_access_denied("permission denied"));
    }

    #[test]
    fn detects_windows_error_code() {
        assert!(is_microphone_access_denied("WASAPI error: 0x80070005"));
    }

    #[test]
    fn does_not_match_unrelated_errors() {
        assert!(!is_microphone_access_denied("device not found"));
    }

    #[test]
    fn detects_no_input_device() {
        assert!(is_no_input_device_error("No input device found"));
    }

    #[test]
    fn detects_coreaudio_config_error() {
        assert!(is_no_input_device_error(
            "Failed to fetch preferred config: A backend-specific error has occurred: An unknown error unknown to the coreaudio-rs API occurred"
        ));
    }

    #[test]
    fn does_not_match_other_errors_for_no_device() {
        assert!(!is_no_input_device_error("permission denied"));
        assert!(!is_no_input_device_error("device not found"));
    }
}

#[allow(clippy::too_many_arguments)]
fn run_consumer(
    in_sample_rate: u32,
    vad: Option<VadConfig>,
    sample_rx: mpsc::Receiver<AudioChunk>,
    cmd_rx: mpsc::Receiver<Cmd>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    audio_cb: Option<AudioFrameCallback>,
    wake_cb: Option<AudioFrameCallback>,
    monitor: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    voiced_samples: Arc<AtomicU64>,
) {
    let mut frame_resampler = FrameResampler::new(
        in_sample_rate as usize,
        constants::WHISPER_SAMPLE_RATE as usize,
        Duration::from_millis(30),
    );

    let mut processed_samples = Vec::<f32>::new();
    let mut recording = false;
    let mut vad_policy = VadPolicy::Offline;

    // ---------- spectrum visualisation setup ---------------------------- //
    const BUCKETS: usize = 16;
    // Scale the FFT window to the device sample rate so the analysis window
    // (~33 ms) and frequency resolution (~30 Hz/bin) stay roughly constant
    // across devices. A fixed 512-sample window collapses the low vocal
    // buckets onto a single bin at 48 kHz (e.g. built-in laptop mics), and
    // would stutter at ~4-8 updates/sec on an 8-16 kHz Bluetooth headset.
    // Targets: 48 kHz -> 2048, 16 kHz -> 512, 8 kHz -> 256.
    let target_window = (f64::from(in_sample_rate) / 30.0).round() as usize;
    let window_size = [256usize, 512, 1024, 2048]
        .into_iter()
        .min_by_key(|w| w.abs_diff(target_window))
        .unwrap();
    let mut visualizer = AudioVisualiser::new(
        in_sample_rate,
        window_size,
        BUCKETS,
        400.0,  // vocal_min_hz
        4000.0, // vocal_max_hz
    );

    // Runs until the stream closes and `recv` returns `Err`.
    while let Ok(chunk) = sample_rx.recv() {
        let raw = match chunk {
            AudioChunk::Samples(s) => s,
            AudioChunk::EndOfStream => continue,
        };

        // ---------- spectrum processing ---------------------------------- //
        if let Some(buckets) = visualizer.feed(&raw) {
            if let Some(cb) = &level_cb {
                cb(buckets);
            }
        }

        // ---------- existing pipeline ------------------------------------ //
        // Read the always-open monitor flag once per chunk (cheap). When set and no
        // manual recording is in progress, VAD-passed frames are forwarded to
        // `wake_cb` without being accumulated.
        let monitoring = monitor.load(Ordering::Relaxed);
        frame_resampler.push(&raw, &mut |frame: &[f32]| {
            handle_frame(
                frame,
                recording,
                monitoring,
                vad_policy,
                &vad,
                &audio_cb,
                &wake_cb,
                &mut processed_samples,
                &voiced_samples,
            )
        });

        // non-blocking check for a command
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Start(policy) => {
                    stop_flag.store(false, Ordering::Relaxed);
                    vad_policy = policy;
                    processed_samples.clear();
                    recording = true;
                    visualizer.reset();
                    // Reconfigure the single VAD engine for this session's policy
                    // and clear its smoothing + recurrent state before it sees
                    // any frames.
                    if vad_policy != VadPolicy::Disabled {
                        if let Some(cfg) = &vad {
                            let mut det = cfg.detector.lock().unwrap();
                            det.set_hangover_frames(cfg.hangover_for(vad_policy));
                            det.reset();
                        }
                    }
                }
                Cmd::Stop(reply_tx) => {
                    recording = false;
                    stop_flag.store(true, Ordering::Relaxed);

                    // Drain all remaining audio until the producer confirms end-of-stream.
                    // The cpal callback sees the stop flag, sends EndOfStream, and goes
                    // silent — guaranteeing every captured sample is in the channel
                    // ahead of the sentinel.
                    loop {
                        match sample_rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(AudioChunk::Samples(remaining)) => {
                                frame_resampler.push(&remaining, &mut |frame: &[f32]| {
                                    handle_frame(
                                        frame,
                                        true,
                                        false,
                                        vad_policy,
                                        &vad,
                                        &audio_cb,
                                        &wake_cb,
                                        &mut processed_samples,
                                        &voiced_samples,
                                    )
                                });
                            }
                            Ok(AudioChunk::EndOfStream) => break,
                            Err(_) => {
                                log::warn!("Timed out waiting for EndOfStream from audio callback");
                                break;
                            }
                        }
                    }

                    frame_resampler.finish(&mut |frame: &[f32]| {
                        handle_frame(
                            frame,
                            true,
                            false,
                            vad_policy,
                            &vad,
                            &audio_cb,
                            &wake_cb,
                            &mut processed_samples,
                            &voiced_samples,
                        )
                    });

                    let _ = reply_tx.send(std::mem::take(&mut processed_samples));

                    // Resume the audio callback so the consumer loop can continue
                    // receiving chunks (important for always-on microphone mode).
                    stop_flag.store(false, Ordering::Relaxed);

                    // A manual session may have used the Streaming policy; if
                    // always-open monitoring is active, restore the Offline
                    // hangover tail and reset the detector so monitoring resumes
                    // with a clean, offline-tuned VAD (see design point 4).
                    if monitor.load(Ordering::Relaxed) {
                        if let Some(cfg) = &vad {
                            let mut det = cfg.detector.lock().unwrap();
                            det.set_hangover_frames(cfg.offline_hangover_frames);
                            det.reset();
                        }
                    }
                }
                Cmd::StartMonitor(policy) => {
                    monitor.store(true, Ordering::Relaxed);
                    // Configure the single VAD engine for offline-tuned monitoring
                    // and clear its smoothing + recurrent state (same reset block as
                    // Cmd::Start). Skipped if a recording session is currently
                    // driving the detector — the next Cmd::Start/Cmd::Stop restores
                    // the appropriate hangover, and monitoring is suppressed while
                    // recording anyway.
                    if !recording {
                        if let Some(cfg) = &vad {
                            let mut det = cfg.detector.lock().unwrap();
                            det.set_hangover_frames(cfg.hangover_for(policy));
                            det.reset();
                        }
                    }
                }
                Cmd::StopMonitor => {
                    monitor.store(false, Ordering::Relaxed);
                }
                Cmd::Shutdown => {
                    stop_flag.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

/// Route one 16 kHz mono frame according to the recorder's current mode.
///
/// Precedence is strict: a manual recording session (`recording == true`) always
/// wins, so wake-word monitoring auto-suppresses while dictating. Extracted from
/// `run_consumer` (was a nested fn) so it can be unit-tested directly.
///
/// - `recording`: the VERBATIM manual-dictation path — accumulates into `out_buf`
///   (`processed_samples`), advances `voiced_samples`, and feeds `audio_cb`
///   (StreamRouter). Its behavior must not change.
/// - else `monitoring`: VAD-gate the frame, advance `voiced_samples`, and forward
///   voiced frames to `wake_cb`. NEVER appends to `out_buf` — continuous
///   monitoring would otherwise leak memory unbounded.
/// - else: drop the frame.
#[allow(clippy::too_many_arguments)]
fn handle_frame(
    samples: &[f32],
    recording: bool,
    monitoring: bool,
    vad_policy: VadPolicy,
    vad: &Option<VadConfig>,
    audio_cb: &Option<AudioFrameCallback>,
    wake_cb: &Option<AudioFrameCallback>,
    out_buf: &mut Vec<f32>,
    voiced_samples: &Arc<AtomicU64>,
) {
    if recording {
        // ===================================================================
        // VERBATIM recording branch — behavior identical to pre-monitor code.
        // Do not modify: manual dictation, StreamRouter streaming, and the
        // voiced_sample_count primitive all depend on it byte-for-byte.
        // ===================================================================
        let mut emit = |buf: &[f32]| {
            out_buf.extend_from_slice(buf);
            // Advance the live voiced-sample counter so hands-free capture can
            // detect that speech is (still) arriving. Only VAD-passed frames reach
            // here under the Offline/Streaming policies, so the counter goes flat
            // during real silence.
            voiced_samples.fetch_add(buf.len() as u64, Ordering::Relaxed);
            if let Some(cb) = audio_cb {
                cb(buf);
            }
        };

        if vad_policy == VadPolicy::Disabled {
            emit(samples);
            return;
        }

        if let Some(cfg) = vad {
            let mut det = cfg.detector.lock().unwrap();
            match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
                VadFrame::Speech(buf) => emit(buf),
                VadFrame::Noise => {}
            }
        } else {
            emit(samples);
        }
        return;
    }

    if monitoring {
        // Always-open wake-word monitoring: VAD-gate the frame, advance the voiced
        // counter, and forward voiced frames to `wake_cb`. Crucially it NEVER
        // touches `out_buf` (processed_samples) — a continuously-open monitor would
        // otherwise grow it without bound.
        let forward = |buf: &[f32]| {
            voiced_samples.fetch_add(buf.len() as u64, Ordering::Relaxed);
            if let Some(cb) = wake_cb {
                cb(buf);
            }
        };

        if let Some(cfg) = vad {
            let mut det = cfg.detector.lock().unwrap();
            match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
                VadFrame::Speech(buf) => forward(buf),
                VadFrame::Noise => {}
            }
        } else {
            // No VAD configured (test/diagnostic recorders only): forward raw.
            forward(samples);
        }
    }
}
