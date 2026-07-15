//! `MeetingManager` — the meeting session state machine (DESIGN-meetings.md §4.1).
//!
//! A *sibling* of the single-flight `TranscriptionCoordinator`, never a client of
//! it: meetings run for an hour and must not block push-to-talk dictation. The
//! manager owns a meeting's lifecycle — capture (mic + system tap) → VAD-chunk →
//! transcribe (reusing `TranscriptionManager::transcribe`) → persist → emit —
//! and coexists with dictation, which keeps its own recorder byte-for-byte.
//!
//! Threading per active meeting:
//! - one **capture handle per channel** (mic recorder / system tap) that pushes
//!   native-rate mono frames into a channel;
//! - one **segmenter thread per channel** that VAD-chunks those frames into timed
//!   [`MeetingSegment`]s and (on stop) returns the full 16 kHz recording for a WAV;
//! - one **transcription worker** that drains segments from both channels,
//!   transcribes each (latency-tolerant; queues behind dictation on the shared
//!   engine), persists it, and emits `meeting-segment`;
//! - one **levels ticker** that emits `meeting-levels` for the two-channel meters.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

use crate::audio_toolkit::vad::{SileroVad, VadFrame, VoiceActivityDetector};
use crate::managers::transcription::TranscriptionManager;
use crate::meeting::capture::{MeetingCapture, MicCapture, SystemAudioTap};
use crate::meeting::detector::pid_for_bundle_id;
use crate::meeting::segmenter::{MeetingSegment, MeetingSegmenter};
use crate::meeting::MeetingChannel;

/// VAD probability threshold for meeting segmentation (matches the dictation
/// recorder's `VAD_THRESHOLD`).
const VAD_THRESHOLD: f32 = 0.3;

/* ─────────────────────────────  events  ──────────────────────────────── */

/// A known meeting app is running and the mic is in use — offer to capture.
/// Emitted by the [`crate::meeting::detector`].
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct MeetingDetected {
    pub bundle_id: String,
    pub app_name: String,
}

/// Meeting session status transition (`recording` | `processing` | `done` |
/// `failed`). `mic_only` / `notice` communicate a graceful system-audio degrade.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct MeetingState {
    pub meeting_id: i64,
    pub status: String,
    #[serde(default)]
    pub mic_only: bool,
    /// Stable machine tag for the degrade notice (`unsupported` | `permission` |
    /// `no_audio` | `mic_only` | `error`), or `None` when both channels captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// A freshly transcribed turn appended to the live transcript.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct MeetingSegmentEvent {
    pub meeting_id: i64,
    pub segment: MeetingSegmentRecord,
}

/// Two-channel input levels (0.0..~1.0 RMS) for the live meters.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct MeetingLevels {
    pub mic: f32,
    pub system: f32,
}

/* ─────────────────────────────  DB records  ──────────────────────────── */

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingSummary {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub title: String,
    pub app_bundle_id: Option<String>,
    pub status: String,
    pub segment_count: i64,
    /// Milliseconds from the first to the last segment (0 while empty).
    pub duration_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingRecord {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub title: String,
    pub app_bundle_id: Option<String>,
    pub status: String,
    pub mic_wav: Option<String>,
    pub system_wav: Option<String>,
    pub diarized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingSegmentRecord {
    pub id: i64,
    pub meeting_id: i64,
    pub t_start_ms: i64,
    pub t_end_ms: i64,
    /// `mic` ("You") or `system` ("Them").
    pub channel: String,
    /// Per-meeting diarization cluster (M2); `None` in M1.
    pub local_speaker: Option<i64>,
    /// Voice-fingerprint registry match (M3); `None` in M1.
    pub speaker_id: Option<i64>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingDetail {
    pub meeting: MeetingRecord,
    pub segments: Vec<MeetingSegmentRecord>,
}

/// Snapshot for the frontend: is a capture running, and did system audio degrade?
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingCaptureStatus {
    pub active: bool,
    pub meeting_id: Option<i64>,
    pub mic_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/* ─────────────────────────────  internals  ───────────────────────────── */

/// A transcription job handed from a segmenter to the worker.
struct SegJob(MeetingSegment);

/// A single voiced-passthrough VAD used only if the Silero model fails to load,
/// so a meeting still chunks (on the 30 s hard cap) instead of failing outright.
struct AlwaysVoiced;
impl VoiceActivityDetector for AlwaysVoiced {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        Ok(VadFrame::Speech(frame))
    }
}

/// Live capture session state. Dropping the captures stops the OS streams.
struct ActiveSession {
    meeting_id: i64,
    /// Kept alive for the meeting's duration; dropping them ends capture.
    captures: Vec<Box<dyn MeetingCapture>>,
    /// (segmenter thread, its channel) — join returns the channel's 16 kHz WAV.
    seg_threads: Vec<(JoinHandle<Vec<f32>>, MeetingChannel)>,
    worker: Option<JoinHandle<()>>,
    levels_stop: Arc<AtomicBool>,
    levels_thread: Option<JoinHandle<()>>,
    mic_only: bool,
    notice: Option<String>,
}

pub struct MeetingManager {
    app_handle: AppHandle,
    active: Mutex<Option<ActiveSession>>,
}

impl MeetingManager {
    pub fn new(app_handle: &AppHandle) -> Self {
        Self {
            app_handle: app_handle.clone(),
            active: Mutex::new(None),
        }
    }

    /// Whether a meeting capture is currently running.
    pub fn is_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    pub fn capture_status(&self) -> MeetingCaptureStatus {
        let guard = self.active.lock().unwrap();
        match guard.as_ref() {
            Some(s) => MeetingCaptureStatus {
                active: true,
                meeting_id: Some(s.meeting_id),
                mic_only: s.mic_only,
                notice: s.notice.clone(),
            },
            None => MeetingCaptureStatus {
                active: false,
                meeting_id: None,
                mic_only: false,
                notice: None,
            },
        }
    }

    fn db_path(&self) -> Result<std::path::PathBuf, String> {
        let dir = crate::portable::app_data_dir(&self.app_handle)
            .map_err(|e| format!("failed to resolve app data dir: {e}"))?;
        Ok(dir.join("history.db"))
    }

    fn recordings_meetings_dir(&self, meeting_id: i64) -> Result<std::path::PathBuf, String> {
        let dir = crate::portable::app_data_dir(&self.app_handle)
            .map_err(|e| format!("failed to resolve app data dir: {e}"))?
            .join("recordings")
            .join("meetings")
            .join(meeting_id.to_string());
        Ok(dir)
    }

    fn vad_path(&self) -> Option<String> {
        self.app_handle
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    }

    /* ---------------------------- start ----------------------------------- */

    /// Begin capturing a meeting. `app_bundle_id` (when known) targets the system
    /// audio tap at that app's PID; system capture degrades to mic-only on any
    /// failure. Returns the new meeting id.
    pub fn start_capture(&self, app_bundle_id: Option<String>) -> Result<i64, String> {
        let mut guard = self.active.lock().unwrap();
        if guard.is_some() {
            return Err("A meeting capture is already running".to_string());
        }

        let settings = crate::settings::get_settings(&self.app_handle);
        if !settings.meetings_enabled {
            return Err("Meetings are disabled in settings".to_string());
        }

        let started_at = Utc::now().timestamp();
        let title = format_title(started_at);
        let meeting_id = self.insert_meeting(started_at, &title, app_bundle_id.as_deref())?;

        // Shared transcription queue drained by the worker; segment senders are
        // cloned to each segmenter and the manager's own copy is dropped so the
        // worker exits once both channels finish.
        let (seg_tx, seg_rx) = mpsc::channel::<SegJob>();
        let levels = Arc::new([
            AtomicU32::new(0f32.to_bits()),
            AtomicU32::new(0f32.to_bits()),
        ]);
        let vad_path = self.vad_path();

        let mut captures: Vec<Box<dyn MeetingCapture>> = Vec::new();
        let mut seg_threads: Vec<(JoinHandle<Vec<f32>>, MeetingChannel)> = Vec::new();

        // ---- mic channel (essential) ----
        let device = self.resolve_mic_device(&settings);
        let (mic_ftx, mic_frx) = mpsc::channel::<Vec<f32>>();
        let mic_cap = MicCapture::start(device, mic_ftx).map_err(|e| {
            // Roll back the meeting row so a failed start leaves no ghost.
            let _ = self.mark_failed(meeting_id);
            format!("failed to start microphone capture: {e}")
        })?;
        let mic_sr = mic_cap.sample_rate();
        captures.push(Box::new(mic_cap));
        seg_threads.push((
            spawn_segmenter(
                MeetingChannel::Mic,
                mic_sr,
                mic_frx,
                seg_tx.clone(),
                vad_path.clone(),
                Arc::clone(&levels),
                0,
            ),
            MeetingChannel::Mic,
        ));

        // ---- system channel (best effort → graceful degrade) ----
        let mut mic_only = true;
        let mut notice: Option<String> = None;
        match app_bundle_id.as_deref().and_then(pid_for_bundle_id) {
            Some(pid) => {
                let (sys_ftx, sys_frx) = mpsc::channel::<Vec<f32>>();
                match SystemAudioTap::start(pid, sys_ftx) {
                    Ok(tap) => {
                        let sr = tap.sample_rate();
                        captures.push(Box::new(tap));
                        seg_threads.push((
                            spawn_segmenter(
                                MeetingChannel::System,
                                sr,
                                sys_frx,
                                seg_tx.clone(),
                                vad_path.clone(),
                                Arc::clone(&levels),
                                1,
                            ),
                            MeetingChannel::System,
                        ));
                        mic_only = false;
                        info!("meeting {meeting_id}: capturing mic + system audio");
                    }
                    Err(e) => {
                        warn!("meeting {meeting_id}: system audio unavailable ({e}); mic-only");
                        notice = Some(e.tag().to_string());
                    }
                }
            }
            None => {
                // Manual capture with no known app (or app not running): mic-only.
                notice = Some("mic_only".to_string());
            }
        }

        // Drop the manager's sender so the worker terminates when segmenters do.
        drop(seg_tx);

        // ---- transcription worker ----
        let worker = spawn_worker(self.app_handle.clone(), self.db_path()?, meeting_id, seg_rx);

        // ---- levels ticker ----
        let levels_stop = Arc::new(AtomicBool::new(false));
        let levels_thread = spawn_levels_ticker(
            self.app_handle.clone(),
            Arc::clone(&levels),
            Arc::clone(&levels_stop),
        );

        *guard = Some(ActiveSession {
            meeting_id,
            captures,
            seg_threads,
            worker: Some(worker),
            levels_stop,
            levels_thread: Some(levels_thread),
            mic_only,
            notice: notice.clone(),
        });
        drop(guard);

        let _ = MeetingState {
            meeting_id,
            status: "recording".to_string(),
            mic_only,
            notice,
        }
        .emit(&self.app_handle);

        Ok(meeting_id)
    }

    /* ---------------------------- stop ------------------------------------ */

    /// Stop the active capture, write channel WAVs, finalize the meeting row, and
    /// emit the terminal state. Idempotent-ish: errors if nothing is running.
    pub fn stop_capture(&self) -> Result<(), String> {
        let session = self.active.lock().unwrap().take();
        let Some(mut s) = session else {
            return Err("No meeting capture is running".to_string());
        };
        let meeting_id = s.meeting_id;
        info!("meeting {meeting_id}: stopping capture");

        // Stop the meters first.
        s.levels_stop.store(true, Ordering::Relaxed);
        if let Some(h) = s.levels_thread.take() {
            let _ = h.join();
        }

        // Dropping the capture handles stops the OS streams and closes the frame
        // channels, so each segmenter thread finalizes and returns its recording.
        s.captures.clear();

        let dir = self.recordings_meetings_dir(meeting_id)?;
        let _ = std::fs::create_dir_all(&dir);
        let mut mic_wav: Option<String> = None;
        let mut system_wav: Option<String> = None;
        for (handle, channel) in s.seg_threads.drain(..) {
            let samples = handle.join().unwrap_or_default();
            if samples.is_empty() {
                continue;
            }
            let file_name = match channel {
                MeetingChannel::Mic => "mic.wav",
                MeetingChannel::System => "system.wav",
            };
            match write_wav_16k_mono(&dir.join(file_name), &samples) {
                Ok(()) => {
                    let rel = format!("meetings/{meeting_id}/{file_name}");
                    match channel {
                        MeetingChannel::Mic => mic_wav = Some(rel),
                        MeetingChannel::System => system_wav = Some(rel),
                    }
                }
                Err(e) => error!("meeting {meeting_id}: failed to write {file_name}: {e}"),
            }
        }

        // Drain any remaining queued transcriptions.
        if let Some(worker) = s.worker.take() {
            let _ = worker.join();
        }

        let ended_at = Utc::now().timestamp();
        self.finish_meeting(meeting_id, ended_at, "done", mic_wav, system_wav)?;

        let _ = MeetingState {
            meeting_id,
            status: "done".to_string(),
            mic_only: s.mic_only,
            notice: None,
        }
        .emit(&self.app_handle);
        info!("meeting {meeting_id}: capture finished");
        Ok(())
    }

    fn resolve_mic_device(&self, settings: &crate::settings::AppSettings) -> Option<cpal::Device> {
        let name = settings.selected_microphone.as_ref()?;
        match crate::audio_toolkit::list_input_devices() {
            Ok(devices) => devices
                .into_iter()
                .find(|d| &d.name == name)
                .map(|d| d.device),
            Err(_) => None,
        }
    }

    /* ---------------------------- DB CRUD --------------------------------- */

    fn conn(&self) -> Result<Connection, String> {
        Connection::open(self.db_path()?).map_err(|e| format!("failed to open history.db: {e}"))
    }

    fn insert_meeting(
        &self,
        started_at: i64,
        title: &str,
        app_bundle_id: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO meetings (started_at, ended_at, title, app_bundle_id, status, mic_wav, system_wav, diarized, cal_event_id, cal_event_title)
             VALUES (?1, NULL, ?2, ?3, 'recording', NULL, NULL, 0, NULL, NULL)",
            params![started_at, title, app_bundle_id],
        )
        .map_err(|e| format!("failed to insert meeting: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    fn mark_failed(&self, meeting_id: i64) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE meetings SET status = 'failed', ended_at = ?2 WHERE id = ?1",
            params![meeting_id, Utc::now().timestamp()],
        )
        .map_err(|e| format!("failed to mark meeting failed: {e}"))?;
        Ok(())
    }

    fn finish_meeting(
        &self,
        meeting_id: i64,
        ended_at: i64,
        status: &str,
        mic_wav: Option<String>,
        system_wav: Option<String>,
    ) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE meetings SET status = ?2, ended_at = ?3, mic_wav = ?4, system_wav = ?5 WHERE id = ?1",
            params![meeting_id, status, ended_at, mic_wav, system_wav],
        )
        .map_err(|e| format!("failed to finalize meeting: {e}"))?;
        Ok(())
    }

    pub fn list_meetings(&self) -> Result<Vec<MeetingSummary>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.started_at, m.ended_at, m.title, m.app_bundle_id, m.status,
                        COUNT(s.id) AS seg_count,
                        COALESCE(MAX(s.t_end_ms), 0) - COALESCE(MIN(s.t_start_ms), 0) AS duration_ms
                 FROM meetings m
                 LEFT JOIN meeting_segments s ON s.meeting_id = m.id
                 GROUP BY m.id
                 ORDER BY m.started_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MeetingSummary {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    title: row.get(3)?,
                    app_bundle_id: row.get(4)?,
                    status: row.get(5)?,
                    segment_count: row.get(6)?,
                    duration_ms: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())
    }

    pub fn get_meeting(&self, meeting_id: i64) -> Result<Option<MeetingDetail>, String> {
        let conn = self.conn()?;
        let meeting = conn
            .query_row(
                "SELECT id, started_at, ended_at, title, app_bundle_id, status, mic_wav, system_wav, diarized
                 FROM meetings WHERE id = ?1",
                params![meeting_id],
                |row| {
                    Ok(MeetingRecord {
                        id: row.get(0)?,
                        started_at: row.get(1)?,
                        ended_at: row.get(2)?,
                        title: row.get(3)?,
                        app_bundle_id: row.get(4)?,
                        status: row.get(5)?,
                        mic_wav: row.get(6)?,
                        system_wav: row.get(7)?,
                        diarized: row.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;

        let Some(meeting) = meeting else {
            return Ok(None);
        };

        let segments = read_segments(&conn, meeting_id).map_err(|e| e.to_string())?;
        Ok(Some(MeetingDetail { meeting, segments }))
    }

    pub fn delete_meeting(&self, meeting_id: i64) -> Result<(), String> {
        // Refuse to delete a meeting that is currently capturing.
        if self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.meeting_id == meeting_id)
            .unwrap_or(false)
        {
            return Err("Stop the capture before deleting this meeting".to_string());
        }

        // Remove the audio directory (best effort), then the rows.
        if let Ok(dir) = self.recordings_meetings_dir(meeting_id) {
            let _ = std::fs::remove_dir_all(&dir);
        }
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM meeting_segments WHERE meeting_id = ?1",
            params![meeting_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM meetings WHERE id = ?1", params![meeting_id])
            .map_err(|e| e.to_string())?;
        debug!("deleted meeting {meeting_id}");
        Ok(())
    }
}

/* ─────────────────────────  free helpers  ─────────────────────────────── */

fn read_segments(
    conn: &Connection,
    meeting_id: i64,
) -> rusqlite::Result<Vec<MeetingSegmentRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, meeting_id, t_start_ms, t_end_ms, channel, local_speaker, speaker_id, text
         FROM meeting_segments WHERE meeting_id = ?1 ORDER BY t_start_ms ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![meeting_id], |row| {
        Ok(MeetingSegmentRecord {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            t_start_ms: row.get(2)?,
            t_end_ms: row.get(3)?,
            channel: row.get(4)?,
            local_speaker: row.get(5)?,
            speaker_id: row.get(6)?,
            text: row.get(7)?,
        })
    })?;
    rows.collect()
}

/// Insert a transcribed segment and return the full record (with its new id).
fn insert_segment(
    db_path: &std::path::Path,
    meeting_id: i64,
    seg: &MeetingSegment,
    text: &str,
) -> Result<MeetingSegmentRecord, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO meeting_segments (meeting_id, t_start_ms, t_end_ms, channel, local_speaker, speaker_id, text)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
        params![
            meeting_id,
            seg.t_start_ms as i64,
            seg.t_end_ms as i64,
            seg.channel.as_str(),
            text,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(MeetingSegmentRecord {
        id: conn.last_insert_rowid(),
        meeting_id,
        t_start_ms: seg.t_start_ms as i64,
        t_end_ms: seg.t_end_ms as i64,
        channel: seg.channel.as_str().to_string(),
        local_speaker: None,
        speaker_id: None,
        text: text.to_string(),
    })
}

fn spawn_segmenter(
    channel: MeetingChannel,
    in_hz: u32,
    frame_rx: Receiver<Vec<f32>>,
    seg_tx: Sender<SegJob>,
    vad_path: Option<String>,
    levels: Arc<[AtomicU32; 2]>,
    level_idx: usize,
) -> JoinHandle<Vec<f32>> {
    std::thread::Builder::new()
        .name(format!("meeting-seg-{}", channel.as_str()))
        .spawn(move || {
            let vad: Box<dyn VoiceActivityDetector> = match vad_path
                .as_deref()
                .and_then(|p| SileroVad::new(p, VAD_THRESHOLD).ok())
            {
                Some(v) => Box::new(v),
                None => {
                    warn!(
                        "meeting {}: VAD unavailable, falling back to fixed chunking",
                        channel.as_str()
                    );
                    Box::new(AlwaysVoiced)
                }
            };
            let mut seg = MeetingSegmenter::new(channel, in_hz, vad, true);

            while let Ok(frame) = frame_rx.recv() {
                let level = rms(&frame);
                levels[level_idx].store(level.to_bits(), Ordering::Relaxed);
                seg.push(&frame, |s| {
                    let _ = seg_tx.send(SegJob(s));
                });
            }
            seg.finish(|s| {
                let _ = seg_tx.send(SegJob(s));
            });
            levels[level_idx].store(0f32.to_bits(), Ordering::Relaxed);
            seg.take_recording()
        })
        .expect("failed to spawn meeting segmenter thread")
}

fn spawn_worker(
    app: AppHandle,
    db_path: std::path::PathBuf,
    meeting_id: i64,
    seg_rx: Receiver<SegJob>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("meeting-transcribe".into())
        .spawn(move || {
            let tm = match app.try_state::<Arc<TranscriptionManager>>() {
                Some(tm) => tm.inner().clone(),
                None => {
                    error!("meeting {meeting_id}: transcription manager unavailable");
                    return;
                }
            };
            while let Ok(SegJob(seg)) = seg_rx.recv() {
                let text = match tm.transcribe(seg.samples.clone()) {
                    Ok(t) => t.trim().to_string(),
                    Err(e) => {
                        warn!("meeting {meeting_id}: transcribe failed: {e}");
                        String::new()
                    }
                };
                if text.is_empty() {
                    continue;
                }
                match insert_segment(&db_path, meeting_id, &seg, &text) {
                    Ok(record) => {
                        let _ = MeetingSegmentEvent {
                            meeting_id,
                            segment: record,
                        }
                        .emit(&app);
                    }
                    Err(e) => error!("meeting {meeting_id}: failed to persist segment: {e}"),
                }
            }
        })
        .expect("failed to spawn meeting transcription worker")
}

fn spawn_levels_ticker(
    app: AppHandle,
    levels: Arc<[AtomicU32; 2]>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("meeting-levels".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mic = f32::from_bits(levels[0].load(Ordering::Relaxed));
                let system = f32::from_bits(levels[1].load(Ordering::Relaxed));
                let _ = MeetingLevels { mic, system }.emit(&app);
                std::thread::sleep(std::time::Duration::from_millis(66));
            }
        })
        .expect("failed to spawn meeting levels ticker")
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    (sum / frame.len() as f32).sqrt()
}

fn write_wav_16k_mono(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        writer.write_sample((clamped * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

fn format_title(timestamp: i64) -> String {
    if let Some(utc) = DateTime::from_timestamp(timestamp, 0) {
        let local = utc.with_timezone(&Local);
        format!("Meeting · {}", local.format("%B %e, %Y %l:%M%p"))
    } else {
        format!("Meeting {timestamp}")
    }
}
