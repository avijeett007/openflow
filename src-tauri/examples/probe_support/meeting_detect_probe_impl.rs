// Standalone probe for the OpenFlow Meetings (M1) detector's *app-running*
// signal — the NSWorkspace bundle-id half of the detection fusion
// (DESIGN-meetings.md §3). Lists any running app whose bundle id is in the
// default meeting allowlist (Zoom / Teams classic+new / FaceTime) with its PID,
// and prints the CoreAudio mic-in-use signal. Run it with FaceTime.app open to
// exercise the app-running signal without needing a real call.
//
// Usage: `cargo run --example meeting_detect_probe`


use core::ffi::c_void;
use core::ptr::NonNull;

use objc2_app_kit::NSWorkspace;
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultInputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress,
};

const ALLOWLIST: &[&str] = &[
    "us.zoom.xos",
    "com.microsoft.teams",
    "com.microsoft.teams2",
    "com.apple.FaceTime",
];

fn main() {
    println!("== OpenFlow Meetings M1 — detector app-running probe ==");
    println!("mic-in-use (kAudioDevicePropertyDeviceIsRunningSomewhere): {}", input_running());

    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();
    let mut found = 0;
    for i in 0..apps.count() {
        let app = apps.objectAtIndex(i);
        if let Some(bundle) = app.bundleIdentifier() {
            let bundle = bundle.to_string();
            if ALLOWLIST.contains(&bundle.as_str()) {
                let name = app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let pid = app.processIdentifier();
                println!("MEETING APP RUNNING: {name} ({bundle}) pid {pid}");
                found += 1;
            }
        }
    }
    if found == 0 {
        println!("no allowlisted meeting app running (launch FaceTime.app to exercise this signal)");
    }
    println!(
        "auto-detect would fire when BOTH signals hold for ~3s (app running AND mic in use). \
         Detection self-suppresses while OpenFlow's own recorder is active."
    );
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
            1,
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
