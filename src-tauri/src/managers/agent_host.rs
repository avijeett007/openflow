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
    next_backoff, relay_ws_url, RelayConnector, RelayTransport, WsConnector, BACKOFF_MIN,
};
use crate::settings::{AgentDefinition, SharingConfig};

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

    /// Install a fresh settings snapshot (used by `republish`).
    pub fn set_config(&self, sharing: SharingConfig, agents: Vec<AgentDefinition>) {
        let mut cfg = self.config.lock().unwrap();
        cfg.sharing = sharing;
        cfg.agents = agents;
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

    /// Dispatch one service message. Never panics, never propagates an error:
    /// a newer or misbehaving service must not be able to kill the host loop.
    pub async fn handle_service_message<T: RelayTransport, L: RunLauncher>(
        &self,
        msg: ServiceMessage,
        transport: &T,
        launcher: &L,
    ) {
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
                    self.refuse(transport, &session_id, "already_open").await;
                    return;
                }

                let Some(action) = self.resolve_action(&offer_id, action_id.as_deref()) else {
                    log::warn!("relay: open for an offer this host cannot resolve ({offer_id})");
                    self.refuse(transport, &session_id, DenyReason::UnknownOffer.outcome())
                        .await;
                    return;
                };

                let instruction = match parse_open_payload(&payload) {
                    Ok(p) => p.instruction,
                    Err(e) => {
                        log::warn!("relay: refusing session {session_id}: {e}");
                        self.refuse(transport, &session_id, "bad_request").await;
                        return;
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
                            self.refuse(transport, &session_id, reason.outcome()).await;
                            return;
                        }
                    };

                // Only reachable with the token in hand.
                let run = brokered_agent(&authorized, &requester.display_name);
                let agent_label = run.name.clone();
                let project = run.project_path.clone();

                // Exactly the hotkey path: hand it to `start` and return at
                // once. A run is seconds→minutes; nothing here awaits it.
                let run_id = launcher.launch(run, instruction);

                self.sessions.lock().unwrap().insert(
                    session_id.clone(),
                    HostSession {
                        run_id: run_id.clone(),
                        member_id: requester.member_id.clone(),
                        display_name: requester.display_name.clone(),
                    },
                );
                self.by_run
                    .lock()
                    .unwrap()
                    .insert(run_id, session_id.clone());

                let header = session_frame(
                    &session_id,
                    HostFrame::Header {
                        agent: agent_label,
                        project,
                    },
                );
                let _ = send_message(transport, header).await;
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
            }

            ServiceMessage::Unknown => {
                log::debug!("relay: ignoring a message this version does not model");
            }
        }
    }

    /// `action_id` from the message when the service supplies it (every real
    /// one does), else the cache an earlier `open` populated, else `None`.
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
    async fn refuse<T: RelayTransport>(&self, transport: &T, session_id: &str, outcome: &str) {
        let _ = send_message(
            transport,
            HostMessage::Closed {
                session_id: session_id.to_string(),
                outcome: outcome.to_string(),
            },
        )
        .await;
    }
}

async fn send_message<T: RelayTransport>(transport: &T, msg: HostMessage) -> Result<(), String> {
    let line = serde_json::to_string(&msg).map_err(|e| format!("relay: unencodable frame: {e}"))?;
    transport.send(line).await
}

// ---------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------

/// Owns the host loop's lifetime. Mirrors `ServiceSyncManager`'s shape: managed
/// in Tauri state, started idempotently, and a no-op until configured.
pub struct AgentHostManager {
    app: AppHandle,
    runs: Arc<AgentRunManager>,
    running: Arc<AtomicBool>,
    state: Mutex<Option<Arc<HostState>>>,
    outbound: Mutex<Option<mpsc::UnboundedSender<HostMessage>>>,
    /// The relay sink + the `agent-run-output` subscription are installed at
    /// most once, and only after the gate has already said yes.
    wired: AtomicBool,
}

impl AgentHostManager {
    pub fn new(app: &AppHandle, runs: Arc<AgentRunManager>) -> Self {
        Self {
            app: app.clone(),
            runs,
            running: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(None),
            outbound: Mutex::new(None),
            wired: AtomicBool::new(false),
        }
    }

    fn state(&self) -> Option<Arc<HostState>> {
        self.state.lock().unwrap().clone()
    }

    /// Queue a message for the live socket. Dropped silently when nothing is
    /// connected — a frame for a dead session has nowhere to go.
    fn enqueue(&self, msg: HostMessage) {
        if let Some(tx) = self.outbound.lock().unwrap().as_ref() {
            let _ = tx.send(msg);
        }
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
        // CAS false→true so only one loop ever runs.
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let state = Arc::new(HostState::new(
            settings.sharing.clone(),
            settings.agents.clone(),
        ));
        *self.state.lock().unwrap() = Some(Arc::clone(&state));
        let (tx, rx) = mpsc::unbounded_channel::<HostMessage>();
        *self.outbound.lock().unwrap() = Some(tx);
        self.wire_run_pipeline();

        let connector = WsConnector {
            url: relay_ws_url(&settings.service_url),
            token,
        };
        let launcher = Arc::new(AgentRunLauncher {
            manager: Arc::clone(&self.runs),
            app: self.app.clone(),
        });
        let running = Arc::clone(&self.running);
        log::info!("relay: hosting {} shared agent(s)", offers.len());
        tauri::async_runtime::spawn(async move {
            // Both `true` by construction: the gate above already established
            // that the service is paired and a device token exists. Unpairing
            // goes through `republish` → `stop`, not through this loop.
            run_host_loop(connector, launcher, state, running, true, true, rx).await;
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

    /// Stop hosting (sharing switched off, service unpaired, last grant
    /// removed). The loop observes the flag and exits; **local runs are left
    /// alone**, exactly as on a dropped socket.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        // Dropping the sender wakes the loop out of its outbound wait.
        self.outbound.lock().unwrap().take();
        if let Some(state) = self.state.lock().unwrap().take() {
            state.on_disconnect();
        }
    }

    /// Re-publish the offer list after ANY settings change, so a revoked grant
    /// disappears immediately rather than at the next reconnect — and so the
    /// live connection re-authorises against the new settings.
    pub fn republish(self: &Arc<Self>) {
        let settings = crate::settings::get_settings(&self.app);
        let token =
            crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
        let offers = offers_from_grants(&settings.sharing, &settings.agents);
        let wanted = should_host(
            &settings.sharing,
            settings.service_enabled,
            !token.is_empty(),
            offers.len(),
        );

        match (wanted, self.state()) {
            // Still hosting: install the new settings on the LIVE state (so the
            // next `open` is re-checked against them) and push a fresh `hello`.
            (true, Some(state)) => {
                state.set_config(settings.sharing.clone(), settings.agents.clone());
                self.enqueue(HostMessage::Hello { offers });
            }
            // Newly eligible (a first grant, a fresh pairing).
            (true, None) => self.ensure_started(),
            // No longer eligible: withdraw everything.
            (false, Some(_)) => self.stop(),
            (false, None) => {}
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

/// Look the host manager up in Tauri state and re-publish. A free function so
/// the settings/agent commands can call it without importing the type or caring
/// whether the feature is configured.
pub fn republish_offers(app: &AppHandle) {
    use tauri::Manager;
    if let Some(host) = app.try_state::<Arc<AgentHostManager>>() {
        host.inner().republish();
    }
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
                log::warn!(
                    "relay: connect failed ({e}); retrying in {}s",
                    backoff.as_secs()
                );
                sleep_interruptible(backoff, &running).await;
                continue;
            }
        };
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
                        state
                            .handle_service_message(msg, conn.as_ref(), launcher.as_ref())
                            .await
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
                // The manager stopped and dropped the sender.
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
    }

    impl FakeTransport {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                inbound: Mutex::new(lines.iter().map(|s| s.to_string()).collect()),
                sent: Arc::new(Mutex::new(Vec::new())),
                hang_when_drained: false,
                stop_on_close: Mutex::new(None),
            }
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
            async { Ok(()) }
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
        block_on(state.handle_service_message(msg, &t, &l));

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
        block_on(state.handle_service_message(msg, &t, &l));

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
        block_on(state.handle_service_message(msg, &t, &l));

        assert!(l.launched.lock().unwrap().is_empty(), "nothing may run");
        let v: serde_json::Value = serde_json::from_str(&t.sent()[0]).unwrap();
        assert_eq!(v["outcome"], json!("denied"));
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
        block_on(state.handle_service_message(msg, &t, &l));
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
        block_on(state.handle_service_message(bare.clone(), &t, &l));
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
        block_on(state.handle_service_message(full, &t, &l));
        block_on(state.handle_service_message(bare, &t, &l));
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
        block_on(state.handle_service_message(open.clone(), &t, &l));
        block_on(state.handle_service_message(open, &t, &l));

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
        block_on(state.handle_service_message(open, &t, &l));

        let stop: ServiceMessage =
            serde_json::from_value(json!({"t": "stop", "session_id": "s1"})).unwrap();
        block_on(state.handle_service_message(stop, &t, &l));
        assert_eq!(l.stopped.lock().unwrap().as_slice(), ["run-1".to_string()]);
    }

    #[test]
    fn a_stop_for_an_unknown_session_is_ignored_not_fatal() {
        let state = HostState::new(sharing_with_grant(), vec![agent("coder")]);
        let t = FakeTransport::new(vec![]);
        let l = FakeLauncher::default();
        let stop: ServiceMessage =
            serde_json::from_value(json!({"t": "stop", "session_id": "nope"})).unwrap();
        block_on(state.handle_service_message(stop, &t, &l));
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
        block_on(state.handle_service_message(open, &t, &l));

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
        block_on(state.handle_service_message(open, &t, &l));
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
        block_on(state.handle_service_message(msg, &t, &l));
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
}
