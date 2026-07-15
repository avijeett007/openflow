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

## Not done in the first pass (needs the user — BLOCKERS §9)
- Live Meetings-UI transcript screenshot: OpenFlow is single-instance and a
  concurrent agent owned the running app on this Mac; I must not touch it.
- Full two-device real Zoom/Teams/FaceTime call (You + Them streaming).
- Signed-build TCC prompt ("capture audio from other apps").

---

## Live-UI verification pass (2026-07-15, in-app, screenshots)

Driven end-to-end through the running app (`bun run tauri dev`) via the webview
automation bridge + a temporary dev-only WKWebView snapshot command (reverted
before commit). Model: Parakeet Unified EN 0.6B (local). Screenshots in this dir.

| # | Item | Result | Evidence |
|---|---|---|---|
| 1 | Meetings section renders (Advanced mode) — empty state + controls | **PASS** | `01-meetings-empty.png` |
| 2 | Manual mic capture end-to-end (live "You" segments → finished meeting w/ duration + detail transcript) | **PASS** | `02-live-capture-mic.png`, `03-meeting-detail-mic.png` |
| 3 | System-audio (tap) channel via the UI | **PARTIAL** — tap attaches + starts through the shipped command (`mic_only:false`, log `capturing mic + system audio`, `aggregate device up (tap, 48000 Hz)`); UI shows the "Them" meter armed (`04-tap-capture-armed.png`). Real app audio→"Them" transcript could NOT be driven here (see below); the FFI probe already proved raw tap samples flow. Needs the 2-device call. |
| 4 | Detection prompt | **PASS (with honest caveat)** — real auto-detect correctly does NOT fire with FaceTime merely open (mic not in use, per design); prompt UI verified by emitting the same `meeting-detected` event → `05-detection-prompt.png` |
| 5 | Dictation regression (idle AND during active meeting capture) | **PASS** — idle `--toggle-transcription` streams+pastes; during an active capture BOTH pipelines run (dictation pastes AND meeting segments transcribe). `cargo test` **209 passed / 0 failed** on the shipped tree. |
| 6 | Delete a meeting from the UI | **PASS** — row + `meetings`/`meeting_segments` rows + `recordings/meetings/<id>/` all removed. `06a-list-before-delete.png`, `06b-list-after-delete.png` |

### Bug found + fixed this pass (managers/meeting.rs)
The meeting transcribe worker called `TranscriptionManager::transcribe()`, which
**errors instead of loading** when the engine isn't in the mutex — so every
segment was dropped when (a) no dictation had loaded the model yet (fresh
session) or (b) a concurrent streaming dictation had **leased the engine out**
(`is_model_loaded()` true, `lock_engine()` None). Fix: per-chunk
`initiate_model_load()` + retry-with-backoff (×12, 500 ms) so the latency-tolerant
chunk queues behind dictation (DESIGN §4.2) instead of failing. Verified both
cases green after the fix. No change to the shared manager's locking; dictation
byte-for-byte unaffected.

### Why item 3's real-audio leg is still open (environment, not product)
No bundle-id GUI media player could be made to emit audio gesture-free in this
headless session: VS Code runs fullscreen and holds activation (frontmost stays
"Electron"), so synthetic keystrokes/`set frontmost` never reach QuickTime/Music;
Automation AppleEvents to QuickTime time out (-1712); the players' windows sit off
the active Space (System Events `windows=0`), so background AXPress is out too.
`pid_for_bundle_id` (NSWorkspace) also can't target `afplay`/CLI, and browsers
route audio through a helper process rather than the main bundle-id PID. The
definitive proof remains the two-device real call (still a user task).
