# Shared agents (C2) — live end-to-end verification

**Date:** 2026-08-04 · **Branch:** `feat/shared-agents` · **Task:** 5
**Counterparty:** `openflow-service` `feat/relay-v0.2` @ `f192b16` (PR #1), built and run from source.

This file is evidence, not narrative. Everything below is pasted terminal output
from a real run on 2026-08-04. Where something could **not** be driven, it says
so explicitly in [What could not be run](#what-could-not-be-run) rather than
being described as if it had been.

---

## 0. What "live" means here

The **production** desktop code was used: `relay::transport::WsConnector` /
`WsRelayTransport` (a real `tokio-tungstenite` socket), `managers::agent_host`'s
`run_host_loop` / `serve_connection` / `HostState` (hello, dispatch,
`authorize_open`, `brokered_agent`, session bookkeeping, disconnect, backoff),
`relay::protocol`'s `HostMessage` / `session_frame`, and the app's own
`spawn_plan` / `build_argv` / `apply_baseline_env` to spawn a **real
subprocess** in a **real throwaway git repo**. On the other side: the real
`openflow-service` binary, and `curl` as the teammate.

The harness is the env-gated test
`managers::agent_host::tests::live_end_to_end_against_a_real_openflow_service`
(inline, uses the explicit `block_on_with_io` helper — no `#[tokio::test]`). It
does nothing and opens no socket unless `OPENFLOW_LIVE_SERVICE_URL` and friends
are set, so `cargo test` is unaffected.

One seam could not be crossed and is **not** claimed: `AgentRunManager::start`.
See [What could not be run](#what-could-not-be-run).

---

## 1. Standing the loop up

```console
$ cd openflow-service && cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.79s

$ OFS_BIND=127.0.0.1:8787 OFS_DATA_DIR=$SCRATCH/svc-data \
  OFS_SETUP_TOKEN=ofs_setup_livecapture OFS_LOG=info ./target/debug/openflow-service &
$ cat service.log
... INFO starting openflow-service version="0.1.0"
... INFO edition resolved edition="community"
... INFO pairing setup token (use this once to pair your first device; rotate via POST /v1/admin/rotate-setup-token) setup_token=ofs_setup_livecapture
... INFO listening bind=127.0.0.1:8787

$ curl -s http://127.0.0.1:8787/health
{"status":"ok","version":"0.1.0"}
```

Pairing (the **shipped v0.15.7** `/v1/pair` flow — C2 adds no second credential),
then `bootstrap` + invite + redeem:

```console
$ curl -s -X POST $U/v1/pair -H 'content-type: application/json' \
    -d '{"setup_token":"ofs_setup_livecapture","device_name":"openflow-desktop-live","platform":"macos"}'
{"device_id":"31905e1d-4f06-4345-8f58-fcfc378d111e","device_token":"ofs_ETJLP6Ht…"}

$ curl -s -X POST $U/v1/pair -H 'content-type: application/json' \
    -d '{"setup_token":"ofs_setup_livecapture","device_name":"teammate-laptop","platform":"linux"}'
{"device_id":"082768b4-c5b7-4a0e-a949-03bc94c6ccdd","device_token":"ofs_K-ikLAr3…"}

$ curl -s $U/v1/info -H "authorization: Bearer $HOST_TOKEN" | jq .modules
{"stt": false, "llm": false, "memory": false, "relay": true}

$ curl -s -X POST $U/v2/members/bootstrap -H "authorization: Bearer $HOST_TOKEN" \
    -H 'content-type: application/json' -d '{"setup_token":"…","display_name":"Live Owner"}'
{"member_id":"4e9cbe53-1fce-4844-af11-e5edbc481324"}

$ CODE=$(curl -s -X POST $U/v2/invites -H "authorization: Bearer $HOST_TOKEN" \
    -H 'content-type: application/json' -d '{"display_name":"Priya"}' | jq -r .code)
$ curl -s -X POST $U/v2/invites/redeem -H "authorization: Bearer $TEAM_TOKEN" \
    -H 'content-type: application/json' -d "{\"code\":\"$CODE\"}"
{"member_id":"b62c9540-67fe-4fdc-833f-c89b3b7649d5"}
```

The grant's project folder is a throwaway git repo in the scratch dir:

```console
$ git -C $SCRATCH/live/shared-repo log --oneline
effa2a1 initial
```

The shared agent is a real CLI agent definition whose binary is
[`fake-coder.sh`](fake-coder.sh) — a deliberately dumb stand-in that takes the
instruction on **stdin** (`PromptDelivery::Stdin`, the app's default), receives
`--cwd {cwd}` substituted by the app's own `build_argv`, and appends a line to
`README.md`. Its `project_path` is set to `/this/must/never/be/used` so that a
run landing in the agent's own folder instead of the grant's would be obvious.

---

## 2. Does the service accept our `hello`'s `OfferWire` shape?

**Yes.** These are the bytes our `HostMessage::Hello` actually put on the socket
(captured by wrapping the production transport; unabridged in
[`capture/outbound.jsonl`](capture/outbound.jsonl)):

```text
{"t":"hello","offers":[{"action_id":"agent:coder","label":"Coder","project":"/private/tmp/claude-501/…/live/shared-repo","allowed":["b62c9540-67fe-4fdc-833f-c89b3b7649d5"]}]}
```

The service's answer, verbatim ([`capture/inbound.jsonl`](capture/inbound.jsonl)):

```text
{"t":"ready","offers":1}
```

…and the row it created:

```console
$ curl -s $U/v2/offers -H "authorization: Bearer $TEAM_TOKEN" | jq
{
  "items": [
    {
      "offer_id": "c4fdbbd1-6eb5-48a9-86c7-8e88aa11b3c9",
      "device_id": "31905e1d-4f06-4345-8f58-fcfc378d111e",
      "action_id": "agent:coder",
      "label": "Coder",
      "project": "/private/tmp/claude-501/…/scratchpad/live/shared-repo",
      "allowed": ["b62c9540-67fe-4fdc-833f-c89b3b7649d5"],
      "updated_at": "2026-08-04T05:13:04Z"
    }
  ]
}
```

**Asserted:** the offer is present; `action_id` is `agent:coder`; `project` is
the **grant's** folder, not the agent's own `project_path`.

### Spec gap #1, answered empirically

The offer row carries `offer_id` **and** `action_id`, and — more to the point —
every real `open` the service sends carries `action_id` as a required field:

```text
{"t":"open","session_id":"8015897c-…","offer_id":"c4fdbbd1-…","action_id":"agent:coder","requester":{"member_id":"b62c9540-…","display_name":"Priya"},"sealed":false,"payload":{"instruction":"add a one-line comment to the top of README.md describing this project"}}
```

So against **this** service Task 4's `offer_actions` fallback is **not
load-bearing** — `resolve_action` always takes the `action_id` branch. It is
still worth keeping as written (it tolerates an older service and costs a map
insert), but nobody should believe it is exercised in production today. The
service's `ServiceFrame::Open` makes `action_id` a hard `missing_field` error if
absent, so it cannot be dropped without a deliberate service change.

---

## 3. One real instruction, end to end

```console
$ SID=$(curl -s -X POST $U/v2/sessions -H "authorization: Bearer $TEAM_TOKEN" \
    -H 'content-type: application/json' \
    -d '{"offer_id":"c4fdbbd1-…","payload":{"instruction":"add a one-line comment to the top of README.md describing this project"}}' | jq -r .session_id)
8015897c-2f89-4198-9892-b5df1ba013f2

$ curl -N $U/v2/sessions/$SID/events -H "authorization: Bearer $TEAM_TOKEN" | tee session.log
event: frame
data: {"seq":2,"kind":"header","sealed":false,"payload":{"agent":"Coder","kind":"header","project":"/private/tmp/claude-501/…/live/shared-repo"}}

event: frame
data: {"seq":3,"kind":"output","sealed":false,"payload":{"chunk":"coder: cwd=/private/tmp/claude-501/…/live/shared-repo","kind":"output"}}

event: frame
data: {"seq":4,"kind":"output","sealed":false,"payload":{"chunk":"coder: instruction=add a one-line comment to the top of README.md describing this project","kind":"output"}}

event: frame
data: {"seq":5,"kind":"output","sealed":false,"payload":{"chunk":"coder: edited README.md","kind":"output"}}

event: frame
data: {"seq":6,"kind":"output","sealed":false,"payload":{"chunk":"coder: done","kind":"output"}}

event: frame
data: {"seq":7,"kind":"status","sealed":false,"payload":{"kind":"status","status":"finished"}}

event: closed
data: {"outcome":"finished"}

(the shell prompt is back — the stream terminated on its own)
```

Full stream: [`session-happy-path.sse.txt`](session-happy-path.sse.txt).

**Assert 1 — the file actually changed.** Not a log line; the repo:

```console
$ git -C $SCRATCH/live/shared-repo diff
diff --git a/README.md b/README.md
index c98c87a..043b497 100644
--- a/README.md
+++ b/README.md
@@ -1,3 +1,4 @@
 # shared-repo

 A throwaway repo used as the grant project folder for the C2 live end-to-end.
+<!-- add a one-line comment to the top of README.md describing this project -->
```

**Assert 2 — one `header`, then `output` in order, then exactly one terminal.**
`seq` 2→7 is contiguous with no repeats, `header` first, four `output` in the
order the process printed them, one `status`, one `closed`. (`seq 1` is the
service's own record of the requester's `open`, in the `to_host` direction.)

**Assert 3 — every frame carries `"sealed": false`.** Visible on all six frames
above and on all 19 lines of `capture/outbound.jsonl`.

**Assert 4 — the run appears in OpenFlow's Agent Runs panel.** **NOT VERIFIED.**
See [What could not be run](#what-could-not-be-run).

**Assert 5 — the audit row agrees.**

```console
$ curl -s $U/v2/sessions -H "authorization: Bearer $TEAM_TOKEN" | jq
{"items":[{"seq":1,"session_id":"8015897c-2f89-4198-9892-b5df1ba013f2",
 "offer_id":"c4fdbbd1-6eb5-48a9-86c7-8e88aa11b3c9",
 "host_device":"31905e1d-4f06-4345-8f58-fcfc378d111e",
 "requester_member":"b62c9540-67fe-4fdc-833f-c89b3b7649d5",
 "requester_device":"082768b4-c5b7-4a0e-a949-03bc94c6ccdd",
 "state":"closed","outcome":"finished",
 "created_at":"2026-08-04T05:13:16Z","closed_at":"2026-08-04T05:13:17Z"}]}
```

The host side of the same run, from the harness's own stderr:

```
live: launching live-run-0: …/live/fake-coder ["--cwd", "…/live/shared-repo"] in …/live/shared-repo
live: [live-run-0] coder: cwd=…/live/shared-repo
live: [live-run-0] coder: instruction=add a one-line comment to the top of README.md describing this project
live: [live-run-0] coder: edited README.md
live: [live-run-0] coder: done
live: [live-run-0] terminal: finished
```

---

## 4. The outbound capture (the debt `protocol.rs` recorded)

`relay/protocol.rs`'s module doc said a genuine capture of this crate's **own**
`HostFrame`/`session_frame` output "is owed by the live end-to-end task", because
`HostFrame` is internally tagged and only something that really writes it to a
socket can capture it honestly. That is now paid.

- Committed verbatim: [`capture/outbound.jsonl`](capture/outbound.jsonl) (19
  lines) and [`capture/inbound.jsonl`](capture/inbound.jsonl) (4 lines).
- Fed through the **production deserializers** by three new tests in
  `relay/protocol.rs`'s `real_captured_frames` module — the outbound mirror of
  what Task 2 did for inbound:
  - `the_real_hello_this_crate_sent_round_trips_through_its_own_types`
  - `every_real_session_frame_this_crate_sent_parses_as_its_own_host_frame`
  - `the_real_terminal_frames_carry_the_outcome_the_audit_row_recorded`

The middle one is the one that matters: it parses each captured envelope into
`HostMessage`, then parses its `payload` into a **`HostFrame`**, asserts the
envelope `kind` equals the frame's own serde tag, and asserts that
`session_frame(session_id, frame)` re-serializes **byte-identically** to the
captured line. The two staged tests Task 2 removed could not do this (the fake
host that produced their bytes emitted a flat payload with no `kind` key).

**All 19 lines, not the 4 that were quoted (review).** Those three tests use
hand-copied constants, which agreed with the file but which nothing kept in
step. The capture files are now `include_str!`d into `protocol.rs` and walked
whole, by two more tests:

- `every_captured_outbound_line_is_pinned_not_just_the_named_ones` — parses all
  19 lines, re-serializes every frame and every `closed` byte-identically,
  asserts no line is `sealed`, asserts no **reserved** frame kind was ever
  emitted, and asserts the session shape (one `hello`, two `header`s, two
  `status`es, terminals `finished` then `stopped`).
- `every_captured_inbound_line_still_parses_as_a_service_message` — all 4
  inbound lines, with `action_id` required on both real `open`s (spec gap #1
  again, from the wire rather than from prose).
- `each_named_constant_is_a_line_of_the_committed_capture` — makes the quoted
  constants unable to drift from the file they quote.

Drift is now impossible rather than merely absent.

---

## 5. The failure modes, deliberately triggered

### 5.1 Stop from the requester

```console
$ S=$(… POST /v2/sessions … '{"instruction":"LONG: keep working until told to stop"}' …)
8ef2a6a6-bd08-4bed-9e44-b39073aba75a
$ ( sleep 5; curl -i -X POST $U/v2/sessions/$S/stop -H "authorization: Bearer $TEAM_TOKEN" ) &
$ curl -N $U/v2/sessions/$S/events -H "authorization: Bearer $TEAM_TOKEN"
…
data: {"seq":17,"kind":"output","sealed":false,"payload":{"chunk":"coder: working 4","kind":"output"}}

--- POST /v2/sessions/8ef2a6a6-…/stop ---
HTTP/1.1 202 Accepted

event: frame
data: {"seq":18,"kind":"status","sealed":false,"payload":{"kind":"status","status":"stopped"}}

event: closed
data: {"outcome":"stopped"}

(stream terminated)
```

Host side: `live: stop requested for live-run-1` → `live: [live-run-1] terminal: stopped`.

No orphaned child. **The check first written here was broken and could never
fire** — review caught it by running it against a definitely-live process. For a
shebang script `args` is `/bin/bash <script> --cwd …`, so `$3` is `/bin/bash`,
never the script path; the correct field is `$4`. Corrected, and then proved
capable of failing before it was believed:

```console
# a definitely-running agent, launched exactly as the host launches it
$ echo "LONG: …" | $SCRATCH/live/fake-coder --cwd $SCRATCH/live/shared-repo &
$ ps -o pid=,args= -p $!
84063 /bin/bash /private/tmp/…/live/fake-coder --cwd /private/tmp/…/live/shared-repo

# the check as first committed — $3
$ ps -ax -o pid=,ppid=,args= | awk -v want="$WANT" '$3 == want {print "ORPHAN:", $0}'
(nothing — against a LIVE process. It could never fire.)

# corrected — $4
$ ps -ax -o pid=,ppid=,args= | awk -v want="$WANT" '$4 == want {print "ORPHAN:", $0}'
ORPHAN: 84063 84060 /bin/bash /private/tmp/…/live/fake-coder --cwd /private/tmp/…/live/shared-repo

# …and quiet again once it is gone (no false positive)
$ kill %1; ps -ax -o pid=,ppid=,args= | awk -v want="$WANT" '$4 == want {print "ORPHAN:", $0}'
(nothing)
```

Re-run against a real requester-stop, with the corrected command sampling
**during** the run and **after** the stop:

```console
DURING THE RUN, the check sees it: 84341 84308 /bin/bash /private/tmp/…/live/fake-coder --cwd /private/tmp/…/live/shared-repo
stop -> HTTP 202
data: {"seq":79,"kind":"status","sealed":false,"payload":{"kind":"status","status":"stopped"}}
event: closed
data: {"outcome":"stopped"}
### AFTER the stop — corrected orphan check:
(nothing above = no orphan)
```

(`grep -c fake-coder` is **not** a usable substitute here: the harness shell's
own command line contains the string, so it counts 2 even when nothing is
running. `$4 == want` is exact.)

Full streams: [`session-requester-stop.sse.txt`](session-requester-stop.sse.txt)
(original) and
[`session-stop-postfix.sse.txt`](session-stop-postfix.sse.txt) (the re-run with
the corrected check).

### 5.2 Stop from the owner

**PARTIALLY VERIFIED.** The owner's Stop button calls
`AgentRunManager::stop_run`, which is reachable only through the Tauri command
layer. What _was_ driven live is the same kill path from the other direction
(5.1 routes through `RunLauncher::stop`). The remaining half — clicking Stop in
the panel — is in [What could not be run](#what-could-not-be-run).

### 5.3 Host killed mid-run

`kill -9` on the host process while it streamed (the harness equivalent of
Cmd+Q):

```console
$ ( sleep 6; pkill -9 -f handy_app_lib-… ) &
$ curl -N $U/v2/sessions/90a5b894-…/events -H "authorization: Bearer $TEAM_TOKEN"
…
data: {"seq":37,"kind":"output","sealed":false,"payload":{"chunk":"coder: working 5","kind":"output"}}

--- kill -9 the host process (== Cmd+Q on OpenFlow) ---
event: frame
data: {"seq":38,"kind":"output","sealed":false,"payload":{"chunk":"coder: working 6","kind":"output"}}

killed
event: closed
data: {"outcome":"host_disconnected"}

(the stream returned; it did not hang)
```

The stream **terminated** rather than hanging — DESIGN-relay-v02 §6's named
failure class. Full stream: [`session-host-killed.sse.txt`](session-host-killed.sse.txt).

Relaunching the host: the run is **not** silently restarted. The relaunched
host's entire outbound traffic was one `hello`:

```console
$ cat capture4/outbound.jsonl
{"t":"hello","offers":[{"action_id":"agent:coder","label":"Coder","project":"…/live/shared-repo","allowed":["b62c9540-…"]}]}
$ grep -c 'launching' host4.log
0
$ curl -s $U/v2/sessions … | grep 90a5b894
90a5b894-3bdd-4c14-be69-979a4df75c56 closed host_disconnected
```

**Observed, not designed:** the agent child did **not** survive the host's
SIGKILL. That is a side effect, not a guarantee — the stand-in agent writes to
stdout in a loop, so the closed pipe killed it with SIGPIPE. A quiet agent would
have been orphaned. DESIGN §5's "local runs keep going" is about a dropped
_socket_, and that is what `HostState::on_disconnect` implements; a killed
_process_ takes its children's fate out of the app's hands either way.

### 5.4 Unauthorised open — the two independent checks

**(a) The service refuses a member who is not in `allowed`.** A third member was
paired and invited but never added to the grant:

```console
$ curl -s $U/v2/offers -H "authorization: Bearer $STRANGER_TOKEN"
{"items": []}

$ curl -s -i -X POST $U/v2/sessions -H "authorization: Bearer $STRANGER_TOKEN" \
    -H 'content-type: application/json' \
    -d '{"offer_id":"c4fdbbd1-…","payload":{"instruction":"rm -rf /"}}'
HTTP/1.1 403 Forbidden
content-type: application/json

{"error":{"code":"forbidden","message":"this offer is not shared with you"}}
```

**(b) The host refuses on its own, with the service still saying yes.** The
grant's member list was emptied on the **live** `HostState` (via
`HostState::set_config`, exactly as `republish` does) and the host deliberately
did **not** republish, so the service kept advertising the stale offer:

```console
$ grep 'revoking the grant' host2.log
live: revoking the grant's member on the live state (no republish)

$ curl -s $U/v2/offers -H "authorization: Bearer $TEAM_TOKEN" | jq '[.items[] | {offer_id, action_id, allowed}]'
[{"offer_id":"b395799c-9d57-4d4c-a166-27affe9c11f5","action_id":"agent:coder",
  "allowed":["b62c9540-67fe-4fdc-833f-c89b3b7649d5"]}]      ← the service still says YES

$ curl -s -X POST $U/v2/sessions -H "authorization: Bearer $TEAM_TOKEN" … | jq -r .session_id
d4a684af-5e53-4eec-8014-4492c9969f73                        ← the service ACCEPTED the open

$ curl -N $U/v2/sessions/d4a684af-…/events -H "authorization: Bearer $TEAM_TOKEN"
event: closed
data: {"outcome":"denied"}                                  ← the HOST refused
```

Nothing launched (`grep -c launching host2.log` → `1`, the pre-revoke run), and
`git diff --stat` in the grant folder was unchanged by it. This is the evidence
that `relay::grants::authorize_open` is an independent boundary, not a
restatement of the relay's decision.

For contrast, the _same_ teammate, _same_ offer, ~20s earlier — before the
revoke — ran normally and returned `finished`.

**Re-run and committed as an artifact after review**, because this is the
security-critical leg and it previously existed only as a markdown quote —
[`session-denied.sse.txt`](session-denied.sse.txt) and
[`session-denied-audit.json`](session-denied-audit.json):

```console
$ grep 'revoking the grant' host6.log
live: revoking the grant's member on the live state (no republish)

$ curl -s $U/v2/offers -H "authorization: Bearer $TEAM_TOKEN" | jq '[.items[] | {offer_id, action_id, allowed}]'
[{"offer_id":"0299bcbc-e636-48ad-8853-bf45bc9d9565","action_id":"agent:coder",
  "allowed":["b62c9540-67fe-4fdc-833f-c89b3b7649d5"]}]      ← stale, still says YES

$ S=$(curl -s -X POST $U/v2/sessions … -d '{"…","payload":{"instruction":"after the revoke — this must never run"}}' | jq -r .session_id)
3e68c6ed-065c-42af-b677-021d9c0e5a79                        ← the service ACCEPTED it

$ curl -N $U/v2/sessions/$S/events -H "authorization: Bearer $TEAM_TOKEN" | tee session-denied.sse.txt
event: closed
data: {"outcome":"denied"}

### launches before this open: 2 ; after: 2
$ grep -c 'this must never run' $SCRATCH/live/shared-repo/README.md
0
```

The launch counter is unchanged across the refused open, and the denied
instruction — which the `LONG`-free branch of `fake-coder.sh` **would** have
appended — appears nowhere in the repo. Audit row: `state: closed`,
`outcome: denied`.

---

## 6. What the service returns for a revoked device token

A device was paired and then revoked with `DELETE /v1/devices/{id}` (`204 No
Content`). The `GET /v2/relay/host` **WebSocket upgrade** with its token:

```console
$ curl -s -i $U/v2/relay/host -H "authorization: Bearer $REVOKED_TOKEN" \
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=='
HTTP/1.1 401 Unauthorized
content-type: application/json
content-length: 70

{"error":{"code":"unauthorized","message":"invalid or revoked token"}}
```

There is **no** WebSocket handshake, no close frame, no relay-level error — the
upgrade is refused by `api::require_auth` before `ws.on_upgrade` is ever
offered. A **never-issued** token gets the byte-identical response, so the
service does not distinguish "revoked" from "bogus" on the wire.

Driven through _our_ `WsConnector`, before any change in this task:

```console
$ cargo test --lib live_dial_with_a_revoked_device_token -- --nocapture
### 1. a REVOKED device token
live: revoked-token dial error verbatim: could not open the relay socket: HTTP error: 401 Unauthorized
### 2. a token that never existed
live: revoked-token dial error verbatim: could not open the relay socket: HTTP error: 401 Unauthorized
### 3. the service is DOWN (a genuine outage) — same code path, port 9
live: revoked-token dial error verbatim: could not open the relay socket: IO error: Connection refused (os error 61)
```

So the information was on the wire, but `run_host_loop` fed all three into the
same `log::warn!("relay: connect failed …")` and the same forever-retry. A
revoked owner and a flaky network looked identical in `handy.log`.

**What changed (small, additive, retry behaviour untouched):**
`WsConnector::connect` now maps a 4xx handshake response to a distinct message,
`relay::transport::dial_was_refused` classifies it, and `run_host_loop` logs a
refusal at `error` with an actionable line instead of a generic `warn`. Both
branches still back off and retry — a 401 is not always permanent, and giving up
would swap a noisy log for a host that silently never returns. Surfacing it in
the UI belongs to the settings task.

After the change, same live dials:

```console
### revoked token, AFTER the classification change
live: revoked-token dial error verbatim: the service refused this device's token (HTTP 401)
### service down, AFTER the change (must stay an outage)
live: revoked-token dial error verbatim: could not open the relay socket: IO error: Connection refused (os error 61)
```

Pinned by `relay::transport::tests::a_refused_device_token_is_told_apart_from_an_unreachable_service`,
whose fixture strings are these verbatim live outputs.

### The remedy has to match the status (review Important 2)

The first version of this said **"re-pair this device in Settings → Service"**
for _any_ 4xx. Review drove it live and found that wrong for the two statuses a
real deployment actually hits after 401:

| status | what the service means                                                           | what actually fixes it                                                          |
| ------ | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `401`  | `invalid or revoked token`                                                       | re-pair the device                                                              |
| `403`  | `this device is not bound to a member; redeem an invite first` — **spec gap #3** | redeem an invite; **re-pairing mints another unbound device and loops forever** |
| `404`  | a service with no `/v2/` routes at all                                           | upgrade the service                                                             |
| other  | unknown to us                                                                    | say what happened, advise nothing                                               |

`refused_message(status)` now carries the remedy **inside** the message, and
`run_host_loop` logs it verbatim instead of appending one of its own. All four
are still classified as refusals, so the backoff still logs at `error` and Task
8's banner still sees them — a banner is exactly why this matters: wrong advice
in a log is bad, wrong advice in a banner is worse.
Pinned by `each_refusal_status_gets_the_remedy_that_actually_fixes_it`, which
was **break-and-reverted**: restoring the re-pair line on 403 fails it with
_"re-pairing does not bind a device to a member; telling the owner to do it
sends them round a loop that cannot terminate"_.

**Related defect found while doing this, NOT fixed here (see Concerns):** once
the host loop is running, re-pairing the device does not change the token it
dials with. Now also recorded as a doc comment on `AgentHostManager::republish`,
where whoever fixes it will be reading.

---

## 7. Regression after the production change

The whole loop was re-run after the dial-classification edit:

```console
event: frame
data: {"seq":40,"kind":"header","sealed":false,"payload":{"agent":"Coder","kind":"header","project":"…/live/shared-repo"}}
…
event: frame
data: {"seq":45,"kind":"status","sealed":false,"payload":{"kind":"status","status":"finished"}}

event: closed
data: {"outcome":"finished"}
```

Final state of the grant folder after all six live sessions:

```console
$ git -C $SCRATCH/live/shared-repo diff
@@ -1,3 +1,7 @@
 # shared-repo

 A throwaway repo used as the grant project folder for the C2 live end-to-end.
+<!-- add a one-line comment to the top of README.md describing this project -->
+<!-- before the revoke -->
+<!-- regression check after the dial-classification change -->
+<!-- post-review regression: registration window closed -->
```

**What that diff does and does not prove** — the first version of this line
over-reached and review caught it. It said "nothing from the `denied` one, the
`stopped` one, or the `host_disconnected` one", as if all three were proved by
absence. They are not:

- **`denied` — load-bearing.** Its instruction went down the ordinary branch of
  `fake-coder.sh`, which appends a line. No line appeared, and the launch
  counter did not move. That is real evidence.
- **`stopped` and `host_disconnected` — prove nothing here.** Both used `LONG:`
  instructions, and `fake-coder.sh`'s `LONG*` branch **never touches
  `README.md` by design** — it only loops. Their absence from the diff is a
  property of the fixture, not of the system. What actually proves those two is
  the SSE terminal (`stopped` / `host_disconnected`), the audit row, and — for
  the stop — the orphan check in §5.1, now that it is capable of failing.

Every audit row, in order (the last three are the post-review re-runs):

| #   | outcome             | what it was                                       |
| --- | ------------------- | ------------------------------------------------- |
| 1   | `finished`          | the happy path                                    |
| 2   | `stopped`           | stop from the requester                           |
| 3   | `finished`          | control, before the host-side revoke              |
| 4   | `denied`            | the host's own re-check, service still saying yes |
| 5   | `host_disconnected` | host killed mid-run                               |
| 6   | `finished`          | regression after the dial-classification change   |
| 7   | `finished`          | regression after the registration-window fix      |
| 8   | `stopped`           | stop re-run, with the corrected orphan check      |
| 9   | `denied`            | denied re-run, committed as an artifact           |

---

## 8. Tests and lint

```console
$ cargo test --lib
test result: ok. 444 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

433 (baseline) + 2 live harness tests (skipped without env vars) + 3 outbound
capture tests + 1 dial-classification test + 5 added on review (the
registration-window regression, the per-status remedies, and three that walk the
whole capture) = 444.

Two of them were **break-and-reverted** rather than merely written:

```console
# Important 3 — move the by_run lock back below launcher.launch(...)
thread '…a_run_that_reaches_a_terminal_before_launch_returns_still_gets_its_closed' panicked at:
  the terminal status frame was dropped: the run finished before it was
  registered, so `frame_for_run` could not find its session
test result: FAILED. 0 passed; 1 failed
--- reverted ---
test result: ok. 1 passed; 0 failed

# Important 2 — give 403 the re-pair remedy again
thread '…each_refusal_status_gets_the_remedy_that_actually_fixes_it' panicked at:
  re-pairing does not bind a device to a member; telling the owner to do it
  sends them round a loop that cannot terminate
test result: FAILED. 0 passed; 1 failed
--- reverted ---
test result: ok. 1 passed; 0 failed
```

Clippy, measured on this machine by stashing the change and re-running:

```console
BASELINE lib:       37
BASELINE lib+tests: 43
AFTER    lib:       37
AFTER    lib+tests: 43
$ diff <(sort clippy-before-set.txt) <(sort clippy-after-libtests-set.txt) && echo "SET DIFF EMPTY"
SET DIFF EMPTY (lib+tests)
```

No warning is in `relay/*` or `managers/agent_host.rs`, before or after.
(The task brief quotes 36/42; on this machine the same commands give 37/43
before _and_ after, so the set-diff — the actual bar — is empty either way.)

---

## What could not be run

Stated plainly, because a false "verified live" is worse than a recorded gap.

1. **`AgentRunManager::start` was not driven live.** It takes a
   `tauri::AppHandle` (= `AppHandle<Wry>`) and streams output via
   `AgentRunOutput::emit` / `listen`. A `Wry` app can only be constructed on the
   process main thread, and libtest runs every `#[test]` on a spawned thread;
   `tauri::test::mock_app()` yields an `App<MockRuntime>`, a different type none
   of these signatures accept. The harness's `LiveLauncher` therefore stands in
   for `AgentRunLauncher`: it spawns the process with the app's **own**
   `spawn_plan`, `build_argv` and `apply_baseline_env`, and feeds output and
   terminal frames through exactly the two `HostState` entry points production
   uses (`frame_for_run`, as `wire_run_pipeline`'s `agent-run-output` listener
   does; `frame_for_run` + `close_run`, as `RelayFrameSink::on_terminal` does).
   What is **not** covered live: `AgentRunManager::start` itself, `drive_run`'s
   own streaming, `finalize`'s relay-sink arm, and the Tauri event hop between
   them.

2. **The Agent Runs panel** (Assert 4) and **Stop from the owner** (5.2) were
   not exercised. Both are UI on the AppHandle side of the same seam.

3. **`AgentHostManager::ensure_started` / `republish`** were not exercised live.
   They read the settings store and the OS keyring, both of which need the real
   app; driving them from a test would also have clobbered the developer's real
   `service`/`device_token` keychain entry. The harness constructs `HostState`,
   the `WsConnector` and `run_host_loop` directly, which is the same sequence
   `ensure_started` performs after its gate.

### The human runbook for the gap

On the machine with the real app:

1. Run `openflow-service` locally as in §1 and note the setup token.
2. In OpenFlow: **Settings → Service → Connect to my service**, `http://127.0.0.1:8787`,
   paste the setup token. (Back up `~/Library/Application Support/…/settings.json`
   first if the machine is paired to a real service — this overwrites it.)
3. Redeem an owner invite for this device if C1 does not auto-bind it, then mint
   and redeem a teammate invite as a second device (or reuse the `curl` teammate
   from §1).
4. Configure the grant. Task 8's UI does not exist yet, so edit the settings
   store directly: `sharing.enabled = true`, one `grants` entry with
   `agent_id`, `project_path` (a throwaway git repo) and `allowed_members`
   (the teammate's `member_id`). Restart the app.
5. Repeat §3 with `curl` as the teammate. Then check, and screenshot:
   - the run appears in the **Agent Runs** panel with the **grant's** folder and
     a `← Priya` requester label (`AgentRunManager::note_brokered_run`);
   - clicking **Stop** in the panel terminates the requester's SSE stream with
     `stopped` rather than leaving it hanging;
   - Cmd+Q mid-stream produces `host_disconnected` (§5.3 verified this at the
     socket level already; this confirms it through the real app's shutdown).

---

## Concerns

1. **A revoked device token retries forever.** Now distinguishable in the log
   (§6), but nothing surfaces it to the owner and nothing stops the retry. A
   settings-page banner is the obvious home for it.

2. **Re-pairing does not change the token the live loop dials with.**
   `AgentHostManager::ensure_started` builds the `WsConnector` (URL + token)
   once and the loop owns it for its whole life; `republish` on a still-hosting
   manager only calls `set_config` + `enqueue(Hello)`, and `ensure_started` is
   idempotent while a loop is live. So after a revoke-then-re-pair, the host
   keeps dialling with the dead token until the app restarts. Found by reading
   the code while chasing §6; **not** reproduced live and **not** fixed here
   because the fix (stop and restart the loop when the token changes) is a
   behavioural change that belongs with the settings work.

3. ~~**A narrow output-loss window in `handle_service_message`.**~~ **FIXED —
   and it was worse than this concern said.** As first written it read "could
   lose its first line". Review corrected it: `AgentRunManager::start` spawns on
   a **multi-threaded** runtime, and its spawn-failure path (`agent_run.rs`) —
   reached by nothing more exotic than a bad `binary_path` — emits, logs and
   calls `finalize` → `on_terminal` immediately. Beating the registration meant
   **both** `frame_for_run(Status)` and `close_run` answering `None`: no
   `closed` ever sent, `by_run`/`sessions` holding an entry nothing would ever
   remove, and **the requester's SSE stream hanging until the socket dropped** —
   precisely the failure class DESIGN-relay-v02 §6 exists to forbid.
   `handle_service_message` now takes the `by_run` lock **before**
   `launcher.launch(...)`, so an early terminal parks on the mutex and finds the
   registration instead of missing it. Regression:
   `a_run_that_reaches_a_terminal_before_launch_returns_still_gets_its_closed`,
   break-and-reverted (§8).

4. **The live agent is a shell script, not `claude`.** `fake-coder.sh` is a
   genuine CLI subprocess driven through the app's own argv/env/stdin contract,
   and the `git diff` is genuine — but it is not a real coding agent, and
   nothing here says anything about how a real one's output volume, latency or
   exit codes behave over the relay.

5. **One service, one machine, localhost.** No TLS (`wss://`), no NAT, no
   reverse proxy, no idle-ping timeout exercised (`OFS_RELAY_PING_SECS` was left
   at its default and no session idled long enough to test the silence
   deadline).

6. ~~**`offer_actions` is dead weight against this service** (§2).~~ **DONE** —
   `HostState::resolve_action` now carries a doc comment saying so and pointing
   here, so nobody later mistakes the cache branch for a tested path.

7. **Owner-side Stop and the Agent Runs panel remain unverified**, and nothing
   downstream should assume otherwise. The runbook above is the only thing
   standing between a later task and an untested claim.
