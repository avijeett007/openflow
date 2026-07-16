# OPENFLOW-NOTES.md — env / gotchas for driving the live app

Env/gotcha lessons learned while verifying features on this Intel x86_64 Mac.

## Meetings M2 — sherpa-onnx diarization linkage (2026-07-16)

- **Official crate = `sherpa-onnx` 1.13.4** (+ `sherpa-onnx-sys` 1.13.4), the k2-fsa
  bindings. The community `sherpa-rs` is archived — not used. Verified via
  `cargo search sherpa` (crates.io web API was 403-ing that day).
- **No C++ compile.** `sherpa-onnx-sys`'s build script _downloads a prebuilt
  static lib_ for the target from the k2-fsa GitHub releases
  (`sherpa-onnx-v1.13.4-osx-x64-static-lib.tar.bz2`) and links it. Feature `static`
  (the default) bundles onnxruntime **inside** our binary as a static archive.
- **Two onnxruntimes coexist fine.** transcribe-rs keeps using the _dynamic_
  system onnxruntime via `ORT_LIB_LOCATION=/usr/local/opt/onnxruntime/lib` +
  `ORT_PREFER_DYNAMIC_LINK=1`; sherpa's _static_ copy is self-contained. The full
  `cargo build` links clean and `cargo test` (268) passes with both present — no
  duplicate-symbol wrangling was needed on this Intel Mac. Keep sherpa macOS-only
  (it's gated in Cargo.toml under the `cfg(target_os = "macos")` table) so
  Linux/Windows builds never pull the prebuilt lib.
- **First build downloads the prebuilt lib** (cached under
  `target/.../sherpa-onnx-prebuilt`); budget ~25 s extra the first time.
- **Regenerating `src/bindings.ts` without `tauri dev`** (app was running, so no
  dev server): the `#[cfg(debug_assertions)]` specta export runs at the very top
  of `run()`, _before_ the single-instance check. So `cargo build` then run the
  debug binary **from `src-tauri/`** (`./target/debug/openflow --start-hidden`) —
  the export writes `../src/bindings.ts` (path is relative to `src-tauri/`, NOT the
  repo root) and then the second instance forwards to the running app and exits.

## Build / run

- Intel Mac requires dynamic ORT + the cmake policy workaround:
  ```bash
  CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  ORT_LIB_LOCATION=/usr/local/opt/onnxruntime/lib ORT_PREFER_DYNAMIC_LINK=1 \
  bun run tauri dev
  ```
- `cargo` and `bun` are NOT on the PATH of a bare `nohup`/detached shell. Export
  `PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"` before launching `tauri dev` in
  the background, or it dies with `failed to run 'cargo metadata' ... No such file`.
- First Rust build ~30s incremental on this machine; app logs to the dev-run log.
  Wait for `Shortcuts initialized successfully` before driving.

## Driving the webview bridge (scratchpad/auto.sh)

- The eval runs in `devAutomation.ts` module scope — `commands` is NOT a global.
  Import it: `const m = await import("/src/bindings.ts");` then `m.commands.*`.
- Every `commands.*` returns a specta Result wrapper `{status:"ok"|"error", data}`.
  Unwrap `.data` (e.g. `getAppSettings()` → `(r.data||r)`).
- `getAppSettings()` is the read command (there is no `getSettings`).
- After calling a mutating command directly via the bridge (createAgent/updateAgent/
  setApiKey/deleteAgent), the Zustand settings store does NOT auto-refresh. Do
  `location.reload()` then re-navigate to see the card reflect the change.
- Sidebar nav items are `<div onClick>` with a child `<p title="<Section>">`, NOT
  `<button>`. To switch section: find `p[title="Agents"]` and click its
  `parentElement`.
- React-controlled inputs: set value via the native setter
  (`Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").set.call(inp,val)`)
  then dispatch `new Event("input",{bubbles:true})`. The agent Test button is
  labeled **"Run test"** (not "Test").

## Global hotkeys / TCC (important)

- Keyboard implementation on this box is **handy_keys** (not the Tauri global-hotkey
  backend). On THIS running build, Input Monitoring + Accessibility were already
  granted, so **synthetic CGEvents reach handy_keys** — a full voice+hotkey e2e is
  drivable without the CLI:
  - `osascript -e 'tell application "System Events" to keystroke "j" using {command down, shift down}'`
    fires a bound agent hotkey; it starts/stops recording (toggle mode, PTT off).
  - Supply audio deterministically with `say -r 180 "<sentence>"` between the
    start and stop keystrokes — the MacBook mic captures the speaker output well
    enough for clean Parakeet transcription.
- Normal (main) dictation is drivable regardless of TCC via single-instance CLI
  forwarding: `./src-tauri/target/debug/openflow --toggle-transcription` (run twice
  to start then stop). There is **no** CLI flag for agent bindings — agent hotkeys
  must be fired as real/synthetic keystrokes.
- Timing gotcha: after the stop keystroke, the streaming "Live preview finalized"
  log appears first, but the **final transcribe + cleanup/agent-LLM + paste** land
  several seconds later (saw ~11s with the remote "custom" cleanup provider). Poll
  the dev-run log for `Using paste method` before reading the target app, or you'll
  read the document too early and see it empty.

## Window screenshots

- `swift scratchpad/winshot.swift <owner-substr> <out.png>` captures a specific
  window off-screen too. The OpenFlow settings window owner is `openflow`; TextEdit
  is `TextEdit`. Bring it frontmost first (`System Events set frontmost of ...`).

## Flow OS increment 2 (CLI / coding agents) — verification lessons

- **Stale vite holds port 1420.** A previous session's `vite`/`esbuild` can stay
  alive after its Tauri app died, so `bun run tauri dev` fails with
  `Port 1420 is already in use` and the app never starts. The `__auto/eval`
  endpoint still 200s (vite is up) but no webview drains the queue, so
  `auto.sh` hangs on a never-`done` result. Kill it first:
  `lsof -ti tcp:1420 | xargs kill -9` then relaunch.
- **Synthetic hotkeys still reach handy_keys on the rebuilt debug binary.**
  `osascript ... keystroke "j" using {command down, shift down}` toggled the
  bound `agent:<id>` hotkey and drove a full voice→agent run — Input Monitoring
  - Accessibility survived the `cargo run` rebuild (same `target/debug/openflow`
    path). The log line `enigo ... has the permission to simulate input` confirms
    output perms; capture perms are proven by a `transcription started` line
    appearing after the keystroke.
- **CLI-agent binding string is `"command+shift+j"`** (macOS spells the Cmd
  modifier `command`, not `cmd`); set it with `commands.changeBinding("agent:<id>", "command+shift+j")`.
- **The verified claude template works headless and edits files:**
  `-p --output-format stream-json --verbose --permission-mode acceptEdits` with
  the instruction on stdin → claude runs with `cwd`=project, Edits the repo,
  exits 0 in ~17s. `git diff` in the project is the definitive proof; the run
  file lands at `<repo>/.openflow/agent-runs/<YYYYMMDD-HHMMSS>-<agentId>.md`.
  Live streaming, Running→Finished/Stopped status flips, and Stop
  (SIGTERM→SIGKILL) all drive the Agent Runs panel via
  `agent-run-output`/`agent-run-status` events (no reload needed).
- **Prompt-agent regression needs a _working_ provider.** The "Formal Rewriter"
  template is created with `provider_id="openai"` + empty model, which won't run.
  Repoint it to the local Ollama cleanup provider before the voice test:
  `provider_id="custom"`, `model="qwen2.5:3b"` (Ollama must be up on
  `localhost:11434`). Then it transforms+injects as in increment 1.
- **Notification (Notify sink) is not verifiable headlessly** — the Notification
  Center db under `~/Library/Group Containers/group.com.apple.usernoted/` is
  SIP/container-protected and unreadable. Rely on the File + status proof; the
  Notify path is invoked in the same `finalize()` that writes the (verified)
  run file, the `notification:default` capability is present, and no error is
  logged.
- **`claude` seven-day rate limit** was at ~91% ("allowed_warning") during
  verification — still allowed, but budget e2e runs (each real run costs tokens;
  a Stopped run costs less if stopped early).

## handy-keys 0.3.0 — mouse-side-button hotkeys (issue #12 macOS half)

- **Mouse buttons ARE valid hotkey strings and pass OpenFlow's validation.**
  `validate_shortcut` for HandyKeys is just `raw.parse::<Hotkey>()` (shortcut/
  handy_keys.rs). handy-keys 0.3.0 `Key::from_str` accepts, for the side buttons:
  `mousex1`/`mouse4`/`back`/`xbutton1` → MouseX1, `mousex2`/`mouse5`/`forward`/
  `xbutton2` → MouseX2, `mousemiddle`/`mouse3`/`mmb` → MouseMiddle (see the crate's
  `src/types/key.rs`). So `commands.changeBinding("transcribe","mousex1")` returns
  `{success:true, current_binding:"mousex1"}` and the crate logs
  `Registered handy-keys shortcut: transcribe -> Hotkey { .. key: Some(MouseX1) }`.
  Note the UI recorder (HandyKeysShortcutInput.tsx) captures whatever the crate's
  listener emits; mouse buttons come through the same `handy-keys-event` path, so
  the reporter could bind it either by recording the button or by the string path.
- **Synthesize a side-button press system-wide** with a CGEvent (this Mac has no
  physical side button): `swift scratchpad/clickx1.swift 3` posts OtherMouseDown+Up
  with `kCGMouseEventButtonNumber = 3`. handy-keys' macOS listener maps otherMouse
  button numbers 2/3/4 → Middle/X1/X2 (`src/platform/macos/listener.rs`). A single
  click = one Pressed event; in toggle mode (push_to_talk=false) only Pressed
  toggles, so drive record-start / record-stop with TWO clicks and `say` between.
  Proof: `handy-keys event: binding=transcribe, hotkey=mousex1, state=Pressed`
  → `TranscribeAction::start` → `Recording started for binding transcribe`
  → (2nd click) stop → `Transcription completed ... 'The quick brown fox ...'`.
- **macOS does NOT suppress the bound mouse button (known upstream 0.3.0 gap).**
  The mac tap runs in `CGEventTapOptions::Default` (blocking-capable), but the
  `OtherMouseDown`/`OtherMouseUp` arms never call `state.should_block(...)` — only
  the keyboard arms do. So a side-button hotkey is DETECTED but the click still
  leaks through to the frontmost app. PR #14/issue #12 fixed _Windows_ suppression;
  the macOS mouse-suppression path is unimplemented. (Keyboard hotkeys DO suppress
  on macOS via should_block.)
- **Proof lines split across log targets.** The console/stderr target (what
  `bun run tauri dev` captures) is filtered to **Info** (EnvFilterBuilder in lib.rs),
  so `TranscribeAction::start` / `Recording started for binding` (both DEBUG) do NOT
  appear there. The **file** log target is Debug: `~/Library/Logs/knotie.ai.openflow/
handy.log`. Tail THAT for the binding-named DEBUG proof; INFO lines like
  `Transcription result:` / `Live streaming transcription started` show in both.
- **This dev run's Enigo paste worked** (`Text pasted successfully`) — unlike the
  earlier run's `Enigo state not initialized`; Input Monitoring/Accessibility were
  live, so the transcript actually injected into the frontmost app.
- **Off-screen window can't be screenshotted in this session.** The OpenFlow window
  parked on an inactive Space (CGWindow `onscreen=false`, no composited backing
  store) → CGWindowListCreateImage returns a blank bitmap; full-display
  `screencapture`/AppleScript `activate` fail ("could not create image from
  display", AppleEvent timed out — Screen Recording TCC / no active GUI focus).
  `set frontmost`/AXRaise via System Events did not switch Spaces here, and the
  `plugin:window|unminimize`/`center` internals invokes are blocked by the app's
  capability set. Fall back to verifying UI state via the webview bridge
  (`getAppSettings().bindings.transcribe.current_binding`) instead of a PNG.

## SOLVED (2026-07-15): in-process WKWebView snapshot beats the off-screen +

## Screen-Recording-TCC wall (BLOCKER #8)

Every OS-level capture path is dead on this box, and the root causes are now
understood — don't waste time re-trying them:

- **Screen Recording TCC is unfixable from here.** The shell descends from
  **VS Code running under macOS App Translocation** (`/private/var/folders/.../
AppTranslocation/.../Visual Studio Code.app`). A translocated app gets a random
  read-only path each launch, so its TCC grants never stick → `screencapture`
  (`-l <winid>`, `-R`, and full-display) and `CGWindowListCreateImage` all fail
  with _"could not create image from display/window"_. No `sudo`, Terminal.app
  and iTerm lack the grant / prompt-hang, and `launchctl asuser` reparent doesn't
  help. Confirmed via `log stream` (screencapture→TCCAccessRequest→denied).
- **The window sits on a desktop Space while a fullscreen app (VS Code) holds
  activation.** Switch the active Space with the private SkyLight API — enumerate
  with `CGSCopyManagedDisplaySpaces`/`CGSGetActiveSpace` (see
  `scratchpad/spaces.swift`), then `CGSManagedDisplaySetCurrentSpace(cid, display,
spaceId)` (`scratchpad/gospace.swift <displayUUID> <spaceId>`; desktop space is
  `type=0`). This flips the OpenFlow window to `onscreen=true`, BUT it stays
  `document.visibilityState:"hidden"` (WKWebView occlusion), because on Sonoma a
  background app can't steal activation from a fullscreen app
  (`activateIgnoringOtherApps` is a no-op). So **rAF stays paused** off-screen.

**The winning approach — capture the webview in-process, no OS capture at all:**

1. **`dev_snapshot` Tauri command** (DEV-ONLY, added in `lib.rs`; deps `objc`+`block`
   in `src-tauri/Cargo.toml`): calls `WKWebView.takeSnapshotWithConfiguration:
completionHandler:` → NSBitmapImageRep PNG → `writeToFile`. Runs inside the app
   process, so it needs **zero Screen Recording permission** and works even when
   the window is fully off-screen/occluded. Optional `width`/`height` args
   `window.set_size()` first so a taller page fits the view bounds (macOS clamps
   height to ~screen when the window is on the active Space → ~963pt max here;
   capture tall sections by scrolling instead). Async completion can take ~15-30s
   cold — poll for the file rather than trusting the Rust-side channel timeout.
   Driver: `scratchpad/snap.sh <out.png> [w h]`.
2. **rAF is paused while occluded, which freezes recharts + CSS entrance
   animations at frame 0** (blank hero, zero-width bars, clipped area/line — looks
   like a rendering _bug_ but is purely a capture artifact). Fix per page before
   snapping (`scratchpad/prep.js`, run via `auto.sh`):
   - inject `<style>.of-rise-in{animation:none!important;opacity:1!important;
transform:none!important}` to settle CSS entrance animation, and
   - **shim `window.requestAnimationFrame` → `setTimeout(cb,16)`** (setTimeout
     still fires when hidden, unlike rAF), then **remount** the view (nav away +
     back) so recharts' animation loop runs to completion. Wait ~2.5s, then snap.
     `dev_activate` (also DEV-ONLY, `activateIgnoringOtherApps`) is kept but is a
     no-op against fullscreen VS Code — the rAF shim is what actually works.
3. Both `dev_*` commands are verification-only and reverted before shipping
   (they're behind the `feat/mission-control` work, not committed to the feature).

Net: full-fidelity PNGs of every Mission Control module, both themes, saved under
`verification/mission-control/`. BLOCKER #8 is beaten — no human capture needed.

## Pre-existing observation (NOT the agents feature)

- With `post_process_enabled=true` and Dictionary entries configured, the cleanup
  LLM can **echo its injected vocabulary block** into the output — a spurious
  trailing paragraph: `Vocabulary — always use these exact spellings of the user's
custom words: ...`. This is built by `dictionary_vocabulary_block` (actions.rs),
  introduced by the **Dictionary** commit `895a745` (before Flow OS agents). It
  reproduces on plain dictation both WITH and WITHOUT any agents configured, so it
  is unrelated to Flow OS agents — a Dictionary/cleanup prompt-robustness issue
  dependent on the weak user-configured "custom" cleanup model. Flagged for the
  Dictionary feature owner; out of scope for the agents increment.

## Meetings M1 — live-UI verification lessons (2026-07-15)

- **`dev_snapshot` re-add: use the objc2 stack, no new-crate hunting.** The
  branch already links `objc2-web-kit` (0.3, WKWebView + WKSnapshotConfiguration),
  `block2`, `objc2-app-kit`, `objc2-foundation` transitively. Add them as _direct_
  deps (`block2 = "0.6"`, `objc2-web-kit` with features
  `["WKWebView","WKSnapshotConfiguration","block2"]`, and app-kit features
  `NSImage,NSImageRep,NSBitmapImageRep,NSGraphics`, foundation `NSData`) — NOT the
  old `objc`/`block` crates the earlier note mentions. Tauri 2.10
  `PlatformWebview::inner()` returns `*mut c_void` = the `WKWebView`; cast and call
  `takeSnapshotWithConfiguration_completionHandler(None, &RcBlock::new(|img:*mut NSImage, _err|{…}))`
  inside `window.with_webview(…)` (runs on the main thread). Completion → NSImage
  `TIFFRepresentation` → `NSBitmapImageRep::imageRepWithData` →
  `representationUsingType_properties(PNG, &NSDictionary::new())` → `writeToFile`.
  Register it in the specta `collect_commands!` list (the `#[cfg(debug_assertions)]`
  auto-export then rewrites `src/bindings.ts`, so **`git restore src/bindings.ts`**
  when reverting). snap.sh/auto.sh from openflow/scratchpad work unchanged.
- **Two capture artifacts beyond the rAF/recharts one:** (1) the WKWebView snapshot
  completion can lag **several seconds** past the async fire, so a **sonner toast
  (12 s)** can EXPIRE before the PNG lands — keep short-lived overlays alive by
  **re-emitting the event in a `setInterval` loop** across the snapshot. (2) sonner's
  toast entrance is a CSS transform/opacity transition; with rAF paused (occluded
  window) it **freezes at opacity 0 / off-screen** → invisible in the PNG though
  present in the DOM. Inject
  `[data-sonner-toast]{opacity:1!important;transform:none!important;transition:none!important}`
  before snapping (same class of fix as prep.js's `.of-rise-in`).
- **You CANNOT trigger gesture-free GUI media playback in this session — the tap
  "money shot" with a real app is blocked here.** VS Code runs **fullscreen**, so
  it holds activation: `frontmost` stays `"Electron"` even after `open -a`,
  `set frontmost of process "QuickTime Player" to true`, etc., so a synthetic
  **space keystroke never reaches** QuickTime/Music (they stay paused). Automation
  AppleEvents to QuickTime **time out (-1712)** (TCC consent dialog hangs off-screen).
  QuickTime/Music **windows sit off the active Space** → System Events sees
  `windows=0` ("Invalid index"), so even background **AXPress** on the play button
  is unavailable. Net: no bundle-id GUI player could be made to emit audio → the
  real tap→"Them" transcript needs a **real 2-device call** (already the standing
  user task) or a non-fullscreen GUI session. The tap itself is fine — see next.
- **The system-audio tap ATTACHES + starts through the shipped UI command.**
  `startMeetingCapture("com.apple.QuickTimePlayerX"/"com.apple.Music")` →
  `mic_only:false`, log `meeting N: capturing mic + system audio` +
  `aggregate device up (tap, 48000 Hz)`. So bundle-id→PID (NSWorkspace)→process
  tap→aggregate→IOProc all work via the real command, not just the FFI probe. The
  UI shows the "Them" meter armed (vs the amber "microphone only" degrade).
- **`pid_for_bundle_id` = NSWorkspace.runningApplications → GUI/bundle-id apps only.**
  `afplay` (and any CLI tool / daemon like speechsynthesisd) is NOT listed, so the
  shipped command **cannot target afplay** despite the probe tapping it by raw PID.
  Also: **browsers route audio through a helper process** ("Google Chrome Helper
  (Audio)"), NOT the main `com.google.Chrome` PID — tapping the main bundle-id PID
  would miss browser-call audio (relevant if browser calls ever become a tap target).
- **Real bug found + fixed (managers/meeting.rs, shipped):** the meeting transcribe
  worker called `TranscriptionManager::transcribe()` which **errors instead of
  loading** when the engine isn't in the mutex. Two failure modes, both dropping
  every segment: (a) **fresh app session** — model never loaded (no prior dictation);
  (b) **concurrent streaming dictation** — the engine is **leased out** of the mutex
  (`is_model_loaded()` returns true via `active_engine_lease`, but `lock_engine()`
  is `None`). Fix: per-chunk `initiate_model_load()` + **retry with 500 ms backoff
  (×12)** so the latency-tolerant chunk queues behind dictation (DESIGN §4.2) instead
  of failing. Verified: meeting segments transcribe in a fresh session AND while a
  `--toggle-transcription` dictation streams+pastes concurrently.
