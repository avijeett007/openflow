//! Meeting audio capture: two independent local streams.
//!
//! - **Mic channel ("You")** — a *dedicated* [`AudioRecorder`] instance, separate
//!   from the dictation recorder in `managers/audio.rs`. Meeting capture and
//!   dictation therefore coexist without touching each other's stream, lazy-close,
//!   or wake-word plumbing.
//! - **System channel ("Them")** — the meeting app's output audio via a macOS
//!   **CoreAudio process tap** (macOS 14.4+): translate the app's PID to an
//!   audio process object, build a `CATapDescription` mono mixdown, wrap the tap
//!   in a private aggregate device, and read frames off its IOProc. All the
//!   CoreAudio FFI is contained in this module (DESIGN-meetings.md §4.1).
//!
//! Every system-capture path degrades gracefully: macOS < 14.4 (no
//! `CATapDescription`), a denied tap-capture TCC grant (unsigned dev builds never
//! prompt), or a process that can't be resolved all return a typed
//! [`CaptureError`] so the caller can fall back to **mic-only** capture with a
//! clear UI notice — never a crash (DESIGN-meetings.md §2, risk #3).

use std::fmt;
use std::sync::mpsc::Sender;

use crate::audio_toolkit::{AudioRecorder, VadPolicy};

/// Uniform handle over a live capture source. Dropping the handle stops capture.
/// Kept object-safe so a Windows (WASAPI loopback) backend can slot in later
/// (DESIGN-meetings.md §2, §4.1).
pub trait MeetingCapture: Send {
    /// Sample rate of the frames delivered to the capture channel, in Hz.
    fn sample_rate(&self) -> u32;
}

/// Why a capture source could not start. `System*` variants drive the mic-only
/// graceful-degrade path; the UI surfaces a matching notice.
#[derive(Debug, Clone)]
pub enum CaptureError {
    /// macOS is older than 14.4 (no CoreAudio process-tap support).
    Unsupported,
    /// The tap-capture TCC grant was denied, or the (unsigned) build never got a
    /// prompt. The fix-it is "grant in System Settings → Privacy & Security".
    PermissionDenied,
    /// The target app's PID could not be resolved to an audio process object
    /// (app not running, or not producing audio).
    ProcessNotFound,
    /// Any other CoreAudio / device failure, with a message for logs.
    Other(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Unsupported => {
                write!(f, "system-audio capture requires macOS 14.4 or newer")
            }
            CaptureError::PermissionDenied => write!(
                f,
                "audio-capture permission was denied (grant it in System Settings → Privacy & Security)"
            ),
            CaptureError::ProcessNotFound => {
                write!(f, "the meeting app is not producing audio yet")
            }
            CaptureError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl CaptureError {
    /// A short, stable machine tag for the frontend degrade notice (i18n key
    /// suffix). Kept separate from the human `Display` string.
    pub fn tag(&self) -> &'static str {
        match self {
            CaptureError::Unsupported => "unsupported",
            CaptureError::PermissionDenied => "permission",
            CaptureError::ProcessNotFound => "no_audio",
            CaptureError::Other(_) => "error",
        }
    }
}

/// Microphone capture for a meeting — a second, independent [`AudioRecorder`].
///
/// Runs with `VadPolicy::Disabled` so every 16 kHz mono frame is forwarded to the
/// channel (the meeting pipeline does its own VAD segmentation); the recorder's
/// own cpal worker thread owns the (`!Send`) stream, so this handle is `Send`.
pub struct MicCapture {
    recorder: AudioRecorder,
}

impl MicCapture {
    /// Open the mic and start forwarding 16 kHz mono frames to `tx`. `device` is
    /// the resolved meeting microphone (or `None` for the system default).
    pub fn start(device: Option<cpal::Device>, tx: Sender<Vec<f32>>) -> Result<Self, CaptureError> {
        let mut recorder = AudioRecorder::new()
            .map_err(|e| CaptureError::Other(format!("failed to create mic recorder: {e}")))?
            .with_audio_callback(move |frame| {
                let _ = tx.send(frame.to_vec());
            });

        recorder
            .open(device)
            .map_err(|e| CaptureError::Other(format!("failed to open mic stream: {e}")))?;
        // Disabled VAD => the recorder forwards raw 16 kHz frames; segmentation is
        // done downstream by MeetingSegmenter so both channels share one policy.
        recorder
            .start(VadPolicy::Disabled)
            .map_err(|e| CaptureError::Other(format!("failed to start mic capture: {e}")))?;

        Ok(Self { recorder })
    }
}

impl MeetingCapture for MicCapture {
    fn sample_rate(&self) -> u32 {
        // AudioRecorder always resamples to the Whisper rate before the callback.
        crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        let _ = self.recorder.stop();
        let _ = self.recorder.close();
    }
}

/* ─────────────────────────  system-audio process tap  ───────────────────── */

#[cfg(target_os = "macos")]
pub use macos_tap::{any_input_device_running, SystemAudioTap};

#[cfg(not(target_os = "macos"))]
pub fn any_input_device_running() -> bool {
    false
}

/// System-audio capture stub on non-macOS: never available in M1.
#[cfg(not(target_os = "macos"))]
pub struct SystemAudioTap;

#[cfg(not(target_os = "macos"))]
impl SystemAudioTap {
    pub fn start(
        _pid: i32,
        _bundle_id: Option<&str>,
        _tx: Sender<Vec<f32>>,
    ) -> Result<Self, CaptureError> {
        Err(CaptureError::Unsupported)
    }
}

#[cfg(not(target_os = "macos"))]
impl MeetingCapture for SystemAudioTap {
    fn sample_rate(&self) -> u32 {
        16000
    }
}

#[cfg(target_os = "macos")]
mod macos_tap {
    use super::{CaptureError, MeetingCapture};
    use core::ffi::c_void;
    use core::ptr::NonNull;
    use log::{debug, info, warn};
    use std::sync::mpsc::Sender;

    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, AnyThread};
    use objc2_core_audio::{
        kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyNominalSampleRate,
        kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyProcessObjectList,
        kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioProcessPropertyBundleID, kAudioProcessPropertyPID,
        AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceStart, AudioDeviceStop,
        AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
        AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
        AudioObjectPropertyAddress, CATapDescription,
    };
    use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
    use objc2_core_foundation::CFDictionary;

    /// `OSStatus` is a private alias in the bindings crate; it is `i32`, and a
    /// transparent alias, so a local `i32` alias is fn-pointer-compatible with the
    /// crate's `AudioDeviceIOProc` signature.
    type OSStatus = i32;
    use objc2_foundation::{
        NSArray, NSMutableArray, NSMutableDictionary, NSNumber, NSString, NSUUID,
    };

    /// `kAudioObjectSystemObject` is `1`; the crate exposes it as `c_int`.
    const SYSTEM_OBJECT: AudioObjectID = 1;

    /// Client data handed to the C IOProc: where tapped frames are forwarded.
    struct TapCtx {
        tx: Sender<Vec<f32>>,
    }

    /// A live CoreAudio process tap wrapped in a private aggregate device.
    /// `Drop` tears the whole thing down (stop IO → destroy IOProc → destroy
    /// aggregate → destroy tap → free client data).
    pub struct SystemAudioTap {
        aggregate_id: AudioObjectID,
        tap_id: AudioObjectID,
        proc_id: objc2_core_audio::AudioDeviceIOProcID,
        ctx: *mut TapCtx,
        sample_rate: u32,
    }

    // The stored ids are integers and the ctx pointer is only touched by the
    // realtime IOProc (which we start/stop under our control); the handle itself
    // is safe to move between threads.
    unsafe impl Send for SystemAudioTap {}

    impl SystemAudioTap {
        /// Attempt to tap the output audio of the target app, forwarding native-rate
        /// mono f32 frames to `tx`. Returns a typed [`CaptureError`] on any failure
        /// so the caller can degrade to mic-only.
        ///
        /// `pid` is the app's main process id (from `NSWorkspace`); `bundle_id` is
        /// its bundle identifier when known. Browsers (Chrome/Brave/Edge/Safari)
        /// render call audio from a *separate* helper process, so tapping the single
        /// main-PID process object yields silence on the "Them" channel. We therefore
        /// enumerate every audio process object and tap the mixdown of ALL that
        /// belong to the target app — matched by exact PID, bundle-id prefix
        /// (`com.google.Chrome` ⊇ `com.google.Chrome.helper*`), or PID descent from
        /// the main process. See [`select_process_objects`].
        pub fn start(
            pid: i32,
            bundle_id: Option<&str>,
            tx: Sender<Vec<f32>>,
        ) -> Result<Self, CaptureError> {
            // Capability probe: CATapDescription only exists on macOS 14.2+. Its
            // absence is the clean "OS too old" signal — no version parsing.
            if AnyClass::get(c"CATapDescription").is_none() {
                return Err(CaptureError::Unsupported);
            }

            // Enumerate the system's audio process objects and select every one that
            // belongs to the target app. Logged verbatim to aid field diagnosis of
            // "which process is Chrome playing Meet audio from".
            let table = enumerate_audio_processes();
            for p in &table {
                debug!(
                    "meeting tap: audio process obj {} pid {} bundle {:?}",
                    p.obj_id, p.pid, p.bundle_id
                );
            }
            let mut selected = select_process_objects(&table, pid, bundle_id, |cand| {
                is_descendant_of(cand, pid, PID_ANCESTRY_MAX_DEPTH)
            });

            // Fallback: if enumeration/selection found nothing (e.g. the app is not
            // yet in the audio process list), tap just the translated main PID —
            // the original single-process behavior — before conceding defeat.
            if selected.is_empty() {
                match process_object_for_pid(pid) {
                    Some(obj) => {
                        warn!(
                            "meeting tap: process enumeration matched nothing for pid {pid} \
                             bundle {bundle_id:?}; falling back to single translated object {obj}"
                        );
                        selected.push(obj);
                    }
                    None => return Err(CaptureError::ProcessNotFound),
                }
            }
            info!(
                "meeting tap: pid {pid} bundle {bundle_id:?} -> {} audio process object(s) {:?}",
                selected.len(),
                selected
            );

            // Build a mono-mixdown tap description over ALL selected processes.
            let uuid = NSUUID::new();
            let uuid_str = unsafe { uuid.UUIDString() }.to_string();
            let (tap_id, _desc_uuid) = unsafe {
                let nums: Vec<objc2::rc::Retained<NSNumber>> = selected
                    .iter()
                    .map(|obj| NSNumber::numberWithUnsignedInt(*obj))
                    .collect();
                let refs: Vec<&NSNumber> = nums.iter().map(|n| &**n).collect();
                let procs: objc2::rc::Retained<NSArray<NSNumber>> = NSArray::from_slice(&refs);
                let desc =
                    CATapDescription::initMonoMixdownOfProcesses(CATapDescription::alloc(), &procs);
                desc.setUUID(&uuid);

                let mut tap_id: AudioObjectID = 0;
                let status = AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id);
                if status != 0 || tap_id == 0 {
                    warn!("AudioHardwareCreateProcessTap failed: OSStatus {status}");
                    return Err(classify_tap_error(status));
                }
                (tap_id, uuid)
            };

            // Wrap the tap in a private aggregate device we can read from.
            let aggregate_id = match create_aggregate_device(&uuid_str) {
                Ok(id) => id,
                Err(e) => {
                    unsafe {
                        AudioHardwareDestroyProcessTap(tap_id);
                    }
                    return Err(e);
                }
            };

            let sample_rate = device_nominal_sample_rate(aggregate_id).unwrap_or(48_000.0) as u32;
            info!(
                "meeting tap: aggregate device {aggregate_id} up (tap {tap_id}, {sample_rate} Hz)"
            );

            // Register the IOProc and start IO. The client data box is leaked into
            // a raw pointer for the C callback and reclaimed in Drop.
            let ctx = Box::into_raw(Box::new(TapCtx { tx }));
            let mut proc_id: objc2_core_audio::AudioDeviceIOProcID = None;
            let status = unsafe {
                AudioDeviceCreateIOProcID(
                    aggregate_id,
                    Some(tap_ioproc),
                    ctx as *mut c_void,
                    NonNull::from(&mut proc_id),
                )
            };
            if status != 0 || proc_id.is_none() {
                warn!("AudioDeviceCreateIOProcID failed: OSStatus {status}");
                unsafe {
                    drop(Box::from_raw(ctx));
                    AudioHardwareDestroyAggregateDevice(aggregate_id);
                    AudioHardwareDestroyProcessTap(tap_id);
                }
                return Err(CaptureError::Other(format!(
                    "AudioDeviceCreateIOProcID failed (OSStatus {status})"
                )));
            }

            let status = unsafe { AudioDeviceStart(aggregate_id, proc_id) };
            if status != 0 {
                warn!("AudioDeviceStart failed: OSStatus {status}");
                unsafe {
                    AudioDeviceDestroyIOProcID(aggregate_id, proc_id);
                    drop(Box::from_raw(ctx));
                    AudioHardwareDestroyAggregateDevice(aggregate_id);
                    AudioHardwareDestroyProcessTap(tap_id);
                }
                return Err(CaptureError::Other(format!(
                    "AudioDeviceStart failed (OSStatus {status})"
                )));
            }

            Ok(Self {
                aggregate_id,
                tap_id,
                proc_id,
                ctx,
                sample_rate,
            })
        }
    }

    impl MeetingCapture for SystemAudioTap {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    impl Drop for SystemAudioTap {
        fn drop(&mut self) {
            unsafe {
                AudioDeviceStop(self.aggregate_id, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate_id, self.proc_id);
                AudioHardwareDestroyAggregateDevice(self.aggregate_id);
                AudioHardwareDestroyProcessTap(self.tap_id);
                if !self.ctx.is_null() {
                    drop(Box::from_raw(self.ctx));
                }
            }
            debug!("meeting tap: torn down aggregate {}", self.aggregate_id);
        }
    }

    /// The C IOProc CoreAudio calls on its realtime thread with tapped audio.
    /// Kept minimal: downmix to mono and forward over the channel.
    unsafe extern "C-unwind" fn tap_ioproc(
        _in_device: AudioObjectID,
        _in_now: NonNull<AudioTimeStamp>,
        in_input: NonNull<AudioBufferList>,
        _in_input_time: NonNull<AudioTimeStamp>,
        _out_output: NonNull<AudioBufferList>,
        _in_output_time: NonNull<AudioTimeStamp>,
        client: *mut c_void,
    ) -> OSStatus {
        if client.is_null() {
            return 0;
        }
        let ctx = &*(client as *const TapCtx);
        let list = in_input.as_ref();
        if list.mNumberBuffers == 0 {
            return 0;
        }
        // mBuffers is a flexible array; read the first buffer (mono mixdown).
        let buffer = &*list.mBuffers.as_ptr();
        if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
            return 0;
        }
        let float_count = (buffer.mDataByteSize as usize) / std::mem::size_of::<f32>();
        let data = std::slice::from_raw_parts(buffer.mData as *const f32, float_count);
        let channels = buffer.mNumberChannels.max(1) as usize;
        let mono: Vec<f32> = if channels <= 1 {
            data.to_vec()
        } else {
            data.chunks(channels)
                .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
                .collect()
        };
        let _ = ctx.tx.send(mono);
        0
    }

    /// Translate a process id to its CoreAudio process AudioObjectID.
    fn process_object_for_pid(pid: i32) -> Option<AudioObjectID> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut obj: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                NonNull::from(&addr),
                std::mem::size_of::<i32>() as u32,
                &pid as *const i32 as *const c_void,
                NonNull::from(&mut size),
                NonNull::new(&mut obj as *mut _ as *mut c_void)?,
            )
        };
        if status == 0 && obj != 0 {
            Some(obj)
        } else {
            None
        }
    }

    /// How far up the parent-PID chain we walk when deciding whether a candidate
    /// process descends from the target app. Browser audio helpers sit 1–2 levels
    /// below the main process; a small cap bounds the sysctl work and stops runaway
    /// walks on a corrupt table.
    const PID_ANCESTRY_MAX_DEPTH: u32 = 6;

    /// One entry of the system audio-process table: an audio `AudioObjectID`, the
    /// OS pid behind it, and its bundle id (absent for helper/system processes that
    /// don't carry one).
    #[derive(Clone, Debug, PartialEq)]
    struct AudioProcInfo {
        obj_id: AudioObjectID,
        pid: i32,
        bundle_id: Option<String>,
    }

    /// Does audio-process `candidate`'s bundle id belong to the app identified by
    /// `target`? True on an exact match or a dot-boundary prefix, so target
    /// `com.google.Chrome` matches the main app *and* `com.google.Chrome.helper`,
    /// `com.google.Chrome.helper.Renderer`, … but NOT `com.google.Chromecast`.
    fn bundle_matches(target: &str, candidate: &str) -> bool {
        if target.is_empty() {
            return false;
        }
        if candidate == target {
            return true;
        }
        candidate
            .strip_prefix(target)
            .is_some_and(|rest| rest.starts_with('.'))
    }

    /// Select every audio process object that belongs to the target app, by the
    /// union of three signals (deduped, input order preserved):
    ///   (a) exact PID match against the app's main pid;
    ///   (b) bundle-id prefix match ([`bundle_matches`]) — catches browser audio
    ///       helpers whose bundle extends the app's;
    ///   (c) `is_descendant` — the candidate's parent-PID chain reaches the main
    ///       pid, catching helpers that don't carry a bundle id.
    ///
    /// Pure over its inputs (`is_descendant` is injected) so it is unit-testable
    /// without CoreAudio or a live process tree.
    fn select_process_objects(
        table: &[AudioProcInfo],
        target_pid: i32,
        target_bundle: Option<&str>,
        is_descendant: impl Fn(i32) -> bool,
    ) -> Vec<AudioObjectID> {
        let mut out: Vec<AudioObjectID> = Vec::new();
        for p in table {
            let exact = p.pid == target_pid;
            let by_bundle = match (target_bundle, p.bundle_id.as_deref()) {
                (Some(t), Some(c)) => bundle_matches(t, c),
                _ => false,
            };
            let by_child = !exact && is_descendant(p.pid);
            if (exact || by_bundle || by_child) && !out.contains(&p.obj_id) {
                out.push(p.obj_id);
            }
        }
        out
    }

    /// Read one `AudioObjectID`-scalar / small-scalar property into `T`.
    fn read_process_scalar<T: Copy>(
        obj: AudioObjectID,
        selector: objc2_core_audio::AudioObjectPropertySelector,
        default: T,
    ) -> Option<T> {
        let addr = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut value = default;
        let mut size = std::mem::size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(&mut value as *mut _ as *mut c_void)?,
            )
        };
        if status == 0 {
            Some(value)
        } else {
            None
        }
    }

    /// Read an audio process object's bundle id (a `CFStringRef` property). The
    /// getter returns a +1 reference; we take ownership via `Retained::from_raw`
    /// (NSString is toll-free bridged to CFString) so it is released on drop.
    fn process_bundle_id(obj: AudioObjectID) -> Option<String> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioProcessPropertyBundleID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut cf: *mut NSString = std::ptr::null_mut();
        let mut size = std::mem::size_of::<*mut NSString>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                obj,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(&mut cf as *mut _ as *mut c_void)?,
            )
        };
        if status != 0 || cf.is_null() {
            return None;
        }
        let s = unsafe { objc2::rc::Retained::from_raw(cf)? };
        let out = s.to_string();
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Enumerate the whole system audio-process table
    /// (`kAudioHardwarePropertyProcessObjectList`), resolving each object's PID and
    /// bundle id. Best-effort: an object whose PID can't be read is dropped; a
    /// missing bundle id is left `None`.
    fn enumerate_audio_processes() -> Vec<AudioProcInfo> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut size: u32 = 0;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                SYSTEM_OBJECT,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
            )
        };
        if status != 0 || size == 0 {
            return Vec::new();
        }
        let count = size as usize / std::mem::size_of::<AudioObjectID>();
        let mut ids: Vec<AudioObjectID> = vec![0; count];
        let mut io_size = size;
        let dst = match NonNull::new(ids.as_mut_ptr() as *mut c_void) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut io_size),
                dst,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(io_size as usize / std::mem::size_of::<AudioObjectID>());
        ids.into_iter()
            .filter_map(|obj| {
                let pid = read_process_scalar::<i32>(obj, kAudioProcessPropertyPID, -1)?;
                Some(AudioProcInfo {
                    obj_id: obj,
                    pid,
                    bundle_id: process_bundle_id(obj),
                })
            })
            .collect()
    }

    /// The direct parent PID of `pid`, via libproc `PROC_PIDTBSDINFO`. `None` if the
    /// process is gone or the call fails.
    fn parent_pid(pid: i32) -> Option<i32> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let sz = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                sz,
            )
        };
        if n == sz {
            Some(info.pbi_ppid as i32)
        } else {
            None
        }
    }

    /// Does `pid`'s parent chain reach `ancestor` within `max_depth` hops? Strict:
    /// `pid == ancestor` is NOT a descendant (that case is the exact-PID rule). The
    /// walk stops at pid ≤ 1 (launchd) so it always terminates.
    fn is_descendant_of(pid: i32, ancestor: i32, max_depth: u32) -> bool {
        if pid == ancestor || pid <= 1 {
            return false;
        }
        let mut cur = pid;
        for _ in 0..max_depth {
            match parent_pid(cur) {
                Some(pp) if pp == ancestor => return true,
                Some(pp) if pp > 1 => cur = pp,
                _ => return false,
            }
        }
        false
    }

    /// Build the private aggregate device that exposes the tap's audio. Keys are
    /// the documented CoreAudio aggregate-device dictionary strings; the sub-tap
    /// is referenced by the tap description's UUID.
    fn create_aggregate_device(tap_uuid: &str) -> Result<AudioObjectID, CaptureError> {
        unsafe {
            let dict: objc2::rc::Retained<NSMutableDictionary<NSString, AnyObject>> =
                NSMutableDictionary::new();

            let name = NSString::from_str("OpenFlow Meeting Capture");
            let agg_uid = NSString::from_str(&format!("openflow.meeting.{tap_uuid}"));
            let yes = NSNumber::numberWithBool(true);
            let no = NSNumber::numberWithBool(false);
            let empty: objc2::rc::Retained<NSMutableArray<AnyObject>> = NSMutableArray::new();

            // Sub-tap entry: { uid: <tap uuid>, drift: true }.
            let sub: objc2::rc::Retained<NSMutableDictionary<NSString, AnyObject>> =
                NSMutableDictionary::new();
            let sub_uid = NSString::from_str(tap_uuid);
            let _: () = msg_send![&*sub, setObject: &*sub_uid, forKey: &*NSString::from_str("uid")];
            let _: () = msg_send![&*sub, setObject: &*yes, forKey: &*NSString::from_str("drift")];
            let taps: objc2::rc::Retained<NSMutableArray<AnyObject>> = NSMutableArray::new();
            let _: () = msg_send![&*taps, addObject: &*sub];

            // Heterogeneous values are passed by their typed refs (all `Message`).
            let _: () = msg_send![&*dict, setObject: &*name, forKey: &*NSString::from_str("name")];
            let _: () =
                msg_send![&*dict, setObject: &*agg_uid, forKey: &*NSString::from_str("uid")];
            let _: () =
                msg_send![&*dict, setObject: &*yes, forKey: &*NSString::from_str("private")];
            let _: () = msg_send![&*dict, setObject: &*no, forKey: &*NSString::from_str("stacked")];
            let _: () =
                msg_send![&*dict, setObject: &*yes, forKey: &*NSString::from_str("tapautostart")];
            let _: () =
                msg_send![&*dict, setObject: &*empty, forKey: &*NSString::from_str("subdevices")];
            let _: () = msg_send![&*dict, setObject: &*taps, forKey: &*NSString::from_str("taps")];

            // NSDictionary is toll-free bridged to CFDictionary.
            let cf: &CFDictionary = &*(objc2::rc::Retained::as_ptr(&dict) as *const CFDictionary);
            let mut aggregate_id: AudioObjectID = 0;
            let status = AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate_id));
            if status != 0 || aggregate_id == 0 {
                return Err(CaptureError::Other(format!(
                    "AudioHardwareCreateAggregateDevice failed (OSStatus {status})"
                )));
            }
            Ok(aggregate_id)
        }
    }

    /// Read a device's nominal sample rate (Hz).
    fn device_nominal_sample_rate(device: AudioObjectID) -> Option<f64> {
        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyNominalSampleRate,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut sr: f64 = 0.0;
        let mut size = std::mem::size_of::<f64>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(&mut sr as *mut _ as *mut c_void)?,
            )
        };
        if status == 0 && sr > 0.0 {
            Some(sr)
        } else {
            None
        }
    }

    /// Classify a `AudioHardwareCreateProcessTap` failure. There is no public
    /// permission-check API, so a create failure after the class exists is most
    /// likely the TCC denial (DESIGN-meetings.md §2). `!obj` errors are surfaced
    /// as generic.
    fn classify_tap_error(status: OSStatus) -> CaptureError {
        // There is no public API to check the audio-capture TCC grant, so a create
        // failure after CATapDescription resolved is treated as the permission
        // path (DESIGN-meetings.md §2): the UI shows the "grant in System Settings"
        // fix-it. The raw OSStatus is already logged by the caller for diagnosis.
        let _ = status;
        CaptureError::PermissionDenied
    }

    /// Whether any input device is currently running (open by *some* process).
    /// Used by the meeting detector as the "mic in use" fusion signal. Note this
    /// is device-level, not per-PID (macOS exposes no per-PID mic attribution).
    pub fn any_input_device_running() -> bool {
        unsafe {
            let mut device: AudioObjectID = 0;
            let mut size = std::mem::size_of::<AudioObjectID>() as u32;
            let dev_addr = AudioObjectPropertyAddress {
                mSelector: kAudioHardwarePropertyDefaultInputDevice,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let dst = match NonNull::new(&mut device as *mut _ as *mut c_void) {
                Some(p) => p,
                None => return false,
            };
            if AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                NonNull::from(&dev_addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                dst,
            ) != 0
                || device == 0
            {
                return false;
            }

            let mut running: u32 = 0;
            let mut rsize = std::mem::size_of::<u32>() as u32;
            let run_addr = AudioObjectPropertyAddress {
                mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let rdst = match NonNull::new(&mut running as *mut _ as *mut c_void) {
                Some(p) => p,
                None => return false,
            };
            if AudioObjectGetPropertyData(
                device,
                NonNull::from(&run_addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut rsize),
                rdst,
            ) != 0
            {
                return false;
            }
            running != 0
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{bundle_matches, select_process_objects, AudioProcInfo};

        fn info(obj_id: u32, pid: i32, bundle: Option<&str>) -> AudioProcInfo {
            AudioProcInfo {
                obj_id,
                pid,
                bundle_id: bundle.map(|s| s.to_string()),
            }
        }

        #[test]
        fn bundle_matches_exact_and_dot_boundary() {
            assert!(bundle_matches("com.google.Chrome", "com.google.Chrome"));
            assert!(bundle_matches(
                "com.google.Chrome",
                "com.google.Chrome.helper"
            ));
            assert!(bundle_matches(
                "com.google.Chrome",
                "com.google.Chrome.helper.Renderer"
            ));
            // Must not leak across a non-dot boundary or to unrelated ids.
            assert!(!bundle_matches(
                "com.google.Chrome",
                "com.google.Chromecast"
            ));
            assert!(!bundle_matches("com.google.Chrome", "com.apple.Safari"));
            assert!(!bundle_matches("", "com.google.Chrome"));
        }

        /// The canonical browser case: main PID + audio helper (bundle prefix) +
        /// renderer child (no bundle id, PID descends from main) are all selected;
        /// an unrelated app's process is left out.
        #[test]
        fn selects_main_helper_and_child_but_not_unrelated() {
            let table = vec![
                info(10, 501, Some("com.google.Chrome")), // (a) exact pid
                info(11, 777, Some("com.google.Chrome.helper")), // (b) bundle prefix
                info(12, 888, None),                      // (c) child of 501, no bundle
                info(13, 999, Some("com.apple.Safari")),  // unrelated
                info(14, 1234, Some("us.zoom.xos")),      // unrelated native app
            ];
            // Only pid 888 is a descendant of the target main pid 501.
            let is_descendant = |pid: i32| pid == 888;
            let selected =
                select_process_objects(&table, 501, Some("com.google.Chrome"), is_descendant);
            assert_eq!(selected, vec![10, 11, 12]);
        }

        #[test]
        fn dedupes_when_multiple_signals_hit_one_object() {
            // Same object is the exact PID AND matches the bundle prefix AND is
            // (spuriously) reported as its own descendant — it appears once.
            let table = vec![info(20, 501, Some("com.google.Chrome"))];
            let selected = select_process_objects(&table, 501, Some("com.google.Chrome"), |_| true);
            assert_eq!(selected, vec![20]);
        }

        #[test]
        fn empty_selection_when_nothing_matches() {
            let table = vec![
                info(30, 999, Some("com.apple.Safari")),
                info(31, 1000, None),
            ];
            let selected =
                select_process_objects(&table, 501, Some("com.google.Chrome"), |_| false);
            assert!(selected.is_empty());
        }

        /// With no known bundle id (manual capture path), selection falls back to
        /// PID exact + descendant signals only.
        #[test]
        fn selects_by_pid_and_child_without_bundle() {
            let table = vec![
                info(40, 501, Some("com.google.Chrome")),        // exact pid
                info(41, 888, Some("com.google.Chrome.helper")), // child of 501
                info(42, 999, Some("com.apple.Safari")),         // unrelated
            ];
            let selected = select_process_objects(&table, 501, None, |pid| pid == 888);
            assert_eq!(selected, vec![40, 41]);
        }

        /// Input order is preserved so the logged/tapped set is stable across runs.
        #[test]
        fn preserves_input_order() {
            let table = vec![
                info(3, 777, Some("com.google.Chrome.helper")),
                info(1, 501, Some("com.google.Chrome")),
                info(2, 888, None),
            ];
            let selected =
                select_process_objects(&table, 501, Some("com.google.Chrome"), |pid| pid == 888);
            assert_eq!(selected, vec![3, 1, 2]);
        }
    }
}
