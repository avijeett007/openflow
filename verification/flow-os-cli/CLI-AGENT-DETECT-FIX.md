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

## Remaining for the user (Apple Silicon Mac)

Final live verification ships in the next release: on the installed app, select
Claude → Detect should now resolve it wherever it lives; for Codex, if Test still
reports the vendor-missing hint, reinstall Codex per the message. The
dev-vs-installed split itself cannot be reproduced on a `tauri dev` run — only a
packaged, Finder-launched build gets the stripped launchd PATH — so the final
confirmation is on the user's machine with the shipped build.
