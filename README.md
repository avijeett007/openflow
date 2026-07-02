<p align="center">
  <img src="docs/icon.png" width="120" alt="OpenFlow"/>
</p>

<h1 align="center">OpenFlow</h1>

<p align="center">Local-first voice dictation you actually control.</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey" alt="Platform: macOS | Windows"/>
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT"/>
  <img src="https://img.shields.io/badge/built%20with-Tauri-24C8DB" alt="Built with Tauri"/>
  <img src="https://img.shields.io/badge/STT-local%20%2F%20self--hosted%20%2F%20remote-brightgreen" alt="STT: local / self-hosted / remote"/>
</p>

<p align="center">
  Press a shortcut (or just say a wake word), speak, and your words appear in whatever field you're in — with speech-to-text and AI cleanup running wherever <em>you</em> decide.
</p>

## What it is

OpenFlow is a local-first, cross-platform (macOS + Windows) voice-dictation desktop app — a privacy-respecting alternative to hosted tools like Wispr Flow. It's built on **Tauri 2 + Rust + React**.

You choose where speech-to-text and AI cleanup run, **independently of each other**: fully local on your machine, a self-hosted endpoint you control, or a remote provider. On top of that, OpenFlow adds a local analytics dashboard, per-app AI cleanup tone, OS-keychain-backed API keys, and a wake-word / hands-free mode so you don't have to touch a hotkey at all.

OpenFlow is a fork of **[Handy](https://github.com/cjpais/Handy)** (MIT). Huge credit to CJ Pais and the Handy contributors — the audio pipeline, transcription managers, and Tauri foundation come from their work.

## Features

- **Three backend modes, set independently for STT and cleanup** — local on-device models, a self-hosted endpoint you run yourself, or a remote hosted provider. Pick one mode for transcription and a different one for cleanup if you want.
- **Model Setup with live Test buttons** — validate a self-hosted or remote endpoint/API key from inside the app before you rely on it mid-dictation.
- **AI cleanup with per-app tone** — an optional post-processing pass that fixes grammar/formatting, with prompt overrides that can change automatically based on which application is focused.
- **Wake-word / hands-free mode** — say a wake phrase to start dictating, no hotkey required.
- **Analytics dashboard** — usage, words-per-minute, time saved, activity by app, and top keywords, all computed locally, with adjustable privacy modes controlling how much detail is stored.
- **API keys in the OS keychain** — remote provider credentials are stored via the system keychain, never in plaintext settings files.
- **Push-to-talk and tap-to-toggle** — configurable global shortcuts to start/stop recording, however you prefer to trigger it.

## Install

Download the latest installer from [GitHub Releases](https://github.com/avijeett007/openflow/releases/latest).

> **Note:** builds are currently **unsigned** — this project is distributed personally, to technical friends, rather than through an App Store or notarized channel. Your OS will show a one-time warning; see below for the unblock steps.

### macOS

1. Open the `.dmg` and drag **OpenFlow** to **Applications**.
2. Because the app is unsigned, Gatekeeper blocks the first launch. Unblock it once, either by:
   - **Right-click** `OpenFlow.app` → **Open** → **Open** again in the dialog, or
   - Running in Terminal:
     ```bash
     xattr -dr com.apple.quarantine /Applications/OpenFlow.app
     ```
3. Launch OpenFlow and grant the required permissions under **System Settings → Privacy & Security**:
   - **Microphone** — to record your voice.
   - **Accessibility** — to register global shortcuts and paste transcribed text into the focused app.
   - **Input Monitoring** — to detect the global hotkey.

### Windows

1. Run the installer.
2. Windows SmartScreen will flag it as unrecognized — click **More info → Run anyway** to proceed.

## Building from source

**Prerequisites:** [Rust](https://rustup.rs/) (stable, via rustup) and [Bun](https://bun.sh/) (`brew install bun`). Install frontend dependencies with `bun install` before building.

### macOS — Apple Silicon (arm64)

Apple Silicon uses a prebuilt ONNX Runtime, so no extra setup is needed:

```bash
bun install
bun run tauri build --target aarch64-apple-darwin
```

Output: `src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/OpenFlow_0.9.0_aarch64.dmg`

### macOS — Intel (x64)

ONNX Runtime has no prebuilt x64 binary, so it needs to be installed and linked dynamically:

```bash
brew install onnxruntime
ORT_LIB_LOCATION=$(brew --prefix onnxruntime)/lib ORT_PREFER_DYNAMIC_LINK=1 \
  bun run tauri build --target x86_64-apple-darwin
```

> On a real Mac with Finder running, the `.dmg` styling step just works. In headless CI it can need a manual `hdiutil` fallback step to produce the disk image.

### Windows

`transcribe-cpp`'s CMake build produces paths that exceed `MAX_PATH`, so long paths need to be enabled first (as admin, in PowerShell):

```powershell
New-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" `
  -Name "LongPathsEnabled" -Value 1 -PropertyType DWORD -Force
git config --system core.longpaths true
```

Then build normally:

```bash
bun install
bun run tauri build
```

Windows speech-to-text builds are CPU-only. Output lands in `src-tauri/target/release/bundle/nsis/` as an NSIS `.exe`.

### CI builds

Pushing a `v*` tag, or manually running the [`Build`](.github/workflows/build.yml) GitHub Actions workflow, builds macOS (arm64 + x64) and Windows installers and attaches them to a draft GitHub Release. This is the recommended way to produce installers for all platforms without juggling toolchains locally.

## Distribution

Installers are published on [GitHub Releases](https://github.com/avijeett007/openflow/releases) — that's the recommended way to get OpenFlow. The app has an auto-updater wired up against this repo's latest release.

Code signing and notarization (an Apple Developer ID certificate, Windows Authenticode) are deferred to a future public-distribution phase; for now, builds are unsigned and use the one-time unblock steps above.

## ⚠️ Contributions & support

> **Contributions are not being accepted right now.** OpenFlow is a solo project that I maintain alongside a full-time job and my own startup, **[knotie.ai](https://knotie.ai)**. I don't have the bandwidth to review and maintain incoming PRs at the moment.
>
> If you'd like to request a feature or report a bug, please **[open an issue](https://github.com/avijeett007/openflow/issues)** — I'll keep adding features over time, I just can't promise a fast turnaround.

## Support

If OpenFlow saves you time, consider supporting its development:

- [GitHub Sponsors](https://github.com/sponsors/avijeett007)
- [Buy Me a Coffee](https://buymeacoffee.com/)

<!-- Maintainer: please confirm/replace these donation links before publicizing them. -->

## Adding a provider

STT and cleanup backends live in `src-tauri/src/backends/` (the backend trait and HTTP adapters for self-hosted/remote STT), with the provider lists and configuration defined in `src-tauri/src/settings.rs`. Start there to wire up a new self-hosted or remote provider.

## License

MIT — see [LICENSE](LICENSE). OpenFlow is a fork of [Handy](https://github.com/cjpais/Handy) by CJ Pais and contributors; the original copyright and license are preserved in [LICENSE](LICENSE).

---

<p><sub>Built by the team behind <a href="https://knotie.ai"><strong>knotie.ai</strong></a> — where you can white-label and sell AI services.</sub></p>
