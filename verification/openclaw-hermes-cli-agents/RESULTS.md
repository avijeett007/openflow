# OpenClaw + Hermes CLI agent verification results

Branch `feat/openclaw-hermes-agents`. `AgentCliType::Openclaw` and
`AgentCliType::Hermes` already existed as first-class enum variants, but their
`default_cli_template`/`prompt_via`/`binary_name` were written BEST-EFFORT and
never installed or verified — this PR does for them exactly what PR #50 did
for Kimi: install the real binary, drive its flags until a usage error becomes
an auth/config error, and fix the defaults to the verified invocation.

Platform tested: macOS (Intel, Darwin 23.5.0).

## OpenClaw

### What it actually is (important context)

OpenClaw (`https://github.com/openclaw/openclaw`, `openclaw.ai`) is **not**
shaped like Claude/Codex/Kimi. Those are one-shot "run in a project directory"
coding CLIs. OpenClaw is a persistent personal-assistant **gateway/daemon**
that bridges an LLM to messaging channels (WhatsApp/Telegram/Slack/Discord/…)
via a background "heartbeat" process, plus a plugin/skills marketplace
(ClawHub). It has no per-run `--cwd`/`--dir` flag anywhere in its CLI surface.

### Install — live-verified

```
$ npm install -g openclaw@latest
npm warn EBADENGINE Unsupported engine {
npm warn EBADENGINE   package: 'openclaw@2026.7.1-2',
npm warn EBADENGINE   required: { node: '>=22.22.3 <23 || >=24.15.0 <25 || >=25.9.0' },
npm warn EBADENGINE   current: { node: 'v24.2.0', npm: '11.3.0' }
npm warn EBADENGINE }
added 309 packages in 19s
```

npm installs it anyway despite the engine warning, but the binary then
**hard-refuses to run at all**:

```
$ openclaw --version
openclaw: Node.js >=22.22.3 <23, >=24.15.0 <25, or >=25.9.0 is required (current: v24.2.0).
If you use nvm, run:
  nvm install 24
  nvm use 24
  nvm alias default 24
```

This Node-version gate is a real, live-discovered gotcha (not documented
anywhere we found) — it would silently look like a broken install to any user
whose Node is a hair outside the supported range. It's an install
prerequisite, not something the command template can work around, so it's
recorded here rather than encoded in code. Installed `node@24` via Homebrew
(`brew install node@24`, kept keg-only/unlinked so it doesn't disturb the
system default Node) and re-ran everything with
`PATH="/usr/local/opt/node@24/bin:$PATH"` (Node v24.18.0):

```
$ openclaw --version
OpenClaw 2026.7.1-2 (0790d9f)
```

### `openclaw --help` — finding the headless equivalent

The full command surface is large (daemon/channels/plugins/skills/etc.). The
one command that matches "run one turn non-interactively" is:

```
agent    Run an agent turn via the Gateway (use --local for embedded)
```

```
$ openclaw agent --help
Usage: openclaw agent [options]

Run an agent turn via the Gateway (use --local for embedded)

Options:
  --agent <id>               Agent id (overrides routing bindings)
  --deliver                  Send the agent's reply back to the selected channel (default: false)
  --json                     Output result as JSON (default: false)
  --local                    Run the embedded agent locally (requires model provider API keys in your shell) (default: false)
  -m, --message <text>       Message body for the agent
  ...
```

`--local` is the analog of Claude's `-p` / Codex's `exec` / Kimi's `-p`: it
runs the embedded agent directly with no Gateway process required. Crucially,
there is **no `--cwd`/`--dir`/`--workspace` flag on `agent` at all** — instead
it addresses a **named agent identity** via `--agent <id>`, and each identity
has its own fixed workspace configured separately (`openclaw agents add
--workspace <dir>`).

### Does a fresh install have a usable default identity?

Yes — confirmed with zero onboarding run:

```
$ openclaw agents list
Agents:
- main (default)
  Workspace: ~/.openclaw/workspace
  Agent dir: ~/.openclaw/agents/main/agent
  Routing rules: 0
  Routing: default (no explicit rules)
```

So the default template hardcodes `--agent main` — it is the tool's own
default identity, not a guess.

### Driving `openclaw agent --local` for real

```
$ openclaw agent --local --agent main --message "say hello"
Error: Pass --to <E.164>, --session-key, --session-id, or --agent to choose a session
```

(That was hit once _without_ `--agent`, confirming the flag is mandatory —
exactly the reason `--agent main` belongs in the default template.) With
`--agent main` supplied:

```
$ openclaw agent --local --agent main --message "say hello" --json
[diagnostic] lane task error: lane=main durationMs=1492 error="ProviderAuthError: No API key found for provider "openai". Auth store: /Users/avijitsarkar/.openclaw/agents/main/agent/openclaw-agent.sqlite (agentDir: /Users/avijitsarkar/.openclaw/agents/main/agent). Configure auth for this agent (openclaw agents add <id>) or copy only portable static auth profiles from the main agentDir."
[diagnostic] lane task error: lane=session:agent:main:main durationMs=1512 error="ProviderAuthError: ..."
[model-fallback/decision] model fallback decision: decision=candidate_failed requested=openai/gpt-5.5 candidate=openai/gpt-5.5 reason=auth next=none detail=...
FailoverError: No API key found for provider "openai". ... | missing-provider-auth
```

This is the exact proof the task asked for: **no usage error** ("Pass --to
...", "unknown option", etc.) — the CLI parsed `--local --agent main
--message ...` fine and failed only on `ProviderAuthError` /
`FailoverError: missing-provider-auth`, which requires the user's own model
provider credentials (`openclaw agents add`/`openclaw configure`) to go
further. Verified stdout/stderr separately (`1>file 2>file`): stdout was
empty and all of the above went to stderr, with the same result whether or
not `--json` was passed (the run fails before any output-formatting stage is
reached either way).

**Verified default template:** `agent --local --agent main --message
{prompt}` with `prompt_via = Arg`.

### Output-format decision: no `--json`

Per `docs.openclaw.ai/cli/agent`, `--json` (with `--deliver`) returns
`{payloads, meta, deliveryStatus}` — not the Claude-shaped
`{"type": "system"|"assistant"|"user"|"result"}` events
`parseAgentOutput.ts` recognizes. No successful run was possible (no API key)
to capture real `--json` output for comparison anyway. Same call as Kimi:
default to plain stdout text, which the raw-output fallback renders cleanly.

### What still needs the user

- **A model provider API key/credential** configured for the `main` agent
  (`openclaw agents add`/`openclaw configure`, or an env var such as
  `OPENAI_API_KEY`) — this agent cannot fabricate one. Once configured, a full
  run + Agent Runs panel screenshot is the natural follow-up.
- **Node version**: OpenClaw requires Node `>=22.22.3<23`, `>=24.15.0<25`, or
  `>=25.9.0`. A user on an older/mismatched Node (as this machine's system
  Node, v24.2.0, was) will see `openclaw` hard-refuse to start with an
  actionable nvm hint — worth knowing before reporting a "broken install."
- **Per-project workspace mismatch (architectural, not a bug in this PR)**:
  OpenFlow spawns the CLI with `current_dir` set to the configured
  `project_path`/`{cwd}`, but OpenClaw's `--agent main` identity has its own
  fixed workspace (`~/.openclaw/workspace`) independent of the spawn cwd —
  `openclaw agent` has no flag to override it per run. Multi-project use would
  need per-project OpenClaw agent identities (`openclaw agents add --workspace
  <dir>`) configured by the user; that's beyond what a CLI-agent-type default
  template can express, so it's called out here rather than silently assumed.

**Release-readiness verdict: template correct, verified past argument parsing
into an auth error — user-verification-pending** (needs the user's own
provider credentials to confirm a full run, same bar as Kimi).

## Hermes Agent

### Install — live-verified

```
$ curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash -s -- \
    --skip-setup --skip-browser --non-interactive
...
✓ Installation Complete!
Config:    /Users/avijitsarkar/.hermes/config.yaml
API Keys:  /Users/avijitsarkar/.hermes/.env
Code:      /Users/avijitsarkar/.hermes/hermes-agent
Commands:
   hermes              Start chatting
   ...
```

```
$ hermes --version
Hermes Agent v0.19.0 (2026.7.20) · upstream cbc1054e
Install directory: /Users/avijitsarkar/.hermes/hermes-agent
Install method: git
Python: 3.11.7
```

Binary lands at `~/.local/bin/hermes` (confirmed with `ls -la`) — already
covered by the existing `.local/bin` entry in `baseline_bin_dirs`, so **no
baseline-dir change was needed** for Hermes.

### `hermes --help` — finding the headless equivalent

```
  -z PROMPT, --oneshot PROMPT
                        One-shot mode: send a single prompt and print ONLY the
                        final response text to stdout. No banner, no spinner,
                        no tool previews, no session_id line. Tools, memory,
                        rules, and AGENTS.md in the CWD are loaded as normal;
                        approvals are auto-bypassed. Intended for scripts / pipes.
  ...
  --yolo                Bypass all dangerous command approval prompts (use at
                        your own risk)
```

`-z`/`--oneshot` is documented, in Hermes's own words, as the purest headless
mode — and it already loads `AGENTS.md`/rules from the process's **current
working directory**, so no `{cwd}` token is needed in the template (matches
how `current_dir` is already set for every CLI-agent spawn).

### Checking for the Kimi-style trap: does `--yolo` conflict with `-z`?

```
$ hermes -z "say hello"
hermes -z: agent failed: No inference provider configured. Run 'hermes model' to choose a
provider and model, or set an API key (OPENROUTER_API_KEY, OPENAI_API_KEY, etc.) in
~/.hermes/.env.

$ hermes -z "say hello" --yolo
hermes -z: agent failed: No inference provider configured. Run 'hermes model' to choose a
provider and model, or set an API key (OPENROUTER_API_KEY, OPENAI_API_KEY, etc.) in
~/.hermes/.env.
```

Unlike Kimi's `-p`/`--auto`/`--yolo` (hard usage error), Hermes's `-z` and
`--yolo` combine cleanly — both reach the **identical** clean
config/auth error (not a usage error), proving the invocation is correct. Since
`-z` already auto-bypasses tool-call approvals per its own docs, `--yolo` is
redundant defense-in-depth (matching Claude's `acceptEdits` in spirit — git
remains the safety net) rather than load-bearing, and it is verified safe to
keep.

**Verified default template:** `-z {prompt} --yolo` with `prompt_via = Arg`.

### Output-format decision: none needed

`-z` has no JSON/stream-json variant — its entire contract is "print ONLY the
final response text to stdout." That is already the plain-text shape the
raw-output fallback in `parseAgentOutput.ts` renders cleanly, so there is no
format flag to choose (unlike Kimi/OpenClaw, which both had to actively opt
out of a JSON mode).

### What still needs the user

- **A model provider credential** (`hermes model`, `hermes setup`, or an env
  var such as `OPENROUTER_API_KEY`/`OPENAI_API_KEY` in `~/.hermes/.env`) —
  cannot be fabricated by this agent. Once configured, a full run + Agent Runs
  panel screenshot is the natural follow-up, but is not required to trust this
  fix: the flag-parsing proof above (clean config error, not a usage error) is
  the evidence the task asked for.

**Release-readiness verdict: template correct, verified past argument parsing
into a config/auth error — user-verification-pending** (needs the user's own
provider credentials to confirm a full run).

## `{prompt}` single-argv-element substitution — regression-tested for both

`build_argv` (`src-tauri/src/managers/agent_run.rs`) tokenizes the template
first, then substitutes `{prompt}` within the already-split token — so a
multi-word instruction never gets re-split. Added agent-specific regression
tests exercising the real default templates end to end:

```rust
build_argv("agent --local --agent main --message {prompt}", "/tmp/proj", "list all the files", PromptDelivery::Arg)
  == vec!["agent", "--local", "--agent", "main", "--message", "list all the files"]

build_argv("-z {prompt} --yolo", "/tmp/proj", "list all the files", PromptDelivery::Arg)
  == vec!["-z", "list all the files", "--yolo"]
```

## Binary detection

- **OpenClaw**: added `.openclaw/bin` to the unix baseline dir list in
  `baseline_bin_dirs` — OpenClaw's `install-cli.sh` variant defaults to
  installing at `<prefix>/bin/openclaw` with prefix `~/.openclaw`, which a
  GUI-launched app's stripped PATH would miss (same class of bug as the
  pre-existing Claude/Codex/Kimi detection fixes). The plain `npm install -g`
  path used for this PR's live verification instead lands in npm's global bin
  dir, already covered by existing entries.
- **Hermes**: no change needed. Its installer symlinks to `~/.local/bin/hermes`,
  which was already in the baseline list (added for Claude Code's native
  install).

Both additions/no-ops are regression-tested in
`baseline_bin_dirs_includes_home_and_static_dirs`.

## Gates (all green)

| Gate                   | Result                                                                                                                                                                                                    |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo test` (`--lib`) | 312 passed, 0 failed (was 308; +4 new tests)                                                                                                                                                              |
| `cargo build`          | clean (only pre-existing unrelated warnings)                                                                                                                                                              |
| `bunx tsc --noEmit`    | clean                                                                                                                                                                                                     |
| `bun run lint`         | 0 errors (1 pre-existing unrelated warning in `devAutomation.ts`)                                                                                                                                         |
| `bun run format`       | clean (only reformatted this PR's own new Rust code)                                                                                                                                                      |
| `bun run build`        | success (2815 modules)                                                                                                                                                                                    |
| `src/bindings.ts`      | unchanged — no type-surface change (`AgentCliType`'s variants and shapes are untouched; only the two variants' default values/docs changed), confirmed via `cargo run -- --list-models` producing no diff |

## Regression — Claude/Codex/Kimi/Custom unchanged

`default_cli_template`/`default_cli_binary_name` for `Claude`, `Codex`,
`Kimi`, `Custom` are untouched; their existing tests
(`claude_default_template_matches_verified_flags`,
`kimi_default_template_matches_live_verified_flags`, the `build_argv_*`
stdin/arg tests, `cli_agent_round_trips_through_serde`) all still pass
unmodified. `CliAgentCard.tsx`'s `CLI_TYPES` list and the
`settings.agents.card.cli.agentType.options.{openclaw,hermes}` i18n labels
("OpenClaw", "Hermes") were already correct and needed no change.
