# Meetings M1 — verification results (2026-07-15, macOS 14.5, Intel)

Feature: meeting capture (mic + system-audio process tap) + on-device
transcription, meeting detection, Meetings UI. Branch `feat/meetings-m1`.

## Gates (all green)

| Gate | Result |
|---|---|
| `cargo test` | **209 passed**, 0 failed (incl. new segmenter, detector-debounce, migration tests) |
| `cargo build` | clean, 0 warnings in meeting code |
| `bunx tsc --noEmit` | 0 errors |
| `bun run lint` | 0 errors (1 pre-existing warning in devAutomation.ts) |
| `bun run format:check` | clean (prettier + cargo fmt) |
| bindings.ts | regenerated headlessly (`cargo run -- --list-models`); 8 commands + 4 events present |

## Live hardware proofs

### (a) CoreAudio process tap — REAL audio captured ✓  (`tap_probe.log`)
`examples/meeting_tap_probe.rs` exercises the exact FFI sequence the app ships,
against a real `afplay` process:
```
CATapDescription available (macOS 14.4+): true
translate pid 79488 -> process object: OSStatus 0, obj 181
AudioHardwareCreateProcessTap: OSStatus 0, tap 183
AudioHardwareCreateAggregateDevice: OSStatus 0, aggregate 184
aggregate nominal sample rate: 48000 Hz
AudioDeviceCreateIOProcID: OSStatus 0
AudioDeviceStart: OSStatus 0
TAP CAPTURE OK — 138 IOProc callbacks, 70656 samples in ~1.5s (1.472 s of audio @ 48000 Hz)
teardown complete
```
Every step returned OSStatus 0 — the tap → aggregate → IOProc plumbing works on
this Mac, and the audio-capture TCC did **not** block the unsigned build here.
(Design risk #3 "tap fragility" retired on this hardware.)

### (c) Meeting detector — app-running + mic-in-use signals ✓  (`detect_probe.log`)
`examples/meeting_detect_probe.rs` with FaceTime open:
```
mic-in-use (kAudioDevicePropertyDeviceIsRunningSomewhere): false
MEETING APP RUNNING: FaceTime (com.apple.FaceTime) pid 80047
```
NSWorkspace bundle-id match works (the detector's app signal); the CoreAudio
mic-in-use query works and correctly reads `false` (FaceTime open without a call
does not open the mic). Full auto-detect fires when both hold for ~3 s — the
debounce/self-suppression is unit-tested (`meeting/detector.rs::tests`).

### (b) Mic channel & (d) dictation regression
Proven by construction + unit tests, not a concurrent live GUI run (blocked —
see BLOCKERS §9): meeting mic capture is a **separate `AudioRecorder` instance**
(the same, already-verified recorder the dictation path uses), and meeting
capture is a **sibling** of the single-flight `TranscriptionCoordinator` with
**zero changes** to dictation / wake-word / agent code — so dictation is
byte-for-byte unchanged and runs during a capture. Segmentation (channel
tagging, VAD boundaries, timing) is covered by `meeting/segmenter.rs::tests`.

## Not done here (needs the user — BLOCKERS §9)
- Live Meetings-UI transcript screenshot: OpenFlow is single-instance and a
  concurrent agent owns the running app on this Mac; I must not touch it.
- Full two-device real Zoom/Teams/FaceTime call (You + Them streaming).
- Signed-build TCC prompt ("capture audio from other apps").
