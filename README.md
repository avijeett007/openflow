# OpenFlow

![OpenFlow](docs/icon.png)

**Local-first voice dictation for your desktop. Press a shortcut, speak, and your words appear in whatever text field you're in — privately, on your own machine.**

## What it is

OpenFlow is a cross-platform desktop dictation app (macOS, Windows, Linux) built with Tauri 2, Rust, and React. It's a privacy-respecting, open-source alternative to hosted dictation tools like Wispr Flow: you choose where transcription and AI cleanup actually run — fully local, a self-hosted endpoint you control, or a remote hosted provider — plus optional AI cleanup of your transcripts and a local analytics dashboard so you can see how much you're dictating and where.

OpenFlow is a fork of [Handy](https://github.com/cjpais/Handy) (MIT). Huge credit to CJ Pais and the Handy contributors — the audio pipeline, transcription managers, and Tauri foundation come from their work. See [Attribution](#attribution).

## How it works

1. **Press** a configurable keyboard shortcut to start/stop recording (push-to-talk is also supported).
2. **Speak** while the shortcut is active.
3. **Release**, and OpenFlow transcribes your speech.
4. **Get** the text pasted into whatever application you're using.

Under the hood: silence is trimmed with Silero VAD (Voice Activity Detection), the audio is transcribed, and the result is delivered to the focused app via the system clipboard / synthetic paste.

## Model backend modes

OpenFlow is designed around pluggable speech-to-text backends. Conceptually there are three modes:

- **Mode A — Local (default):** Transcription runs entirely on-device using bundled/downloaded models (Whisper-family models with GPU acceleration where available, and the CPU-optimized Parakeet V3). Nothing leaves your machine. This is the privacy-first default.
- **Mode B — Self-hosted endpoint:** Point OpenFlow at a speech-to-text server you run yourself (for example on your LAN or a private box). You keep control of the data while offloading inference to more capable hardware.
- **Mode C — Remote provider:** Use a third-party hosted transcription API. This trades some privacy for convenience/quality and requires network access and, typically, an API key.

Local (Mode A) is fully local and the recommended default. Modes B and C are opt-in — if you use them, audio (or its transcript) is sent to the endpoint/provider you configure.

## Features

- **Three backend modes** — local on-device models, a self-hosted OpenAI-compatible endpoint, or a remote hosted provider, switchable per speech-to-text and per text-cleanup independently.
- **Model Setup with Test buttons** — validate your self-hosted or remote endpoint/API key from within the app before relying on it, instead of finding out mid-dictation.
- **AI text cleanup** — an optional post-processing pass with reusable prompt templates for grammar/formatting cleanup, including **per-app prompt overrides** so the cleanup instructions used can change automatically based on which application is focused.
- **Analytics dashboard** — local-only charts for total dictations, words, average WPM, time saved, streaks, usage by app/project, and top keywords, with three **privacy modes** (Full, Keywords only, Off) controlling how much detail is stored, plus a one-click "clear analytics data" action.
- **Keychain-stored keys** — API keys for remote providers are saved in the OS keychain, not in plaintext settings files.
- **Configurable shortcuts** — global keyboard shortcuts with push-to-talk support.
- **Local speech-to-text** — bundled/downloadable Whisper-family models (GPU-accelerated where available) and the CPU-optimized Parakeet V3, with Silero VAD trimming silence before transcription.

## Installation

Download the latest installer for your OS from the [Releases page](https://github.com/avijeett007/openflow/releases/latest).

> **Note on signing:** OpenFlow v1 ships **unsigned / ad-hoc** — there is no Apple notarization and no Windows Authenticode signature yet. Your OS will warn you the first time you open it. This is expected; the one-time unblock steps below are safe. (Signing and notarization are planned for a future public-distribution phase — see [Distribution & signing status](#distribution--signing-status).)

### macOS

1. Open the `.dmg` and drag **OpenFlow** to **Applications**.
2. Because the app is unsigned, macOS Gatekeeper blocks the first launch. Do the one-time unblock in one of these ways:
   - **Right-click** `OpenFlow.app` in Applications → **Open** → **Open** again in the dialog, **or**
   - Run this in Terminal to strip the quarantine flag:

     ```bash
     xattr -dr com.apple.quarantine /Applications/OpenFlow.app
     ```
3. Launch OpenFlow and grant the [required permissions](#required-permissions-macos).

### Windows

1. Run the `.exe` (NSIS) or `.msi` installer.
2. Because the app is unsigned, Windows SmartScreen shows a "Windows protected your PC" dialog on first run. Click **More info → Run anyway** to proceed.

### Linux

Install from the `.deb`, `.rpm`, or AppImage bundle. See [BUILD.md](BUILD.md) for runtime dependencies (e.g. `libgtk-layer-shell0`) and per-distro notes.

## Required permissions (macOS)

OpenFlow needs these macOS permissions to function. Grant them under **System Settings → Privacy & Security**:

- **Accessibility** — required to register global keyboard shortcuts and to paste transcribed text into the focused app.
- **Microphone** — required to record your voice.
- **Input Monitoring** — required to detect your shortcut key presses globally.

On Windows and Linux no special permission grants are needed for the core flow (Linux Wayland users may need extra text-input tooling — see [BUILD.md](BUILD.md)).

### macOS Tahoe (26.x) note

On macOS 26 (Tahoe), the OS restricts synthetic keyboard events, which can interfere with the "type the transcript" path. OpenFlow handles this by falling back to **clipboard paste**, so dictation still works — but you **must** grant the **Accessibility** permission above for the paste to reach the target app.

## Building from source

Full platform matrix and troubleshooting are in [BUILD.md](BUILD.md). Quick version:

**Prerequisites:** [Rust](https://rustup.rs/) (stable, via rustup), [Bun](https://bun.sh/), and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```bash
bun install
bun run tauri build      # or: bun run tauri dev
```

### Intel Mac (x86_64) — extra step

The `ort` crate has no prebuilt binary for `x86_64-apple-darwin`, so ONNX Runtime must be installed and linked dynamically:

```bash
brew install onnxruntime
ORT_LIB_LOCATION=$(brew --prefix onnxruntime)/lib ORT_PREFER_DYNAMIC_LINK=1 bun run tauri build
```

Apple Silicon macOS, Windows, and Linux use prebuilt `ort` binaries and need no special step.

### Continuous integration

The [`Build`](.github/workflows/build.yml) workflow builds unsigned installers for macOS arm64 (`macos-14`), macOS x64 (`macos-13`), and Windows (`windows-latest`). It runs on manual dispatch and on `v*` tags; tag builds attach the installers to a draft GitHub Release. The Intel-macOS job installs `onnxruntime` via Homebrew and exports the `ORT_*` env vars before building.

## Distribution & signing status

OpenFlow v1 is intentionally **unsigned**:

- **macOS:** ad-hoc signed only (`signingIdentity: "-"`), no Apple Developer certificate, no notarization.
- **Windows:** no Authenticode signing (the previous Azure Trusted Signing step was removed so builds run with zero credentials).

Proper code signing and notarization are **deferred to a future public-distribution phase**. Until then, use the one-time unblock steps in [Installation](#installation).

**Auto-update:** The Tauri updater plugin is wired (pubkey + a GitHub releases endpoint), but stubbed for now — updater artifact generation is disabled and the signing keys will be regenerated as part of the signing phase. **TODO:** regenerate the updater signing keypair, update the `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`, and re-enable `bundle.createUpdaterArtifacts` once signing is in place.

## Adding a new provider

Speech-to-text backends live in the Rust layer under `src-tauri/src/managers/` (`model.rs`, `transcription.rs`) with Tauri command handlers in `src-tauri/src/commands/`. To add a new provider (a self-hosted endpoint or a remote API), implement it alongside the existing transcription path there and surface its settings through the settings store (`src/stores/settingsStore.ts`) and model-selector UI (`src/components/model-selector/`). See [AGENTS.md](AGENTS.md) for the architecture overview.

## Attribution

OpenFlow is a fork of **[Handy](https://github.com/cjpais/Handy)** by CJ Pais and contributors, used under the MIT License. The original copyright and license are preserved in [LICENSE](LICENSE).

## License

MIT — see [LICENSE](LICENSE).
