# Kimi Code CLI agent — verification results

Branch `feat/kimi-cli-agent`. Adds `AgentCliType::Kimi` as a first-class CLI
agent type, following the established Claude/Codex pattern.

## The two reported errors, diagnosed

The user configured Kimi as a **Custom** CLI agent (Kimi had no first-class
type before this change) and hit two errors:

1. **`option '-p, --prompt <prompt>' argument missing`** — Custom agents default
   to `PromptDelivery::Stdin`. With `Stdin` delivery, `build_argv` deliberately
   _drops_ a bare `{prompt}` token from the template (the instruction goes to
   stdin instead — see `build_argv`'s doc comment). So a hand-typed template
   like `-p {prompt} --output-format stream-json --auto` was sent to Kimi as
   just `-p --output-format stream-json --auto` with the instruction piped to
   stdin — but Kimi's `-p/--prompt` takes the prompt as an **argument value**,
   not stdin, so it saw `-p` with nothing after it. Fix: Kimi's default
   `prompt_via` is `Arg`.
2. **`unknown command '—yolo'`** — that dash is an **em-dash (U+2014)**, not two
   hyphens. macOS/WebKit's smart-dashes text substitution silently mangled a
   hand-typed `--yolo` in the command-template `<textarea>` before it was ever
   saved. Fix (two-pronged, from the task spec): (a) ship a **prefilled**
   default template so most users never hand-type flags at all, and (b)
   **harden** the command-template/name/binary-path inputs against the
   substitution (`autoCorrect="off" autoCapitalize="off" spellCheck={false}
autoComplete="off"`) so even a hand-edit is safe.

## Live verification — Kimi Code CLI installed and driven for real

Installed via the official installer
(`curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash`), which drops a
**self-contained native binary** (no Node.js) at `$HOME/.kimi-code/bin/kimi` —
confirmed `kimi --version` → `0.28.1`.

`kimi --help` confirmed the flags in the task brief exist as described:
`-p/--prompt`, `--output-format text|stream-json`, `-y/--yolo`, `--auto`,
`-m/--model`, `--add-dir`, `acp` subcommand.

### Critical finding NOT in the docs: `--auto`/`--yolo` cannot combine with `-p`

The task brief's working assumption was `--auto` is the best headless flag.
Running the real binary immediately falsified that:

```
$ kimi -p "say hello" --output-format stream-json --auto
error: Cannot combine --prompt with --auto.

$ kimi -p "say hello" --output-format stream-json -y
error: Cannot combine --prompt with --yolo.
```

Neither `kimi --help` nor the published docs mention this restriction — it only
surfaced by actually running the binary. Dropping the auto-approve flag
entirely gets past argument parsing and reaches a real (auth) error, proving
the invocation is correct:

```
$ kimi -p "say hello"
error: failed to run prompt: No model configured. Run `kimi` and use /login to
sign in, then retry; or set default_model in config.toml.
See log: /Users/avijitsarkar/.kimi-code/logs/kimi-code.log
```

This is exactly the proof the task asked for: **no "argument missing", no
"unknown command"** — the CLI parsed the flags fine and failed only on
"no model configured" (an auth/config error), which requires the user's real
Kimi account to go further. `--output-format text` and `--output-format
stream-json` were both tried and both pass argument parsing identically (the
failure is the same downstream auth error either way), confirming the format
choice below is a rendering decision, not a correctness one.

**Verified default template:** `-p {prompt} --output-format text` with
`prompt_via = Arg`, **no** `--yolo`/`--auto`. `command_template` doc comment in
`src-tauri/src/settings.rs` records the exact live-tested error strings so a
future reader isn't tempted to "helpfully" add `--auto` back.

### `kimi login` — device-code flow (stopped here, needs the user)

```
$ kimi login
Opening browser for Kimi device login: https://www.kimi.com/code/authorize_device?user_code=E9FG-NDMY
If the browser did not open, paste the URL above and enter code: E9FG-NDMY
Code expires in 1800s.
Waiting for authorization to complete...
```

Per the task's explicit instruction, authentication requires the user's own
Kimi account and no secret was fabricated — this step was **not completed**.
Everything up to "the flags are objectively correct" is proven above; a full
authenticated run (and an Agent Runs panel screenshot of real Kimi output) is
the one remaining step and needs the user to complete the browser
authorization.

## `{prompt}` single-argv-element substitution — already correct, now regression-tested

Traced `build_argv` (`src-tauri/src/managers/agent_run.rs`): the template is
tokenized on whitespace **first** (honoring quotes), then `{cwd}`/`{prompt}` are
substituted **within each already-split token**. So a multi-word instruction
never gets re-split — it lands in argv as the single element that replaced the
`{prompt}` token. This was already correct (an existing test,
`build_argv_arg_delivery_substitutes_prompt`, covered the general case) — the
"argument missing" bug was the `Stdin`-drops-the-flag issue above, not a
tokenization bug. Added a Kimi-specific regression test using the task's exact
example:

```rust
build_argv("-p {prompt} --output-format text", "/tmp/proj", "list all the files", PromptDelivery::Arg)
  == vec!["-p", "list all the files", "--output-format", "text"]
```

`"list all the files"` reaches `-p` as ONE argv element, not three.

## Output-format decision: `text`, not `stream-json`

`kimi --output-format stream-json` docs (moonshotai.github.io/kimi-code)
describe an **OpenAI-style chat-message schema** — `Assistant` message,
`tool_calls`, then a `Tool` message, then further `Assistant` messages. This
does not match the Claude-shaped schema
`src/components/settings/agent-runs/parseAgentOutput.ts` recognizes
(`{"type": "system"|"assistant"|"user"|"result"}` with Claude's specific
content-block shapes). Rather than guess at extending the parser for an
unverified schema (no authenticated run was possible to capture real
stream-json output), the safer, live-reasoned default is `--output-format
text`: plain, human-readable stdout. `parseAgentOutput` still cannot crash on
it — every line fails `JSON.parse`, is skipped silently, `structured` stays
`false`, and the panel falls back to the raw-text renderer, which is exactly
what "readable" means for `text` output. No parser changes were needed.

## Binary detection — `.kimi-code/bin` added to the baseline dirs

The Kimi installer only appends `$HOME/.kimi-code/bin` to `PATH` via the user's
shell rc file (`.bash_profile`/`.zshrc`), which a GUI-launched macOS app never
sources (same class of bug as the pre-existing Claude/Codex detection fix in
`verification/flow-os-cli/`). Added `.kimi-code/bin` to the unix baseline dir
list in `baseline_bin_dirs` (`src-tauri/src/managers/agent_run.rs`) so
`detect_agent_binary("kimi")` finds it without relying on the login-shell
fallback. Regression-tested (`baseline_bin_dirs_includes_home_and_static_dirs`
now also asserts `.kimi-code/bin` is present).

## Gates (all green)

| Gate                           | Result                                                              |
| ------------------------------ | ------------------------------------------------------------------- |
| `cargo test` (`--lib`)         | 308 passed, 0 failed (was 306; +2 new Kimi tests)                   |
| `cargo build`                  | clean (only pre-existing unrelated warnings)                        |
| `cargo clippy` (default level) | 14 pre-existing warnings, none new/none touching Kimi code          |
| `bunx tsc --noEmit`            | clean                                                               |
| `bun run lint`                 | 0 errors (1 pre-existing unrelated warning in `devAutomation.ts`)   |
| `bun run format`               | clean, no changes needed                                            |
| `bun run build`                | success (2815 modules)                                              |
| `src/bindings.ts`              | regenerated via `cargo run -- --list-models` (run from `src-tauri`) |

## Regression — Claude/Codex/Custom unchanged

`default_cli_template`/`default_cli_binary_name` for `Claude`, `Codex`,
`Openclaw`, `Hermes`, `Custom` are untouched (only a new match arm was added);
their existing tests (`claude_default_template_matches_verified_flags`, the
`build_argv_*` stdin/arg tests, `cli_agent_round_trips_through_serde`) all
still pass unmodified. `CLI_TYPES` in `CliAgentCard.tsx` only had `"kimi"`
inserted before `"custom"` — no existing option removed or reordered
relative to each other.

## What still needs the user

- **Kimi authentication** (`kimi login`, RFC 8628 device-code flow tied to the
  user's own Kimi account) — cannot be completed by this agent. Once
  authenticated, a full run + Agent Runs panel screenshot would be the natural
  follow-up, but is not required to trust this fix: the flag-parsing proof
  above (auth error, not usage error) is the evidence the task asked for.
