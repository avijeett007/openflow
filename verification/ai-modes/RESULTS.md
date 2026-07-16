# Phase B — AI Modes — verification results

Branch `feat/ai-modes`, on top of Phase A (`bdcfe40`).

## Gates (all green)

| Gate                | Result                                                          |
| ------------------- | --------------------------------------------------------------- |
| `cargo test`        | 243 passed, 0 failed (was ~221; +new AI-mode tests)             |
| `cargo build`       | clean (only pre-existing warnings)                              |
| `cargo fmt --check` | clean                                                           |
| `bunx tsc --noEmit` | clean                                                           |
| `bun run lint`      | 0 errors (1 pre-existing unrelated warning in devAutomation.ts) |
| `bun run format`    | applied                                                         |
| `bun run build`     | success (2806 modules)                                          |
| bindings.ts         | regenerated headless via `cargo run -- --list-models`           |

## Live e2e — NOT run (honest note)

The user's **packaged release** `OpenFlow.app` (pid checked at verification time,
`.../target/release/bundle/macos/OpenFlow.app`) was running. Constraints:

- Single-instance forwarding means launching `tauri dev` / a new GUI instance
  would just forward to the running production app — I must not disrupt it.
- The running app is the **pre-change release binary**, so it cannot exercise the
  new code even if driven.
- No vite dev server (port 1420 closed) → the `devAutomation.ts` webview eval
  bridge used in earlier phases is unavailable; and the OS screenshot paths are
  the documented BLOCKER-#8 TCC walls.

Therefore live UI screenshots + a voice→mode e2e were not run. Verification is by
**comprehensive unit tests + full code-trace** instead. This matches the
contract's "if the user's production app runs, do NOT launch tauri dev" clause.

## What the tests cover (the contract's Phase-B test list)

- **Resolution funnel precedence, all 5 sources** — `actions::tests::funnel_*`
  (HotkeyMode wins over a matching AppRule; missing hotkey mode falls through;
  AppRuleMode when no hotkey; disabled mode ignored; LegacyPerAppPrompt labeled;
  DefaultCleanup vs Raw; no-target falls through to exactly today's behavior).
- **App-rule matching semantics** — `funnel_*` + `app_rules_match_substring_both_directions`
  (case-insensitive substring both ways vs bundle id AND name; empty rule /
  unknown app never match).
- **Command fence-strip fallback** — `strip_command_fences_variants`
  (bare / `lang fenced / ` fenced / single-backtick / multi-line).
- **Direct bypasses LLM** — `direct_mode_bypasses_llm` (`mode_requires_llm`
  short-circuits before any provider/network work).
- **Hidden base + editable body** — `mode_system_prompt_has_hidden_base_plus_body`.
- **Mode CRUD + binding seeding/cleanup** — `commands::ai_modes::tests::*`
  (seed `mode:<id>` unbound binding, force binding_id, reject invalid slug /
  duplicate, preserve kind+app_rules+overrides, enabled-flip report, delete
  removes mode + binding and leaves defaults intact).
- **`mode:` binding recognition + disjointness** —
  `transcription_coordinator::tests::recognizes_mode_bindings` /
  `transcribe_agent_and_mode_bindings_are_disjoint`.
- **Legacy-store additive safety** — `settings::tests::legacy_store_without_ai_modes_deserializes_to_empty`
  - `ai_mode_partial_entry_defaults_cleanly`.

## Regression half (non-breaking rule)

- `ai_modes` empty → `resolve_ai_mode` returns `DefaultCleanup`/`Raw`/`LegacyPerAppPrompt`
  with `mode: None`, so `finish_dictation` routes through the unchanged
  `process_transcription_output` path. The existing cleanup path,
  `per_app_prompts`, wake-word, agents, and meetings callers pass `None, None`
  for the two new params — byte-for-byte unchanged when no mode matches.
- All pre-existing tests still green (243 total).

## Resolution funnel (as implemented) + debug log line

`resolve_ai_mode(settings, mode_id, target, post_process)` precedence:

1. `HotkeyMode` — a `mode:<id>` hotkey fired and that mode exists + is enabled.
2. `AppRuleMode` — no hotkey mode; an enabled mode's `app_rules` match the app
   captured at press time (case-insensitive substring both ways vs bundle+name).
3. `LegacyPerAppPrompt` — cleanup will run AND a `per_app_prompts` entry matches.
4. `DefaultCleanup` — cleanup will run with the selected prompt.
5. `Raw` — no cleanup; inject the raw transcript.

Only sources 1–2 change behavior; 3–5 reproduce today's pipeline exactly.

Per-utterance debug log (emitted once in `finish_dictation`):

```
AI mode resolution: source=<ModeSource>, mode=Some("<id>")|None, target_app=Some(("<bundle>","<name>"))|None
```

## Command-mode safety (as implemented)

- Command output is produced via `send_chat_completion_with_schema` with a
  `{command: string}` JSON schema (primary contamination guard); on providers
  without structured output, or on a structured-output error, it falls back to a
  plain completion + `strip_command_fences`.
- The command is TYPED via `clipboard::paste_without_auto_submit`, which forces
  auto-submit OFF regardless of the global `auto_submit` setting — the command is
  never executed.
