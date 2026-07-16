# CLI-agent detect/test fix — installed-app PATH split + Codex shim

Fixes BLOCKERS.md §10b (CLI agents broken on the user's Apple Silicon Mac,
installed app). Branch `fix/cli-agents-installed-app`.

## Root cause 1 — dev-vs-installed PATH split (Detect finds nothing)

A Finder/Dock-launched macOS app inherits **launchd's stripped PATH**
(`/usr/bin:/bin:/usr/sbin:/sbin`) — no `/opt/homebrew/bin`, no `/usr/local/bin`,
no per-user tool dirs. `tauri dev` inherits the Terminal's full PATH, which is
why detection worked in dev but not in the installed app. The base commit had
already added a Homebrew/system baseline to `baseline_path`, so a Homebrew
install was found — but a CLI installed by a **native installer (`~/.local/bin`,
`~/.claude/local`)** or a **node version manager (nvm / volta / bun global)** was
still missed. That is exactly the user's case: `codex` (Homebrew, in the old
baseline) was found; `claude` (elsewhere) was not.

### Fix

`detect_agent_binary` now searches, in order:

1. the process PATH,
2. an explicit baseline dir list — `/opt/homebrew/{bin,sbin}`,
   `/usr/local/{bin,sbin}`, `~/.local/bin`, `~/.claude/local`, `~/.bun/bin`,
   `~/.cargo/bin`, `~/.volta/bin`, `~/.deno/bin`, `~/.nvm/current/bin`,
   `~/.npm-global/bin`, every `~/.nvm/versions/node/*/bin` (newest first), and
   the system dirs,
3. a **login-shell fallback** — `<login-shell> -lc 'command -v <name>'`
   (5 s timeout; the user's `$SHELL` when it is an absolute known shell, else
   `/bin/zsh`, the macOS default) so any custom profile PATH (rbenv/asdf/fnm)
   resolves exactly as in their Terminal.

The pure dir-candidate builder (`detect_search_dirs` / `baseline_bin_dirs`) and
the `command -v` output parser are unit-tested; `is_executable_file` verifies the
exec bit rather than mere existence.

The same augmented PATH now flows through **one shared helper**
(`apply_baseline_env`) used by the run pipeline **and** the Test button, so
detect, test, and run all agree on PATH + SHELL + HOME.

### Reproduction on this Intel Mac (honest)

Real `claude` here is `/usr/local/bin/claude` (Homebrew cask symlink). Under the
stripped launchd PATH:

| logic                            | result                         |
| -------------------------------- | ------------------------------ |
| OLD (process PATH only)          | NOT FOUND (reproduces the bug) |
| NEW (PATH + baseline dirs)       | `/usr/local/bin/claude`        |
| login-shell fallback (`zsh -lc`) | `/usr/local/bin/claude`        |

Incremental proof (a CLI in `~/.local/bin`, where the base baseline had no dir):

| logic                                   | result                          |
| --------------------------------------- | ------------------------------- |
| base-commit baseline (no home dirs)     | NOT FOUND                       |
| new baseline (adds `~/.local/bin` etc.) | `~/.local/bin/claude_reprotest` |

Also covered by unit test
`detect_search_dirs_finds_home_installed_cli_under_stripped_path`.

## Root cause 2 — Codex `spawn …/vendor/…/codex ENOENT`

Confirmed by installing `@openai/codex@0.144.5` in a scratch dir and inspecting
`bin/codex.js`. **The npm package is a thin Node launcher**; the real ~283 MB
native binary ships in a **separate per-platform optional-dependency package**
(`@openai/codex-darwin-arm64` on Apple Silicon), resolved at runtime via
`require.resolve`. When that optional dep is missing/partial — npm silently skips
optional deps offline, with `--omit=optional`, or on some Homebrew-node setups —
the launcher can't find the binary:

- **older launcher** (the user's): raw
  `Error: spawn …/@openai/codex/vendor/aarch64-apple-darwin/codex/codex ENOENT`;
- **newer launcher** (reproduced here by removing the platform package):
  `Error: Missing optional dependency @openai/codex-darwin-arm64. Reinstall
Codex: npm install -g @openai/codex@latest`.

**This is NOT caused by our spawn env** — the launcher inherits `process.env` and
re-spawns; a real run and our Test hit the same failure. It is a genuinely
broken/partial install, **not fixable on our side**.

### Fix (make it actionable)

`test_agent_binary` classifies the combined stdout+stderr
(`classify_binary_output`, unit-tested against both launcher variants) and
returns a machine-readable `hint: "codex_vendor_missing"`. The UI renders a
localized, actionable message instead of the raw stack:

> Codex's native binary is missing from its install. Reinstall it with
> `npm i -g @openai/codex` or `brew reinstall codex`.

Running Test through the shared baseline env also means that if the user's
install is fine but was only failing because the launcher's own PATH lookups
were starved (a possible secondary factor for shims), it now resolves.

## Root cause 3 — UI kept a stale path on type-switch; no manual recourse

`CliAgentCard` now, on agent-type change, **always** refreshes the template +
delivery and re-runs detect; on detect failure it **clears** the stale
`binary_path`, shows an inline "not found — enter the path manually or click
Browse" notice, and offers a **Browse…** button (`open({directory:false})`,
starting in the Homebrew bin dir). Test failures render the actionable Codex
message rather than a raw spawn stack.

## Gates

`cargo test` (green; +11 detect/env unit tests, +3 classifier tests),
`cargo build`, `bunx tsc --noEmit`, `bun run lint` (0 errors), `bun run format`,
`bun run build` — all pass. `src/bindings.ts` updated for the new
`AgentBinaryTest.hint` / `AgentBinaryHint`. i18n added to all 20 locales.

## Windows parity (follow-up commit)

The user asked whether the fix works on Windows. Inspection said no — and one
part of the original detect code was actively wrong there. All detect/test/run
plumbing is now platform-parameterized:

1. **PATH separator.** Detect/baseline split PATH with `split_path_list(raw,
windows)` — a pure mirror of `std::env::split_paths` semantics (Windows:
   `;`-separated, double-quoted segments protected and quotes stripped; unix:
   `:`) — and join with `join_path_list` (`;` vs `:`). The pre-fix (and first
   fix) hardcoded `':'`, which would mangle `C:\bin` at the drive-letter colon.
   A host-parity test pins the mirror against `std::env::split_paths`.
2. **Windows baseline dirs.** `baseline_bin_dirs(windows, env, nvm)` takes an
   injected env lookup; on Windows it yields `%APPDATA%\npm` (npm's `.cmd`
   shims — where `claude.cmd`/`codex.cmd` live), `%LOCALAPPDATA%\Programs`,
   `%USERPROFILE%\{.bun\bin,.volta\bin,.cargo\bin,scoop\shims}`, and
   `%ProgramFiles%\nodejs` — all from env vars, no hardcoded drives; missing
   vars are skipped.
3. **PATHEXT candidate names.** `candidate_file_names(name, windows, pathext)`
   probes `name.exe`/`name.cmd`/`name.bat` (or the parsed `PATHEXT` list) per
   dir on Windows; the bare name on unix. Pure and parameterized, so the
   Windows behavior is unit-tested from macOS.
4. **Shell fallback.** The login-shell fallback is cfg-gated: unix keeps
   `<login-shell> -lc 'command -v <name>'`; Windows uses `where.exe <name>`
   (5 s bound, first line, must be an absolute existing file). `SHELL` is only
   set in the spawn env on unix.
5. **`.cmd`/`.bat` spawning.** `spawn_plan(binary, windows)` (pure,
   unit-tested) wraps batch scripts as `cmd.exe /C <script> <args…>` — raw
   `CreateProcess` cannot exec them — and both `test_agent_binary` AND the
   `AgentRunManager` spawn path use the same plan. `.exe` and unix binaries
   spawn directly.

### Pre-fix regression check (verdict)

Read the git history: the original `detect_agent_binary` (introduced in
`8fa079a`, unchanged through `0307688`, the commit before the first fix) used a
manual `path.split(':')` over `baseline_path` plus a bare-name `is_file()`
probe. **No `which` crate or other cross-platform resolver was ever used**
(checked `Cargo.toml` history). So on Windows the pre-fix code was already
non-functional (colon-split mangles drive letters; bare names miss `.cmd`
shims), and the first fix did not regress it further — it was equally broken
before and after. This follow-up makes Windows detection/testing/running
actually capable for the first time.

### Tested vs compile-only (honest)

- **Unit-tested from macOS (37 tests green, 13 new):** `;` splitting incl.
  drive letters + quoted entries, `:`/`;` joining, std-parity contract test,
  PATHEXT parsing + default trio, Windows baseline dirs from injected env
  (incl. missing-var skipping), `cmd.exe /C` wrap decisions (`.cmd`/`.bat`/
  `.CMD` wrapped; `.exe`/extensionless/unix direct), plus all earlier unix
  coverage.
- **Compile-only (Windows CI on PR #22):** the `#[cfg(windows)]` bodies — the
  `where.exe` invocation and the cfg-gated env application. No Windows
  hardware here; live Windows behavior (real `%APPDATA%\npm` layout, an actual
  `claude.cmd` run) still needs the user's Windows PC — BLOCKERS #1 stands.

## Remaining for the user (Apple Silicon Mac + Windows PC)

Final live verification ships in the next release: on the installed app, select
Claude → Detect should now resolve it wherever it lives; for Codex, if Test still
reports the vendor-missing hint, reinstall Codex per the message. The
dev-vs-installed split itself cannot be reproduced on a `tauri dev` run — only a
packaged, Finder-launched build gets the stripped launchd PATH — so the final
confirmation is on the user's machine with the shipped build. On Windows, the
whole CLI-agent flow (detect → test → run with `.cmd` shims) needs a first-ever
live pass on the user's PC (BLOCKERS #1).
