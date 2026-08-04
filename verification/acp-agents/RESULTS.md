# ACP agents — live verification results

**Date:** 2026-08-03
**Branch:** `feat/acp-agents` @ `8c486e1`
**Machine:** Intel Mac (`x86_64-apple-darwin`), macOS 24.6.0, node v24.2.0, npm 11.3.0
**Spec:** `documentation/design/DESIGN-acp-agents.md` §2 / §7.1
**Task:** `.superpowers/sdd/PLAN/task-12-brief.md`

---

## 0. Executive summary

| #       | Item                                                          | Verdict                                                                                                    |
| ------- | ------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| **V1**  | `fs`/`terminal` declared **false** — do agents still operate? | ✅ **VERIFIED — the design assumption HOLDS** (Kimi, Claude Code; **Codex too as of 2026-08-04 — §12**).   |
| **V2**  | Permission prompt → Allow → file changed                      | ✅ **Verified at protocol level** (Kimi + Claude Code; **Codex too — §12**). ❌ GUI half **not verified**. |
| **V3**  | Same session id reused, context retained                      | ✅ **Verified** (Kimi + Claude Code).                                                                      |
| **V4**  | Deny → agent reports it could not proceed, no side effect     | ✅ **Verified** (Kimi).                                                                                    |
| **V5**  | Stop mid-turn → `cancelled`, no orphan, session still warm    | ✅ **Verified at protocol level** (Kimi).                                                                  |
| **V5b** | Orphan-at-quit (`pending_children`)                           | ❌ **NOT VERIFIED** — requires quitting the GUI app mid-spawn.                                             |
| **V6**  | Idle timeout → child reaped, next instruction respawns        | ❌ **NOT VERIFIED** — requires the GUI app.                                                                |
| **V7**  | Regression half                                               | ⚠️ **PARTIAL** — static/settings evidence yes, in-app agent runs no.                                       |
| —       | Full gates                                                    | ✅ All pass (see §7).                                                                                      |

### 🚨 One blocking defect found — `stopReason` vocabulary was wrong — **NOW FIXED**

**Every successful ACP turn was reported to the user as a FAILURE.** A real,
live-reproduced defect that the 465 unit tests could not catch, because the tests
asserted against a stop-reason vocabulary that **no real agent emits**.

Diagnosis in §2. **The fix and its live re-verification are in §10** — the defect is
resolved, re-proved against both working agents, and pinned by regression tests that were
break-and-revert checked. Test count 465 → **469**.

---

## 1. V1 — the riskiest assumption (`fs`/`terminal` = false)

### 1.1 Method

OpenFlow does **not** need to be running for this. A standalone probe
(`scratchpad/acp-probe.mjs`, reproduced in §8) spawns each agent's ACP command over
stdio and speaks the **exact** frames OpenFlow speaks. The `initialize` params are
byte-for-byte what `ClientCapabilities::v1_defaults()` +
`do_initialize()` produce (`src-tauri/src/acp/protocol.rs:39`,
`src-tauri/src/managers/acp_session.rs:983`):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "fs": { "readTextFile": false, "writeTextFile": false },
      "terminal": false
    },
    "clientInfo": { "name": "OpenFlow", "version": "0.15.7" }
  }
}
```

The probe also replicates OpenFlow's inbound-request doctrine: it answers
`session/request_permission`, and refuses **everything else** with
`-32601 Method not found` — exactly as `refuse_inbound()`
(`acp_session.rs:969`) and the turn driver (`agent_run.rs:1385`) do. So if an agent
tried to call `fs/read_text_file` back to us, the probe would refuse it the same way the
real app does, and we would see it in the log.

### 1.2 `initialize` results — all three agents

**Kimi** — `kimi acp` (`~/.kimi-code/bin/kimi`)

```
[INIT-OK] kimi in 1527ms
```

| Field               | Value                                                                       |
| ------------------- | --------------------------------------------------------------------------- |
| `agentInfo.name`    | `Kimi Code CLI`                                                             |
| `agentInfo.version` | `0.31.0`                                                                    |
| `protocolVersion`   | `1`                                                                         |
| `authMethods`       | `[{ id: "login", type: "terminal", name: "Login with Kimi account", ... }]` |

```json
"agentCapabilities": {
  "loadSession": true,
  "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
  "mcpCapabilities": { "http": true, "sse": true },
  "sessionCapabilities": { "list": {}, "resume": {} }
}
```

**Claude Code** — `npx -y @agentclientprotocol/claude-agent-acp`

```
[INIT-OK] claude-code in 13872ms
```

| Field               | Value                                                          |
| ------------------- | -------------------------------------------------------------- |
| `agentInfo.name`    | `@agentclientprotocol/claude-agent-acp` (title `Claude Agent`) |
| `agentInfo.version` | `0.64.2`                                                       |
| `protocolVersion`   | `1`                                                            |
| `authMethods`       | `[]` (already authenticated on this machine)                   |

```json
"agentCapabilities": {
  "_meta": { "claudeCode": { "promptQueueing": true } },
  "promptCapabilities": { "image": true, "embeddedContext": true },
  "mcpCapabilities": { "http": true, "sse": true },
  "auth": { "logout": {} },
  "providers": {},
  "loadSession": true,
  "sessionCapabilities": {
    "additionalDirectories": {}, "close": {}, "delete": {},
    "fork": {}, "list": {}, "resume": {}
  }
}
```

**Codex** — `npx -y @agentclientprotocol/codex-acp`

```
[INIT-OK] codex in 22247ms
```

| Field               | Value                                               |
| ------------------- | --------------------------------------------------- |
| `agentInfo.name`    | `@agentclientprotocol/codex-acp` (title `Codex`)    |
| `agentInfo.version` | `1.1.9`                                             |
| `protocolVersion`   | `1`                                                 |
| `authMethods`       | `[{ id: "api-key", ... }, { id: "chat-gpt", ... }]` |

```json
"agentCapabilities": {
  "auth": { "logout": {} }, "providers": {}, "loadSession": true,
  "promptCapabilities": { "embeddedContext": true, "image": true },
  "sessionCapabilities": {
    "resume": {}, "list": {}, "close": {}, "delete": {}, "additionalDirectories": {}
  },
  "mcpCapabilities": { "acp": false, "http": true, "sse": false }
}
```

> **All three negotiate `protocolVersion: 1`, matching
> `SUPPORTED_PROTOCOL_VERSION` (`protocol.rs:10`). Not one of them rejected, warned
> about, or renegotiated over the `fs`/`terminal: false` declaration.**

### 1.3 Session-level check — does the agent do its own I/O?

Throwaway git repos under the scratch dir, each with a committed `README.md` +
`CONTRIBUTING.md`. Instruction: _"Read README.md in this directory and add a one-line
comment at the very top describing this project. Make the edit directly to the file."_

**Kimi — ✅ PASS**

```
[SESSION-ID] session_916bf000-e3cb-46c1-94e7-fb03f0caa8ba
[PROMPT-RESULT] {"stopReason":"end_turn"}
[INBOUND-METHODS-SEEN] []
[TOOL-CALL-COUNT] 83
```

```diff
$ git diff
--- a/README.md
+++ b/README.md
@@ -1,3 +1,4 @@
+<!-- Probe repo: a throwaway repo for ACP live verification. -->
 # Probe Repo
```

**Claude Code — ✅ PASS**

```
[SESSION-ID] b757feeb-eb3e-4f5f-8e74-1ffb713cf9de
[PROMPT-RESULT] {"stopReason":"end_turn","usage":{...,"totalTokens":88378}}
[INBOUND-METHODS-SEEN] []
[TOOL-CALL-COUNT] 10
```

```diff
$ git diff
--- a/README.md
+++ b/README.md
@@ -1,3 +1,5 @@
+<!-- Probe Repo: a disposable scratch repository used for ACP live verification runs. -->
+
 # Probe Repo
```

**Codex — ⚠️ NOT REACHED (auth, not capabilities)**

```
[SEND] {"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":".../repo-codex","mcpServers":[]}}
[RECV] {"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required"}}
[RESULT] codex: SESSION/NEW FAILED
```

Confirmed independently — this is a machine state issue, not a protocol one:

```
$ codex login status
Not logged in
```

`codex-cli 0.146.0` is installed, `~/.codex/auth.json` does not exist, and no
`OPENAI_API_KEY` / `CODEX_ACCESS_TOKEN` is present in the environment. **Codex's
`initialize` succeeded with `fs`/`terminal` declared false** — the refusal is at
`session/new` and its message is `Authentication required`, which is unrelated to
client capabilities. I did not attempt to authenticate it: doing so needs the founder's
own OpenAI/ChatGPT credentials.

### 1.4 V1 VERDICT

> **The design assumption in DESIGN-acp-agents.md §2/§7.1 HOLDS.**
>
> **No agent refused to operate with `fs.readTextFile: false`,
> `fs.writeTextFile: false`, `terminal: false`.** Both agents that reached a session
> performed **100% of their own file I/O**: across every prompt run in this document,
> the count of inbound `fs/*` and `terminal/*` requests from an agent was **zero**
> (`[INBOUND-METHODS-TOTAL] []` in every log except the permission tests, where the only
> inbound method was `session/request_permission` — which we _do_ support). The files
> on disk actually changed, proven by `git diff`.
>
> **`BLOCKERS.md` was NOT written for V1, because V1 did not fail.** The blocker in §2
> is a separate implementation defect, not a capability-model failure, and it does not
> invalidate the §7.1 decision.

**Caveat, stated plainly:** the V1 result is 2-of-3. Codex remains **unverified** at
session level. It is the one agent whose own sandbox policy could still interact badly
with a capability-less client, so this should be re-run once someone logs Codex in.

---

## 2. 🚨 BLOCKING DEFECT — `stopReason` vocabulary does not match the ACP spec

**Severity: high. Affects 100% of successful ACP runs. Not catchable by the current unit tests.**

### 2.1 What was observed

Every single successful turn from **both** working agents returned:

```json
{ "stopReason": "end_turn" }
```

Kimi: `[PROMPT-RESULT] {"stopReason":"end_turn"}` — 6 separate runs.
Claude Code: `[PROMPT-RESULT] {"stopReason":"end_turn","usage":{...}}` — 4 separate runs.

### 2.2 What OpenFlow expects

`src-tauri/src/acp/protocol.rs:129-134` parses only:

```rust
"completed"         => StopReason::Completed,
"max_steps_reached" => StopReason::MaxStepsReached,
"cancelled"         => StopReason::Cancelled,
"request_timeout"   => StopReason::RequestTimeout,
_                   => StopReason::Other,
```

### 2.3 The authoritative ACP enum

From the ACP SDK schema shipped inside the adapters
(`@agentclientprotocol/sdk/schema/schema.json` → `definitions.StopReason`):

| Const               | Description (verbatim from schema)                                          |
| ------------------- | --------------------------------------------------------------------------- |
| `end_turn`          | "The turn ended successfully."                                              |
| `max_tokens`        | "…reached the maximum number of tokens."                                    |
| `max_turn_requests` | "…reached the maximum number of allowed agent requests between user turns." |
| `refusal`           | "…the agent refused to continue."                                           |
| `cancelled`         | "…cancelled by the client via `session/cancel`."                            |

**Only `cancelled` overlaps.** `completed`, `max_steps_reached` and `request_timeout`
**do not exist anywhere in the ACP specification.**

### 2.4 Consequence

`end_turn` falls to `_ => StopReason::Other`, and `stop_reason_to_status()`
(`agent_run.rs:1034`) maps that to:

```rust
StopReason::Other => RunStatus::Failed {
    error: "The agent stopped for a reason this version doesn't recognise.".into(),
},
```

So **a perfectly successful agent run — file edited, task done — is surfaced to the user
as a red `Failed` run with a confusing error string.** `max_tokens`, `max_turn_requests`
and `refusal` collapse into the same generic failure, losing three distinct, actionable
states (notably `refusal`, which the spec says "should be reflected in the UI").

The unit tests do not catch this because they construct fixtures with the wrong
vocabulary — e.g. `stop("completed")` in `agent_run.rs` tests. They are self-consistent
and green against a vocabulary no agent emits.

### 2.5 Suggested fix (NOT applied — reporting only, per instruction)

In `protocol.rs`, align the parse with the spec and extend the enum:

```rust
"end_turn"          => StopReason::Completed,      // success
"cancelled"         => StopReason::Cancelled,
"max_tokens"        => StopReason::MaxTokens,      // new
"max_turn_requests" => StopReason::MaxTurnRequests,// new
"refusal"           => StopReason::Refusal,        // new
_                   => StopReason::Other,
```

Keeping `"completed"`/`"max_steps_reached"`/`"request_timeout"` as tolerated aliases is
harmless and preserves the existing tests. `stop_reason_to_status()` needs matching arms
with distinct, human-meaningful messages. `RunStatus` still gains no variant, so DESIGN §6
is respected.

**One good side effect already proven:** `cancelled` _is_ correct, so V5's stop path
(§5) maps to `RunStatus::Stopped` properly today.

---

## 3. V2 — permission prompt → Allow → file changed

### 3.1 First attempt did not trigger a prompt — and why that is not a bug

The initial V1 file-edit runs produced **zero** `session/request_permission` calls from
either agent. Investigated rather than assumed:

```
$ python3 -c "...json.load(open('~/.claude/settings.json'))..."
ALLOW: [..., "Read", "Edit", "Write", "Glob", "Grep", "Agent", "Bash(git *)", ...]
DENY:  ["Bash(rm *)", "Bash(rmdir *)", ...]
```

The founder's own `~/.claude/settings.json` pre-approves `Edit`/`Write`, and the Claude
adapter's session opened in mode `default` ("Standard behavior, prompts for dangerous
operations"). A pre-approved edit is therefore _correctly_ not escalated. A second probe
asking for `date -u` also auto-approved — the adapter classifies it read-only.

> **Consequence worth flagging to the founder:** OpenFlow's `Ask` policy can only gate
> what the agent actually _asks_ about. An agent whose own config pre-approves edits will
> perform them with **no OpenFlow prompt at all**. That is the agent's policy winning,
> not a gate failure — but the UI should not imply OpenFlow is gating everything.

### 3.2 Forcing a real permission request — both agents ✅

Using a shell command that is neither allow-listed nor deny-listed.

**Kimi — Allow path**

```
[PERMISSION-REQUEST] options=[
  {"optionId":"approve_once","name":"Approve once","kind":"allow_once"},
  {"optionId":"approve_always","name":"Approve for this session","kind":"allow_always"},
  {"optionId":"reject","name":"Reject","kind":"reject_once"}
] -> policy=allow picking={"optionId":"approve_once",...,"kind":"allow_once"}
[PROMPT-RESULT] {"stopReason":"end_turn"}
[INBOUND-METHODS-SEEN] ["session/request_permission"]

$ cat /tmp/openflow-acp-kimi-perm.txt
kimi-perm-ok
```

**Claude Code — Allow path**

```
[PERMISSION-REQUEST] options=[
  {"kind":"reject_once","name":"Deny","optionId":"reject"},
  {"kind":"allow_once","name":"Allow Once","optionId":"allow"},
  {"kind":"allow_always","name":"Always Allow","optionId":"allow_always","_meta":{...}}
] -> policy=allow picking={"kind":"allow_once","name":"Allow Once","optionId":"allow"}
[PROMPT-RESULT] {"stopReason":"end_turn",...}
[INBOUND-METHODS-SEEN] ["session/request_permission"]

$ cat /tmp/openflow-acp-perm-test.txt
perm-ok
```

### 3.3 An important detail that OpenFlow gets right

The two agents present their options in **different order** and with **different
`optionId` strings**:

| Agent       | Option ids                                   | Order            |
| ----------- | -------------------------------------------- | ---------------- |
| Kimi        | `approve_once` / `approve_always` / `reject` | allow first      |
| Claude Code | `allow` / `allow_always` / `reject`          | **reject first** |

`acp/permission.rs:69` selects by **`kind`** (`allow_once`, falling back to
`allow_always`), never by index or by a hard-coded id:

```rust
.find(|o| o.kind == once)
.or_else(|| options.iter().find(|o| o.kind == always))
```

**This is verified correct against both live shapes.** An index-based implementation
would have silently clicked _Deny_ on Claude Code. Good call by whoever wrote it.

### 3.4 V2 status

- Protocol round trip (request → OpenFlow's exact option selection → answer → agent acts
  → file on disk changes): ✅ **verified, both agents**.
- Prompt rendered in the run panel, tool-call rows, run present in panel **and** File
  sink: ❌ **NOT VERIFIED — needs a human at the GUI** (see §6).

---

## 4. V3 — session reuse and multi-turn context

Turn 1: _"Add a one-line comment to the top of README.md describing this project."_
Turn 2: _"Now do the same for CONTRIBUTING.md."_ — deliberately anaphoric, so it only
works if the agent retained turn 1.

**Claude Code ✅**

```
[SESSION-ID]              26da8e95-8d3d-44a4-9736-50cf133bb0e1
[PROMPT-RESULT]           {"stopReason":"end_turn",...}
[PROMPT2-RESULT]          {"stopReason":"end_turn",...}
[TURN2-SESSION-ID-REUSED] 26da8e95-8d3d-44a4-9736-50cf133bb0e1
[TOOL-CALL-COUNT-TOTAL]   26
```

```diff
$ git status --short
 M CONTRIBUTING.md
 M README.md
+<!-- Contribution guidelines for Probe Repo, a disposable scratch repository used for ACP live verification. -->
+<!-- Probe Repo: a disposable scratch repository used for ACP live verification. -->
```

**Kimi ✅**

```
[SESSION-ID]              session_afcd453a-c378-4b4f-b87a-ebea3b6a58db
[TURN2-SESSION-ID-REUSED] session_afcd453a-c378-4b4f-b87a-ebea3b6a58db
[TOOL-CALL-COUNT-TOTAL]   138
```

```diff
 M CONTRIBUTING.md
 M README.md
+<!-- A throwaway probe repository used for ACP live verification. -->  (both files)
```

Both agents resolved "the same" correctly against turn 1 on a **single reused
`sessionId`** — multi-turn context over one long-lived session is real. ✅ **VERIFIED.**

"The two runs group into one thread" is a **UI** assertion — not verified (§6).

---

## 5. V4 (deny) and V5 (stop mid-turn)

### 5.1 V4 — deny ✅

Same forced-permission command, probe answering with the `reject_once` option:

```
[PERMISSION-REQUEST] ... -> policy=deny picking={"optionId":"reject",...,"kind":"reject_once"}
[PROMPT-RESULT] {"stopReason":"end_turn"}
[INBOUND-METHODS-SEEN] ["session/request_permission"]

$ ls -la /tmp/openflow-acp-kimi-deny.txt
ls: /tmp/openflow-acp-kimi-deny.txt: No such file or directory
```

The agent's own closing message:

> "The command was not executed — the Bash tool approval was rejected, so I did not run it."

**The denied side effect did not happen, and the agent reported it could not proceed.**
✅ **VERIFIED.**

> Honest note on the brief's "`git status` is clean": in that repo `git status` showed
> ` M README.md`, which is **left over from the earlier V1 edit in the same repo**, not
> from the denied operation. The denied operation targeted `/tmp/openflow-acp-kimi-deny.txt`,
> whose non-existence is the actual proof. Recorded this way rather than re-running in a
> pristine repo to avoid overstating.

### 5.2 V5 — stop mid-turn ✅ (protocol level)

Long-running prompt ("write a detailed 2000-word architecture report"), with a
`session/cancel` notification sent 6s in, then a **third** prompt on the same session to
prove it stayed warm:

```
[CANCELLING] after 6000ms — sending session/cancel notification
[PROMPT-RESULT]           {"stopReason":"cancelled"}
[PROMPT2-RESULT]          {"stopReason":"end_turn"}
[TURN2-SESSION-ID-REUSED] session_8bb75e23-194a-4d6e-9151-758809e0fe0d
```

- `stopReason: "cancelled"` → `StopReason::Cancelled` → `RunStatus::Stopped`. ✅ Correct
  (and the one stop reason OpenFlow parses correctly today — see §2).
- **Session still warm and usable:** the follow-up turn succeeded on the _same_
  `sessionId`, and correctly recalled the cancelled turn's instruction:
  > "The first thing you asked me to do in this session was: **You asked me to carefully
  > analyze this directory and write a detailed 2000-word architecture report into ARCH.md.**"
- **No orphan process:**
  ```
  $ pgrep -f 'kimi-code/bin/kimi' | wc -l
  0
  $ pgrep -f 'claude-agent-acp|codex-acp' | wc -l
  0
  ```

The `Stopped` status label itself is a UI assertion — not verified (§6).

---

## 6. NOT VERIFIED — everything that needs a human at the keyboard

**These were not tested. No result is claimed for them.** This agent has no GUI access,
no microphone, and no way to click. Recording them as unverified rather than inferring
them from the protocol results.

| Item                      | Why not verified                    | Exact steps for a human                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------------------------- | ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **V2 GUI**                | No way to click Allow               | Settings → Agents → add a CLI agent, protocol **ACP**, command `kimi`, ACP args `acp`, policy **Ask**. Bind a hotkey. Open a git repo as project path. Speak _"add a one-line comment to the top of README describing this project"_. Confirm: prompt appears inline in the run panel; click **Allow**; `git diff` shows the change; tool-call rows render; the run is in **both** the panel and the File sink.                                                                                                   |
| **V3 GUI**                | Same                                | Immediately speak _"now do the same for CONTRIBUTING.md"_. Confirm the run panel groups both runs into **one thread** and shows the same session id.                                                                                                                                                                                                                                                                                                                                                              |
| **V4 GUI**                | Same                                | Repeat V2 but click **Deny**. Confirm the panel shows the agent reporting it could not proceed, and `git status` is clean.                                                                                                                                                                                                                                                                                                                                                                                        |
| **V5 GUI**                | Same                                | Start a long instruction, click **Stop** mid-turn. Confirm status renders **`Stopped`**, `ps` shows no orphan, and the next instruction reuses the warm session.                                                                                                                                                                                                                                                                                                                                                  |
| **V5b orphan-at-quit**    | Requires quitting the app mid-spawn | `npm cache clean --force`; trigger an ACP agent using `npx -y @agentclientprotocol/codex-acp` so the spawn sits in its cold-install window; **Cmd+Q OpenFlow while it is still starting**. Assert `ps aux \| grep -E 'npx\|claude-agent-acp\|codex-acp\|kimi'` shows **no** survivor and that quit took seconds (not blocked on the install). Repeat with the agent mid-_handshake_. Record both `ps` outputs. **This is the only check for Task 7's `pending_children` registry — it has never been exercised.** |
| **V6 idle timeout**       | Requires the app                    | Set ACP idle timeout to 60s, run one instruction, wait >60s. Assert `pgrep -f 'kimi\|acp'` shows the child gone, then give another instruction and confirm it respawns transparently.                                                                                                                                                                                                                                                                                                                             |
| **V7 in-app regressions** | Requires the app                    | Run a **Raw-mode** CLI agent (identical output + file sink as before), a **prompt agent** (persona-LLM result still injected), and **plain dictation** (untouched).                                                                                                                                                                                                                                                                                                                                               |

---

## 7. V7 regression half — what _was_ provable, and gates

### 7.1 Provable without the GUI ✅

**The core dictation path is not touched by this branch:**

```
$ git diff --name-only main...HEAD | grep -E "transcription|audio_toolkit|shortcut|signal_handle|overlay"
NONE — core dictation path untouched
```

The diff is 28 files / +7368 / −57, and is **additive**: new `acp/` module, new
`managers/acp_session.rs`, new `commands/acp_agents.rs`, new run-panel components. The
only pre-existing backend files modified are `settings.rs` (+179, `#[serde(default)]`
fields), `agent_run.rs` (+2149, new ACP driver alongside the raw one), `lib.rs` (+39,
registration) and `actions.rs` (+4).

**Settings file is byte-identical over an app lifetime with zero ACP agents configured:**

```
$ shasum -a 256 ~/Library/Application\ Support/knotie.ai.openflow/settings_store.json   # T0
a88738921761b5a98a2b805840366d01c6b4e27896b2c5896de6915378cbfd16
$ shasum -a 256 ~/Library/Application\ Support/knotie.ai.openflow/settings_store.json   # T1, ~15 min later
a88738921761b5a98a2b805840366d01c6b4e27896b2c5896de6915378cbfd16
```

Identical. The store contains **no agent keys at all** (`[k for k in settings if 'agent'
in k.lower()]` → `[]`), i.e. the feature is genuinely unconfigured, and **no ACP child
process exists**:

```
$ pgrep -f 'claude-agent-acp|codex-acp|kimi acp' | wc -l
0
```

> ⚠️ **Honest limitation:** the running app during this window was the **released
> `/Applications/OpenFlow.app` (v0.15.7, pid 4917)**, which does _not_ contain the ACP
> code. So this proves the settings store is stable and unconfigured, but it is **not**
> the branch-build start/stop cycle the brief asks for. Producing that requires quitting
> the founder's running app and launching the dev build against the same real settings
> store — deliberately not done. **The byte-identical-across-a-branch-build-cycle proof
> is still outstanding.**

**The full app compiles with all the new code wired in:**

```
$ export ORT_LIB_LOCATION=$(brew --prefix onnxruntime)/lib ORT_PREFER_DYNAMIC_LINK=1
$ cargo build --manifest-path src-tauri/Cargo.toml --bins
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.78s
$ ls -la src-tauri/target/debug/openflow
-rwxr-xr-x@ 1 avijitsarkar staff 142922872 Aug  3 16:35 src-tauri/target/debug/openflow
```

Exit code 0. `bun run tauri dev` was **not** launched — it would have collided with the
founder's running instance via `tauri_plugin_single_instance`. Compilation of the real
binary is the safe equivalent and is what is claimed here; **no claim is made that the
app was launched or driven.**

### 7.2 Gates — all pass ✅

```
$ cargo test --lib
test result: ok. 465 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
```

```
$ cargo clippy --all-targets
warning: `openflow` (lib) generated 34 warnings
warning: `openflow` (lib test) generated 39 warnings (34 duplicates)
warning: `openflow` (example "diarize_ground_truth") generated 1 warning
```

**34 / 39 / 1 — exactly the documented baseline. Zero new findings.**

```
$ cargo fmt -- --check
FMT_OK

$ bun run build
✓ built in 8.38s

$ bun run lint
✖ 1 problem (0 errors, 1 warning)
  src/devAutomation.ts  12:9  warning  Unused eslint-disable directive
```

0 errors. The single warning is **pre-existing** in `devAutomation.ts`, untouched by this
branch.

```
$ bun run format:check
```

Passes for all tracked files. Before `bun run format` it flagged
`.superpowers/sdd/PLAN/progress.md` and `.superpowers/sdd/PLAN/task-12-brief.md` — both
**gitignored local planning scaffolding** (`git check-ignore` →
`.superpowers/sdd/.gitignore:1:*`), never committed, and unformatted before this task
began. `bun run format` was run as required by `AGENTS.md`.

---

## 8. Reproducing this

The probe is a single self-contained Node script; no OpenFlow build is required for
V1–V5's protocol half.

```bash
SCRATCH=/private/tmp/claude-501/-Users-avijitsarkar-personal-projects-fable-5-projects/\
4b5b1dfd-7b5d-46d2-bce0-7faffb8c481d/scratchpad

# V1 initialize only
node acp-probe.mjs --name kimi   --cmd kimi --args 'acp' --phase init
node acp-probe.mjs --name claude --cmd npx  --args '-y @agentclientprotocol/claude-agent-acp' --phase init
node acp-probe.mjs --name codex  --cmd npx  --args '-y @agentclientprotocol/codex-acp'        --phase init

# V1 session-level file edit
node acp-probe.mjs --name kimi --cmd kimi --args 'acp' --cwd "$SCRATCH/repo-kimi" \
  --phase prompt --policy allow \
  --prompt 'Read README.md and add a one-line comment at the very top describing this project.'

# V3 multi-turn on one session
node acp-probe.mjs ... --prompt 'Add a one-line comment to the top of README.md ...' \
                       --prompt2 'Now do the same for CONTRIBUTING.md.'

# V4 deny
node acp-probe.mjs ... --policy deny --prompt '<a command needing approval>'

# V5 cancel mid-turn, then prove the session is still warm
node acp-probe.mjs ... --cancel-after 6000 --prompt '<long task>' --prompt2 'What did I first ask you?'
```

Full verbatim transcripts (every frame sent and received) are in `$SCRATCH/logs/`:
`v1-kimi-init.log`, `v1-claude-init.log`, `v1-codex-init.log`, `v1-kimi-prompt.log`,
`v1-claude-prompt.log`, `v1-codex-prompt.log`, `v2-claude-perm-allow.log`,
`v2-claude-perm2.log`, `v2-kimi-perm.log`, `v3-claude.log`, `v3-kimi.log`,
`v4-kimi-deny.log`, `v5-kimi-cancel.log`.

> These logs live in the session scratch dir, not in the repo — they contain absolute
> paths and agent chatter. The material findings are quoted verbatim above.

---

## 9. Recommendations

1. **Fix the `stopReason` vocabulary before merge (§2).** Without it, every successful
   ACP run reads as a failure. This is the one finding that should block.
2. **Re-run V1 §1.3 against Codex** once it is authenticated (`codex login`). 2-of-3 is
   not 3-of-3, and Codex has the strictest sandbox of the three.
3. **Have a human complete §6** — especially **V5b**, which is the _only_ check covering
   Task 7's `pending_children` registry. That code path has never executed.
4. **Consider surfacing the "agent pre-approved it" case in the UI (§3.1).** With `Ask`
   policy, a user may reasonably believe OpenFlow gates every action; it only gates what
   the agent chooses to ask about. **ADDRESSED (2026-08-04).** By the time this was
   picked up, §12.1's Codex pass had made the case concrete rather than hypothetical —
   Codex demonstrably edits inside its workspace root with **zero**
   `session/request_permission` calls, a second agent doing exactly what §3.1 warned
   about. `CliAgentCard.tsx`'s ACP permission `SettingContainer` now renders an
   unconditional `Alert` above the policy `Dropdown` (visible under every policy value,
   not only when something has already gone unrequested), carrying a new i18n key,
   `settings.agents.acp.permission.scopeNotice`: "OpenFlow can only prompt for actions
   an agent chooses to ask about — it does not gate everything an agent does. Some
   agents edit files inside the project folder without asking, no matter which policy
   is selected here. Git is your safety net here, exactly as it is for a local agent:
   commit before a run so you can see, and undo, what changed." Modelled on
   `SharingSettings.tsx`'s non-dismissible not-encrypted banner (same problem shape: a
   permission-adjacent setting name implies a stronger guarantee than the protocol
   delivers) — same register (plain negation, no reassurance words), same closing move
   (name the real safety net: git, echoing `openclaw-hermes-cli-agents/RESULTS.md`'s
   "git remains the safety net" for one-shot mode's own bypassed approval gate). No
   vendor named — this is ACP's shape, not a Codex defect. No permission _behaviour_
   changed; this is disclosure only. Gates: `bun run build` ✓, `bun run lint` 0 errors
   (1 pre-existing unrelated warning), `bun run format` clean. No Rust touched, so the
   488-test baseline is unaffected.

---

## 10. The `stopReason` fix and its live re-verification (2026-08-03, later)

§2 diagnosed the defect. This section records the fix and the evidence that it works
against real agents. **Origin: the design doc.** `DESIGN-acp-agents.md` §3 listed
StopReason values taken from a prose summary rather than the schema, and they were wrong;
every downstream task faithfully implemented them and every fixture inherited them. That
is precisely why 465 green tests proved nothing here.

### 10.1 What changed

**Source of truth:** `@agentclientprotocol/sdk/schema/schema.json` → `definitions.StopReason`.

`src-tauri/src/acp/protocol.rs` — `StopReason`'s variants replaced with ACP's real ones.
The hand-written `Deserialize` and its `_ => Other` fallback are **kept** (that fallback is
what made the bug a wrong label instead of a crash):

| Wire value (ACP)    | Variant           | Was                                     |
| ------------------- | ----------------- | --------------------------------------- |
| `end_turn`          | `EndTurn`         | _unrecognised_ → `Other`                |
| `max_tokens`        | `MaxTokens`       | _unrecognised_ → `Other`                |
| `max_turn_requests` | `MaxTurnRequests` | _unrecognised_ → `Other`                |
| `refusal`           | `Refusal`         | _unrecognised_ → `Other`                |
| `cancelled`         | `Cancelled`       | `Cancelled` (the only one ever correct) |
| anything else       | `Other`           | `Other`                                 |

The fictional `completed` / `max_steps_reached` / `request_timeout` were **not** kept as
aliases. Aliasing them would let a stale fixture keep passing while testing nothing real.

`src-tauri/src/managers/agent_run.rs`:

- `stop_reason_to_status` — `EndTurn` → `Finished { code: 0 }`; `Cancelled` → `Stopped`;
  `MaxTokens` / `MaxTurnRequests` → `Failed` with distinct limit wording; `Refusal` →
  `Failed` saying the agent **declined** (a decision, not a crash); `Other` → unchanged.
- `stop_reason_label` — now emits ACP's spellings, which are the frontend's lookup keys.

`src/components/settings/agent-runs/RunEventList.tsx` — `TURN_END_KEYS` re-keyed to
`end_turn` / `cancelled` / `max_tokens` / `max_turn_requests` / `refusal` / `failed`. The
reviewer's note that it handled "all 5 wire values plus a fallback" was true, but they were
the wrong 5, so the success row silently fell back to its generic label.

`src/i18n/locales/en/translation.json` — `settings.agentRuns.acp.turnEnd`: dropped
`maxSteps`/`timeout`, added `maxTokens` ("Stopped — ran out of tokens"),
`maxTurnRequests` ("Stopped — request limit reached"), `refusal` ("The agent declined the
request"). Only `en` carries the ACP block; other locales have no `turnEnd` node at all
(pre-existing translation gap, unrelated to this fix).

Whole-tree sweep for the stale strings in an ACP context (Rust, TS, i18n) returns only
intentional hits: the history note in `protocol.rs`'s doc comment, the same note in
`RunEventList.tsx`, and the deliberate `the_fictional_pre_fix_stop_reasons_are_not_recognised`
regression test. `a2a.rs` and `history.rs` also contain the string `"completed"`, but those
are A2A task states and transcript text — a different vocabulary, correctly left alone.

### 10.2 Live re-verification — the acceptance criterion

Fresh probe runs against both working agents, in new throwaway git repos:

**Kimi (Kimi Code CLI 0.31.0)**

```
[PROMPT-RESULT] {"stopReason":"end_turn"}
[INBOUND-METHODS-SEEN] []
$ git diff --stat
 README.md | 2 ++
```

**Claude Code (`@agentclientprotocol/claude-agent-acp` 0.64.2)**

```
[PROMPT-RESULT] {"stopReason":"end_turn","usage":{"inputTokens":8,"outputTokens":691,"cachedReadTokens":106876,"cachedWriteTokens":12224,"totalTokens":119799}}
[INBOUND-METHODS-SEEN] []
$ git diff --stat
 README.md | 2 ++
```

Those exact payloads were then fed through **OpenFlow's own `PromptResult` deserializer
and its own `stop_reason_to_status`** — not a re-implementation — via a throwaway
`cargo run --example` harness (deleted afterwards; `lib.rs` restored byte-identical):

```
wire JSON : {"stopReason":"end_turn"}
  StopReason : EndTurn
  label      : "end_turn"
  RunStatus  : Finished { code: 0 }

wire JSON : {"stopReason":"end_turn","usage":{"inputTokens":8,"outputTokens":691,"cachedReadTokens":106876,"cachedWriteTokens":12224,"totalTokens":119799}}
  StopReason : EndTurn
  label      : "end_turn"
  RunStatus  : Finished { code: 0 }

wire JSON : {"stopReason":"cancelled"}
  StopReason : Cancelled
  label      : "cancelled"
  RunStatus  : Stopped

wire JSON : {"stopReason":"refusal"}
  StopReason : Refusal
  label      : "refusal"
  RunStatus  : Failed { error: "The agent declined to carry out this request." }

wire JSON : {"stopReason":"max_tokens"}
  StopReason : MaxTokens
  label      : "max_tokens"
  RunStatus  : Failed { error: "The agent ran out of tokens before finishing." }

wire JSON : {"stopReason":"max_turn_requests"}
  StopReason : MaxTurnRequests
  label      : "max_turn_requests"
  RunStatus  : Failed { error: "The agent hit its request limit for this turn before finishing." }

wire JSON : {"stopReason":"completed"}
  StopReason : Other
  label      : "other"
  RunStatus  : Failed { error: "The agent stopped for a reason this version doesn't recognise." }
```

**Verdict:** a real successful turn from **both** agents now yields
**`Finished { code: 0 }`**, where before it yielded
`Failed { error: "The agent stopped for a reason this version doesn't recognise." }`.
The last line is the design doc's invented `completed`, now correctly _unrecognised_ —
the old bug's exact failure mode, preserved as proof that the fictional vocabulary is
genuinely gone rather than merely renamed.

### 10.3 Regression tests, break-and-revert checked

Four new tests (465 → **469**):

| Test                                                                        | Guards                                                                                                         |
| --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `protocol::stop_reason_parses_every_spec_value_and_unknown_is_not_fatal`    | all 5 ACP values + unknown → `Other`                                                                           |
| `protocol::the_fictional_pre_fix_stop_reasons_are_not_recognised`           | the 3 invented names must parse as `Other`, never be aliased                                                   |
| `protocol::a_real_agents_success_value_is_the_success_variant`              | `end_turn` ≠ `Other`                                                                                           |
| `agent_run::a_real_agents_successful_turn_finishes_rather_than_fails`       | **the defect itself**: deserializes the literal `{"stopReason":"end_turn"}` and asserts `Finished { code: 0 }` |
| `agent_run::driving_a_turn_that_ends_with_the_real_wire_value_completes_it` | the driver path, not just the pure mapper                                                                      |

**Break-and-revert:** the `"end_turn" => StopReason::EndTurn` arm was commented out to
simulate the pre-fix bug. Three tests failed with the exact original symptom:

```
test acp::protocol::tests::a_real_agents_success_value_is_the_success_variant ... FAILED
test acp::protocol::tests::stop_reason_parses_every_spec_value_and_unknown_is_not_fatal ... FAILED
test managers::agent_run::tests::a_real_agents_successful_turn_finishes_rather_than_fails ... FAILED

assertion `left == right` failed: ACP wire value "end_turn" must parse to EndTurn
  left: Other
 right: EndTurn
```

Arm restored → `469 passed; 0 failed`.

### 10.4 Gates after the fix

| Gate                         | Before fix  | After fix                                                    |
| ---------------------------- | ----------- | ------------------------------------------------------------ |
| `cargo test --lib`           | 465 passed  | ✅ **469 passed; 0 failed**                                  |
| `cargo clippy --all-targets` | 34 / 39 / 1 | ✅ **34 / 39 / 1 — still exactly baseline, zero new**        |
| `cargo fmt -- --check`       | clean       | ✅ clean                                                     |
| `bun run build`              | ✓           | ✅ ✓ built in 7.92s                                          |
| `bun run lint`               | 0 errors    | ✅ 0 errors (same 1 pre-existing `devAutomation.ts` warning) |
| `bun run format:check`       | clean       | ✅ clean                                                     |

### 10.5 What this does **not** change

- **V1's verdict is unaffected** — the capability assumption held before the fix and
  holds after it. `fs`/`terminal: false` was never the problem.
- **Codex is still 2-of-3.** `codex login` is a user action, correctly out of scope. Its
  session-level check remains unverified and should be re-run once authenticated.
- **Everything in §6 is still unverified** — V5b (orphan-at-quit, still never executed),
  V6, and the GUI halves. The fix makes the success path _correct_; it does not make the
  GUI path _tested_.

---

## 11. The frame capture and what replaying it found (2026-08-03, final fix wave)

§10 fixed one bug that only a real agent could reveal. This section records the
**structural** answer to "how do we stop the third one", and the second bug it caught
immediately.

### 11.1 The gap that let §2 happen twice

Task 12's live probe (§8) was a Node script that _logged_ frames. Every payload it
captured was read by a human and by `jq` — **not one of them had ever been through
OpenFlow's own deserializer**, except the two `{"stopReason":…}` objects in §10.2. Every
other fixture in the 469-test suite was hand-written from `DESIGN-acp-agents.md`, which
was itself written from prose. A suite built that way cannot detect a wire assumption
that is wrong; it can only confirm it is self-consistent.

### 11.2 What was captured

`src-tauri/src/acp/fixtures/real-agent-frames.jsonl` — **27 unedited JSON-RPC frames**,
one per line, exactly as the agents wrote them to stdout:

| Source                    | Frames                                                    |
| ------------------------- | --------------------------------------------------------- |
| `claude-agent-acp` 0.64.2 | 17 (incl. `plan`, `config_option_update`, `usage_update`) |
| `kimi` 0.31.0             | 7                                                         |
| permission requests       | 2 (one from each agent)                                   |

Deduplicated by (agent, method, update kind, **exact key set**), so every line is a
distinct SHAPE the crate must survive, not a repetition. Harvested from the §8 probe
logs plus one fresh 2026-08-03 run against Claude Code (`--prompt` forcing a todo/plan
plus a shell command), which is where the `plan` frames came from.

`src-tauri/src/acp/replay_tests.rs` feeds every line through `SessionNotification` /
`RequestPermissionParams` / `map_session_update` / `render_line`. It is the only test in
the suite whose input we did not write.

### 11.3 🚨 Second blocking defect, found by the replay — `ToolCallUpdate` optionality

**The schema** (`@agentclientprotocol/sdk@1.3.0`, `$defs.ToolCallUpdate`):

```
required = ["toolCallId"]
props    = toolCallId, kind, status, title, name, content, locations, rawInput, rawOutput, _meta
```

**Only `toolCallId` is required.** OpenFlow modelled `status` as an always-present
`String` and did not model `title`/`kind`/`locations` at all.

**What Claude Code actually sends** (verbatim, from the capture):

```json
{
  "toolCallId": "toolu_01V54kbxK7U3Fgz17XyuHBk3",
  "sessionUpdate": "tool_call_update",
  "title": "Read README.md",
  "kind": "read",
  "locations": [{ "path": "…/repo-fix/README.md" }]
}
```

No `status`. Its own source comment says a refining `tool_call_update` "carries neither".
Three of the 19 captured `tool_call_update` shapes from Claude Code have no `status`;
one has nothing but `toolCallId` and `rawOutput`.

**Consequences, all live before the fix:**

1. `status` deserialized to `""`, and `runEventRows.ts` assigned it unconditionally —
   **overwriting the tool call's real `pending`/`completed`**.
2. `render_line` wrote a bare `"  ✓ "` into `AgentRunInfo.output` **and the File sink** —
   the permanent record — claiming a status the agent never reported.
3. The refinement's `title`/`kind`/`locations` were discarded. **Delivering the resolved
   file path is the entire purpose of a refinement**, so the path never reached the panel.

Replayed through the panel's own reducer, before and after (real frames, `bun`):

```
BEFORE:  status=""          title="Read File"       locations=[]
AFTER:   status="pending"   title="Read README.md"  locations=["…/repo-fix/README.md"]
```

The replay test fails **by assertion** on the captured Claude Code frame under either
form of the bug (`status` defaulted at the serde layer, or absence collapsed at the
mapping layer) — verified by break-and-revert both ways.

### 11.4 Also learned from the real frames

- **Kimi sends `session/request_permission` with no `kind` at all** — its `toolCall` is
  `{toolCallId, title, content}`. That made an "Always allow" record `allowed_kinds:
[""]`, auto-approving every future kind-less request from that agent. Now non-persistable.
- **`session/request_permission`'s `toolCall` is schema-typed as a `ToolCallUpdate`**, so
  even its `title` is optional. Previously a `#[serde(default)] String`, which rejects an
  explicit `null` outright — one null would have cost the whole frame.
- **Claude Code lists `reject` FIRST** in its options array (already noted in §5.1, still
  true) and offers `optionId: "allow_always"` carrying a rich `_meta.permission` policy
  block we correctly ignore.
- **Kimi's option ids are `approve_once`/`approve_always`/`reject`** while its `kind`s are
  ACP-standard. Selection by `kind` (not id, not index) remains the right call.
- Unmodelled `session/update` variants seen in the wild: `available_commands_update`,
  `usage_update`, `session_info_update`, `config_option_update`. All drop silently.
- **`session/close` is capability-gated** (`sessionCapabilities.close`) and **Kimi does
  not advertise it**, so teardown always `-32601`s through to the SIGTERM ladder. Logged,
  deliberately not fixed — the ladder is the guarantee.
- **Codex's `authMethods`** are `api-key` and `chat-gpt`, and `session/new` returns
  `-32000 "Authentication required"`. Now mapped to "run `codex login`".

### 11.5 The `may_end` defect (no live agent needed, but worth recording)

`run_acp_turn`'s guard used to drop when the function returned, and `end_if_current` then
did a `try_lock` to decide whether the session was safe to drop. With a follow-up parked
on `begin_turn()`, tokio assigns the semaphore permit **at that release**, so the
`try_lock` fails and the drop becomes a no-op. Measured **200/200** — not a race, a
certainty. The turn guard is now returned to the caller and moved into
`end_if_current_owned`, making the invariant structural. Reproduced 200/200 before and
200/200 clean after, both asserted in the same permanent test.

### 11.6 Gates after the fix wave

| Gate                         | Baseline    | After                                                        |
| ---------------------------- | ----------- | ------------------------------------------------------------ |
| `cargo test --lib`           | 469 passed  | ✅ **481 passed; 0 failed**                                  |
| `cargo clippy --all-targets` | 34 / 39 / 1 | ✅ **34 / 39 / 1 — baseline, zero new**                      |
| `cargo fmt -- --check`       | clean       | ✅ clean                                                     |
| `bun run build`              | ✓           | ✅ ✓                                                         |
| `bun run lint`               | 0 errors    | ✅ 0 errors (same 1 pre-existing `devAutomation.ts` warning) |
| `bun run format:check`       | clean       | ✅ clean (`.superpowers` now in `.prettierignore`)           |

### 11.7 Still unverified

Unchanged from §6 and §10.5: **V5b orphan-at-quit has still never executed**, V6 idle
timeout, the GUI halves of V2–V5, and the in-app V7 regression clicks all need a human at
the keyboard. Codex remains 2-of-3 pending `codex login`. — **Superseded 2026-08-04:
Codex is now 3-of-3 at the protocol level; see §12.**

---

## 12. Codex: the third-of-three live pass, and the two divergences it found (2026-08-04)

**Date:** 2026-08-04 · **Branch:** `feat/acp-agents` @ `f86fee9` (pre-fix) ·
**Agent:** `codex-acp 1.1.9` via `npx -y @agentclientprotocol/codex-acp`,
`codex-cli 0.146.0`, authenticated (`~/.codex/auth.json` present — §9's
recommendation 2 unblocked).

§9 recommendation 2 said "2-of-3 is not 3-of-3, and Codex has the strictest sandbox of
the three". It was right, and not for the sandbox reason.

### 12.1 What was driven

The §8 probe, unchanged, against a throwaway git repo in the session scratch dir (never
the OpenFlow repo). Two live sessions, both `initialize` → `session/new` →
`session/prompt`, both ending `"stopReason":"end_turn"`.

| Check                                                                  | Result                                                                                                  |
| ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| **V1** — `fs`/`terminal` declared **false**, does Codex still operate? | ✅ **YES.** Never called back for I/O; `refuse_inbound`'s `-32601` path was never exercised.            |
| `initialize`                                                           | ✅ 1760 ms. `agentInfo` = `{name: "@agentclientprotocol/codex-acp", title: "Codex", version: "1.1.9"}`. |
| `session/new`                                                          | ✅ `019fcc92-a80b-7d21-8700-72b28cc9ff05` — no `-32000 "Authentication required"` (contrast §11.4).     |
| **File edit actually landed**                                          | ✅ `git diff` shows `+verified-by-codex` in `NOTES.md`; Codex then ran `wc -l` and reported `2`.        |
| **V2** — permission prompt → Allow → side effect                       | ✅ Forced by asking for a write to `$HOME` (outside the sandbox). Real request, allowed, file written.  |
| Terminal `stopReason`                                                  | ✅ `end_turn` on both turns → `RunStatus::Finished { code: 0 }`.                                        |

Codex does **not** ask permission for edits inside its workspace root — the edit
`tool_call` went straight to `status: "in_progress"` with no `session/request_permission`
at all. This is Codex's own sandbox policy, not an OpenFlow decision, and it is worth
knowing: under `Ask` policy a user may believe OpenFlow gates every action; it only gates
what the agent chooses to ask about (§9 recommendation 4, now with a second agent
demonstrating it).

**12 new frame shapes** were added to `acp/fixtures/real-agent-frames.jsonl` (lines
30–41), deduplicated by (agent, method, update kind, exact key set) exactly as the
existing waves were, plus the `session/prompt` RESPONSE frames from all three agents
(lines 28–29 and one inside the Codex block). Every line is byte-for-byte what the agent
wrote to stdout. Nothing was hand-written, reformatted or redacted.

### 12.2 🚨 Divergence 1 — a Codex edit names no file

Codex announces a file edit with **no `locations` key at all**:

```json
{
  "sessionUpdate": "tool_call",
  "toolCallId": "exec-800532ce-844a-4dfc-ab3c-8e5bb2aaa028",
  "title": "Editing files",
  "kind": "edit",
  "status": "in_progress",
  "content": [
    {
      "type": "diff",
      "oldText": "notes\n",
      "newText": "notes\nverified-by-codex\n",
      "path": "…/repo-codex-live/NOTES.md",
      "_meta": { "kind": "update" }
    }
  ]
}
```

The absolute path is stated **only** inside the `diff` content block — where the schema
(`$defs.Diff`) makes `path` **required** and documents it as _"the absolute file path
being modified"_. `claude-agent-acp` sends both `locations` and `content`, which is
precisely why two vendors could not reveal this.

`SessionUpdate::ToolCall` did not model `content` at all, so `RunEvent::ToolCall.locations`
came out `[]` and **every Codex file edit reached the run panel and the permanent
File-sink record as a bare `▸ Editing files`, naming no file.** Same class of loss as
MF1 (§11.3): the agent delivered the resolved path and we discarded it.

**Fix:** `protocol::ToolCallContentWire` + `protocol::stated_paths`. `locations` wins
whenever the agent sent any; `diff` paths are the fallback, in wire order, deduplicated;
an agent that named nothing anywhere still gets an empty list. Modelled on the **CREATE**
variant only — on `ToolCallUpdate`, absence is a three-way that must survive verbatim, so
nothing is derived there and MF1's invariant is untouched.

**Break-and-revert:** reverting `map_session_update` to `locations.iter().map(…)` fails
`a_real_codex_edit_states_its_file_only_in_a_diff_block_and_still_names_it` with
`left: [] right: ["…/repo-codex-live/NOTES.md"]`, on line 33 of the capture.

### 12.3 🚨 Divergence 2 — a Codex permission request has no title

```json
{
  "sessionId": "019fcc93-d227-75c2-bd82-cf0e061cf162",
  "toolCall": { "toolCallId": "exec-fa8e68f8-…", "kind": "execute", "status": "pending",
                "rawInput": { "command": "…", "cwd": "…" } },
  "options": [ … ]
}
```

**No `title`.** Legal: `RequestPermissionRequest.toolCall` is schema-typed as a
`ToolCallUpdate`, where only `toolCallId` is required (§11.4 already noted this for
`title` specifically — Codex is the agent that actually does it). Kimi omits `kind`,
Codex omits `title`; between them almost nothing about that object is guaranteed.

Consequences, both live before the fix:

1. `render_line` wrote a bare `"? "` into `AgentRunInfo.output` and the File sink — the
   permanent record of a security decision, saying only that _something_ was authorised.
2. `PermissionPrompt` rendered an **empty headline above the Allow button**. `targetPaths`
   and `toolKind` already fell back to the joined `ToolCall`; `title` was the one field
   that did not, and the join would have supplied it (the matching `tool_call` carries
   `title: "perl -e … > $HOME/openflow-codex-perm-test.txt"`).

**Fix:** `render_line` falls back to the `kind` the agent DID state (`"? execute"`),
untouched whenever a title is present; `runEventRows.ts` joins `title` from the matching
tool call, the same fallback its two siblings already had.

**Break-and-revert:** removing the fallback fails
`a_real_codex_permission_request_has_no_title_and_must_still_say_something` with
`left: "? " right: "? execute"`.

### 12.4 Also learned from the Codex frames

- **Codex offers TWO options of kind `allow_always`** — `allow_always` ("Allow for
  Session") and `accept_execpolicy_amendment`, the latter carrying a `_meta.permission`
  policy block that would write a persistent execpolicy rule into Codex itself. Harmless
  here **because `pick_option` prefers the `*_once` form**, so an automatic decision can
  never reach either. This is the first live case where that preference does real work
  rather than being merely prudent.
- Codex's `_meta.codex.params.reason` explains _why_ approval is needed ("The exact
  command was blocked because it writes to your home directory"). Deliberately not
  consumed — `_meta` is vendor-private by spec.
- Unmodelled `session/update` variants from Codex: `available_commands_update`,
  `usage_update`, `session_info_update`. All drop silently, as designed.
- Codex's `session/prompt` response carries `usage` and `_meta.quota` alongside
  `stopReason`. `PromptResult` ignores both; now proved by the real frame.
- Codex sends `agent_thought_chunk` with a `messageId`, and `agent_message_chunk` with
  `_meta`. Both ignored without incident.

### 12.5 Gates

| Gate                         | Baseline (§11.6) | After                                   |
| ---------------------------- | ---------------- | --------------------------------------- |
| `cargo test --lib`           | 481 passed       | ✅ **488 passed; 0 failed**             |
| `cargo clippy --all-targets` | 34 / 39 / 1      | ✅ **34 / 39 / 1 — baseline, zero new** |
| `cargo fmt -- --check`       | clean            | ✅ clean                                |
| `bun run build`              | ✓                | ✅ ✓                                    |
| `bun run lint`               | 0 errors         | ✅ 0 errors (same pre-existing warning) |
| `bun run format:check`       | clean            | ✅ clean                                |

### 12.6 Still unverified

Codex's live pass is now **3-of-3 at the protocol level**, and V1/V2 for Codex are done.
Everything that needs a human at the keyboard is unchanged from §11.7: **V5b
orphan-at-quit has still never executed**, V6 idle timeout, and the GUI halves of V2–V5
(including the Codex permission CARD itself — the empty-headline fix was verified through
the reducer's types and the replayed frame, not by clicking it). V3/V4/V5 were not
re-driven against Codex: they are agent-independent protocol behaviours already proved
twice, and the two divergences found here are both shape defects, not session-lifecycle
ones.
