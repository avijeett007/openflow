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
use crate::meeting::diarize::{self, DiarTurn};
use crate::meeting::segmenter::{MeetingSegment, MeetingSegmenter};
use crate::meeting::MeetingChannel;

/// VAD probability threshold for meeting segmentation (matches the dictation
/// recorder's `VAD_THRESHOLD`).
const VAD_THRESHOLD: f32 = 0.3;

/// Meeting-chunk transcription retry budget. A chunk can transiently fail when
/// the shared STT engine is loading or leased to the streaming dictation worker;
/// we retry with a short backoff so the (latency-tolerant) chunk queues behind
/// dictation instead of being dropped. ~12 × 500 ms ≈ 6 s covers a typical
/// dictation utterance plus a cold model load.
const MEETING_TRANSCRIBE_MAX_ATTEMPTS: u32 = 12;
const MEETING_TRANSCRIBE_RETRY_MS: u64 = 500;

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

/// Diarization relabeled a meeting's remote segments (after a provisional cycle
/// or the canonical final pass). The UI re-renders past segments with the new
/// `local_speaker` labels. `final_pass` marks the canonical result.
#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
pub struct MeetingSpeakersUpdated {
    pub meeting_id: i64,
    /// Distinct per-meeting speaker ordinals present after relabeling.
    pub speakers: Vec<i64>,
    pub final_pass: bool,
}

/// A per-meeting speaker display name ("Speaker 1" renamed to "Alice"), scoped to
/// one meeting. Does not touch the M3 fingerprint registry.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingSpeakerRecord {
    pub local_speaker: i64,
    pub name: String,
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
    /// Bit flags — bit 0 = private (spoken to OpenFlow during the meeting).
    #[serde(default)]
    pub flags: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct MeetingDetail {
    pub meeting: MeetingRecord,
    pub segments: Vec<MeetingSegmentRecord>,
    /// Per-meeting speaker display names (M2 rename). Empty when none set.
    #[serde(default)]
    pub speakers: Vec<MeetingSpeakerRecord>,
}

/// Diarization availability + the mode a meeting would run in, for the settings
/// card and the transcript status chip.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DiarizationStatus {
    /// The `meetings_diarization` setting.
    pub enabled: bool,
    /// Whether both diarization models are on disk.
    pub models_installed: bool,
    /// The `meetings_diarization_provisional` opt-in.
    pub provisional: bool,
    /// Effective mode (`provisional` | `final_only` | `off`).
    pub mode: diarize::DiarizationMode,
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
    /// Set true to stop the provisional worker; joined on stop.
    provisional_stop: Arc<AtomicBool>,
    provisional_thread: Option<JoinHandle<()>>,
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

        // Shared "is OpenFlow dictating right now" flag, updated by the levels
        // ticker and read by the segmenters to flag private mic utterances.
        let dictation_probe = Arc::new(AtomicBool::new(false));

        // Diarization mode for this meeting (after the Intel auto-degrade). Only a
        // provisional-capable mode needs the live system accumulator + worker; the
        // final pass runs on stop regardless of mode (when models are present).
        let models_installed = diarization_models_installed(&self.app_handle);
        let mode = effective_diarization_mode(&settings, models_installed);
        let system_accum: Option<Arc<Mutex<Vec<f32>>>> =
            if mode == diarize::DiarizationMode::Provisional {
                Some(Arc::new(Mutex::new(Vec::new())))
            } else {
                None
            };

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
                Some(Arc::clone(&dictation_probe)),
                None,
            ),
            MeetingChannel::Mic,
        ));

        // ---- system channel (best effort → graceful degrade) ----
        let mut mic_only = true;
        let mut notice: Option<String> = None;
        match app_bundle_id.as_deref().and_then(pid_for_bundle_id) {
            Some(pid) => {
                let (sys_ftx, sys_frx) = mpsc::channel::<Vec<f32>>();
                match SystemAudioTap::start(pid, app_bundle_id.as_deref(), sys_ftx) {
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
                                None,
                                system_accum.clone(),
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

        // ---- levels ticker (also drives the dictation probe) ----
        let levels_stop = Arc::new(AtomicBool::new(false));
        let levels_thread = spawn_levels_ticker(
            self.app_handle.clone(),
            Arc::clone(&levels),
            Arc::clone(&levels_stop),
            Arc::clone(&dictation_probe),
        );

        // ---- provisional diarization worker (opt-in; off by default on Intel) ----
        let provisional_stop = Arc::new(AtomicBool::new(false));
        let provisional_thread = match (&system_accum, models_installed) {
            (Some(accum), true) => {
                if let Some((seg_model, emb_model)) = diarization_model_paths(&self.app_handle) {
                    info!("meeting {meeting_id}: provisional diarization enabled");
                    if !diarize::provisional_default_enabled(diarize::REFERENCE_RATIO_RT) {
                        warn!(
                            "meeting {meeting_id}: live provisional labels may lag on this class \
                             of hardware (reference Intel can't keep up past a few minutes); the \
                             canonical final pass is unaffected"
                        );
                    }
                    Some(spawn_provisional_worker(
                        self.app_handle.clone(),
                        self.db_path()?,
                        meeting_id,
                        Arc::clone(accum),
                        seg_model,
                        emb_model,
                        Arc::clone(&provisional_stop),
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };

        *guard = Some(ActiveSession {
            meeting_id,
            captures,
            seg_threads,
            worker: Some(worker),
            levels_stop,
            levels_thread: Some(levels_thread),
            mic_only,
            notice: notice.clone(),
            provisional_stop,
            provisional_thread,
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

        // Stop the meters + provisional worker first.
        s.levels_stop.store(true, Ordering::Relaxed);
        if let Some(h) = s.levels_thread.take() {
            let _ = h.join();
        }
        s.provisional_stop.store(true, Ordering::Relaxed);
        if let Some(h) = s.provisional_thread.take() {
            let _ = h.join();
        }

        // Dropping the capture handles stops the OS streams and closes the frame
        // channels, so each segmenter thread finalizes and returns its recording.
        s.captures.clear();

        let dir = self.recordings_meetings_dir(meeting_id)?;
        let _ = std::fs::create_dir_all(&dir);
        let mut mic_wav: Option<String> = None;
        let mut system_wav: Option<String> = None;
        // Hold the remote (system) 16 kHz samples for the canonical final pass.
        let mut system_samples: Vec<f32> = Vec::new();
        for (handle, channel) in s.seg_threads.drain(..) {
            let samples = handle.join().unwrap_or_default();
            if samples.is_empty() {
                continue;
            }
            let file_name = match channel {
                MeetingChannel::Mic => "mic.wav",
                MeetingChannel::System => "system.wav",
            };
            if channel == MeetingChannel::System {
                system_samples = samples.clone();
            }
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

        // Drain any remaining queued transcriptions so every system segment exists
        // in the DB before the final pass fuses turns onto them.
        if let Some(worker) = s.worker.take() {
            let _ = worker.join();
        }

        let ended_at = Utc::now().timestamp();

        // Decide whether the canonical final diarization pass runs. When it can't
        // (disabled / models missing / no remote audio / non-macOS), the meeting
        // completes exactly as M1 — segments stay "Them" — never a failure.
        let settings = crate::settings::get_settings(&self.app_handle);
        let can_diarize = settings.meetings_diarization
            && !system_samples.is_empty()
            && diarization_model_paths(&self.app_handle).is_some();

        if can_diarize {
            // Mark processing, then diarize off-thread (a 30-min pass is ~4 min on
            // Intel — must not block the stop command). The bg pass sets diarized
            // and flips to done itself.
            self.finish_meeting(meeting_id, ended_at, "processing", mic_wav, system_wav)?;
            let _ = MeetingState {
                meeting_id,
                status: "processing".to_string(),
                mic_only: s.mic_only,
                notice: None,
            }
            .emit(&self.app_handle);
            let (seg_model, emb_model) =
                diarization_model_paths(&self.app_handle).expect("checked by can_diarize");
            spawn_final_pass(
                self.app_handle.clone(),
                self.db_path()?,
                meeting_id,
                system_samples,
                seg_model,
                emb_model,
                s.mic_only,
            );
        } else {
            self.finish_meeting(meeting_id, ended_at, "done", mic_wav, system_wav)?;
            let _ = MeetingState {
                meeting_id,
                status: "done".to_string(),
                mic_only: s.mic_only,
                notice: None,
            }
            .emit(&self.app_handle);
        }
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
        let speakers = read_speakers(&conn, meeting_id).map_err(|e| e.to_string())?;
        Ok(Some(MeetingDetail {
            meeting,
            segments,
            speakers,
        }))
    }

    /// Rename a per-meeting diarization cluster (M2). Upserts into
    /// `meeting_speakers`; an empty name clears the custom label.
    pub fn rename_speaker(
        &self,
        meeting_id: i64,
        local_speaker: i64,
        name: String,
    ) -> Result<(), String> {
        let conn = self.conn()?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            conn.execute(
                "DELETE FROM meeting_speakers WHERE meeting_id = ?1 AND local_speaker = ?2",
                params![meeting_id, local_speaker],
            )
            .map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "INSERT INTO meeting_speakers (meeting_id, local_speaker, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(meeting_id, local_speaker) DO UPDATE SET name = excluded.name",
                params![meeting_id, local_speaker, trimmed],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The per-meeting speaker display names.
    pub fn get_speakers(&self, meeting_id: i64) -> Result<Vec<MeetingSpeakerRecord>, String> {
        let conn = self.conn()?;
        read_speakers(&conn, meeting_id).map_err(|e| e.to_string())
    }

    /// Diarization availability + effective mode for the settings card / status
    /// chip. Never fails: returns `Off` when models are missing or the platform
    /// has no engine.
    pub fn diarization_status(&self) -> DiarizationStatus {
        let settings = crate::settings::get_settings(&self.app_handle);
        let models_installed = diarization_models_installed(&self.app_handle);
        let mode = effective_diarization_mode(&settings, models_installed);
        DiarizationStatus {
            enabled: settings.meetings_diarization,
            models_installed,
            provisional: settings.meetings_diarization_provisional,
            mode,
        }
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
        "SELECT id, meeting_id, t_start_ms, t_end_ms, channel, local_speaker, speaker_id, text, flags
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
            flags: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// Read the per-meeting speaker display names.
fn read_speakers(
    conn: &Connection,
    meeting_id: i64,
) -> rusqlite::Result<Vec<MeetingSpeakerRecord>> {
    let mut stmt = conn.prepare(
        "SELECT local_speaker, name FROM meeting_speakers WHERE meeting_id = ?1 ORDER BY local_speaker ASC",
    )?;
    let rows = stmt.query_map(params![meeting_id], |row| {
        Ok(MeetingSpeakerRecord {
            local_speaker: row.get(0)?,
            name: row.get(1)?,
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
        "INSERT INTO meeting_segments (meeting_id, t_start_ms, t_end_ms, channel, local_speaker, speaker_id, text, flags)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6)",
        params![
            meeting_id,
            seg.t_start_ms as i64,
            seg.t_end_ms as i64,
            seg.channel.as_str(),
            text,
            seg.flags,
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
        flags: seg.flags,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_segmenter(
    channel: MeetingChannel,
    in_hz: u32,
    frame_rx: Receiver<Vec<f32>>,
    seg_tx: Sender<SegJob>,
    vad_path: Option<String>,
    levels: Arc<[AtomicU32; 2]>,
    level_idx: usize,
    dictation_probe: Option<Arc<AtomicBool>>,
    live_sink: Option<Arc<Mutex<Vec<f32>>>>,
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
            if let Some(probe) = dictation_probe {
                seg.set_dictation_probe(probe);
            }
            if let Some(sink) = live_sink {
                seg.set_live_sink(sink);
            }

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
                // Transcribe on the shared engine, tolerating the two ways it can
                // be unavailable for a meeting chunk (DESIGN-meetings.md §4.2 —
                // "meeting chunks queue behind dictation"). `transcribe()` does
                // NOT load on its own; it errors when the engine isn't in the
                // mutex, so we drive the same on-demand load dictation uses and
                // retry with backoff:
                //  1. Not loaded (fresh session / idle-unload mid-meeting):
                //     `initiate_model_load()` kicks off a load and `transcribe()`
                //     blocks on the load condvar.
                //  2. Loaded but leased out to the streaming dictation worker
                //     (`is_model_loaded()` true, `lock_engine()` None): the chunk
                //     must wait for the lease to return rather than be dropped.
                // No change to TranscriptionManager's locking; dictation keeps
                // priority and the meeting chunk (latency-tolerant) queues behind.
                let text = {
                    let mut out = String::new();
                    for attempt in 0..MEETING_TRANSCRIBE_MAX_ATTEMPTS {
                        tm.initiate_model_load();
                        match tm.transcribe(seg.samples.clone()) {
                            Ok(t) => {
                                out = t.trim().to_string();
                                break;
                            }
                            Err(e) => {
                                if attempt + 1 == MEETING_TRANSCRIBE_MAX_ATTEMPTS {
                                    warn!("meeting {meeting_id}: transcribe failed after {} attempts: {e}", attempt + 1);
                                } else {
                                    std::thread::sleep(std::time::Duration::from_millis(
                                        MEETING_TRANSCRIBE_RETRY_MS,
                                    ));
                                }
                            }
                        }
                    }
                    out
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
    dictation_probe: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("meeting-levels".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mic = f32::from_bits(levels[0].load(Ordering::Relaxed));
                let system = f32::from_bits(levels[1].load(Ordering::Relaxed));
                let _ = MeetingLevels { mic, system }.emit(&app);
                // Track whether OpenFlow's own dictation is actively capturing, so
                // a mic utterance that begins during it is flagged private. Only
                // active dictation counts — passive wake-word monitoring does not.
                let dictating = app
                    .try_state::<Arc<crate::managers::audio::AudioRecordingManager>>()
                    .map(|rm| rm.is_recording())
                    .unwrap_or(false);
                dictation_probe.store(dictating, Ordering::Relaxed);
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

/* ─────────────────────────  diarization glue  ─────────────────────────── */

/// Are both diarization models installed? macOS-only (no engine elsewhere).
fn diarization_models_installed(app: &AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::meeting::diar_models::models_installed(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

/// Resolve the (segmentation, embedding) model paths iff both installed.
fn diarization_model_paths(app: &AppHandle) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        crate::meeting::diar_models::resolve_model_paths(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        None
    }
}

/// The mode a meeting runs in, given settings + model availability.
fn effective_diarization_mode(
    settings: &crate::settings::AppSettings,
    models_installed: bool,
) -> diarize::DiarizationMode {
    if !settings.meetings_diarization || !models_installed {
        return diarize::DiarizationMode::Off;
    }
    if settings.meetings_diarization_provisional {
        diarize::DiarizationMode::Provisional
    } else {
        diarize::DiarizationMode::FinalOnly
    }
}

/// Remote (system) channel segments as `(id, t_start_ms, t_end_ms)` for fusion.
fn read_system_segments(
    db_path: &std::path::Path,
    meeting_id: i64,
) -> Result<Vec<(i64, i64, i64)>, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, t_start_ms, t_end_ms FROM meeting_segments
             WHERE meeting_id = ?1 AND channel = 'system' ORDER BY t_start_ms ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![meeting_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// Persist the fused per-segment speaker labels.
fn apply_speaker_labels(
    db_path: &std::path::Path,
    labels: &[(i64, Option<i64>)],
) -> Result<(), String> {
    let mut conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (seg_id, speaker) in labels {
        tx.execute(
            "UPDATE meeting_segments SET local_speaker = ?2 WHERE id = ?1",
            params![seg_id, speaker],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

fn set_diarized(db_path: &std::path::Path, meeting_id: i64) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE meetings SET diarized = 1 WHERE id = ?1",
        params![meeting_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn finalize_meeting_done(db_path: &std::path::Path, meeting_id: i64) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE meetings SET status = 'done' WHERE id = ?1",
        params![meeting_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Run the engine over `samples`, fuse turns onto the remote segments, persist
/// labels, and emit `meeting-speakers-updated`. Returns the (relabeled) turns so
/// the provisional worker can keep them as the next cycle's baseline. macOS-only
/// engine; a no-op returning `None` on other platforms.
#[allow(unused_variables)]
fn diarize_and_apply(
    app: &AppHandle,
    db_path: &std::path::Path,
    meeting_id: i64,
    samples: &[f32],
    seg_model: &std::path::Path,
    emb_model: &std::path::Path,
    previous: &[DiarTurn],
    final_pass: bool,
) -> Option<Vec<DiarTurn>> {
    #[cfg(target_os = "macos")]
    {
        let engine = match diarize::open_default(seg_model, emb_model) {
            Ok(e) => e,
            Err(e) => {
                warn!("meeting {meeting_id}: diarization engine load failed: {e}");
                return None;
            }
        };
        debug!(
            "meeting {meeting_id}: diarization engine sample_rate={}",
            engine.sample_rate()
        );
        let raw = match engine.diarize(samples) {
            Ok(t) => t,
            Err(e) => {
                warn!("meeting {meeting_id}: diarization failed: {e}");
                return None;
            }
        };
        // Keep provisional cluster ids stable across cycles; the final pass is
        // canonical and stands on its own.
        let turns = if final_pass {
            raw
        } else {
            diarize::stabilize_labels(previous, &raw)
        };

        let segments = match read_system_segments(db_path, meeting_id) {
            Ok(s) => s,
            Err(e) => {
                warn!("meeting {meeting_id}: read segments failed: {e}");
                return Some(turns);
            }
        };
        let labels = diarize::fuse_speaker_labels(&segments, &turns);
        if let Err(e) = apply_speaker_labels(db_path, &labels) {
            warn!("meeting {meeting_id}: apply labels failed: {e}");
            return Some(turns);
        }
        let speakers = {
            let mut v: Vec<i64> = turns.iter().map(|t| t.speaker).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let _ = MeetingSpeakersUpdated {
            meeting_id,
            speakers,
            final_pass,
        }
        .emit(app);
        Some(turns)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Background canonical final pass: diarize the whole remote channel, overwrite
/// provisional labels, set `diarized = 1`, flip the meeting to `done`. Any
/// failure leaves the meeting done-as-M1 (labels stay "Them").
#[allow(clippy::too_many_arguments)]
fn spawn_final_pass(
    app: AppHandle,
    db_path: std::path::PathBuf,
    meeting_id: i64,
    system_samples: Vec<f32>,
    seg_model: std::path::PathBuf,
    emb_model: std::path::PathBuf,
    mic_only: bool,
) {
    std::thread::Builder::new()
        .name("meeting-diarize-final".into())
        .spawn(move || {
            info!("meeting {meeting_id}: canonical diarization pass started");
            let applied = diarize_and_apply(
                &app,
                &db_path,
                meeting_id,
                &system_samples,
                &seg_model,
                &emb_model,
                &[],
                true,
            );
            if applied.is_some() {
                let _ = set_diarized(&db_path, meeting_id);
                info!("meeting {meeting_id}: diarization complete");
            }
            let _ = finalize_meeting_done(&db_path, meeting_id);
            let _ = MeetingState {
                meeting_id,
                status: "done".to_string(),
                mic_only,
                notice: None,
            }
            .emit(&app);
        })
        .expect("failed to spawn final diarization pass");
}

/// Provisional worker: every ~30 s re-diarize the accumulated remote audio and
/// relabel, so live labels appear during the call. Skips a cycle if the audio
/// hasn't grown. Off by default on Intel (auto-degrade); only spawned when the
/// user opts in and models are present.
#[allow(clippy::too_many_arguments)]
fn spawn_provisional_worker(
    app: AppHandle,
    db_path: std::path::PathBuf,
    meeting_id: i64,
    accum: Arc<Mutex<Vec<f32>>>,
    seg_model: std::path::PathBuf,
    emb_model: std::path::PathBuf,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    const CYCLE_MS: u64 = 30_000;
    const TICK_MS: u64 = 500;
    std::thread::Builder::new()
        .name("meeting-diarize-provisional".into())
        .spawn(move || {
            let mut previous: Vec<DiarTurn> = Vec::new();
            let mut last_len = 0usize;
            let mut elapsed = 0u64;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
                elapsed += TICK_MS;
                if elapsed < CYCLE_MS {
                    continue;
                }
                elapsed = 0;
                let snapshot = match accum.lock() {
                    Ok(g) => g.clone(),
                    Err(_) => continue,
                };
                // Skip if no new audio since last cycle (nothing to relabel).
                if snapshot.len() <= last_len || snapshot.is_empty() {
                    continue;
                }
                last_len = snapshot.len();
                let snap_s = snapshot.len() as f32
                    / crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as f32;
                if !diarize::provisional_cycle_budget_ok(
                    diarize::REFERENCE_RATIO_RT,
                    snap_s,
                    diarize::PROVISIONAL_CYCLE_BUDGET_S,
                ) {
                    warn!(
                        "meeting {meeting_id}: provisional cycle over {snap_s:.0}s of audio likely \
                         exceeds the {}s budget — labels will lag",
                        diarize::PROVISIONAL_CYCLE_BUDGET_S
                    );
                }
                if let Some(turns) = diarize_and_apply(
                    &app, &db_path, meeting_id, &snapshot, &seg_model, &emb_model, &previous, false,
                ) {
                    previous = turns;
                }
            }
        })
        .expect("failed to spawn provisional diarization worker")
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
