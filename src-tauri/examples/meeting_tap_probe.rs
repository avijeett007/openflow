//! Standalone hardware probe for the OpenFlow Meetings (M1) CoreAudio process
//! tap. It exercises the SAME sequence `meeting/capture.rs` uses — translate a
//! PID to an audio process object, build a `CATapDescription` mono mixdown, wrap
//! it in a private aggregate device, register an IOProc, and read frames — but as
//! its own process, so it can be run on this Mac without colliding with a running
//! OpenFlow instance (single-instance) and without TCC granted to the GUI build.
//!
//! Usage: `cargo run --example meeting_tap_probe -- <PID>`
//! Prints the OSStatus at each step and how many tapped samples arrived, proving
//! whether the tap/aggregate plumbing initializes on this hardware or hits the
//! audio-capture TCC wall (the graceful-degrade trigger, DESIGN-meetings.md §2).

#![cfg(target_os = "macos")]

use core::ffi::c_void;
use core::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, AnyThread};
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyNominalSampleRate,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSMutableArray, NSMutableDictionary, NSNumber, NSString, NSUUID, NSArray};

type OSStatus = i32;
const SYSTEM_OBJECT: AudioObjectID = 1;

static FRAMES: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);

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
        if !b.mData.is_null() {
            FRAMES.fetch_add((b.mDataByteSize as u64) / 4, Ordering::Relaxed);
        }
    }
    0
}

fn main() {
    let pid: i32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: meeting_tap_probe <PID>");

    println!("== OpenFlow Meetings M1 — CoreAudio process-tap hardware probe ==");
    println!(
        "CATapDescription available (macOS 14.4+): {}",
        AnyClass::get(c"CATapDescription").is_some()
    );
    println!("default input running (mic-in-use signal): {}", input_running());

    // PID -> audio process object.
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut proc_obj: AudioObjectID = 0;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&addr),
            std::mem::size_of::<i32>() as u32,
            &pid as *const i32 as *const c_void,
            NonNull::from(&mut size),
            NonNull::new(&mut proc_obj as *mut _ as *mut c_void).unwrap(),
        )
    };
    println!("translate pid {pid} -> process object: OSStatus {st}, obj {proc_obj}");
    if st != 0 || proc_obj == 0 {
        eprintln!("could not resolve pid to an audio process object (is it producing audio?)");
        return;
    }

    unsafe {
        let uuid = NSUUID::new();
        let uuid_str = uuid.UUIDString().to_string();
        let num = NSNumber::numberWithUnsignedInt(proc_obj);
        let procs: objc2::rc::Retained<NSArray<NSNumber>> = NSArray::from_slice(&[&*num]);
        let desc = CATapDescription::initMonoMixdownOfProcesses(CATapDescription::alloc(), &procs);
        desc.setUUID(&uuid);

        let mut tap_id: AudioObjectID = 0;
        let st = AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id);
        println!("AudioHardwareCreateProcessTap: OSStatus {st}, tap {tap_id}");
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

        // IOProc + start; collect ~1.5 s of frames.
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
            std::thread::sleep(std::time::Duration::from_millis(1500));
            AudioDeviceStop(agg, proc_id);
            AudioDeviceDestroyIOProcID(agg, proc_id);
            println!(
                "TAP CAPTURE OK — {} IOProc callbacks, {} samples in ~1.5s ({} s of audio @ {} Hz)",
                CALLS.load(Ordering::Relaxed),
                FRAMES.load(Ordering::Relaxed),
                FRAMES.load(Ordering::Relaxed) as f64 / sr.max(1.0),
                sr,
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
