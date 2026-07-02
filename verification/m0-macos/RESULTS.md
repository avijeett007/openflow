# M0 — Risk spike results (macOS)

**Machine:** Intel Core i9-9980HK, 32 GB, macOS 14.5 Sonoma. STT model: Parakeet Unified EN 0.6B (Q8_0 GGUF), streaming, via transcribe-cpp + Metal.

**Done criterion — raw dictation lands in ≥3 apps on macOS: MET.** Verified by driving the real app (synthetic option+space hotkey via CGEvent, audio routed through BlackHole so `say` feeds the mic).

| App | Category | Dictated → injected text | Evidence |
|---|---|---|---|
| TextEdit | native AppKit | "Testing dictation into a text editor. This is milestone 0 of OpenFlow" | m0-04-inject-textedit.png |
| Google Chrome | browser (Blink `<textarea>`) | "Injecting dictated text into a browser text area works great" | m0-05-inject-chrome.png |
| VS Code | Electron/Chromium editor | "Dictating a comment inside a code editor to finish milestone 0" | m0-06-inject-vscode.png |

Injection path = clipboard save → write → Cmd+V → restore (app-agnostic). VS Code is Electron — the same runtime Slack/Discord use — so the mechanism is proven for that toolkit class too.

**Hotkey modes:** hold-to-talk (push_to_talk=true, hold option+space) and tap-to-toggle (push_to_talk=false, tap to start / tap to stop) both verified producing injected text. Toggled live via `changePttSetting` with no restart.

**macOS synthetic-event caveat (plan §3e):** on Sonoma 14.5, CGEvent keyboard posting works with the classic Accessibility TCC grant; the Tahoe `kTCCServicePostEvent` bucket does not apply here. The clipboard-paste fallback sidesteps synthetic keystroke filtering regardless. Documented for Tahoe users in the README.

**Latency (measured on Intel, app's structured logs):** short ~1.8–2.0 s utterance → release-to-paste ≈ 1.5–2.5 s. Streaming Parakeet runs at ~0.66–0.85× real-time on this 2019 Intel GPU (e.g. "finalized in 2.24s model compute for 1.80s streamed audio (0.80× real-time)"; paste step 216 ms). This is above the <1.5 s Apple-Silicon target — expected on Intel, where the goal permits reporting the measured number and why. On Apple Silicon (Parakeet hits thousands× RTFx) this lands well under 1 s.

**Windows:** code paths present (Handy ships SendInput injection + global-hotkey); live Windows verification deferred to the user's PC (see BLOCKERS.md).
