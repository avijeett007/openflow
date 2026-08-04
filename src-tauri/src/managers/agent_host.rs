//! C2 — the agent host. OpenFlow dials OUT to the owner's own openflow-service,
//! publishes what it is willing to run, and answers brokered sessions from
//! named teammates.
//!
//! **This module is a caller, not a driver.** A brokered run is another caller
//! of `AgentRunManager::start`: `start`, the run registry, the kill channel and
//! both Tauri events are used exactly as they ship. `brokered_agent` hands
//! `start` a CLONE of the owner's `AgentDefinition` with the grant's folder and
//! the `Relay` sink, which is the whole reason `drive_run`, `drive_remote_run`,
//! `build_argv`, the `TranscriptionCoordinator` and `finish_dictation` needed no
//! change at all. Live output rides the `agent-run-output` event that already
//! exists; the terminal frame rides one additive `finalize` arm.
//!
//! **Default OFF and inert.** [`should_host`] is the single gate: sharing
//! enabled AND at least one publishable offer AND a paired service AND a device
//! token. Until it says yes nothing is constructed — no [`HostState`], no event
//! listener, no relay sink on the run manager, and above all **no socket**
//! (`tests::no_connect_attempt_when_sharing_is_off`).
//!
//! **The security boundary is structural.** DESIGN-shared-agents §8: the relay's
//! membership check is a convenience, this desktop decides. That re-check is
//! `relay::grants::authorize_open`, and it is the only source of an
//! [`Authorized`](crate::relay::grants::Authorized) token; `brokered_agent`
//! demands one and returns a `BrokeredRun`, which is the only thing
//! [`RunLauncher::launch`] accepts. Skipping the re-check is therefore a
//! compile error, not a review catch.
//!
//! The network sits behind Task 3's `RelayConnector`/`RelayTransport` traits, so
//! everything below — the gate, hello, dispatch, authorisation, session
//! bookkeeping, disconnect, and the connect loop itself — is unit-tested against
//! a scripted in-memory transport with no socket and no subprocess. Mirrors
//! `a2a.rs`.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::mpsc;

use crate::managers::agent_run::{
    run_status_outcome, AgentRunManager, AgentRunOutput, RelayFrameSink, RunStatus,
};
use crate::managers::service_sync::{KEYRING_ACCOUNT, KEYRING_SCOPE};
use crate::relay::grants::{
    authorize_open, brokered_agent, offers_from_grants, BrokeredRun, DenyReason,
};
use crate::relay::protocol::{
    parse_open_payload, session_frame, HostFrame, HostMessage, OfferWire, ServiceMessage,
};
use crate::relay::transport::{
    dial_was_refused, next_backoff, relay_ws_url, RelayConnector, RelayTransport, WsConnector,
    BACKOFF_MIN,
};
use crate::settings::{AgentDefinition, AppSettings, SharingConfig};

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Every condition that must hold before this machine hosts anything. Pure, so
/// "sharing off ⇒ nothing happens" is a table test rather than a code reading.
///
/// All four are required: the master switch, at least one grant that actually
/// resolves to a publishable offer, a paired service to dial, and a device token
/// to authenticate with. Any one missing and the app behaves exactly as v0.15.7.
pub fn should_host(
    sharing: &SharingConfig,
    service_enabled: bool,
    has_token: bool,
    offer_count: usize,
) -> bool {
    sharing.enabled && offer_count > 0 && service_enabled && has_token
}

/// Whether a settings write can skip the republish work entirely.
///
/// Pure, because `republish` now runs on EVERY settings write in the app and
/// this predicate is the whole reason that is affordable: when it holds, the
/// call returns before touching the OS keyring. Kept testable so "the choke
/// point is free when the feature is off" is asserted rather than assumed.
pub fn republish_is_a_no_op(sharing: &SharingConfig, is_hosting: bool) -> bool {
    sharing.is_dormant() && !is_hosting
}

/// **The one place a relay socket is ever opened**, including on every
/// reconnect — `run_host_loop` goes through here rather than calling
/// `connector.connect()` itself. Wrapping `connect` in the gate (instead of
/// checking the gate at the call site) is what makes
/// `no_connect_attempt_when_sharing_is_off` a real assertion about production
/// code: the test counts `connect` calls on an injected connector and requires
/// zero.
///
/// `None` means "the gate said no, nothing was attempted"; `Some(Err(_))` means
/// a real dial failed and is worth a backoff. Conflating the two would let a
/// switched-off host spin on a retry timer.
pub async fn maybe_connect<C: RelayConnector>(
    sharing: &SharingConfig,
    service_enabled: bool,
    has_token: bool,
    offers: &[OfferWire],
    connector: &C,
) -> Option<Result<C::Conn, String>> {
    if !should_host(sharing, service_enabled, has_token, offers.len()) {
        return None;
    }
    Some(connector.connect().await)
}

// ---------------------------------------------------------------------------
// The launch seam
// ---------------------------------------------------------------------------

/// How a brokered session reaches the run pipeline. Behind a trait purely so no
/// test spawns a process — the production implementation is four lines.
///
/// `launch` takes a [`BrokeredRun`], which only
/// `relay::grants::brokered_agent(&Authorized, _)` can mint. That is the
/// structural form of "authorise before you run": a caller with nothing but an
/// `AgentDefinition` cannot call this.
pub trait RunLauncher: Send + Sync + 'static {
    fn launch(&self, agent: BrokeredRun, instruction: String) -> String;
    fn stop(&self, run_id: &str);
}

/// Production launcher: `AgentRunManager::start`, called **unmodified**, with
/// the requester noted for the panel label in the same breath so no run ever
/// exists without its `← Priya`.
pub struct AgentRunLauncher {
    manager: Arc<AgentRunManager>,
    app: AppHandle,
}

impl RunLauncher for AgentRunLauncher {
    fn launch(&self, agent: BrokeredRun, instruction: String) -> String {
        let requester = agent.requester().to_string();
        let run_id = self
            .manager
            .start(&self.app, agent.into_definition(), instruction);
        self.manager.note_brokered_run(&run_id, &requester);
        run_id
    }

    fn stop(&self, run_id: &str) {
        if let Err(e) = self.manager.stop_run(run_id) {
            log::debug!("relay: stop for {run_id} did nothing ({e})");
        }
    }
}

// ---------------------------------------------------------------------------
// Session bookkeeping
// ---------------------------------------------------------------------------

/// One live brokered session: a teammate and a run of ours. Keyed by session id
/// in `sessions`, so the id itself is not repeated here.
#[derive(Clone, Debug)]
struct HostSession {
    run_id: String,
    member_id: String,
    display_name: String,
    /// The action this session was opened against, so `apply_settings` can
    /// re-run [`authorize_open`] later exactly as `handle_service_message` did
    /// at `open` time — without it there would be no way to tell whether a
    /// settings change still authorises an ALREADY-RUNNING session.
    action_id: String,
}

/// The result of reconciling a settings change against every live session
/// (see [`HostState::apply_settings`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsDelta {
    /// Whether the published offer list actually changed, so a fresh `hello`
    /// is owed. Diffed on the OFFER LIST rather than the raw config so an
    /// unrelated edit (renaming a different agent, say) does not churn the
    /// service's offer table.
    pub republish: bool,
    /// Run ids whose session no longer re-authorises under the NEW settings.
    /// Pause, revoke, disable and delete all fall out of this one check
    /// against [`authorize_open`] rather than four ad-hoc rules that could
    /// disagree with it (or with each other).
    pub sessions_to_stop: Vec<String>,
}

/// The settings snapshot the host authorises against. Behind a `Mutex` so
/// `republish` can install fresh grants on a LIVE connection — a grant the owner
/// revoked must stop working immediately, not at the next reconnect.
struct HostConfig {
    sharing: SharingConfig,
    agents: Vec<AgentDefinition>,
}

/// Everything the host loop knows, with no socket and no Tauri in sight.
pub struct HostState {
    config: Mutex<HostConfig>,
    sessions: Mutex<HashMap<String, HostSession>>,
    /// run_id → session_id. The reverse index is what makes
    /// [`Self::frame_for_run`] able to answer "not mine" for a local run.
    by_run: Mutex<HashMap<String, String>>,
    /// offer_id → action_id, learned from any `open` that carries both. See
    /// "spec gaps" #1: `action_id` is present on every real service's `open`,
    /// but an older service may send only `offer_id`.
    offer_actions: Mutex<HashMap<String, String>>,
}

impl HostState {
    pub fn new(sharing: SharingConfig, agents: Vec<AgentDefinition>) -> Self {
        Self {
            config: Mutex::new(HostConfig { sharing, agents }),
            sessions: Mutex::new(HashMap::new()),
            by_run: Mutex::new(HashMap::new()),
            offer_actions: Mutex::new(HashMap::new()),
        }
    }

    /// Install a fresh settings snapshot with none of `apply_settings`'s
    /// diffing — no `republish`/`sessions_to_stop` decision, just the raw
    /// swap. Production code now goes through `apply_settings` instead (Task
    /// 6), so the only callers left are tests that deliberately want to prove
    /// [`Self::handle_service_message`]'s live `authorize_open` re-check is
    /// independent of ANY settings-change bookkeeping — most pointedly the
    /// live end-to-end harness, which revokes a grant with this and
    /// **never republishes**, precisely to show the re-check is a second,
    /// independent line of defence rather than a side effect of `hello`.
    #[allow(dead_code)] // test-only now; see the doc comment above
    pub fn set_config(&self, sharing: SharingConfig, agents: Vec<AgentDefinition>) {
        let mut cfg = self.config.lock().unwrap();
        cfg.sharing = sharing;
        cfg.agents = agents;
        // The offer_id → action_id cache describes the offers we published
        // BEFORE this change. Republishing re-mints them, and the service may
        // reuse an offer_id for a different action; resolving a bare `open`
        // through a stale entry would then run the wrong agent in the wrong
        // grant folder. Guessing is exactly how that happens, so forget.
        self.offer_actions.lock().unwrap().clear();
    }

    /// Reconcile a settings change against every live session. Pure data in,
    /// pure data out — it does not itself stop anything. The caller
    /// (`AgentHostManager`, which owns the launcher) is responsible for
    /// calling `launcher.stop` on every run id this returns; that split is
    /// what makes the decision unit-testable with no socket and no
    /// subprocess.
    ///
    /// `republish` is computed by comparing the OFFER LIST, not the raw
    /// config, so renaming an unrelated agent or editing an unrelated grant
    /// does not churn the service's offer table on every keystroke.
    /// `sessions_to_stop` re-runs [`authorize_open`] for every live session
    /// against the NEW settings — pause, revoke, disable and delete all fall
    /// out of the one authorisation function rather than four ad-hoc checks
    /// that could disagree with it.
    pub fn apply_settings(
        &self,
        sharing: SharingConfig,
        agents: Vec<AgentDefinition>,
    ) -> SettingsDelta {
        let (old_sharing, old_agents) = self.snapshot();
        let before = offers_from_grants(&old_sharing, &old_agents);
        let after = offers_from_grants(&sharing, &agents);
        let republish = before != after;

        // Install the NEW config FIRST, before deciding what must stop.
        // `handle_service_message`'s `Open` arm reads `self.config` live,
        // fully unlocked from this function's own session read below — if
        // the install happened LAST (as it originally did), a concurrent
        // `open` could authorise against the STALE config in the gap and
        // insert a session this call's `sessions_to_stop` snapshot could
        // never have known about, because the snapshot had already been
        // taken. Installing first does not close that window completely (a
        // session whose INSERT into `self.sessions` still lands after the
        // read below is still missed on THIS pass — see
        // `config_installs_before_the_stop_list_is_computed...` for exactly
        // what is and is not proven), but it shrinks the window from "this
        // entire function" to "between here and the lock below." A run that
        // slips through is not silently unstoppable either way: it stays
        // visible and stoppable in the owner's run panel, and `republish`
        // runs on every subsequent settings write, so the very next one
        // reconciles it via this same check.
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.sharing = sharing.clone();
            cfg.agents = agents.clone();
        }
        if republish {
            // Mirrors `set_config`: offer ids are re-minted on the fresh
            // `hello` this delta triggers, and the service may reuse one for
            // a different action, so a mapping learned BEFORE this change
            // must not survive it.
            self.offer_actions.lock().unwrap().clear();
        }

        let sessions_to_stop: Vec<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| authorize_open(&sharing, &agents, &s.action_id, &s.member_id).is_err())
            .map(|s| s.run_id.clone())
            .collect();

        SettingsDelta {
            republish,
            sessions_to_stop,
        }
    }

    /// Every run this host is currently serving for a teammate. Used by
    /// `AgentHostManager::pause` to know what to stop BEFORE the bookkeeping
    /// that names them is torn down by `on_disconnect`.
    fn running_run_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.run_id.clone())
            .collect()
    }

    fn snapshot(&self) -> (SharingConfig, Vec<AgentDefinition>) {
        let cfg = self.config.lock().unwrap();
        (cfg.sharing.clone(), cfg.agents.clone())
    }

    /// The offers this host currently stands behind.
    pub fn offers(&self) -> Vec<OfferWire> {
        let (sharing, agents) = self.snapshot();
        offers_from_grants(&sharing, &agents)
    }

    /// `hello` — republished on every connect AND on any settings change, so the
    /// service replaces this host's offers wholesale and a removed grant
    /// disappears at once.
    pub async fn publish<T: RelayTransport>(&self, transport: &T) -> Result<(), String> {
        send_message(
            transport,
            HostMessage::Hello {
                offers: self.offers(),
            },
        )
        .await
    }

    /// The session id a run's output belongs to, or `None` when this host does
    /// not own the run.
    pub fn session_for_run(&self, run_id: &str) -> Option<String> {
        self.by_run.lock().unwrap().get(run_id).cloned()
    }

    /// Wrap a frame for a run **we own**. `None` for a local hotkey run, which
    /// is how a private run's output is structurally unable to reach the relay.
    pub fn frame_for_run(&self, run_id: &str, frame: HostFrame) -> Option<HostMessage> {
        let session_id = self.session_for_run(run_id)?;
        Some(session_frame(&session_id, frame))
    }

    /// Terminal: forget the session and return the `closed` envelope — **once**.
    /// A second call returns `None`, so a requester never sees two terminals.
    pub fn close_run(&self, run_id: &str, outcome: &str) -> Option<HostMessage> {
        let session_id = self.by_run.lock().unwrap().remove(run_id)?;
        self.sessions.lock().unwrap().remove(&session_id);
        Some(HostMessage::Closed {
            session_id,
            outcome: outcome.to_string(),
        })
    }

    /// The socket dropped. DESIGN-relay-v02 §6 / ruling R4: this ends the
    /// SESSIONS and **nothing else**. The local runs keep going — killing a
    /// teammate's half-applied edit because OUR network blipped is worse than
    /// letting it finish in the owner's own panel — and no run is ever
    /// re-launched on reconnect. The service reports `host_disconnected`.
    pub fn on_disconnect(&self) {
        self.sessions.lock().unwrap().clear();
        self.by_run.lock().unwrap().clear();
        // Offer ids are minted per connection; a stale mapping would resolve an
        // offer this host no longer publishes (harmless — `authorize_open` would
        // refuse it — but pointless to keep).
        self.offer_actions.lock().unwrap().clear();
    }

    /// Dispatch one service message. Never panics and never propagates an
    /// error: a newer or misbehaving service must not be able to kill the host
    /// loop.
    ///
    /// Returns [`ControlFlow::Break`] when the socket failed to accept a write.
    /// That matters most on the `open` response path: a socket that is still
    /// readable but no longer writable would otherwise let the host launch the
    /// run, record the session, silently lose the header, and keep serving —
    /// leaving the agent to run to completion in the owner's folder with the
    /// teammate never told it started, and a service retry refused
    /// `already_open`. Dropping the connection instead lets the service report
    /// `host_disconnected` and the reconnect re-publish cleanly.
    pub async fn handle_service_message<T: RelayTransport, L: RunLauncher>(
        &self,
        msg: ServiceMessage,
        transport: &T,
        launcher: &L,
    ) -> ControlFlow<()> {
        match msg {
            ServiceMessage::Open {
                session_id,
                offer_id,
                action_id,
                requester,
                payload,
            } => {
                // A repeated `open` for a session we already serve would start a
                // SECOND run and silently re-point the session at it, leaving
                // the first run streaming into a session it no longer owns.
                // Refuse instead — the requester keeps the session it has.
                if self.sessions.lock().unwrap().contains_key(&session_id) {
                    log::warn!("relay: duplicate open for live session {session_id}");
                    return self.refuse(transport, &session_id, "already_open").await;
                }

                let Some(action) = self.resolve_action(&offer_id, action_id.as_deref()) else {
                    log::warn!("relay: open for an offer this host cannot resolve ({offer_id})");
                    return self
                        .refuse(transport, &session_id, DenyReason::UnknownOffer.outcome())
                        .await;
                };

                let instruction = match parse_open_payload(&payload) {
                    Ok(p) => p.instruction,
                    Err(e) => {
                        log::warn!("relay: refusing session {session_id}: {e}");
                        return self.refuse(transport, &session_id, "bad_request").await;
                    }
                };

                // *** The re-check. DESIGN-shared-agents §8. *** Read against
                // the CURRENT settings, not the ones in force when the offer was
                // published, and independent of whatever the relay decided.
                let (sharing, agents) = self.snapshot();
                let authorized =
                    match authorize_open(&sharing, &agents, &action, &requester.member_id) {
                        Ok(a) => a,
                        Err(reason) => {
                            log::warn!(
                            "relay: refusing session {session_id} for {} on {action}: {reason:?}",
                            requester.member_id
                        );
                            return self.refuse(transport, &session_id, reason.outcome()).await;
                        }
                    };

                // Only reachable with the token in hand.
                //
                // NOTE: `display_name` is supplied by the SERVICE and is never
                // validated here. It is only ever shown (the `← Priya` panel
                // label) and never used to decide anything — `member_id` is what
                // `authorize_open` checks. Treat it as untrusted display text:
                // it is the owner's only in-app signal of who is running code on
                // their machine, so a UI task rendering it must not let it
                // impersonate another member.
                let run = brokered_agent(&authorized, &requester.display_name);
                let agent_label = run.name.clone();
                let project = run.project_path.clone();

                // *** The registration window, closed by holding the lock. ***
                //
                // `launch` spawns a task and returns a run id immediately, and
                // that task can reach a TERMINAL before this function gets round
                // to recording the run. `AgentRunManager::start`'s spawn-failure
                // path is the mundane trigger — a bad `binary_path` emits, logs
                // and calls `finalize` -> `on_terminal` right away, on a
                // multi-threaded runtime. If that beat the insert below, BOTH
                // `frame_for_run` and `close_run` would answer `None`, no
                // `closed` would ever be sent, and `by_run`/`sessions` would
                // keep a permanently orphaned entry — the requester's SSE stream
                // hanging until the socket happens to drop. That is exactly the
                // failure class DESIGN-relay-v02 §6 exists to forbid.
                //
                // Taking `by_run` BEFORE the launch makes the window
                // unreachable: an early terminal blocks in `frame_for_run` /
                // `close_run` on this same mutex until the registration is
                // complete, then finds it. `sessions` is taken inside it, which
                // is the same order `close_run` uses (by_run -> sessions), so
                // this cannot deadlock. The explicit block (rather than a
                // `drop`) is load-bearing: `handle_service_message` awaits the
                // header below, and rustc will not call the future `Send` if a
                // `MutexGuard` is merely dropped rather than scoped out.
                {
                    let mut by_run = self.by_run.lock().unwrap();
                    let run_id = launcher.launch(run, instruction);

                    self.sessions.lock().unwrap().insert(
                        session_id.clone(),
                        HostSession {
                            run_id: run_id.clone(),
                            member_id: requester.member_id.clone(),
                            display_name: requester.display_name.clone(),
                            action_id: action.clone(),
                        },
                    );
                    by_run.insert(run_id, session_id.clone());
                }

                let header = session_frame(
                    &session_id,
                    HostFrame::Header {
                        agent: agent_label,
                        project,
                    },
                );
                if let Err(e) = send_message(transport, header).await {
                    // The run is already going and is deliberately left alone
                    // (ruling R4). Drop the socket so the service tells the
                    // requester, rather than serving on a write-broken one.
                    log::warn!("relay: could not send the header for {session_id} ({e})");
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }

            ServiceMessage::Stop { session_id } => {
                let session = self.sessions.lock().unwrap().get(&session_id).cloned();
                match session {
                    Some(s) => {
                        log::info!(
                            "relay: {} ({}) stopped brokered run {}",
                            s.display_name,
                            s.member_id,
                            s.run_id
                        );
                        // The EXISTING kill channel — `stop_run` is untouched.
                        launcher.stop(&s.run_id);
                    }
                    // Never routed anywhere: an unknown session id must not be
                    // able to stop a run it does not own (DESIGN-relay-v02 §6).
                    None => log::debug!("relay: stop for an unknown session {session_id}"),
                }
                ControlFlow::Continue(())
            }

            ServiceMessage::Unknown => {
                log::debug!("relay: ignoring a message this version does not model");
                ControlFlow::Continue(())
            }
        }
    }

    /// `action_id` from the message when the service supplies it (every real
    /// one does), else the cache an earlier `open` populated, else `None`.
    ///
    /// **The cache branch is untested weight against the real service, and that
    /// is now a measured fact rather than a guess.** The live end-to-end task
    /// captured every `open` a real `openflow-service` (`feat/relay-v0.2`)
    /// sends: all of them carry `action_id`, and the service's own
    /// `ServiceFrame::Open` makes it a hard `missing_field` error to omit — so
    /// production only ever takes the first branch. See
    /// `verification/shared-agents/RESULTS.md` §2 and
    /// `relay::protocol::tests::real_captured_frames::every_captured_inbound_line_still_parses_as_a_service_message`.
    /// Kept anyway (it costs one map insert and tolerates an older service),
    /// but nobody should read it as a path anything exercises.
    fn resolve_action(&self, offer_id: &str, action_id: Option<&str>) -> Option<String> {
        if let Some(action) = action_id.filter(|a| !a.trim().is_empty()) {
            self.offer_actions
                .lock()
                .unwrap()
                .insert(offer_id.to_string(), action.to_string());
            return Some(action.to_string());
        }
        self.offer_actions.lock().unwrap().get(offer_id).cloned()
    }

    /// Close a session that never started a run. Deliberately terse on the
    /// wire: a stranger learns they were refused, not the owner's configuration.
    async fn refuse<T: RelayTransport>(
        &self,
        transport: &T,
        session_id: &str,
        outcome: &str,
    ) -> ControlFlow<()> {
        match send_message(
            transport,
            HostMessage::Closed {
                session_id: session_id.to_string(),
                outcome: outcome.to_string(),
            },
        )
        .await
        {
            Ok(()) => ControlFlow::Continue(()),
            Err(e) => {
                log::warn!("relay: could not send the refusal for {session_id} ({e})");
                ControlFlow::Break(())
            }
        }
    }
}

async fn send_message<T: RelayTransport>(transport: &T, msg: HostMessage) -> Result<(), String> {
    let line = serde_json::to_string(&msg).map_err(|e| format!("relay: unencodable frame: {e}"))?;
    transport.send(line).await
}

// ---------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------

/// Everything ONE live host loop owns. Replaced wholesale on restart and never
/// mutated in place.
///
/// The `running` flag is per-loop and that is the whole point: a single shared
/// flag re-armed by a restart would revive a loop that had already been stopped,
/// leaving it hosting from the `HostState` it captured at birth — i.e. still
/// honouring a grant the owner had revoked. Two loops would then also race to
/// clear the one flag, silently killing the survivor.
struct HostSlot {
    running: Arc<AtomicBool>,
    state: Arc<HostState>,
    outbound: mpsc::UnboundedSender<HostMessage>,
}

/// The manager's start/stop bookkeeping, deliberately split out with **no
/// `AppHandle` in it** so it can be unit tested. The bug this shape exists to
/// prevent lived in exactly the region that was previously declared untestable.
#[derive(Default)]
struct HostSlots(Mutex<Option<HostSlot>>);

impl HostSlots {
    /// Is a loop live right now? A slot whose flag has been cleared is a loop on
    /// its way out, and does not count.
    fn is_hosting(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.running.load(Ordering::SeqCst))
    }

    fn state(&self) -> Option<Arc<HostState>> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| Arc::clone(&s.state))
    }

    fn enqueue(&self, msg: HostMessage) {
        if let Some(slot) = self.0.lock().unwrap().as_ref() {
            let _ = slot.outbound.send(msg);
        }
    }

    /// Claim the slot for a new loop, returning its **own** flag and receiver.
    /// `None` when a loop is already live, which is what makes `ensure_started`
    /// idempotent.
    fn install(
        &self,
        state: Arc<HostState>,
    ) -> Option<(Arc<AtomicBool>, mpsc::UnboundedReceiver<HostMessage>)> {
        let mut current = self.0.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|s| s.running.load(Ordering::SeqCst))
        {
            return None;
        }
        // A NEW flag every time. Any previous loop keeps the old one, cleared
        // for good, so this restart cannot revive it.
        let running = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::unbounded_channel();
        *current = Some(HostSlot {
            running: Arc::clone(&running),
            state,
            outbound: tx,
        });
        Some((running, rx))
    }

    /// Stop the loop that is live now (if any) and forget it. Only ever clears
    /// the flag it took out of the slot, never a successor's.
    ///
    /// **Invariant relied on elsewhere:** clearing the flag and dropping the
    /// outbound sender happen together, in this one operation, and this is the
    /// only external path that clears a live loop's flag. That is what makes
    /// `serve_connection` promptly observable — its closed-sender arm fires as
    /// soon as the sender drops, rather than waiting for the next flag read —
    /// and it is why a stopped loop cannot go on to authorise an `open`. Any
    /// future "just clear the flag" shortcut would silently remove that
    /// guarantee.
    ///
    /// Production no longer calls this directly — `AgentHostManager::pause`
    /// goes through [`Self::pause`] below instead, which does the same
    /// teardown ATOMICALLY with the in-flight-run cancellation (one lock
    /// acquisition, so no window where a session could open between "read
    /// what to stop" and "the socket is gone"). Kept as its own tested
    /// primitive because `a_stopped_loop_stays_stopped_when_the_manager_restarts`
    /// deliberately exercises slot teardown on its own, with no launcher in
    /// the picture — that regression is about the flag/slot invariant, not
    /// about run cancellation.
    #[allow(dead_code)] // see the doc comment above
    fn stop(&self) {
        let slot = self.0.lock().unwrap().take();
        if let Some(slot) = slot {
            slot.running.store(false, Ordering::SeqCst);
            // Dropping the sender wakes the loop out of its outbound wait.
            drop(slot.outbound);
            slot.state.on_disconnect();
        }
    }

    /// The master kill switch: everything [`Self::stop`] does, PLUS cancelling
    /// every in-flight run the loop was serving, via `launcher`. This is the
    /// concrete, testable half of `AgentHostManager::pause` — the `AppHandle`
    /// wrapper around the production launcher (Concern 7: it cannot be built
    /// in a test harness) is the only part this does not cover, so this is
    /// where "pausing actually cancels in-flight brokered runs" is proven.
    ///
    /// Run ids are read from the slot's [`HostState`] BEFORE `on_disconnect`
    /// clears its session bookkeeping below — after that point they are gone.
    fn pause<L: RunLauncher>(&self, launcher: &L) {
        let slot = self.0.lock().unwrap().take();
        if let Some(slot) = slot {
            for run_id in slot.state.running_run_ids() {
                launcher.stop(&run_id);
            }
            slot.running.store(false, Ordering::SeqCst);
            drop(slot.outbound);
            slot.state.on_disconnect();
        }
    }

    /// Called by a loop as it exits. Clears the slot **only if it is still that
    /// loop's slot**, so a loop that outlived a restart cannot tear down its
    /// successor — and so a loop that exited on its own leaves no stale slot for
    /// `republish` to enqueue into.
    ///
    /// The identity test is `Arc::ptr_eq`, and it is free of pointer-ABA for a
    /// reason worth stating: the caller — the spawned task in `ensure_started` —
    /// holds its **own strong `Arc` clone** of the flag for the whole life of
    /// the loop and passes a borrow of it here. That keeps the allocation alive,
    /// so a later `Arc::new` cannot land on the same address and make a
    /// different loop's flag compare equal. Do not "optimise" that clone into a
    /// `Weak`, or a raw pointer, or drop it before this call.
    fn retire(&self, running: &Arc<AtomicBool>) {
        let mut current = self.0.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.running, running))
        {
            *current = None;
            log::info!("relay: host loop exited; no longer hosting");
        }
    }
}

/// Owns the host loop's lifetime. Mirrors `ServiceSyncManager`'s shape: managed
/// in Tauri state, started idempotently, and a no-op until configured.
pub struct AgentHostManager {
    app: AppHandle,
    runs: Arc<AgentRunManager>,
    slots: HostSlots,
    /// The relay sink + the `agent-run-output` subscription are installed at
    /// most once, and only after the gate has already said yes.
    wired: AtomicBool,
}

impl AgentHostManager {
    pub fn new(app: &AppHandle, runs: Arc<AgentRunManager>) -> Self {
        Self {
            app: app.clone(),
            runs,
            slots: HostSlots::default(),
            wired: AtomicBool::new(false),
        }
    }

    fn state(&self) -> Option<Arc<HostState>> {
        self.slots.state()
    }

    /// Queue a message for the live socket. Dropped silently when nothing is
    /// connected — a frame for a dead session has nowhere to go.
    fn enqueue(&self, msg: HostMessage) {
        self.slots.enqueue(msg);
    }

    /// Start the host loop if it is not already running AND the feature is
    /// fully configured. Idempotent — safe from setup, after pairing, and on
    /// settings changes.
    ///
    /// **Nothing is constructed when the gate says no**: no state, no listener,
    /// no relay sink, no task and no socket.
    pub fn ensure_started(self: &Arc<Self>) {
        let settings = crate::settings::get_settings(&self.app);
        let token =
            crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
        let offers = offers_from_grants(&settings.sharing, &settings.agents);
        if !should_host(
            &settings.sharing,
            settings.service_enabled,
            !token.is_empty(),
            offers.len(),
        ) {
            return;
        }
        if settings.service_url.trim().is_empty() {
            return;
        }

        let state = Arc::new(HostState::new(
            settings.sharing.clone(),
            settings.agents.clone(),
        ));
        // Claims the slot and mints this loop's OWN flag; `None` means a loop is
        // already live, which is what makes this idempotent.
        let Some((running, rx)) = self.slots.install(Arc::clone(&state)) else {
            return;
        };
        self.wire_run_pipeline();

        let connector = WsConnector {
            url: relay_ws_url(&settings.service_url),
            token,
        };
        let launcher = Arc::new(AgentRunLauncher {
            manager: Arc::clone(&self.runs),
            app: self.app.clone(),
        });
        log::info!("relay: hosting {} shared agent(s)", offers.len());
        // A STRONG clone, held by the task for the whole life of the loop. It is
        // what makes `retire`'s `Arc::ptr_eq` free of pointer-ABA — see the note
        // there before changing this to a `Weak` or dropping it early.
        let flag = Arc::clone(&running);
        let manager = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            // Both `true` by construction: the gate above already established
            // that the service is paired and a device token exists. Unpairing
            // goes through `republish` → `pause`, not through this loop.
            run_host_loop(connector, launcher, state, running, true, true, rx).await;
            // Leave no stale slot behind: without this, a loop that exited on
            // its own would leave `state`/`outbound` set, and every later
            // `republish` would enqueue into a channel with no receiver — a
            // silently dead host with nothing in the log to say so.
            if let Some(manager) = manager.upgrade() {
                manager.slots.retire(&flag);
            }
        });
    }

    /// Subscribe to the run pipeline. Called only from `ensure_started`, after
    /// the gate, and at most once.
    fn wire_run_pipeline(self: &Arc<Self>) {
        if self
            .wired
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        // Terminal status: one additive arm in `finalize`, reached only by a run
        // whose sinks carry `Relay`.
        self.runs
            .set_relay_sink(Arc::clone(self) as Arc<dyn RelayFrameSink>);

        // Live output: subscribe to the event the run pipeline ALREADY emits
        // (`agent-run-output`). Nothing in `drive_run` changes; a chunk for a
        // run this host does not own is dropped by `frame_for_run`.
        let host = Arc::clone(self);
        AgentRunOutput::listen(&self.app, move |ev| {
            let Some(state) = host.state() else {
                return;
            };
            if let Some(msg) = state.frame_for_run(
                &ev.payload.run_id,
                HostFrame::Output {
                    chunk: ev.payload.chunk.clone(),
                },
            ) {
                host.enqueue(msg);
            }
        });
    }

    /// The production launcher, built fresh — cheap (two `Arc`/`AppHandle`
    /// clones) and stateless, so there is no reason to cache it.
    fn launcher(&self) -> AgentRunLauncher {
        AgentRunLauncher {
            manager: Arc::clone(&self.runs),
            app: self.app.clone(),
        }
    }

    /// Stop a batch of runs via the production launcher. Used by
    /// [`Self::republish`]'s revoke/disable/delete path — the narrower
    /// `sessions_to_stop` `HostState::apply_settings` returns when the loop
    /// stays live. The full-pause case is [`Self::pause`], which goes through
    /// [`HostSlots::pause`] instead so the slot-take and the stop happen
    /// under the one lock.
    fn stop_runs(&self, run_ids: &[String]) {
        if run_ids.is_empty() {
            return;
        }
        let launcher = self.launcher();
        for run_id in run_ids {
            launcher.stop(run_id);
        }
    }

    /// The master kill switch (DESIGN-shared-agents §8). Reached when sharing
    /// is switched off, the service is unpaired, the device token is gone, or
    /// the last grant is removed.
    ///
    /// This is deliberately **not** the same as [`HostState::on_disconnect`]
    /// (ruling R4), which leaves local runs alone because a network blip is
    /// not the owner's decision. Pausing IS the owner's decision, so every run
    /// this host is serving for a teammate is stopped here too, not merely
    /// forgotten — a socket that closes while the process it was serving
    /// keeps going (or, worse, keeps being reachable to republish offers) is
    /// exactly the bug the sibling C1 branch shipped for device revocation.
    pub fn pause(&self) {
        self.slots.pause(&self.launcher());
    }

    /// Re-publish the offer list after ANY settings change, so a revoked grant
    /// disappears immediately rather than at the next reconnect — and so the
    /// live connection re-authorises against the new settings.
    ///
    /// **KNOWN DEFECT, found by the live end-to-end task and deliberately not
    /// fixed here: a live loop keeps dialling with the token it was born with.**
    /// [`Self::ensure_started`] builds the [`WsConnector`] — URL *and* device
    /// token — once, and the loop owns it for its whole life. The `(true,
    /// Some(state))` arm below only installs fresh settings and pushes a
    /// `hello`, and `ensure_started` is a no-op while a loop is live. So after
    /// the owner unpairs and re-pairs (a new device token in the keyring), or
    /// edits `service_url`, the host goes on dialling with the DEAD credential
    /// until the app restarts — and, since v0.15.7's pairing writes the token
    /// without restarting anything, the owner has no signal that it did.
    /// Reproduce by revoking the device (`DELETE /v1/devices/{id}`), watching
    /// the loop log `dial_was_refused` at `error`, then re-pairing: it keeps
    /// failing. The fix is for `republish` to compare the connector's inputs
    /// and `pause()` + `ensure_started()` when they change, which is a
    /// behavioural change that belongs with the settings/UI task rather than
    /// with a verification task. Recorded here, not only in
    /// `verification/shared-agents/RESULTS.md`, because this is the function
    /// whoever fixes it will be reading.
    pub fn republish(self: &Arc<Self>, settings: &AppSettings) {
        // Hot path. Every settings write in the app reaches here, so the common
        // case — sharing never configured, no loop running — must be nearly
        // free. It is: `settings` is the struct the caller had already built, so
        // there is no store read, and the OS keyring is not touched until after
        // this check.
        if republish_is_a_no_op(&settings.sharing, self.slots.is_hosting()) {
            return;
        }
        let token =
            crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
        let offers = offers_from_grants(&settings.sharing, &settings.agents);
        let wanted = should_host(
            &settings.sharing,
            settings.service_enabled,
            !token.is_empty(),
            offers.len(),
        );

        if !wanted {
            // No longer eligible: the master kill switch. `pause` stops every
            // live run itself (a fully-disabled config isn't the only way to
            // get here — service unpaired / token revoked leave `sharing`
            // untouched, so `authorize_open` alone cannot be trusted to name
            // every session that must go), THEN closes the socket.
            self.pause();
            return;
        }
        // `is_hosting` — not merely "a slot exists" — because a loop on its way
        // out still has a slot, and enqueueing into its channel would drop the
        // republish on the floor.
        match (self.slots.is_hosting(), self.state()) {
            // Still hosting: reconcile the live state against the NEW settings.
            // `apply_settings` re-runs `authorize_open` for every live session,
            // so a revoked member, a disabled agent or a deleted grant all stop
            // their run right here — the next `open` was already covered by
            // installing the config, but an ALREADY-RUNNING session needed this
            // too. Only push a fresh `hello` when the offer list actually
            // changed, so an unrelated edit does not churn the service.
            (true, Some(state)) => {
                let delta = state.apply_settings(settings.sharing.clone(), settings.agents.clone());
                self.stop_runs(&delta.sessions_to_stop);
                if delta.republish {
                    self.enqueue(HostMessage::Hello { offers });
                }
            }
            // Newly eligible (a first grant, a fresh pairing), or the previous
            // loop has stopped and a fresh one is owed.
            _ => self.ensure_started(),
        }
    }
}

/// The terminal frame for a brokered run. Reached from `finalize` only when the
/// run carries the `Relay` sink, which only `brokered_agent` adds.
impl RelayFrameSink for AgentHostManager {
    fn on_terminal(&self, run_id: &str, status: &RunStatus) {
        let Some(state) = self.state() else {
            return;
        };
        let outcome = run_status_outcome(status);
        if let Some(msg) = state.frame_for_run(
            run_id,
            HostFrame::Status {
                status: outcome.clone(),
            },
        ) {
            self.enqueue(msg);
        }
        if let Some(msg) = state.close_run(run_id, &outcome) {
            self.enqueue(msg);
        }
    }
}

/// Re-publish this host's offers because settings changed.
///
/// Called from **`settings::write_settings`** — the single choke point every
/// settings write already goes through — rather than from each of the ~40
/// scattered call sites. That is deliberate: "remember to call republish after
/// touching `settings.sharing`" is exactly the kind of convention this task
/// exists to abolish, and the failure mode is not merely a stale offer. A
/// revoked grant that never reaches the live `HostState` keeps being honoured
/// until the next reconnect.
///
/// Cost on that path, stated precisely rather than waved at: `settings` is the
/// struct `write_settings` had already built, so there is **no store read and no
/// deserialize** here. When sharing has never been configured and no loop is
/// live, the whole call is a `try_state` lookup, an atomic load and a bool test,
/// and it returns before touching the OS keyring — which matters because the
/// keyring read is a synchronous syscall on the calling thread, and the callers
/// include `shortcut/mod.rs`'s ~56 sites. Once sharing IS configured, a settings
/// write does pay that keyring read; still nowhere near the dictation path.
pub fn republish_offers(app: &AppHandle, settings: &AppSettings) {
    use tauri::Manager;
    let Some(host) = app.try_state::<Arc<AgentHostManager>>() else {
        // Before `initialize_core_logic` has run, or in a test harness.
        return;
    };
    host.inner().republish(settings);
}

// ---------------------------------------------------------------------------
// The connect loop
// ---------------------------------------------------------------------------

/// How long to wait before reconnecting, given how long the connection that
/// just ended actually lasted.
///
/// A session that ran for a while is evidence the service is healthy, so the
/// backoff resets. A connection that was accepted and dropped again inside the
/// floor is a FLAPPING service, and resetting there would quietly turn "capped
/// exponential backoff" into a 1 Hz reconnect loop against a struggling server
/// — which is how a backoff stops being a backoff.
fn backoff_after_session(previous: Duration, session_len: Duration) -> Duration {
    if session_len >= BACKOFF_MIN {
        next_backoff(Duration::ZERO)
    } else {
        next_backoff(previous)
    }
}

/// Connect → publish → serve → reconnect with capped exponential backoff.
///
/// A dropped socket ends the SESSIONS and never restarts a run: an interrupted
/// run is reported terminated by the service, not silently replayed here. C0
/// paid for that lesson twice.
async fn run_host_loop<C: RelayConnector, L: RunLauncher>(
    connector: C,
    launcher: Arc<L>,
    state: Arc<HostState>,
    running: Arc<AtomicBool>,
    service_enabled: bool,
    has_token: bool,
    mut outbound: mpsc::UnboundedReceiver<HostMessage>,
) {
    let mut backoff = Duration::ZERO;

    while running.load(Ordering::SeqCst) {
        // Re-gated on EVERY reconnect against the live config, so grants
        // revoked while the socket was down are never re-offered.
        let (sharing, _) = state.snapshot();
        let offers = state.offers();
        let attempt =
            maybe_connect(&sharing, service_enabled, has_token, &offers, &connector).await;

        let conn = match attempt {
            // The gate closed underneath us (the last grant was revoked while
            // we were disconnected). Stand down rather than dial.
            None => {
                log::info!("relay: nothing left to publish; standing down");
                break;
            }
            Some(Ok(conn)) => Arc::new(conn),
            Some(Err(e)) => {
                backoff = next_backoff(backoff);
                // A credential the service REFUSED and a service that never
                // answered are the same retry, but they are not the same
                // problem, and until the live end-to-end task nothing could
                // tell them apart in the log. Verified against a real
                // openflow-service: a revoked device token gets HTTP 401 on
                // the upgrade; an outage gets `Connection refused`.
                //
                // The remedy travels INSIDE the message (`refused_message`),
                // never appended here: only a 401 is fixed by re-pairing, and
                // a 403 (paired but not bound to a member) or a 404 (a service
                // older than v0.2) needs a different action entirely. Review
                // Important 2 — the first cut appended one remedy to all of
                // them.
                if dial_was_refused(&e) {
                    log::error!("relay: {e}. Retrying in {}s", backoff.as_secs());
                } else {
                    log::warn!(
                        "relay: connect failed ({e}); retrying in {}s",
                        backoff.as_secs()
                    );
                }
                sleep_interruptible(backoff, &running).await;
                continue;
            }
        };

        // The FOURTH window the flag is read in. A dial is not instant — it can
        // take up to `CONNECT_TIMEOUT` — and a `stop()` can land inside it.
        // Publishing here would put a REVOKED offer list onto a socket opened by
        // a loop that has already been told to stand down.
        if !running.load(Ordering::SeqCst) {
            conn.close().await;
            break;
        }

        let session_started = std::time::Instant::now();

        match state.publish(conn.as_ref()).await {
            Ok(()) => serve_connection(&conn, &launcher, &state, &running, &mut outbound).await,
            Err(e) => log::warn!("relay: could not publish offers ({e})"),
        }

        conn.close().await;
        state.on_disconnect();
        // Frames still queued belong to sessions the service has forgotten.
        // Discarding them is the point: replaying a dead session's output onto
        // the NEXT connection would be worse than losing it.
        while outbound.try_recv().is_ok() {}

        if !running.load(Ordering::SeqCst) {
            break;
        }
        backoff = backoff_after_session(backoff, session_started.elapsed());
        sleep_interruptible(backoff, &running).await;
    }

    running.store(false, Ordering::SeqCst);
    log::debug!("relay: host loop stopped");
}

/// Serve one connection until the socket closes or hosting stops.
///
/// Inbound lines are pumped through a channel rather than `select!`-ing on
/// `transport.recv()` directly: `mpsc::Receiver::recv` is cancel-safe, a
/// half-read WebSocket frame is not.
async fn serve_connection<T: RelayTransport, L: RunLauncher>(
    conn: &Arc<T>,
    launcher: &Arc<L>,
    state: &Arc<HostState>,
    running: &Arc<AtomicBool>,
    outbound: &mut mpsc::UnboundedReceiver<HostMessage>,
) {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    let reader = {
        let conn = Arc::clone(conn);
        tokio::spawn(async move {
            while let Some(line) = conn.recv().await {
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        })
    };

    loop {
        tokio::select! {
            incoming = line_rx.recv() => match incoming {
                Some(line) => match serde_json::from_str::<ServiceMessage>(&line) {
                    Ok(msg) => {
                        // A write failure while answering ends the connection,
                        // exactly as it does on the outbound arm below.
                        if state
                            .handle_service_message(msg, conn.as_ref(), launcher.as_ref())
                            .await
                            .is_break()
                        {
                            break;
                        }
                    }
                    // Not fatal, by design: a service that speaks a dialect we
                    // cannot parse must not take the host down.
                    Err(e) => log::debug!("relay: ignoring an unparseable frame ({e})"),
                },
                // The socket closed.
                None => break,
            },
            queued = outbound.recv() => match queued {
                Some(msg) => {
                    if let Err(e) = send_message(conn.as_ref(), msg).await {
                        log::warn!("relay: send failed ({e}); dropping the connection");
                        break;
                    }
                }
                // The manager stopped and dropped the sender. `HostSlots::stop`
                // clears the flag and drops this sender in ONE operation, and is
                // the only external path that clears a live loop's flag — so
                // this arm fires on any stop, and it is why a stopped loop can
                // never get as far as authorising an inbound `open`.
                None => {
                    running.store(false, Ordering::SeqCst);
                    break;
                }
            },
        }

        if !running.load(Ordering::SeqCst) {
            break;
        }
    }

    reader.abort();
}

/// Sleep in slices so a `stop()` is observed promptly rather than after a full
/// backoff (mirrors `service_sync`'s worker).
async fn sleep_interruptible(total: Duration, running: &AtomicBool) {
    const STEP: Duration = Duration::from_millis(250);
    let mut remaining = total;
    while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
        let slice = STEP.min(remaining);
        tokio::time::sleep(slice).await;
        remaining -= slice;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AgentOutputSink, ShareGrant};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    /// Run a future to completion on a current-thread runtime with the time
    /// driver enabled (the repo uses no `#[tokio::test]` — see a2a.rs:1686).
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn agent(id: &str) -> AgentDefinition {
        serde_json::from_value(json!({
            "id": id, "name": format!("Agent {id}"), "enabled": true,
            "binding_id": format!("agent:{id}"), "provider_id": "",
            "kind": "cli", "cli_type": "claude", "binary_path": "claude",
            "command_template": "-p", "project_path": "/home/me/personal"
        }))
        .unwrap()
    }

    fn sharing_with_grant() -> SharingConfig {
        SharingConfig {
            enabled: true,
            grants: vec![ShareGrant {
                agent_id: "coder".into(),
                project_path: "/repo/site".into(),
                allowed_members: vec!["m-priya".into()],
            }],
        }
    }

    /// A ready-to-run `open` for `sharing_with_grant()`'s "coder" offer.
    fn open_msg(session_id: &str, member_id: &str) -> ServiceMessage {
        serde_json::from_value(json!({
            "t": "open", "session_id": session_id, "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": member_id, "display_name": "Priya"},
            "payload": {"instruction": "add a comment to README"}
        }))
        .unwrap()
    }

    /// Scripted transport: replays canned inbound lines, records outbound ones.
    struct FakeTransport {
        inbound: Mutex<std::collections::VecDeque<String>>,
        /// Shared so a loop test can still read what was written after the
        /// connector has taken ownership of the transport.
        sent: Arc<Mutex<Vec<String>>>,
        /// When set, `recv` never resolves once the script is exhausted, which
        /// models a socket that is simply idle rather than closed.
        hang_when_drained: bool,
        /// When set, `close` clears this flag — the loop tests' way of saying
        /// "the app shut down as the socket went away", so they exit without
        /// waiting out a real reconnect backoff.
        stop_on_close: Mutex<Option<Arc<AtomicBool>>>,
        /// A socket that is still READABLE but no longer writable — the exact
        /// half-broken state that made losing a header silent.
        fail_sends: bool,
    }

    impl FakeTransport {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                inbound: Mutex::new(lines.iter().map(|s| s.to_string()).collect()),
                sent: Arc::new(Mutex::new(Vec::new())),
                hang_when_drained: false,
                stop_on_close: Mutex::new(None),
                fail_sends: false,
            }
        }
        fn write_broken() -> Self {
            let mut t = Self::new(vec![]);
            t.fail_sends = true;
            t
        }
        fn idle(lines: Vec<&str>) -> Self {
            let mut t = Self::new(lines);
            t.hang_when_drained = true;
            t
        }
        fn stopping(lines: Vec<&str>, running: Arc<AtomicBool>) -> Self {
            let t = Self::new(lines);
            *t.stop_on_close.lock().unwrap() = Some(running);
            t
        }
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
        /// A handle on the outbound log that outlives the transport itself.
        fn log(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.sent)
        }
    }

    impl RelayTransport for FakeTransport {
        fn send(
            &self,
            line: String,
        ) -> impl std::future::Future<Output = Result<(), String>> + Send {
            self.sent.lock().unwrap().push(line);
            let broken = self.fail_sends;
            async move {
                if broken {
                    return Err("broken pipe".to_string());
                }
                Ok(())
            }
        }
        fn recv(&self) -> impl std::future::Future<Output = Option<String>> + Send {
            let next = self.inbound.lock().unwrap().pop_front();
            let hang = self.hang_when_drained && next.is_none();
            async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                next
            }
        }
        fn close(&self) -> impl std::future::Future<Output = ()> + Send {
            if let Some(running) = self.stop_on_close.lock().unwrap().take() {
                running.store(false, Ordering::SeqCst);
            }
            async {}
        }
    }

    /// Records launches instead of spawning anything. NO test spawns a process.
    #[derive(Default)]
    struct FakeLauncher {
        launched: Mutex<Vec<(AgentDefinition, String)>>,
        requesters: Mutex<Vec<String>>,
        stopped: Mutex<Vec<String>>,
    }
    impl RunLauncher for FakeLauncher {
        fn launch(&self, agent: BrokeredRun, instruction: String) -> String {
            self.requesters
                .lock()
                .unwrap()
                .push(agent.requester().to_string());
            let mut l = self.launched.lock().unwrap();
            // `BrokeredRun` derefs to the definition for reading; the token
            // itself is what `launch` demanded, and no test can forge one.
            l.push(((*agent).clone(), instruction));
            format!("run-{}", l.len())
        }
        fn stop(&self, run_id: &str) {
            self.stopped.lock().unwrap().push(run_id.to_string());
        }
    }

    /// Hands out a scripted transport per connect.
    struct FakeConnector {
        conns: Mutex<std::collections::VecDeque<FakeTransport>>,
    }
    impl FakeConnector {
        fn new(conns: Vec<FakeTransport>) -> Self {
            Self {
                conns: Mutex::new(conns.into_iter().collect()),
            }
        }
    }
    impl RelayConnector for FakeConnector {
        type Conn = FakeTransport;
        fn connect(
            &self,
        ) -> impl std::future::Future<Output = Result<FakeTransport, String>> + Send {
            let next = self.conns.lock().unwrap().pop_front();
            async move { next.ok_or_else(|| "no more connections scripted".to_string()) }
        }
    }

    #[test]
    fn no_connect_attempt_when_sharing_is_off() {
        // DESIGN-shared-agents §6, asserted by test rather than by reading the
        // code: with sharing unconfigured NOTHING dials out.
        struct CountingConnector(AtomicUsize);
        impl RelayConnector for CountingConnector {
            type Conn = FakeTransport;
            fn connect(
                &self,
            ) -> impl std::future::Future<Output = Result<FakeTransport, String>> + Send
            {
                self.0.fetch_add(1, Ordering::SeqCst);
                async { Err("should never be called".to_string()) }
            }
        }

        let agents = vec![agent("coder")];
        let off = SharingConfig::default();
        let offers = offers_from_grants(&off, &agents);
        let connector = CountingConnector(AtomicUsize::new(0));
        block_on(maybe_connect(&off, true, true, &offers, &connector));
        assert_eq!(
            connector.0.load(Ordering::SeqCst),
            0,
            "no socket may be opened"
        );

        // …and the same holds for every other way the feature can be
        // unconfigured, because they are all the same gate.
        let on = sharing_with_grant();
        let offers = offers_from_grants(&on, &agents);
        assert_eq!(offers.len(), 1);
        block_on(maybe_connect(&on, false, true, &offers, &connector)); // unpaired
        block_on(maybe_connect(&on, true, false, &offers, &connector)); // no token
        block_on(maybe_connect(&on, true, true, &[], &connector)); // no offers
        assert_eq!(
            connector.0.load(Ordering::SeqCst),
            0,
            "no socket may be opened"
        );

        // Positive control: with everything configured it DOES dial (and the
        // failure above is the gate, not a connector that never works).
        block_on(maybe_connect(&on, true, true, &offers, &connector));
        assert_eq!(connector.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn republishing_is_free_when_sharing_was_never_configured() {
        // `write_settings` is the app's single settings choke point and every
        // write now calls republish. That is only acceptable because a dormant
        // config with no live loop returns before reading the OS keyring.
        let dormant = SharingConfig::default();
        assert!(dormant.is_dormant());
        assert!(republish_is_a_no_op(&dormant, false));

        // …but a live loop must still be told, even with a now-dormant config —
        // that is exactly the "sharing was switched off" case, which has to
        // reach `stop()`.
        assert!(!republish_is_a_no_op(&dormant, true));
        // …and a configured host always does the work.
        assert!(!republish_is_a_no_op(&sharing_with_grant(), false));
        assert!(!republish_is_a_no_op(&sharing_with_grant(), true));
    }

    #[test]
    fn the_gate_requires_switch_grants_pairing_and_a_token_all_at_once() {
        let agents = vec![agent("coder")];
        let on = sharing_with_grant();
        let offers = offers_from_grants(&on, &agents);
        assert_eq!(offers.len(), 1);

        assert!(should_host(&on, true, true, offers.len()));
        assert!(
            !should_host(&SharingConfig::default(), true, true, 0),
            "switch off"
        );
        // The master switch pinned ON ITS OWN. Without this case, deleting
        // `sharing.enabled &&` from `should_host` would still pass every
        // assertion here, because a disabled config also yields zero offers —
        // i.e. the belt would be tested and the braces would not.
        assert!(
            !should_host(
                &SharingConfig {
                    enabled: false,
                    grants: sharing_with_grant().grants,
                },
                true,
                true,
                1
            ),
            "the master switch alone must be able to refuse"
        );
        assert!(
            !should_host(
                &SharingConfig {
                    enabled: true,
                    grants: vec![]
                },
                true,
                true,
                0
            ),
            "no grants"
        );
        assert!(
            !should_host(&on, false, true, offers.len()),
            "service not paired"
        );
        assert!(
            !should_host(&on, true, false, offers.len()),
            "no device token"
        );
    }

    #[test]
    fn hello_publishes_the_offers_on_connect() {
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents);
        let t = FakeTransport::new(vec![]);
        block_on(state.publish(&t)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["t"], json!("hello"));
        assert_eq!(v["offers"][0]["action_id"], json!("agent:coder"));
        assert_eq!(v["offers"][0]["project"], json!("/repo/site"));
    }

    #[test]
    fn an_authorised_open_launches_in_the_grants_folder_and_answers_with_a_header() {
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();

        let msg: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": {"instruction": "add a comment to README"}
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(msg, &t, &l));

        let launched = l.launched.lock().unwrap();
        assert_eq!(launched.len(), 1);
        // The grant's folder — NEVER the agent's own project_path.
        assert_eq!(launched[0].0.project_path, "/repo/site");
        assert!(launched[0].0.output_sinks.contains(&AgentOutputSink::Relay));
        assert_eq!(launched[0].1, "add a comment to README");
        // The panel label travels with the run, so no run exists unlabelled.
        assert_eq!(
            l.requesters.lock().unwrap().as_slice(),
            ["Priya".to_string()]
        );

        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["kind"], json!("header"));
        assert_eq!(v["session_id"], json!("s1"));
        assert_eq!(v["sealed"], json!(false));
        assert_eq!(v["payload"]["project"], json!("/repo/site"));

        // The run is now owned, so its output can be routed back.
        assert_eq!(state.session_for_run("run-1").as_deref(), Some("s1"));
    }

    #[test]
    fn a_run_that_reaches_a_terminal_before_launch_returns_still_gets_its_closed() {
        // *** Review Important 3: the registration window, and it is a HANG. ***
        //
        // `launch` returns a run id and the run is registered afterwards, so a
        // run that terminates in between is invisible to both `frame_for_run`
        // and `close_run`. The trigger is mundane: `AgentRunManager::start`
        // spawns on a MULTI-THREADED runtime and its spawn-failure path (a bad
        // `binary_path`) emits, logs and calls `finalize` -> `on_terminal`
        // immediately.
        //
        // The consequence is not a lost line — Concern #3 as first written
        // undersold it. Both calls answer `None`, so NO `closed` is ever sent
        // and `by_run`/`sessions` keep an entry nothing will ever remove: the
        // requester's SSE stream hangs until the socket drops. DESIGN-relay-v02
        // §6 exists to forbid exactly that.
        //
        // The single production edit this catches: moving
        // `let mut by_run = self.by_run.lock()` back below `launcher.launch(...)`.
        struct InstantTerminalLauncher {
            state: Mutex<Option<Arc<HostState>>>,
            /// `(the status frame was routed, the closed envelope was minted)`.
            results: Arc<Mutex<Vec<(bool, bool)>>>,
            reached_terminal: Arc<AtomicBool>,
            joiner: Mutex<Option<std::thread::JoinHandle<()>>>,
        }
        impl RunLauncher for InstantTerminalLauncher {
            fn launch(&self, _agent: BrokeredRun, _instruction: String) -> String {
                let state = self.state.lock().unwrap().clone().unwrap();
                let results = Arc::clone(&self.results);
                let reached = Arc::clone(&self.reached_terminal);
                let handle = std::thread::spawn(move || {
                    reached.store(true, Ordering::SeqCst);
                    // Verbatim what `RelayFrameSink::on_terminal` does.
                    let status = state
                        .frame_for_run(
                            "run-1",
                            HostFrame::Status {
                                status: "failed".into(),
                            },
                        )
                        .is_some();
                    let closed = state.close_run("run-1", "failed").is_some();
                    results.lock().unwrap().push((status, closed));
                });
                // Hand the terminal a decisive head start. Without the lock
                // held across this call it wins outright; with it, it is
                // parked on `by_run` for the whole 50ms and finds the
                // registration the moment it is released.
                while !self.reached_terminal.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                std::thread::sleep(Duration::from_millis(50));
                *self.joiner.lock().unwrap() = Some(handle);
                "run-1".to_string()
            }
            fn stop(&self, _run_id: &str) {}
        }

        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let results = Arc::new(Mutex::new(Vec::new()));
        let launcher = InstantTerminalLauncher {
            state: Mutex::new(Some(Arc::clone(&state))),
            results: Arc::clone(&results),
            reached_terminal: Arc::new(AtomicBool::new(false)),
            joiner: Mutex::new(None),
        };
        let t = FakeTransport::new(vec![]);

        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": {"instruction": "run a binary that does not exist"}
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(open, &t, &launcher));
        launcher
            .joiner
            .lock()
            .unwrap()
            .take()
            .expect("the terminal thread was never handed over")
            .join()
            .expect("the terminal thread panicked");

        let results = results.lock().unwrap().clone();
        assert_eq!(results.len(), 1);
        let (status_routed, closed_minted) = results[0];
        assert!(
            status_routed,
            "the terminal status frame was dropped: the run finished before it \
             was registered, so `frame_for_run` could not find its session"
        );
        assert!(
            closed_minted,
            "NO `closed` was sent for a run that terminated instantly — the \
             requester's stream has nothing to end it and hangs until the \
             socket drops (DESIGN-relay-v02 §6)"
        );
        // …and the bookkeeping is clean rather than permanently orphaned.
        assert!(
            state.session_for_run("run-1").is_none(),
            "a closed run must leave no entry behind"
        );
        assert!(
            state.close_run("run-1", "failed").is_none(),
            "and `closed` is still sent exactly once"
        );
    }

    #[test]
    fn an_open_the_relay_authorised_but_the_host_refuses_never_launches() {
        // The two-independent-checks rule, exercised: the service let this
        // through; this desktop refuses and closes the session cleanly.
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();

        let msg: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s9", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-stranger", "display_name": "Stranger"},
            "payload": {"instruction": "rm -rf /"}
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(msg, &t, &l));

        assert!(l.launched.lock().unwrap().is_empty(), "nothing may run");
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["t"], json!("closed"));
        assert_eq!(v["session_id"], json!("s9"));
        assert_eq!(v["outcome"], json!("denied"));
    }

    #[test]
    fn a_grant_revoked_since_the_offer_was_published_is_refused_on_the_live_socket() {
        // "Republish on any settings change" is the fast path; THIS is the
        // guarantee. Even if the service still holds the old offer, the host
        // re-checks against the settings in force right now.
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents.clone());
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();

        // The owner hits "pause sharing" (or deletes the grant).
        state.set_config(SharingConfig::default(), agents);
        assert!(state.offers().is_empty(), "nothing is offered any more");

        let msg: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(msg, &t, &l));

        assert!(l.launched.lock().unwrap().is_empty(), "nothing may run");
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["outcome"], json!("denied"));
    }

    // -----------------------------------------------------------------------
    // Task 6: pause, revoke, republish — `HostState::apply_settings` and the
    // master kill switch (`HostSlots::pause` / `AgentHostManager::pause`).
    // -----------------------------------------------------------------------

    #[test]
    fn pausing_sharing_stops_every_in_flight_brokered_run() {
        // DESIGN-shared-agents §8: pause is a MASTER KILL SWITCH — it drops the
        // socket and cancels in-flight brokered runs.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let delta = state.apply_settings(SharingConfig::default(), vec![agent("coder")]);
        assert!(delta.republish);
        assert_eq!(delta.sessions_to_stop, vec!["run-1".to_string()]);
    }

    #[test]
    fn revoking_a_member_stops_their_running_session_and_republishes() {
        // §8: "Revoking a member's grant takes effect on the next open;
        // already-running sessions are stopped."
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let revoked = SharingConfig {
            enabled: true,
            grants: vec![ShareGrant {
                agent_id: "coder".into(),
                project_path: "/repo/site".into(),
                allowed_members: vec!["m-someone-else".into()],
            }],
        };
        let delta = state.apply_settings(revoked, vec![agent("coder")]);
        assert!(delta.republish, "the offer's allowed list changed");
        assert_eq!(delta.sessions_to_stop, vec!["run-1".to_string()]);
    }

    #[test]
    fn an_unrelated_settings_change_neither_republishes_nor_stops_anything() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let delta = state.apply_settings(sharing_with_grant(), vec![agent("coder")]);
        assert!(
            !delta.republish,
            "identical offers must not churn the service"
        );
        assert!(
            delta.sessions_to_stop.is_empty(),
            "a live teammate is not interrupted"
        );
    }

    #[test]
    fn disabling_the_agent_itself_stops_its_brokered_runs() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let mut off = agent("coder");
        off.enabled = false;
        let delta = state.apply_settings(sharing_with_grant(), vec![off]);
        assert!(delta.republish);
        assert_eq!(delta.sessions_to_stop, vec!["run-1".to_string()]);
    }

    #[test]
    fn deleting_a_grant_entirely_stops_its_brokered_runs() {
        // The fourth transition the unification claims to cover: the owner
        // removes the WHOLE grant (shrinks `sharing.grants`), not just a
        // member from it or the agent it points at. Same `authorize_open`
        // path as a revoked member (`DenyReason::NoGrant`) — functionally
        // covered by inspection already, but the review asked for all four
        // to be tested, not three of four.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let grant_deleted = SharingConfig {
            enabled: true,
            grants: vec![],
        };
        let delta = state.apply_settings(grant_deleted, vec![agent("coder")]);
        assert!(delta.republish, "the offer disappears entirely");
        assert_eq!(delta.sessions_to_stop, vec!["run-1".to_string()]);
    }

    #[test]
    fn config_installs_before_the_stop_list_is_computed_so_a_racing_open_sees_the_new_settings() {
        // Regression for the Task 6 review's "Important" finding:
        // `apply_settings` used to compute `sessions_to_stop` BEFORE
        // installing the new config. `handle_service_message`'s `Open` arm
        // reads `self.config` live (via its own `self.snapshot()`), fully
        // unlocked from this function's session read — so a concurrent
        // `open` for a member being revoked right now could authorise
        // against the STALE config and insert a session in the gap that this
        // call's already-computed stop list could never have known about.
        //
        // Reproduced deterministically rather than by timing luck, the same
        // technique `a_run_that_reaches_a_terminal_before_launch_returns_still_gets_its_closed`
        // uses: hold the lock `apply_settings` needs for its LATER step
        // (the session filter) so a background call blocks there, then
        // observe that the EARLIER step (the config install) has already
        // completed. `sessions` is a private field this `tests` submodule
        // can reach directly.
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));

        // Block `apply_settings`'s session-read/filter step by holding the
        // lock it needs for that step.
        let guard = state.sessions.lock().unwrap();

        let worker_state = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            worker_state.apply_settings(SharingConfig::default(), vec![agent("coder")])
        });

        // A decisive head start for the worker to reach (and block on) the
        // lock we are holding.
        std::thread::sleep(Duration::from_millis(100));
        // While it is blocked there, the new config must ALREADY be
        // installed — proving the install happened BEFORE the (still
        // blocked) stop-list computation. Under the pre-fix ordering this
        // would still show the OLD config: the worker would have blocked on
        // `sessions` for the filter FIRST, before ever reaching the install.
        assert!(
            state.offers().is_empty(),
            "the new config must install before the stop-list filter runs, \
             so a racing `open` reading `self.config` in this same window \
             sees the change"
        );

        drop(guard);
        let delta = handle.join().unwrap();
        assert!(delta.republish);
    }

    #[test]
    fn revoking_one_member_refuses_their_new_open_while_a_still_granted_member_succeeds() {
        // The positive control the verification bar asks for: if EVERY open
        // were refused here, the test would prove nothing about revocation
        // specifically — it could just as well be a bug that refuses
        // everyone. Priya losing her grant while Zola keeps hers, checked in
        // the SAME live state in the SAME test, is what makes the refusal
        // attributable to the revocation.
        let both_allowed = SharingConfig {
            enabled: true,
            grants: vec![ShareGrant {
                agent_id: "coder".into(),
                project_path: "/repo/site".into(),
                allowed_members: vec!["m-priya".into(), "m-zola".into()],
            }],
        };
        let agents = vec![agent("coder")];
        let state = HostState::new(both_allowed, agents.clone());
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();

        // Priya is revoked; Zola is untouched. Nobody has opened a session
        // yet, so this is purely "the next open" — the companion tests above
        // already cover an ALREADY-RUNNING session.
        let zola_only = SharingConfig {
            enabled: true,
            grants: vec![ShareGrant {
                agent_id: "coder".into(),
                project_path: "/repo/site".into(),
                allowed_members: vec!["m-zola".into()],
            }],
        };
        let delta = state.apply_settings(zola_only, agents);
        assert!(delta.sessions_to_stop.is_empty(), "nobody was running yet");

        let _ = block_on(state.handle_service_message(open_msg("s-priya", "m-priya"), &t, &l));
        let _ = block_on(state.handle_service_message(open_msg("s-zola", "m-zola"), &t, &l));

        assert_eq!(
            l.launched.lock().unwrap().len(),
            1,
            "only Zola's open may launch"
        );
        let priya_result: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(priya_result["outcome"], json!("denied"));
        let zola_result: serde_json::Value = serde_json::from_str(&t.sent()[1]).unwrap();
        assert_eq!(
            zola_result["kind"],
            json!("header"),
            "the still-granted member's open succeeds"
        );
    }

    #[test]
    fn pause_via_host_slots_stops_the_socket_and_every_in_flight_run() {
        // The concrete, testable half of `AgentHostManager::pause` — see
        // `HostSlots::pause`'s doc comment. `AgentHostManager` itself cannot
        // be constructed here (Concern 7: `AgentRunManager::start` needs a
        // real `AppHandle<Wry>`), so this is where "pausing actually cancels
        // in-flight brokered runs" is proven: a real `HostSlots`, a real
        // `HostState` with a live session, a `FakeLauncher` standing in only
        // for the process-spawning half of the production launcher.
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let _ = block_on(state.handle_service_message(open_msg("s1", "m-priya"), &t, &l));

        let slots = HostSlots::default();
        slots.install(Arc::clone(&state)).unwrap();
        assert!(slots.is_hosting());

        slots.pause(&l);

        assert_eq!(
            l.stopped.lock().unwrap().as_slice(),
            ["run-1".to_string()],
            "the in-flight run must be stopped, not merely forgotten"
        );
        assert!(!slots.is_hosting(), "the socket must close too");
    }

    #[test]
    fn an_open_with_no_instruction_is_closed_not_run_blank() {
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let msg: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s2", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": {}
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(msg, &t, &l));
        assert!(l.launched.lock().unwrap().is_empty());
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["outcome"], json!("bad_request"));
    }

    #[test]
    fn an_open_for_an_offer_this_host_cannot_resolve_is_closed_not_guessed() {
        // Spec gap #1: an older service may send only `offer_id`. Without a
        // cached mapping the host refuses — it never guesses which agent was
        // meant, because guessing is how the wrong folder gets a stranger's run.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let bare: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(bare.clone(), &t, &l));
        assert!(l.launched.lock().unwrap().is_empty());
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["outcome"], json!("unknown_offer"));

        // Once ONE open has carried both ids, the mapping is cached and a later
        // bare open resolves.
        let full: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s2", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(full, &t, &l));
        let _ = block_on(state.handle_service_message(bare, &t, &l));
        assert_eq!(l.launched.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_repeated_open_for_a_live_session_does_not_start_a_second_run() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(open.clone(), &t, &l));
        let _ = block_on(state.handle_service_message(open, &t, &l));

        assert_eq!(
            l.launched.lock().unwrap().len(),
            1,
            "a duplicate open must not start a second run"
        );
        // …and the session still points at the FIRST run, so its output keeps
        // flowing rather than being re-pointed at a run the requester never saw.
        assert_eq!(state.session_for_run("run-1").as_deref(), Some("s1"));
        let v: serde_json::Value = serde_json::from_str(&t.sent()[1]).unwrap();
        assert_eq!(v["t"], json!("closed"));
        assert_eq!(v["outcome"], json!("already_open"));
    }

    #[test]
    fn republishing_forgets_the_offer_id_cache() {
        // Offer ids are re-minted by the service on republish and may be reused
        // for a DIFFERENT action. Resolving a bare `open` through a stale entry
        // would run the wrong agent in the wrong grant folder.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let full: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(full, &t, &l));
        assert_eq!(
            l.launched.lock().unwrap().len(),
            1,
            "the cache is populated"
        );

        // The owner edits sharing; offers are republished.
        state.set_config(sharing_with_grant(), vec![agent("coder")]);

        let bare: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s2", "offer_id": "o1",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(bare, &t, &l));
        assert_eq!(
            l.launched.lock().unwrap().len(),
            1,
            "a stale offer_id must not resolve after a republish"
        );
        let v: serde_json::Value = serde_json::from_str(t.sent().last().unwrap()).unwrap();
        assert_eq!(v["outcome"], json!("unknown_offer"));
    }

    #[test]
    fn stop_from_the_requester_routes_to_the_existing_kill_channel() {
        let agents = vec![agent("coder")];
        let state = HostState::new(sharing_with_grant(), agents);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(open, &t, &l));

        let stop: ServiceMessage =
            serde_json::from_value(json!({"t": "stop", "session_id": "s1"})).unwrap();
        let _ = block_on(state.handle_service_message(stop, &t, &l));
        assert_eq!(l.stopped.lock().unwrap().as_slice(), ["run-1".to_string()]);
    }

    #[test]
    fn a_write_broken_socket_drops_the_connection_instead_of_serving_on() {
        // The half-broken case: still readable, no longer writable. The run is
        // launched (and deliberately left running — ruling R4), but the header
        // never reaches the requester. Serving on would leave the agent working
        // in the owner's folder with the teammate never told it started, and a
        // service retry refused `already_open`.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::write_broken();
        let l = FakeLauncher::default();
        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();

        let flow = block_on(state.handle_service_message(open, &t, &l));
        assert!(
            flow.is_break(),
            "a failed header write must drop the connection, not be swallowed"
        );
        // The run really did start, and is NOT killed by the write failure.
        assert_eq!(l.launched.lock().unwrap().len(), 1);
        assert!(l.stopped.lock().unwrap().is_empty());

        // A refusal that cannot be written is equally fatal to the connection.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::write_broken();
        let denied: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s2", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-stranger", "display_name": "Stranger"},
            "payload": "go"
        }))
        .unwrap();
        assert!(block_on(state.handle_service_message(denied, &t, &l)).is_break());
        assert_eq!(l.launched.lock().unwrap().len(), 1, "still nothing new ran");
    }

    #[test]
    fn a_stop_for_an_unknown_session_is_ignored_not_fatal() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let stop: ServiceMessage =
            serde_json::from_value(json!({"t": "stop", "session_id": "nope"})).unwrap();
        let _ = block_on(state.handle_service_message(stop, &t, &l));
        assert!(l.stopped.lock().unwrap().is_empty());
    }

    #[test]
    fn output_and_terminal_frames_are_routed_only_for_owned_runs() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(open, &t, &l));

        assert!(state
            .frame_for_run(
                "run-1",
                HostFrame::Output {
                    chunk: "line".into()
                }
            )
            .is_some());
        // A local hotkey run is not ours and must never leak to the relay.
        assert!(state
            .frame_for_run(
                "run-999",
                HostFrame::Output {
                    chunk: "secret".into()
                }
            )
            .is_none());

        // Terminal closes the session exactly once.
        assert!(state.close_run("run-1", "finished").is_some());
        assert!(
            state.close_run("run-1", "finished").is_none(),
            "closed is sent once"
        );
        assert!(state.session_for_run("run-1").is_none());
    }

    #[test]
    fn a_disconnect_terminates_sessions_and_never_restarts_a_run() {
        // DESIGN-shared-agents §5: an interrupted run is reported terminated,
        // NOT silently restarted. C0 paid for this lesson.
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let open: ServiceMessage = serde_json::from_value(json!({
            "t": "open", "session_id": "s1", "offer_id": "o1", "action_id": "agent:coder",
            "requester": {"member_id": "m-priya", "display_name": "Priya"},
            "payload": "go"
        }))
        .unwrap();
        let _ = block_on(state.handle_service_message(open, &t, &l));
        assert_eq!(l.launched.lock().unwrap().len(), 1);

        state.on_disconnect();
        assert!(
            state.session_for_run("run-1").is_none(),
            "the session is gone"
        );
        assert_eq!(
            l.launched.lock().unwrap().len(),
            1,
            "nothing is re-launched"
        );
        assert!(
            l.stopped.lock().unwrap().is_empty(),
            "the local run keeps going: killing a teammate's half-applied edit \
             because OUR socket blipped is worse than letting it finish in the \
             owner's own panel. The service reports host_disconnected."
        );

        // And after reconnect, its output is no longer routed anywhere.
        assert!(state
            .frame_for_run("run-1", HostFrame::Output { chunk: "x".into() })
            .is_none());
    }

    #[test]
    fn an_unknown_service_message_does_not_kill_the_loop() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let msg: ServiceMessage = serde_json::from_value(json!({"t": "ping_v3"})).unwrap();
        let _ = block_on(state.handle_service_message(msg, &t, &l));
        assert!(t.sent().is_empty());
        assert!(l.launched.lock().unwrap().is_empty());
    }

    // ---- The loop itself ----

    #[test]
    fn the_loop_publishes_then_serves_and_a_dropped_socket_ends_only_the_session() {
        let running = Arc::new(AtomicBool::new(true));
        let open = r#"{"t":"open","session_id":"s1","offer_id":"o1","action_id":"agent:coder","requester":{"member_id":"m-priya","display_name":"Priya"},"payload":{"instruction":"go"}}"#;
        // The script ends, so `recv` returns None — a closed socket. `close`
        // then clears `running`, standing in for the app shutting down, which
        // is what keeps this test from waiting out a real reconnect backoff.
        let transport = FakeTransport::stopping(vec![open], Arc::clone(&running));
        let sent = transport.log();
        let connector = FakeConnector::new(vec![transport]);
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let launcher = Arc::new(FakeLauncher::default());
        let (_tx, rx) = mpsc::unbounded_channel::<HostMessage>();

        block_on(run_host_loop(
            connector,
            Arc::clone(&launcher),
            Arc::clone(&state),
            Arc::clone(&running),
            true,
            true,
            rx,
        ));

        let sent = sent.lock().unwrap().clone();
        let hello: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(hello["t"], json!("hello"), "offers are published first");
        assert_eq!(hello["offers"][0]["action_id"], json!("agent:coder"));
        let header: serde_json::Value = serde_json::from_str(&sent[1]).unwrap();
        assert_eq!(header["kind"], json!("header"));

        assert_eq!(launcher.launched.lock().unwrap().len(), 1, "the open ran");
        assert!(
            launcher.stopped.lock().unwrap().is_empty(),
            "a dropped socket never kills the local run (ruling R4)"
        );
        assert!(
            state.session_for_run("run-1").is_none(),
            "the session is gone with the socket"
        );
        assert!(!running.load(Ordering::SeqCst));
    }

    #[test]
    fn the_loop_writes_queued_frames_to_the_socket_after_the_hello() {
        // The live-output path end to end, minus the socket: whatever the
        // agent-run-output listener enqueues is what the service receives.
        let running = Arc::new(AtomicBool::new(true));
        let transport = FakeTransport::idle(vec![]);
        let sent = transport.log();
        let connector = FakeConnector::new(vec![transport]);
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let launcher = Arc::new(FakeLauncher::default());
        let (tx, rx) = mpsc::unbounded_channel::<HostMessage>();
        tx.send(session_frame(
            "s1",
            HostFrame::Output {
                chunk: "hello from the agent".into(),
            },
        ))
        .unwrap();
        // Dropping the sender is the only thing that ends this loop: the
        // transport is idle rather than closed, so the ordering below is
        // deterministic rather than a race between the two select arms.
        drop(tx);

        block_on(run_host_loop(
            connector,
            launcher,
            Arc::clone(&state),
            Arc::clone(&running),
            true,
            true,
            rx,
        ));

        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "the hello, then the queued frame");
        let frame: serde_json::Value = serde_json::from_str(&sent[1]).unwrap();
        assert_eq!(frame["t"], json!("frame"));
        assert_eq!(frame["session_id"], json!("s1"));
        assert_eq!(frame["kind"], json!("output"));
        assert_eq!(frame["payload"]["chunk"], json!("hello from the agent"));
        assert!(!running.load(Ordering::SeqCst), "the loop stopped cleanly");
    }

    #[test]
    fn a_flapping_service_is_backed_off_instead_of_reconnected_at_one_hertz() {
        // A session that actually lasted resets the wait to the floor…
        assert_eq!(
            backoff_after_session(Duration::from_secs(16), Duration::from_secs(300)),
            BACKOFF_MIN
        );
        assert_eq!(
            backoff_after_session(Duration::ZERO, BACKOFF_MIN),
            BACKOFF_MIN
        );
        // …but a socket accepted and dropped inside the floor escalates, so a
        // struggling service is not hammered once a second forever.
        let mut b = Duration::ZERO;
        b = backoff_after_session(b, Duration::from_millis(20));
        assert_eq!(b, Duration::from_secs(1));
        b = backoff_after_session(b, Duration::from_millis(20));
        assert_eq!(b, Duration::from_secs(2));
        b = backoff_after_session(b, Duration::from_millis(20));
        assert_eq!(b, Duration::from_secs(4), "it must actually escalate");
    }

    #[test]
    fn a_stopped_loop_stays_stopped_when_the_manager_restarts() {
        // *** Regression: the stale-loop revival. ***
        //
        // `stop()` then `ensure_started()` is an ordinary sequence — revoke the
        // last grant on agent A (gate false ⇒ stop), then share agent B (gate
        // true ⇒ start). When both loops shared ONE `Arc<AtomicBool>`, the
        // restart re-armed the flag the stopped loop was still watching, so
        // loop 1 woke up inside its backoff and carried on hosting from the
        // `HostState` it captured at birth — i.e. it kept publishing and
        // authorising the REVOKED grant, alongside the new loop.
        //
        // The single production edit this catches: making `HostSlots::install`
        // reuse the previous slot's flag instead of minting a new one.
        struct RefusingConnector {
            attempts: Arc<AtomicUsize>,
        }
        impl RelayConnector for RefusingConnector {
            type Conn = FakeTransport;
            fn connect(
                &self,
            ) -> impl std::future::Future<Output = Result<FakeTransport, String>> + Send
            {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                async { Err("refused".to_string()) }
            }
        }

        let slots = Arc::new(HostSlots::default());
        let stale_state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let attempts = Arc::new(AtomicUsize::new(0));

        // Loop 1 takes the slot and gets ITS flag from the real bookkeeping.
        let (running_1, rx_1) = slots.install(Arc::clone(&stale_state)).unwrap();
        assert!(slots.is_hosting());

        let connector = RefusingConnector {
            attempts: Arc::clone(&attempts),
        };
        let launcher = Arc::new(FakeLauncher::default());
        let slots_probe = Arc::clone(&slots);
        let attempts_probe = Arc::clone(&attempts);
        let running_1_probe = Arc::clone(&running_1);

        block_on(async move {
            let loop_1 = tokio::spawn(run_host_loop(
                connector,
                launcher,
                stale_state,
                running_1,
                true,
                true,
                rx_1,
            ));
            // Loop 1 has dialled once and is now inside its backoff.
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(attempts_probe.load(Ordering::SeqCst), 1);

            // The owner revokes the grant…
            slots_probe.stop();
            assert!(!slots_probe.is_hosting());
            // …and immediately shares a different agent. This is the restart.
            let fresh = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
            let (running_2, _rx_2) = slots_probe.install(fresh).unwrap();

            assert!(
                !Arc::ptr_eq(&running_1_probe, &running_2),
                "each loop must own its flag; sharing one is the bug"
            );
            assert!(
                !running_1_probe.load(Ordering::SeqCst),
                "the restart must not re-arm the stopped loop's flag"
            );

            // Well past loop 1's first backoff (BACKOFF_MIN = 1s).
            tokio::time::sleep(Duration::from_millis(1300)).await;
            assert_eq!(
                attempts_probe.load(Ordering::SeqCst),
                1,
                "the stopped loop dialled again: it is still hosting with its \
                 STALE HostState alongside the restarted loop"
            );

            // It really did exit, rather than merely not dialling yet.
            let joined = tokio::time::timeout(Duration::from_secs(3), loop_1).await;
            assert!(joined.is_ok(), "the stopped loop never exited");

            // And retiring loop 1 must NOT tear down loop 2's slot.
            slots_probe.retire(&running_1_probe);
            assert!(
                slots_probe.is_hosting(),
                "a departing loop tore down its successor"
            );
        });
    }

    #[test]
    fn a_loop_that_exits_on_its_own_leaves_no_stale_slot() {
        // The second failure from the same root: if a finished loop left its
        // state/outbound behind, every later `republish` would enqueue into a
        // channel with no receiver — a permanently dead host, silently.
        let slots = HostSlots::default();
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let (running, _rx) = slots.install(state).unwrap();
        assert!(slots.is_hosting() && slots.state().is_some());

        // The loop finishes and retires itself.
        running.store(false, Ordering::SeqCst);
        slots.retire(&running);

        assert!(!slots.is_hosting());
        assert!(
            slots.state().is_none(),
            "a retired loop must leave nothing for republish to enqueue into"
        );
        // …and the slot is free, so a later ensure_started can take it.
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        assert!(slots.install(state).is_some(), "the slot must be reusable");
    }

    #[test]
    fn a_restart_over_a_dead_but_unretired_slot_mints_a_fresh_flag() {
        // The reachable window for flag REUSE: `run_host_loop` has returned and
        // cleared its own flag, but the spawned task has not yet called
        // `retire`. An `ensure_started` landing here sees a slot that is present
        // but dead. If it recycled that slot's flag it would re-arm a loop
        // that is already exiting — and that loop's `retire`, now matching by
        // pointer, would then clear the NEW slot and kill the live host.
        let slots = HostSlots::default();
        let first = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let (running_1, _rx_1) = slots.install(first).unwrap();

        running_1.store(false, Ordering::SeqCst); // the loop has finished
        assert!(!slots.is_hosting(), "a cleared flag means not hosting");

        let second = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let (running_2, _rx_2) = slots
            .install(second)
            .expect("a dead slot must be replaceable");

        assert!(
            !Arc::ptr_eq(&running_1, &running_2),
            "the restart recycled the departing loop's flag"
        );
        assert!(
            !running_1.load(Ordering::SeqCst),
            "the departing loop must not be re-armed by a restart"
        );

        // …and when loop 1 finally retires, it must not take loop 2 with it.
        slots.retire(&running_1);
        assert!(
            slots.is_hosting(),
            "a departing loop tore down the live host"
        );
        assert!(running_2.load(Ordering::SeqCst));
    }

    #[test]
    fn a_second_ensure_started_while_hosting_is_a_no_op() {
        let slots = HostSlots::default();
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        assert!(slots.install(Arc::clone(&state)).is_some());
        assert!(
            slots.install(state).is_none(),
            "only one loop may ever be live"
        );
    }

    #[test]
    fn a_loop_stopped_mid_dial_never_publishes_on_the_socket_it_opened() {
        // The connect→publish stretch is the one place the running flag was not
        // re-read, and `CONNECT_TIMEOUT` makes it a bounded 15s window rather
        // than an instant. Reachable: the owner revokes the last grant (or
        // switches sharing off, or unpairs) while a dial is in flight, the dial
        // then succeeds, and a REVOKED offer list goes out on a fresh socket.
        //
        // Same defect class as the stale-loop Critical, through a narrower door.
        struct SlowConnector {
            transport: Mutex<Option<FakeTransport>>,
            delay: Duration,
        }
        impl RelayConnector for SlowConnector {
            type Conn = FakeTransport;
            fn connect(
                &self,
            ) -> impl std::future::Future<Output = Result<FakeTransport, String>> + Send
            {
                let next = self.transport.lock().unwrap().take();
                let delay = self.delay;
                async move {
                    tokio::time::sleep(delay).await;
                    next.ok_or_else(|| "no more connections scripted".to_string())
                }
            }
        }

        let running = Arc::new(AtomicBool::new(true));
        let transport = FakeTransport::new(vec![]);
        let sent = transport.log();
        let connector = SlowConnector {
            transport: Mutex::new(Some(transport)),
            delay: Duration::from_millis(300),
        };
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let launcher = Arc::new(FakeLauncher::default());
        let (_tx, rx) = mpsc::unbounded_channel::<HostMessage>();

        let flag = Arc::clone(&running);
        block_on(async move {
            let host = tokio::spawn(run_host_loop(
                connector, launcher, state, running, true, true, rx,
            ));
            // The dial is in flight; the owner revokes the grant.
            tokio::time::sleep(Duration::from_millis(50)).await;
            flag.store(false, Ordering::SeqCst);
            let joined = tokio::time::timeout(Duration::from_secs(3), host).await;
            assert!(
                joined.is_ok(),
                "the loop never exited after the dial landed"
            );
        });

        let sent = sent.lock().unwrap().clone();
        assert!(
            sent.is_empty(),
            "a STOPPED loop opened a socket and published: {sent:?}"
        );
    }

    #[test]
    fn a_backoff_never_sleeps_through_a_shutdown() {
        // A capped exponential backoff that ignored the running flag would hold
        // a task for up to a minute after the user switched sharing off, so the
        // wait must be sliced and re-checked rather than slept in one go.
        //
        // The single production edit this catches: replacing
        // `sleep_interruptible`'s loop with a plain `tokio::time::sleep(total)`.
        // The flag is cleared 50ms in — DURING the backoff, not before it —
        // which is the only arrangement that tells the two apart.
        struct RefusingConnector {
            attempts: Arc<AtomicUsize>,
        }
        impl RelayConnector for RefusingConnector {
            type Conn = FakeTransport;
            fn connect(
                &self,
            ) -> impl std::future::Future<Output = Result<FakeTransport, String>> + Send
            {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                async { Err("connection refused".to_string()) }
            }
        }

        let running = Arc::new(AtomicBool::new(true));
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = RefusingConnector {
            attempts: Arc::clone(&attempts),
        };
        let state = Arc::new(HostState::new(sharing_with_grant(), vec![agent("coder")]));
        let launcher = Arc::new(FakeLauncher::default());
        let (_tx, rx) = mpsc::unbounded_channel::<HostMessage>();

        let started = std::time::Instant::now();
        let flag = Arc::clone(&running);
        block_on(async move {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                flag.store(false, Ordering::SeqCst);
            });
            run_host_loop(
                connector,
                Arc::clone(&launcher),
                state,
                Arc::clone(&running),
                true,
                true,
                rx,
            )
            .await;
            assert!(launcher.launched.lock().unwrap().is_empty());
        });
        let elapsed = started.elapsed();

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "one dial, then shutdown"
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "the loop slept through a shutdown ({elapsed:?}); the first backoff \
             is BACKOFF_MIN = 1s, so anything near that means the wait was not \
             sliced against the running flag"
        );
    }

    // ---------------------------------------------------------------------
    // The live harness (Task 5): the SAME production code above, but against a
    // real running `openflow-service` over a real WebSocket.
    // ---------------------------------------------------------------------

    /// A live end-to-end against a real `openflow-service` (branch
    /// `feat/relay-v0.2`).
    ///
    /// **Why this exists, and what it is honest about.** Every other test in
    /// this file scripts the transport. This one uses the production
    /// [`WsConnector`]/[`WsRelayTransport`], the production [`run_host_loop`],
    /// the production [`HostState`] (hello, dispatch, `authorize_open`,
    /// `brokered_agent`, session bookkeeping) and a REAL subprocess in a REAL
    /// git repo, against a REAL service that answers back. It closes the
    /// outbound-capture debt `relay/protocol.rs`'s module doc records: nothing
    /// had ever fed this crate's own `HostMessage` bytes to the service's
    /// deserializer.
    ///
    /// **The one seam it cannot cross.** `AgentRunManager::start` takes a
    /// `tauri::AppHandle` (= `AppHandle<Wry>`), and streams output through
    /// `AgentRunOutput::emit`/`listen`. A `Wry` app cannot be constructed off
    /// the process main thread, and libtest runs every `#[test]` on a spawned
    /// thread; `tauri::test::mock_app()` yields an `App<MockRuntime>`, a
    /// different type this codebase's signatures do not accept. So
    /// [`LiveLauncher`] below stands in for `AgentRunLauncher`: it spawns the
    /// same process with the app's OWN [`spawn_plan`], [`build_argv`] and
    /// [`apply_baseline_env`], and it feeds the output/terminal frames through
    /// exactly the two `HostState` entry points the production wiring uses
    /// (`frame_for_run` from `wire_run_pipeline`'s listener, `frame_for_run` +
    /// `close_run` from `RelayFrameSink::on_terminal`). What is NOT covered
    /// live is `AgentRunManager::start` itself and the Tauri event hop between
    /// them. See the task report for the human runbook that covers it.
    ///
    /// Skipped (returns immediately, no socket) unless
    /// `OPENFLOW_LIVE_SERVICE_URL` is set, so `cargo test` in CI is unaffected.
    #[test]
    fn live_end_to_end_against_a_real_openflow_service() {
        let Some(env) = LiveEnv::from_env() else {
            eprintln!(
                "live: SKIPPED — set OPENFLOW_LIVE_SERVICE_URL, \
                 OPENFLOW_LIVE_DEVICE_TOKEN, OPENFLOW_LIVE_MEMBER_ID, \
                 OPENFLOW_LIVE_PROJECT, OPENFLOW_LIVE_AGENT_BIN to run it"
            );
            return;
        };
        eprintln!("live: service={} project={}", env.url, env.project);

        let sharing = SharingConfig {
            enabled: true,
            grants: vec![ShareGrant {
                agent_id: "coder".into(),
                project_path: env.project.clone(),
                allowed_members: vec![env.member.clone()],
            }],
        };
        let agents = vec![live_agent(&env.agent_bin)];
        let state = Arc::new(HostState::new(sharing, agents));
        let running = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::unbounded_channel::<HostMessage>();
        let launcher = Arc::new(LiveLauncher::new(Arc::clone(&state), tx.clone()));

        let connector = RecordingConnector {
            inner: WsConnector {
                url: relay_ws_url(&env.url),
                token: env.token.clone(),
            },
            outbound: Arc::new(Mutex::new(Vec::new())),
            inbound: Arc::new(Mutex::new(Vec::new())),
        };
        let outbound_log = Arc::clone(&connector.outbound);
        let inbound_log = Arc::clone(&connector.inbound);

        let flag = Arc::clone(&running);
        let launcher_probe = Arc::clone(&launcher);
        block_on_with_io(async move {
            let host = tokio::spawn(run_host_loop(
                connector,
                Arc::clone(&launcher),
                Arc::clone(&state),
                running,
                true,
                true,
                rx,
            ));
            // The owner removes the teammate from the grant on the LIVE state
            // and — deliberately — never republishes, so the service still
            // holds the old offer and only the host's own re-check can refuse.
            // This is the evidence that two independent checks exist.
            if let Some(after) = env.revoke_after {
                let state = Arc::clone(&state);
                let agents = state.snapshot().1;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(after)).await;
                    eprintln!("live: revoking the grant's member on the live state (no republish)");
                    let (mut sharing, _) = state.snapshot();
                    for g in &mut sharing.grants {
                        g.allowed_members.clear();
                    }
                    state.set_config(sharing, agents);
                });
            }
            // The teammate side is driven from outside this process (curl).
            tokio::time::sleep(Duration::from_secs(env.seconds)).await;
            // Clean shutdown: clear the flag AND drop every outbound sender, so
            // `serve_connection`'s closed-sender arm fires (the invariant
            // `HostSlots::stop` documents).
            flag.store(false, Ordering::SeqCst);
            launcher_probe.shutdown();
            drop(tx);
            let _ = tokio::time::timeout(Duration::from_secs(15), host).await;
        });

        let out = outbound_log.lock().unwrap().clone();
        let inb = inbound_log.lock().unwrap().clone();
        std::fs::write(env.capture.join("outbound.jsonl"), out.join("\n") + "\n").unwrap();
        std::fs::write(env.capture.join("inbound.jsonl"), inb.join("\n") + "\n").unwrap();
        eprintln!("live: ---- OUTBOUND (this crate -> the real service) ----");
        for l in &out {
            eprintln!("live: OUT {l}");
        }
        eprintln!("live: ---- INBOUND (the real service -> this crate) ----");
        for l in &inb {
            eprintln!("live: IN  {l}");
        }
        assert!(!out.is_empty(), "nothing was ever sent to the service");
        let hello: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(hello["t"], json!("hello"), "the first frame is the hello");
    }

    /// A live dial with a token the service has revoked. Answers the question
    /// `run_host_loop` cannot: is a refused credential distinguishable from an
    /// outage? Skipped unless `OPENFLOW_LIVE_REVOKED_TOKEN` is set.
    #[test]
    fn live_dial_with_a_revoked_device_token() {
        let (Ok(url), Ok(token)) = (
            std::env::var("OPENFLOW_LIVE_SERVICE_URL"),
            std::env::var("OPENFLOW_LIVE_REVOKED_TOKEN"),
        ) else {
            eprintln!("live: SKIPPED (no OPENFLOW_LIVE_REVOKED_TOKEN)");
            return;
        };
        let connector = WsConnector {
            url: relay_ws_url(&url),
            token,
        };
        let result = block_on_with_io(connector.connect());
        match result {
            Ok(_) => panic!("live: a revoked token OPENED the host socket"),
            Err(e) => eprintln!("live: revoked-token dial error verbatim: {e}"),
        }
    }

    /// The live sibling of [`block_on`]: same explicit-runtime shape (this repo
    /// uses no `#[tokio::test]`), but with the IO driver enabled, because unlike
    /// every other test here these two open a REAL socket and spawn a REAL
    /// process.
    fn block_on_with_io<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    struct LiveEnv {
        url: String,
        token: String,
        member: String,
        project: String,
        agent_bin: String,
        capture: std::path::PathBuf,
        seconds: u64,
        /// Seconds after which the grant's member list is emptied on the LIVE
        /// `HostState` without a republish (`OPENFLOW_LIVE_REVOKE_AFTER`).
        revoke_after: Option<u64>,
    }

    impl LiveEnv {
        fn from_env() -> Option<Self> {
            let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
            Some(Self {
                url: var("OPENFLOW_LIVE_SERVICE_URL")?,
                token: var("OPENFLOW_LIVE_DEVICE_TOKEN")?,
                member: var("OPENFLOW_LIVE_MEMBER_ID")?,
                project: var("OPENFLOW_LIVE_PROJECT")?,
                agent_bin: var("OPENFLOW_LIVE_AGENT_BIN")?,
                capture: std::path::PathBuf::from(var("OPENFLOW_LIVE_CAPTURE")?),
                seconds: var("OPENFLOW_LIVE_SECONDS")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(90),
                revoke_after: var("OPENFLOW_LIVE_REVOKE_AFTER").and_then(|s| s.parse().ok()),
            })
        }
    }

    fn live_agent(binary: &str) -> AgentDefinition {
        serde_json::from_value(json!({
            "id": "coder", "name": "Coder", "enabled": true,
            "binding_id": "agent:coder", "provider_id": "",
            "kind": "cli", "cli_type": "claude", "binary_path": binary,
            // `{cwd}` is substituted by the app's own `build_argv`; the
            // instruction rides stdin (`PromptDelivery::Stdin`, the default).
            "command_template": "--cwd {cwd}",
            "project_path": "/this/must/never/be/used"
        }))
        .unwrap()
    }

    /// Wraps the PRODUCTION transport and records the exact bytes that cross it
    /// in both directions. The recorded line is the same `String`
    /// `send_message` produced and handed to `WsRelayTransport::send`, so the
    /// capture cannot drift from what actually went on the socket.
    struct RecordingTransport<T: RelayTransport> {
        inner: T,
        outbound: Arc<Mutex<Vec<String>>>,
        inbound: Arc<Mutex<Vec<String>>>,
    }

    impl<T: RelayTransport> RelayTransport for RecordingTransport<T> {
        fn send(
            &self,
            line: String,
        ) -> impl std::future::Future<Output = Result<(), String>> + Send {
            self.outbound.lock().unwrap().push(line.clone());
            self.inner.send(line)
        }
        fn recv(&self) -> impl std::future::Future<Output = Option<String>> + Send {
            let log = Arc::clone(&self.inbound);
            let fut = self.inner.recv();
            async move {
                let got = fut.await;
                if let Some(line) = &got {
                    log.lock().unwrap().push(line.clone());
                }
                got
            }
        }
        fn close(&self) -> impl std::future::Future<Output = ()> + Send {
            self.inner.close()
        }
    }

    struct RecordingConnector<C: RelayConnector> {
        inner: C,
        outbound: Arc<Mutex<Vec<String>>>,
        inbound: Arc<Mutex<Vec<String>>>,
    }

    impl<C: RelayConnector> RelayConnector for RecordingConnector<C> {
        type Conn = RecordingTransport<C::Conn>;
        fn connect(&self) -> impl std::future::Future<Output = Result<Self::Conn, String>> + Send {
            let outbound = Arc::clone(&self.outbound);
            let inbound = Arc::clone(&self.inbound);
            let fut = self.inner.connect();
            async move {
                fut.await.map(|inner| RecordingTransport {
                    inner,
                    outbound,
                    inbound,
                })
            }
        }
    }

    /// The live stand-in for [`AgentRunLauncher`]. It really spawns the agent,
    /// really streams its output, and routes both through the same two
    /// `HostState` entry points the production wiring uses.
    struct LiveLauncher {
        state: Arc<HostState>,
        outbound: Mutex<Option<mpsc::UnboundedSender<HostMessage>>>,
        seq: AtomicUsize,
        kills: Mutex<HashMap<String, mpsc::UnboundedSender<()>>>,
    }

    impl LiveLauncher {
        fn new(state: Arc<HostState>, outbound: mpsc::UnboundedSender<HostMessage>) -> Self {
            Self {
                state,
                outbound: Mutex::new(Some(outbound)),
                seq: AtomicUsize::new(0),
                kills: Mutex::new(HashMap::new()),
            }
        }

        fn shutdown(&self) {
            self.outbound.lock().unwrap().take();
        }
    }

    impl RunLauncher for LiveLauncher {
        fn launch(&self, agent: BrokeredRun, instruction: String) -> String {
            use crate::managers::agent_run::{apply_baseline_env, build_argv, spawn_plan};
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

            let run_id = format!("live-run-{}", self.seq.fetch_add(1, Ordering::SeqCst));
            let def = agent.into_definition();
            let cwd = std::path::PathBuf::from(&def.project_path);
            let argv = build_argv(
                &def.command_template,
                &cwd.to_string_lossy(),
                &instruction,
                def.prompt_via,
            );
            let plan = spawn_plan(&def.binary_path, cfg!(windows));
            eprintln!(
                "live: launching {run_id}: {} {:?} in {}",
                plan.program,
                argv,
                cwd.display()
            );

            let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
            self.kills
                .lock()
                .unwrap()
                .insert(run_id.clone(), kill_tx.clone());

            let state = Arc::clone(&self.state);
            let outbound = self.outbound.lock().unwrap().clone();
            let run_id_task = run_id.clone();
            tokio::spawn(async move {
                let mut cmd = tokio::process::Command::new(&plan.program);
                cmd.args(&plan.pre_args)
                    .args(&argv)
                    .current_dir(&cwd)
                    .env("NO_COLOR", "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                apply_baseline_env(&mut cmd);
                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("live: spawn failed: {e}");
                        return;
                    }
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(instruction.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                }

                let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
                for stream in [
                    child.stdout.take().map(BufReader::new).map(|r| {
                        Box::pin(r) as std::pin::Pin<Box<dyn tokio::io::AsyncBufRead + Send>>
                    }),
                    child.stderr.take().map(BufReader::new).map(|r| {
                        Box::pin(r) as std::pin::Pin<Box<dyn tokio::io::AsyncBufRead + Send>>
                    }),
                ]
                .into_iter()
                .flatten()
                {
                    let tx = line_tx.clone();
                    tokio::spawn(async move {
                        let mut lines = stream.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let _ = tx.send(line);
                        }
                    });
                }
                drop(line_tx);

                // Exactly what `wire_run_pipeline`'s `agent-run-output`
                // listener does with each chunk.
                let pump_state = Arc::clone(&state);
                let pump_out = outbound.clone();
                let pump_run = run_id_task.clone();
                let pump = tokio::spawn(async move {
                    while let Some(chunk) = line_rx.recv().await {
                        eprintln!("live: [{pump_run}] {chunk}");
                        if let Some(msg) =
                            pump_state.frame_for_run(&pump_run, HostFrame::Output { chunk })
                        {
                            if let Some(tx) = &pump_out {
                                let _ = tx.send(msg);
                            }
                        }
                    }
                });

                let status = tokio::select! {
                    exit = child.wait() => match exit {
                        Ok(s) => RunStatus::Finished { code: s.code().unwrap_or(-1) },
                        Err(e) => RunStatus::Failed { error: e.to_string() },
                    },
                    _ = kill_rx.recv() => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        RunStatus::Stopped
                    }
                };
                let _ = pump.await;

                // Exactly what `RelayFrameSink::on_terminal` does.
                let outcome = run_status_outcome(&status);
                eprintln!("live: [{run_id_task}] terminal: {outcome}");
                if let Some(msg) = state.frame_for_run(
                    &run_id_task,
                    HostFrame::Status {
                        status: outcome.clone(),
                    },
                ) {
                    if let Some(tx) = &outbound {
                        let _ = tx.send(msg);
                    }
                }
                if let Some(msg) = state.close_run(&run_id_task, &outcome) {
                    if let Some(tx) = &outbound {
                        let _ = tx.send(msg);
                    }
                }
            });

            run_id
        }

        fn stop(&self, run_id: &str) {
            eprintln!("live: stop requested for {run_id}");
            if let Some(tx) = self.kills.lock().unwrap().get(run_id) {
                let _ = tx.send(());
            }
        }
    }
}
