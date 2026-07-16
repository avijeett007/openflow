// Standalone hardware probe for the OpenFlow Meetings (M1) CoreAudio process
// tap. It exercises the SAME sequence `meeting/capture.rs` uses — enumerate the
// system audio-process objects, select every one belonging to the target app,
// build a `CATapDescription` mono mixdown over ALL of them, wrap it in a private
// aggregate device, register an IOProc, and read frames — but as its own
// process, so it can be run on this Mac without colliding with a running OpenFlow
// instance (single-instance) and without TCC granted to the GUI build.
//
// Usage:
//   cargo run --example meeting_tap_probe -- <PID>
//   cargo run --example meeting_tap_probe -- <bundle-id>   (e.g. com.google.Chrome)
//
// With a bundle id it resolves the running app's main PID (NSWorkspace), then
// mirrors the browser-aware multi-process selection: main PID + audio helper
// (bundle prefix) + descendant helper PIDs. It prints the full enumerated audio
// process table, the selected set, the OSStatus at each tap step, and the RMS of
// the captured samples — proving both process selection AND non-silence on this
// hardware (or where it hits the audio-capture TCC wall, the degrade trigger).


use core::ffi::c_void;
use core::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, AnyThread};
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyNominalSampleRate,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioProcessPropertyBundleID, kAudioProcessPropertyPID,
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertySelector, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSMutableArray, NSMutableDictionary, NSNumber, NSString, NSUUID};

type OSStatus = i32;
const SYSTEM_OBJECT: AudioObjectID = 1;
const PID_ANCESTRY_MAX_DEPTH: u32 = 6;

// Accumulate sum-of-squares and sample count across IOProc callbacks so we can
// report an RMS at the end (the non-silence proof).
static FRAMES: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static SUMSQ_BITS: AtomicU64 = AtomicU64::new(0);

unsafe extern "C-unwind" fn ioproc(
    _d: AudioObjectID,
    _n: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _it: NonNull<AudioTimeStamp>,
    _o: NonNull<AudioBufferList>,
    _ot: NonNull<AudioTimeStamp>,
    _c: *mut c_void,
) -> OSStatus {
    CALLS.fetch_add(1, Ordering::Relaxed);
    let list = input.as_ref();
    if list.mNumberBuffers > 0 {
        let b = &*list.mBuffers.as_ptr();
        if !b.mData.is_null() && b.mDataByteSize > 0 {
            let n = (b.mDataByteSize as usize) / 4;
            let data = std::slice::from_raw_parts(b.mData as *const f32, n);
            let mut sumsq = 0.0f64;
            for &s in data {
                sumsq += (s as f64) * (s as f64);
            }
            FRAMES.fetch_add(n as u64, Ordering::Relaxed);
            // f64 running sum via CAS on the bit pattern.
            let mut cur = SUMSQ_BITS.load(Ordering::Relaxed);
            loop {
                let next = (f64::from_bits(cur) + sumsq).to_bits();
                match SUMSQ_BITS.compare_exchange_weak(
                    cur,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => cur = observed,
                }
            }
        }
    }
    0
}

#[derive(Clone, Debug)]
struct AudioProcInfo {
    obj_id: AudioObjectID,
    pid: i32,
    bundle_id: Option<String>,
}

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

fn read_process_scalar<T: Copy>(
    obj: AudioObjectID,
    selector: AudioObjectPropertySelector,
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
    let mut io = size;
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
            NonNull::from(&mut io),
            dst,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(io as usize / std::mem::size_of::<AudioObjectID>());
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

/// Resolve a bundle id to the running app's main PID via NSWorkspace.
fn pid_for_bundle_id(bundle_id: &str) -> Option<i32> {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();
    for i in 0..apps.count() {
        let app = apps.objectAtIndex(i);
        if let Some(b) = app.bundleIdentifier() {
            if b.to_string() == bundle_id {
                return Some(app.processIdentifier());
            }
        }
    }
    None
}

/// Translate a single PID to its audio process object (the single-PID fallback).
fn process_object_for_pid(pid: i32) -> Option<AudioObjectID> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut obj: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&addr),
            std::mem::size_of::<i32>() as u32,
            &pid as *const i32 as *const c_void,
            NonNull::from(&mut size),
            NonNull::new(&mut obj as *mut _ as *mut c_void)?,
        )
    };
    if st == 0 && obj != 0 {
        Some(obj)
    } else {
        None
    }
}

fn main() {
    let arg = std::env::args()
        .nth(1)
        .expect("usage: meeting_tap_probe <PID|bundle-id>");

    println!("== OpenFlow Meetings M1 — CoreAudio process-tap hardware probe ==");
    println!(
        "CATapDescription available (macOS 14.4+): {}",
        AnyClass::get(c"CATapDescription").is_some()
    );
    println!(
        "default input running (mic-in-use signal): {}",
        input_running()
    );

    // Resolve the target: numeric arg = PID, else a bundle id to look up.
    let (pid, bundle): (i32, Option<String>) = match arg.parse::<i32>() {
        Ok(pid) => (pid, None),
        Err(_) => match pid_for_bundle_id(&arg) {
            Some(pid) => {
                println!("resolved bundle '{arg}' -> main pid {pid}");
                (pid, Some(arg.clone()))
            }
            None => {
                eprintln!("bundle id '{arg}' is not a running application");
                return;
            }
        },
    };

    // Enumerate the whole audio-process table and print it verbatim.
    let table = enumerate_audio_processes();
    println!("\n-- enumerated audio process objects ({}) --", table.len());
    for p in &table {
        println!(
            "  obj {:>4}  pid {:>6}  bundle {:?}",
            p.obj_id, p.pid, p.bundle_id
        );
    }

    // Browser-aware multi-process selection (the fix under test).
    let mut selected = select_process_objects(&table, pid, bundle.as_deref(), |cand| {
        is_descendant_of(cand, pid, PID_ANCESTRY_MAX_DEPTH)
    });
    println!(
        "\nselection for pid {pid} bundle {bundle:?}: {} object(s) {selected:?}",
        selected.len()
    );
    if selected.is_empty() {
        match process_object_for_pid(pid) {
            Some(obj) => {
                println!("  (empty selection; single-PID fallback -> {obj})");
                selected.push(obj);
            }
            None => {
                eprintln!("could not resolve any audio process object (is it producing audio?)");
                return;
            }
        }
    }

    unsafe {
        let uuid = NSUUID::new();
        let uuid_str = uuid.UUIDString().to_string();
        let nums: Vec<objc2::rc::Retained<NSNumber>> = selected
            .iter()
            .map(|o| NSNumber::numberWithUnsignedInt(*o))
            .collect();
        let refs: Vec<&NSNumber> = nums.iter().map(|n| &**n).collect();
        let procs: objc2::rc::Retained<NSArray<NSNumber>> = NSArray::from_slice(&refs);
        let desc = CATapDescription::initMonoMixdownOfProcesses(CATapDescription::alloc(), &procs);
        desc.setUUID(&uuid);

        let mut tap_id: AudioObjectID = 0;
        let st = AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id);
        println!("\nAudioHardwareCreateProcessTap: OSStatus {st}, tap {tap_id}");
        if st != 0 || tap_id == 0 {
            eprintln!("TAP CREATE FAILED — most likely the audio-capture TCC grant (expected for an unsigned/ad-hoc build). Graceful-degrade path would return mic-only here.");
            return;
        }

        // Aggregate device around the tap.
        let dict: objc2::rc::Retained<NSMutableDictionary<NSString, AnyObject>> =
            NSMutableDictionary::new();
        let name = NSString::from_str("OpenFlow Probe");
        let agg_uid = NSString::from_str(&format!("openflow.probe.{uuid_str}"));
        let yes = NSNumber::numberWithBool(true);
        let no = NSNumber::numberWithBool(false);
        let empty: objc2::rc::Retained<NSMutableArray<AnyObject>> = NSMutableArray::new();
        let sub: objc2::rc::Retained<NSMutableDictionary<NSString, AnyObject>> =
            NSMutableDictionary::new();
        let sub_uid = NSString::from_str(&uuid_str);
        let _: () = msg_send![&*sub, setObject: &*sub_uid, forKey: &*NSString::from_str("uid")];
        let _: () = msg_send![&*sub, setObject: &*yes, forKey: &*NSString::from_str("drift")];
        let taps: objc2::rc::Retained<NSMutableArray<AnyObject>> = NSMutableArray::new();
        let _: () = msg_send![&*taps, addObject: &*sub];
        let _: () = msg_send![&*dict, setObject: &*name, forKey: &*NSString::from_str("name")];
        let _: () = msg_send![&*dict, setObject: &*agg_uid, forKey: &*NSString::from_str("uid")];
        let _: () = msg_send![&*dict, setObject: &*yes, forKey: &*NSString::from_str("private")];
        let _: () = msg_send![&*dict, setObject: &*no, forKey: &*NSString::from_str("stacked")];
        let _: () =
            msg_send![&*dict, setObject: &*yes, forKey: &*NSString::from_str("tapautostart")];
        let _: () =
            msg_send![&*dict, setObject: &*empty, forKey: &*NSString::from_str("subdevices")];
        let _: () = msg_send![&*dict, setObject: &*taps, forKey: &*NSString::from_str("taps")];
        let cf: &CFDictionary = &*(objc2::rc::Retained::as_ptr(&dict) as *const CFDictionary);

        let mut agg: AudioObjectID = 0;
        let st = AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut agg));
        println!("AudioHardwareCreateAggregateDevice: OSStatus {st}, aggregate {agg}");
        if st != 0 || agg == 0 {
            AudioHardwareDestroyProcessTap(tap_id);
            return;
        }

        // Nominal sample rate.
        let sr_addr = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyNominalSampleRate,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut sr: f64 = 0.0;
        let mut ssz = 8u32;
        let _ = AudioObjectGetPropertyData(
            agg,
            NonNull::from(&sr_addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut ssz),
            NonNull::new(&mut sr as *mut _ as *mut c_void).unwrap(),
        );
        println!("aggregate nominal sample rate: {sr} Hz");

        // IOProc + start; collect ~3 s of frames.
        let mut proc_id: AudioDeviceIOProcID = None;
        let st = AudioDeviceCreateIOProcID(
            agg,
            Some(ioproc),
            std::ptr::null_mut(),
            NonNull::from(&mut proc_id),
        );
        println!("AudioDeviceCreateIOProcID: OSStatus {st}");
        if st == 0 && proc_id.is_some() {
            let st = AudioDeviceStart(agg, proc_id);
            println!("AudioDeviceStart: OSStatus {st}");
            std::thread::sleep(std::time::Duration::from_millis(3000));
            AudioDeviceStop(agg, proc_id);
            AudioDeviceDestroyIOProcID(agg, proc_id);
            let frames = FRAMES.load(Ordering::Relaxed);
            let sumsq = f64::from_bits(SUMSQ_BITS.load(Ordering::Relaxed));
            let rms = if frames > 0 {
                (sumsq / frames as f64).sqrt()
            } else {
                0.0
            };
            println!(
                "TAP CAPTURE OK — {} IOProc callbacks, {} samples in ~3s ({:.2} s @ {} Hz)",
                CALLS.load(Ordering::Relaxed),
                frames,
                frames as f64 / sr.max(1.0),
                sr,
            );
            println!(
                "captured RMS: {rms:.6}  ({})",
                if rms > 1e-4 {
                    "NON-SILENT — system audio captured"
                } else {
                    "silent (no audio playing on the selected processes)"
                }
            );
        }

        AudioHardwareDestroyAggregateDevice(agg);
        AudioHardwareDestroyProcessTap(tap_id);
        println!("teardown complete");
    }
}

fn input_running() -> bool {
    unsafe {
        let mut dev: AudioObjectID = 0;
        let mut size = 4u32;
        let da = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        if AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&da),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut dev as *mut _ as *mut c_void).unwrap(),
        ) != 0
            || dev == 0
        {
            return false;
        }
        let mut running: u32 = 0;
        let mut rs = 4u32;
        let ra = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        if AudioObjectGetPropertyData(
            dev,
            NonNull::from(&ra),
            0,
            std::ptr::null(),
            NonNull::from(&mut rs),
            NonNull::new(&mut running as *mut _ as *mut c_void).unwrap(),
        ) != 0
        {
            return false;
        }
        running != 0
    }
}
