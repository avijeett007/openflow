//! The host-side authorisation re-check and offer publication.
//!
//! DESIGN-shared-agents §8, stated without softening: **the relay is not the
//! boundary.** The service checks membership as a convenience and rejects
//! unauthorised requesters early; this file re-checks every grant on receipt and
//! is the only thing that actually decides. Both checks exist deliberately — the
//! service could be compromised, out of date, or simply wrong about a grant the
//! owner revoked two seconds ago.
//!
//! Pure: no I/O, no Tauri, no socket. The whole authorisation matrix is table-
//! tested.

use crate::relay::protocol::OfferWire;
use crate::settings::{AgentDefinition, AgentKind, AgentOutputSink, ShareGrant, SharingConfig};

/// The relay `action_id` for an agent: its existing `binding_id`, which is
/// always `"agent:<id>"` (settings.rs:303) and already matches the shape
/// DESIGN-relay-v02 §4 uses. Derived if a legacy store left it blank.
pub fn action_id_for(agent: &AgentDefinition) -> String {
    if agent.binding_id.trim().is_empty() {
        format!("agent:{}", agent.id)
    } else {
        agent.binding_id.clone()
    }
}

/// The relay `action_id` for one **grant**: the agent's binding id, then the
/// grant's own stable id.
///
/// Keyed on the grant, not the agent, and that is the whole point. An owner may
/// legitimately share one agent twice — `coder` in `~/repo/acme-client` with
/// Priya, `coder` in `~/repo/public-website` with Priya and Sam. Keyed on the
/// agent, both grants published the SAME `action_id`, `authorize_open` resolved
/// whichever grant came first in the vector, and opening the public-website
/// offer ran the agent in the client repo while Sam — explicitly allowed — was
/// refused outright. DESIGN-shared-agents §3 promises the blast radius is the
/// directory the owner chose *for that grant*; this is what makes that true.
///
/// `action_id` is opaque to `openflow-service` (it stores it and echoes it back
/// on `open`), so the shape is this host's to choose.
pub fn action_id_for_grant(agent: &AgentDefinition, grant: &ShareGrant) -> String {
    format!("{}#{}", action_id_for(agent), grant.stable_id())
}

/// The same identity for a grant whose agent no longer exists, so a deleted
/// agent is still reported as `AgentMissing` rather than vanishing into
/// `NoGrant`. Mirrors `action_id_for`'s derivation of a blank `binding_id`.
fn orphan_action_id(grant: &ShareGrant) -> String {
    format!("agent:{}#{}", grant.agent_id, grant.stable_id())
}

fn find_agent<'a>(agents: &'a [AgentDefinition], agent_id: &str) -> Option<&'a AgentDefinition> {
    agents.iter().find(|a| a.id == agent_id)
}

/// The only agent kind C2 v1 will share. DESIGN-shared-agents §2: "C2 v1 rides
/// the shipped raw-CLI driver."
///
/// A `Remote` (A2A) agent must never be shareable, and the reason is not
/// tidiness. `AgentRunManager::start` routes `AgentKind::Remote` to
/// `drive_remote_run`, which uses neither `cwd` nor `argv` — the work happens at
/// the owner's remote endpoint, under the owner's credentials, billed to the
/// owner's account. The grant's `project_path` would bound **nothing**, and the
/// three places the Sharing UI says a shared agent "runs on this machine, in the
/// folder you pick for each grant" would be false. `Prompt` agents are not
/// runnable this way at all.
pub fn is_shareable_kind(kind: AgentKind) -> bool {
    matches!(kind, AgentKind::Cli)
}

/// The offer list to publish on connect. A grant that cannot actually run —
/// disabled agent, deleted agent, an agent kind C2 does not share, no project
/// chosen, nobody allowed — is not advertised, so a teammate never sees an offer
/// that would be refused on open.
pub fn offers_from_grants(sharing: &SharingConfig, agents: &[AgentDefinition]) -> Vec<OfferWire> {
    if !sharing.enabled {
        return Vec::new();
    }
    sharing
        .grants
        .iter()
        .filter_map(|g| {
            let agent = find_agent(agents, &g.agent_id)?;
            // A blank member id is nobody. Publishing one would hand the
            // service's convenience check a wildcard that `authorize_open`
            // refuses anyway — see the blank-requester guard there.
            let allowed: Vec<String> = g
                .allowed_members
                .iter()
                .filter(|m| !m.trim().is_empty())
                .cloned()
                .collect();
            if !agent.enabled
                || !is_shareable_kind(agent.kind)
                || g.project_path.trim().is_empty()
                || allowed.is_empty()
            {
                return None;
            }
            Some(OfferWire {
                action_id: action_id_for_grant(agent, g),
                label: agent.name.clone(),
                project: g.project_path.clone(),
                allowed,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The service sent an `offer_id` this host cannot resolve to an action.
    UnknownOffer,
    NoGrant,
    AgentMissing,
    AgentDisabled,
    /// The grant names an agent kind C2 does not share — today anything that is
    /// not `Cli`. See [`is_shareable_kind`].
    AgentNotShareable,
    MemberNotAllowed,
    NoProject,
}

impl DenyReason {
    /// The terminal `outcome` reported to the requester. Deliberately coarse —
    /// a stranger learns that they were refused, not the shape of the owner's
    /// configuration.
    pub fn outcome(&self) -> &'static str {
        match self {
            DenyReason::UnknownOffer => "unknown_offer",
            DenyReason::AgentMissing
            | DenyReason::AgentDisabled
            | DenyReason::AgentNotShareable
            | DenyReason::NoProject => "unavailable",
            DenyReason::NoGrant | DenyReason::MemberNotAllowed => "denied",
        }
    }
}

/// **The proof that this desktop said yes.** Its fields are private and no
/// public constructor exists, so the ONLY way to hold one is to have called
/// [`authorize_open`] and had it return `Ok`. [`brokered_agent`] takes it by
/// reference and [`crate::managers::agent_host::RunLauncher::launch`] takes the
/// [`BrokeredRun`] that only `brokered_agent` can mint — which makes "launch a
/// teammate's run without re-checking the grant" a **compile error** rather
/// than something a reviewer has to notice.
///
/// This is deliberate structural design, not decoration: DESIGN-shared-agents
/// §8 puts the whole trust boundary of C2 on `authorize_open`, and until this
/// token existed the ordering was only a convention (Task 3 review).
///
/// It deliberately exposes **no accessors at all**: outside this module the one
/// and only thing that can be done with an `Authorized` is hand it to
/// [`brokered_agent`].
#[derive(Debug)]
pub struct Authorized {
    agent: AgentDefinition,
    grant: ShareGrant,
}

/// An `AgentDefinition` that has passed [`authorize_open`], plus the display
/// name of the teammate who asked for it. Private field, no public constructor:
/// [`brokered_agent`] is the only way to obtain one, and it demands an
/// [`Authorized`].
///
/// `Deref` to the definition is for READING (name, project_path, sinks); the
/// definition can only be moved out by [`Self::into_definition`], which still
/// requires having held the token.
#[derive(Debug, Clone)]
pub struct BrokeredRun {
    agent: AgentDefinition,
    requester: String,
}

impl BrokeredRun {
    /// The teammate's display name, for the `← Priya` label on the owner's own
    /// run panel (`AgentRunManager::note_brokered_run`).
    pub fn requester(&self) -> &str {
        &self.requester
    }

    /// Hand the definition to `AgentRunManager::start`, consuming the token.
    pub fn into_definition(self) -> AgentDefinition {
        self.agent
    }
}

impl std::ops::Deref for BrokeredRun {
    type Target = AgentDefinition;
    fn deref(&self) -> &AgentDefinition {
        &self.agent
    }
}

/// Re-check an incoming `open` against the CURRENT settings. Called for every
/// session, however confident the relay was.
///
/// **Caller obligation, and the limit of what the token proves.** This is a pure
/// function of the config it is handed. The [`Authorized`] it mints therefore
/// proves *"a re-check ran, and it resolved to exactly this agent and this
/// folder"* — it does **not** prove the config was the owner's live settings,
/// because `SharingConfig`/`AgentDefinition` are plain settings data that any
/// caller in this crate can construct.
///
/// **That gap is closable, and was deliberately not closed.** Not impossible —
/// costly. What does *not* work: `pub(in crate::managers)` is illegal from here
/// (`crate::managers` is not an ancestor of `crate::relay::grants`), and a
/// witness type on *this* side of the call closes nothing, since whoever calls
/// the constructor still chooses its contents. What *does* work is a witness on
/// the **caller's** side: a `LiveSnapshot(SharingConfig, Vec<AgentDefinition>)`
/// in `managers::agent_host` with a **private constructor** and public
/// accessors, taken by this function — then only `agent_host` can mint the
/// argument, and `SharingConfig`'s own field visibility stops mattering. A
/// sealed trait gets there too. (Field privacy is not the obstacle either:
/// serde and specta derives work fine on private fields; `pub` on
/// `SharingConfig` exists so other modules can read it, nothing more.)
///
/// The price is a layering inversion — `relay::grants`, the leaf that
/// deliberately depends on nothing, would have to name a type from
/// `managers::agent_host` — plus a `#[cfg(test)]` constructor so this module's
/// own authorisation table tests can still call it. Judged not worth it while
/// the exposure is intra-crate with exactly one caller. Revisit if a second
/// production caller ever appears.
///
/// What holds the line meanwhile: that ONE caller —
/// `managers::agent_host::HostState::handle_service_message` — reads the live
/// `Mutex` snapshot that `set_config` keeps current. Guarded by
/// `agent_host::tests::a_grant_revoked_since_the_offer_was_published_is_refused_on_the_live_socket`,
/// which fails if the caller ever stops reading live settings. A second caller
/// must do the same; passing a hand-built `SharingConfig` here would be
/// authorising against fiction.
pub fn authorize_open(
    sharing: &SharingConfig,
    agents: &[AgentDefinition],
    action_id: &str,
    member_id: &str,
) -> Result<Authorized, DenyReason> {
    if !sharing.enabled {
        // Pause sharing has been hit since the offer was published.
        return Err(DenyReason::NoGrant);
    }
    // Resolved by GRANT identity, so two grants for the same agent stay two
    // distinct offers — see [`action_id_for_grant`].
    let grant = sharing
        .grants
        .iter()
        .find(|g| match find_agent(agents, &g.agent_id) {
            Some(a) => action_id_for_grant(a, g) == action_id,
            None => orphan_action_id(g) == action_id,
        })
        .ok_or(DenyReason::NoGrant)?;

    let agent = find_agent(agents, &grant.agent_id).ok_or(DenyReason::AgentMissing)?;
    if !agent.enabled {
        return Err(DenyReason::AgentDisabled);
    }
    if !is_shareable_kind(agent.kind) {
        return Err(DenyReason::AgentNotShareable);
    }
    if grant.project_path.trim().is_empty() {
        return Err(DenyReason::NoProject);
    }
    // A blank `member_id` is nobody, and `Requester` defaults both of its fields
    // — an `open` with no `requester` object at all parses to `member_id: ""`.
    // Equality alone would then let a grant that somehow stored a blank entry
    // (an older store, a hand-edited settings file) authorise an anonymous
    // requester.
    if member_id.trim().is_empty() {
        return Err(DenyReason::MemberNotAllowed);
    }
    if !grant
        .allowed_members
        .iter()
        .any(|m| !m.trim().is_empty() && m == member_id)
    {
        return Err(DenyReason::MemberNotAllowed);
    }
    Ok(Authorized {
        agent: agent.clone(),
        grant: grant.clone(),
    })
}

/// The AgentDefinition a brokered run actually uses: the stored agent, with the
/// grant's folder substituted and the `Relay` sink added.
///
/// This clone is **never persisted**. It is the whole reason
/// `AgentRunManager::start` needs no modification: `start` reads
/// `agent.project_path` and `finalize` reads `agent.output_sinks`, so handing it
/// a clone with both adjusted routes a brokered run correctly without touching
/// one line of the run pipeline.
///
/// Takes [`Authorized`] rather than a bare `(&AgentDefinition, &ShareGrant)`
/// pair **on purpose**: those two values are exactly what a caller who skipped
/// `authorize_open` would have to hand, so accepting them would leave the
/// security ordering as a convention. See [`Authorized`].
pub fn brokered_agent(auth: &Authorized, requester_display_name: &str) -> BrokeredRun {
    let mut a = auth.agent.clone();
    a.project_path = auth.grant.project_path.clone();
    if !a.output_sinks.contains(&AgentOutputSink::Relay) {
        a.output_sinks.push(AgentOutputSink::Relay);
    }
    BrokeredRun {
        agent: a,
        requester: requester_display_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, enabled: bool) -> AgentDefinition {
        serde_json::from_value(json!({
            "id": id, "name": format!("Agent {id}"), "enabled": enabled,
            "binding_id": format!("agent:{id}"), "provider_id": "",
            "kind": "cli", "cli_type": "claude", "binary_path": "/usr/local/bin/claude",
            "command_template": "-p", "project_path": "/home/me/personal"
        }))
        .unwrap()
    }

    /// An A2A agent: `drive_remote_run` ignores `cwd` and `argv` entirely, so a
    /// grant's folder bounds nothing for it.
    fn remote_agent(id: &str) -> AgentDefinition {
        serde_json::from_value(json!({
            "id": id, "name": format!("Agent {id}"), "enabled": true,
            "binding_id": format!("agent:{id}"), "provider_id": "",
            "kind": "remote", "remote_url": "https://agent.example.com/a2a",
            "project_path": "/home/me/personal"
        }))
        .unwrap()
    }

    fn sharing(enabled: bool, grants: Vec<ShareGrant>) -> SharingConfig {
        SharingConfig { enabled, grants }
    }

    fn grant(id: &str, agent_id: &str, project: &str, members: &[&str]) -> ShareGrant {
        ShareGrant {
            id: id.into(),
            agent_id: agent_id.into(),
            project_path: project.into(),
            allowed_members: members.iter().map(|m| m.to_string()).collect(),
        }
    }

    /// A grant as persisted before `ShareGrant::id` existed.
    fn legacy_grant(agent_id: &str, project: &str, members: &[&str]) -> ShareGrant {
        grant("", agent_id, project, members)
    }

    #[test]
    fn action_id_is_the_agents_existing_binding_id() {
        // `binding_id` is ALWAYS "agent:<id>" (settings.rs:303) and is exactly
        // the shape DESIGN-relay-v02 §4 uses for action_id. No new identifier.
        assert_eq!(action_id_for(&agent("coder", true)), "agent:coder");
        let mut blank = agent("coder", true);
        blank.binding_id = String::new();
        assert_eq!(action_id_for(&blank), "agent:coder", "derives when absent");
    }

    #[test]
    fn offers_are_published_only_for_live_grants() {
        let agents = vec![agent("coder", true), agent("off", false)];
        let s = sharing(
            true,
            vec![
                grant("g1", "coder", "/repo/site", &["m-priya"]),
                grant("g2", "off", "/repo/x", &["m-priya"]), // agent disabled
                grant("g3", "ghost", "/repo/y", &["m-priya"]), // agent deleted
                grant("g4", "coder", "", &["m-priya"]),      // no project chosen
                grant("g5", "coder", "/repo/z", &[]),        // nobody allowed
                grant("g6", "coder", "/repo/w", &["  "]),    // nobody real allowed
            ],
        );
        let offers = offers_from_grants(&s, &agents);
        assert_eq!(
            offers.len(),
            1,
            "only the one complete, live grant is offered"
        );
        assert_eq!(offers[0].action_id, "agent:coder#g1");
        assert_eq!(offers[0].label, "Agent coder");
        assert_eq!(offers[0].project, "/repo/site");
        assert_eq!(offers[0].allowed, vec!["m-priya".to_string()]);
    }

    #[test]
    fn nothing_is_offered_while_the_master_switch_is_off() {
        let agents = vec![agent("coder", true)];
        let s = sharing(
            false,
            vec![grant("g1", "coder", "/repo/site", &["m-priya"])],
        );
        assert!(offers_from_grants(&s, &agents).is_empty());
    }

    #[test]
    fn the_host_re_checks_and_can_disagree_with_the_relay() {
        // DESIGN-shared-agents §8: the relay's check is a convenience; this
        // desktop is the only thing that actually decides. Every one of these
        // arrives ALREADY authorised by the service and is still refused here.
        let agents = vec![agent("coder", true), agent("off", false)];

        let allowed = sharing(true, vec![grant("g1", "coder", "/repo/site", &["m-priya"])]);
        let ok = authorize_open(&allowed, &agents, "agent:coder#g1", "m-priya").unwrap();
        assert_eq!(ok.agent.id, "coder");
        assert_eq!(ok.grant.project_path, "/repo/site");

        assert_eq!(
            authorize_open(&allowed, &agents, "agent:coder#g1", "m-someone-else").unwrap_err(),
            DenyReason::MemberNotAllowed
        );
        assert_eq!(
            authorize_open(&allowed, &agents, "agent:nope#g1", "m-priya").unwrap_err(),
            DenyReason::NoGrant
        );
        assert_eq!(
            authorize_open(&allowed, &agents, "agent:coder#g-other", "m-priya").unwrap_err(),
            DenyReason::NoGrant,
            "an action id for a grant that no longer exists resolves to nothing"
        );
        assert_eq!(
            authorize_open(
                &sharing(false, allowed.grants.clone()),
                &agents,
                "agent:coder#g1",
                "m-priya"
            )
            .unwrap_err(),
            DenyReason::NoGrant,
            "the master switch off means no grant exists, whatever the relay thinks"
        );

        let disabled = sharing(true, vec![grant("g2", "off", "/repo/x", &["m-priya"])]);
        assert_eq!(
            authorize_open(&disabled, &agents, "agent:off#g2", "m-priya").unwrap_err(),
            DenyReason::AgentDisabled
        );

        let missing = sharing(true, vec![grant("g3", "ghost", "/repo/y", &["m-priya"])]);
        assert_eq!(
            authorize_open(&missing, &agents, "agent:ghost#g3", "m-priya").unwrap_err(),
            DenyReason::AgentMissing
        );

        let no_project = sharing(true, vec![grant("g4", "coder", "  ", &["m-priya"])]);
        assert_eq!(
            authorize_open(&no_project, &agents, "agent:coder#g4", "m-priya").unwrap_err(),
            DenyReason::NoProject
        );
    }

    #[test]
    fn deny_reasons_render_a_terminal_outcome_the_requester_can_read() {
        for r in [
            DenyReason::UnknownOffer,
            DenyReason::NoGrant,
            DenyReason::AgentMissing,
            DenyReason::AgentDisabled,
            DenyReason::AgentNotShareable,
            DenyReason::MemberNotAllowed,
            DenyReason::NoProject,
        ] {
            assert!(!r.outcome().is_empty());
        }
        assert_eq!(DenyReason::MemberNotAllowed.outcome(), "denied");
        assert_eq!(DenyReason::UnknownOffer.outcome(), "unknown_offer");
    }

    #[test]
    fn the_brokered_clone_uses_the_grants_folder_and_never_mutates_the_source() {
        let agents = vec![agent("coder", true)];
        assert_eq!(agents[0].project_path, "/home/me/personal");
        let s = sharing(true, vec![grant("g1", "coder", "/repo/site", &["m-priya"])]);

        // The ONLY way to reach brokered_agent: hold the token authorize_open
        // mints. There is no other constructor for `Authorized`.
        let auth = authorize_open(&s, &agents, "agent:coder#g1", "m-priya").unwrap();
        let brokered = brokered_agent(&auth, "Priya");
        // The grant's folder wins — project_path is NEVER inherited (DESIGN §3).
        assert_eq!(brokered.project_path, "/repo/site");
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Relay));
        assert_eq!(brokered.requester(), "Priya");
        // …and the stored agent is untouched, so a local hotkey run is unchanged.
        assert_eq!(agents[0].project_path, "/home/me/personal");
        assert!(!agents[0].output_sinks.contains(&AgentOutputSink::Relay));
    }

    #[test]
    fn the_brokered_clone_keeps_the_owners_own_sinks() {
        let mut source = agent("coder", true);
        source.output_sinks = vec![AgentOutputSink::Panel, AgentOutputSink::File];
        let agents = vec![source];
        let s = sharing(true, vec![grant("g1", "coder", "/r", &["m"])]);
        let auth = authorize_open(&s, &agents, "agent:coder#g1", "m").unwrap();
        let brokered = brokered_agent(&auth, "M");
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Panel));
        assert!(brokered.output_sinks.contains(&AgentOutputSink::File));
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Relay));

        // Idempotent: an agent that somehow ALREADY carries Relay must not end
        // up with it twice (the run pipeline uses `.contains`, but a duplicated
        // sink would double a future sink's side effect).
        let mut already = agent("coder", true);
        already.output_sinks = vec![AgentOutputSink::Panel, AgentOutputSink::Relay];
        let agents = vec![already];
        let auth = authorize_open(&s, &agents, "agent:coder#g1", "m").unwrap();
        let twice = brokered_agent(&auth, "M");
        assert_eq!(
            twice
                .output_sinks
                .iter()
                .filter(|s| **s == AgentOutputSink::Relay)
                .count(),
            1
        );
    }

    #[test]
    fn the_only_way_to_reach_a_launch_is_through_the_authorisation_token() {
        // The structural half of DESIGN-shared-agents §8, stated as a test so
        // the intent is greppable — the ENFORCEMENT is the type system, and is
        // shown in the task report as a compile error:
        //   * `Authorized` has private fields and no public constructor, so it
        //     cannot be forged from an (agent, grant) pair a caller happens to
        //     have; `authorize_open` is its only source.
        //   * `brokered_agent` demands `&Authorized`.
        //   * `BrokeredRun` has private fields and no public constructor, and
        //     `RunLauncher::launch` demands one.
        // A future author who skips the re-check therefore cannot get a value
        // of the type the launch seam requires.
        let agents = vec![agent("coder", true)];
        let s = sharing(true, vec![grant("g1", "coder", "/repo/site", &["m-priya"])]);

        // A refusal yields NO token at all — there is nothing to pass on.
        assert!(authorize_open(&s, &agents, "agent:coder#g1", "m-stranger").is_err());

        let auth = authorize_open(&s, &agents, "agent:coder#g1", "m-priya").unwrap();
        // (These read the PRIVATE fields — legal only because this test module
        // is a child of `grants`. No other module can do this.)
        assert_eq!(auth.agent.id, "coder");
        assert_eq!(auth.grant.project_path, "/repo/site");
        // The definition can only be moved out of a BrokeredRun, which can only
        // be made from the token.
        let def = brokered_agent(&auth, "Priya").into_definition();
        assert_eq!(def.project_path, "/repo/site");
    }

    #[test]
    fn two_grants_for_one_agent_stay_two_grants() {
        // The reviewer's reproduction, as a test. The owner shares `coder` on
        // the client repo with Priya, and `coder` on the public site with Priya
        // AND Sam. Keyed on the agent, both offers carried `action_id`
        // "agent:coder", `authorize_open`'s `.find` returned the FIRST grant
        // whichever offer was opened, and so: Priya opening the public-website
        // offer got a run in the client repo, and Sam — explicitly allowed on
        // public-website — was refused outright.
        //
        // The single production edit that makes this fail: `offers_from_grants`
        // and `authorize_open` going back to `action_id_for(agent)` instead of
        // `action_id_for_grant(agent, grant)`.
        let agents = vec![agent("coder", true)];
        let s = sharing(
            true,
            vec![
                grant("g-client", "coder", "/repo/acme-client", &["m-priya"]),
                grant(
                    "g-site",
                    "coder",
                    "/repo/public-website",
                    &["m-priya", "m-sam"],
                ),
            ],
        );

        let offers = offers_from_grants(&s, &agents);
        assert_eq!(offers.len(), 2);
        assert_ne!(
            offers[0].action_id, offers[1].action_id,
            "two grants must publish two distinguishable offers"
        );
        assert_eq!(offers[0].action_id, "agent:coder#g-client");
        assert_eq!(offers[1].action_id, "agent:coder#g-site");

        // Each offer resolves to ITS OWN folder…
        let client = authorize_open(&s, &agents, &offers[0].action_id, "m-priya").unwrap();
        assert_eq!(
            brokered_agent(&client, "Priya").project_path,
            "/repo/acme-client"
        );
        let site = authorize_open(&s, &agents, &offers[1].action_id, "m-priya").unwrap();
        assert_eq!(
            brokered_agent(&site, "Priya").project_path,
            "/repo/public-website",
            "opening the public-website offer must not run in the client repo"
        );

        // …and each carries its own member list.
        let sam = authorize_open(&s, &agents, &offers[1].action_id, "m-sam").unwrap();
        assert_eq!(
            brokered_agent(&sam, "Sam").project_path,
            "/repo/public-website"
        );
        assert_eq!(
            authorize_open(&s, &agents, &offers[0].action_id, "m-sam").unwrap_err(),
            DenyReason::MemberNotAllowed,
            "Sam is allowed on public-website only — the client repo stays closed"
        );
    }

    #[test]
    fn a_grant_persisted_before_ids_existed_still_resolves() {
        // Non-breaking principle: the settings store wipes to defaults on ANY
        // parse failure, so `ShareGrant::id` is `#[serde(default)]` and a grant
        // written by a build that predates it must keep working — the same
        // derived identity on both sides, so the offer this host publishes is
        // the offer it later authorises.
        //
        // The single production edit that makes this fail: dropping
        // `#[serde(default)]` from `ShareGrant::id` (the `from_value` below
        // stops parsing), or making `stable_id()` return `""` / something
        // non-deterministic for a blank id (the round trip below stops
        // resolving).
        let stored: ShareGrant = serde_json::from_value(json!({
            "agent_id": "coder",
            "project_path": "/repo/site",
            "allowed_members": ["m-priya"]
        }))
        .expect("a grant with no id must still load");
        assert_eq!(stored, legacy_grant("coder", "/repo/site", &["m-priya"]));
        assert!(!stored.stable_id().is_empty());

        let agents = vec![agent("coder", true)];
        let s = sharing(true, vec![stored]);
        let offers = offers_from_grants(&s, &agents);
        assert_eq!(offers.len(), 1);
        let auth = authorize_open(&s, &agents, &offers[0].action_id, "m-priya").unwrap();
        assert_eq!(
            brokered_agent(&auth, "Priya").project_path,
            "/repo/site",
            "a legacy grant's published offer must authorise back to itself"
        );

        // Two legacy grants for the same agent in different folders are still
        // two grants, with no id ever having been stored for either.
        let two = sharing(
            true,
            vec![
                legacy_grant("coder", "/repo/acme-client", &["m-priya"]),
                legacy_grant("coder", "/repo/public-website", &["m-priya"]),
            ],
        );
        let offers = offers_from_grants(&two, &agents);
        assert_ne!(offers[0].action_id, offers[1].action_id);
        let site = authorize_open(&two, &agents, &offers[1].action_id, "m-priya").unwrap();
        assert_eq!(
            brokered_agent(&site, "Priya").project_path,
            "/repo/public-website"
        );
    }

    #[test]
    fn a_remote_agent_is_never_offered_and_never_authorised() {
        // DESIGN-shared-agents §2: C2 v1 rides the shipped raw-CLI driver. An
        // A2A agent runs at the owner's remote endpoint under the owner's
        // credentials — `drive_remote_run` reads neither `cwd` nor `argv` — so
        // the grant's folder would bound nothing and the Sharing UI's "runs on
        // this machine, in the folder you pick" would be false. The picker
        // refuses to offer one; this is the half that holds even if a grant for
        // one is already stored, or is written into the settings file by hand.
        //
        // The single production edit that makes this fail: dropping the
        // `is_shareable_kind` check from `authorize_open` (the refusal below
        // becomes an `Ok`) or from `offers_from_grants` (the offer reappears).
        let agents = vec![remote_agent("a2a")];
        let s = sharing(true, vec![grant("g1", "a2a", "/repo/site", &["m-priya"])]);

        assert!(
            offers_from_grants(&s, &agents).is_empty(),
            "a remote agent is not shareable, so nothing is published"
        );
        assert_eq!(
            authorize_open(&s, &agents, "agent:a2a#g1", "m-priya").unwrap_err(),
            DenyReason::AgentNotShareable,
        );
        assert_eq!(DenyReason::AgentNotShareable.outcome(), "unavailable");
        assert!(is_shareable_kind(AgentKind::Cli));
        assert!(!is_shareable_kind(AgentKind::Remote));
        assert!(!is_shareable_kind(AgentKind::Prompt));
    }

    #[test]
    fn a_blank_member_id_is_nobody() {
        // `Requester` defaults both of its fields, so an `open` carrying no
        // `requester` object at all arrives with `member_id: ""`. It must never
        // match — not even a grant that somehow stored a blank entry.
        //
        // The single production edit that makes this fail: removing the
        // blank-`member_id` guard from `authorize_open` (the first assertion
        // below turns into an `Ok` as soon as a grant holds a blank entry).
        let agents = vec![agent("coder", true)];

        let blank_entry = sharing(true, vec![grant("g1", "coder", "/repo/site", &["", "m"])]);
        assert_eq!(
            authorize_open(&blank_entry, &agents, "agent:coder#g1", "").unwrap_err(),
            DenyReason::MemberNotAllowed,
            "an anonymous requester must not match a blank allowed_members entry"
        );
        assert_eq!(
            authorize_open(&blank_entry, &agents, "agent:coder#g1", "   ").unwrap_err(),
            DenyReason::MemberNotAllowed
        );
        // The real member on the same grant is unaffected.
        assert!(authorize_open(&blank_entry, &agents, "agent:coder#g1", "m").is_ok());

        let normal = sharing(true, vec![grant("g1", "coder", "/repo/site", &["m-priya"])]);
        assert_eq!(
            authorize_open(&normal, &agents, "agent:coder#g1", "").unwrap_err(),
            DenyReason::MemberNotAllowed
        );
    }
}
