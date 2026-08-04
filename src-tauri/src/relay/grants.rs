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
use crate::settings::{AgentDefinition, AgentOutputSink, ShareGrant, SharingConfig};

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

fn find_agent<'a>(agents: &'a [AgentDefinition], agent_id: &str) -> Option<&'a AgentDefinition> {
    agents.iter().find(|a| a.id == agent_id)
}

/// The offer list to publish on connect. A grant that cannot actually run —
/// disabled agent, deleted agent, no project chosen, nobody allowed — is not
/// advertised, so a teammate never sees an offer that would be refused on open.
pub fn offers_from_grants(sharing: &SharingConfig, agents: &[AgentDefinition]) -> Vec<OfferWire> {
    if !sharing.enabled {
        return Vec::new();
    }
    sharing
        .grants
        .iter()
        .filter_map(|g| {
            let agent = find_agent(agents, &g.agent_id)?;
            if !agent.enabled || g.project_path.trim().is_empty() || g.allowed_members.is_empty() {
                return None;
            }
            Some(OfferWire {
                action_id: action_id_for(agent),
                label: agent.name.clone(),
                project: g.project_path.clone(),
                allowed: g.allowed_members.clone(),
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
            DenyReason::AgentMissing | DenyReason::AgentDisabled | DenyReason::NoProject => {
                "unavailable"
            }
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
/// folder"* — it cannot prove the config was the owner's live settings, because
/// `SharingConfig`/`AgentDefinition` are plain serde settings data with public
/// fields that any caller can construct. No visibility modifier closes that:
/// `pub(in crate::managers)` is illegal from here (`crate::managers` is not an
/// ancestor of `crate::relay::grants`), and moving the config behind a newtype
/// only moves the same forgeable constructor one level up.
///
/// What actually holds the line is that there is exactly ONE production caller
/// — `managers::agent_host::HostState::handle_service_message` — and it reads
/// the live `Mutex` snapshot that `set_config` keeps current. That property is
/// guarded by
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
    let grant = sharing
        .grants
        .iter()
        .find(|g| {
            find_agent(agents, &g.agent_id)
                .map(|a| action_id_for(a) == action_id)
                .unwrap_or(false)
                || format!("agent:{}", g.agent_id) == action_id
        })
        .ok_or(DenyReason::NoGrant)?;

    let agent = find_agent(agents, &grant.agent_id).ok_or(DenyReason::AgentMissing)?;
    if !agent.enabled {
        return Err(DenyReason::AgentDisabled);
    }
    if grant.project_path.trim().is_empty() {
        return Err(DenyReason::NoProject);
    }
    if !grant.allowed_members.iter().any(|m| m == member_id) {
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

    fn sharing(enabled: bool, grants: Vec<ShareGrant>) -> SharingConfig {
        SharingConfig { enabled, grants }
    }

    fn grant(agent_id: &str, project: &str, members: &[&str]) -> ShareGrant {
        ShareGrant {
            agent_id: agent_id.into(),
            project_path: project.into(),
            allowed_members: members.iter().map(|m| m.to_string()).collect(),
        }
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
                grant("coder", "/repo/site", &["m-priya"]),
                grant("off", "/repo/x", &["m-priya"]), // agent disabled
                grant("ghost", "/repo/y", &["m-priya"]), // agent deleted
                grant("coder", "", &["m-priya"]),      // no project chosen
                grant("coder", "/repo/z", &[]),        // nobody allowed
            ],
        );
        let offers = offers_from_grants(&s, &agents);
        assert_eq!(
            offers.len(),
            1,
            "only the one complete, live grant is offered"
        );
        assert_eq!(offers[0].action_id, "agent:coder");
        assert_eq!(offers[0].label, "Agent coder");
        assert_eq!(offers[0].project, "/repo/site");
        assert_eq!(offers[0].allowed, vec!["m-priya".to_string()]);
    }

    #[test]
    fn nothing_is_offered_while_the_master_switch_is_off() {
        let agents = vec![agent("coder", true)];
        let s = sharing(false, vec![grant("coder", "/repo/site", &["m-priya"])]);
        assert!(offers_from_grants(&s, &agents).is_empty());
    }

    #[test]
    fn the_host_re_checks_and_can_disagree_with_the_relay() {
        // DESIGN-shared-agents §8: the relay's check is a convenience; this
        // desktop is the only thing that actually decides. Every one of these
        // arrives ALREADY authorised by the service and is still refused here.
        let agents = vec![agent("coder", true), agent("off", false)];

        let allowed = sharing(true, vec![grant("coder", "/repo/site", &["m-priya"])]);
        let ok = authorize_open(&allowed, &agents, "agent:coder", "m-priya").unwrap();
        assert_eq!(ok.agent.id, "coder");
        assert_eq!(ok.grant.project_path, "/repo/site");

        assert_eq!(
            authorize_open(&allowed, &agents, "agent:coder", "m-someone-else").unwrap_err(),
            DenyReason::MemberNotAllowed
        );
        assert_eq!(
            authorize_open(&allowed, &agents, "agent:nope", "m-priya").unwrap_err(),
            DenyReason::NoGrant
        );
        assert_eq!(
            authorize_open(
                &sharing(false, allowed.grants.clone()),
                &agents,
                "agent:coder",
                "m-priya"
            )
            .unwrap_err(),
            DenyReason::NoGrant,
            "the master switch off means no grant exists, whatever the relay thinks"
        );

        let disabled = sharing(true, vec![grant("off", "/repo/x", &["m-priya"])]);
        assert_eq!(
            authorize_open(&disabled, &agents, "agent:off", "m-priya").unwrap_err(),
            DenyReason::AgentDisabled
        );

        let missing = sharing(true, vec![grant("ghost", "/repo/y", &["m-priya"])]);
        assert_eq!(
            authorize_open(&missing, &agents, "agent:ghost", "m-priya").unwrap_err(),
            DenyReason::AgentMissing
        );

        let no_project = sharing(true, vec![grant("coder", "  ", &["m-priya"])]);
        assert_eq!(
            authorize_open(&no_project, &agents, "agent:coder", "m-priya").unwrap_err(),
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
        let s = sharing(true, vec![grant("coder", "/repo/site", &["m-priya"])]);

        // The ONLY way to reach brokered_agent: hold the token authorize_open
        // mints. There is no other constructor for `Authorized`.
        let auth = authorize_open(&s, &agents, "agent:coder", "m-priya").unwrap();
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
        let s = sharing(true, vec![grant("coder", "/r", &["m"])]);
        let auth = authorize_open(&s, &agents, "agent:coder", "m").unwrap();
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
        let auth = authorize_open(&s, &agents, "agent:coder", "m").unwrap();
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
        let s = sharing(true, vec![grant("coder", "/repo/site", &["m-priya"])]);

        // A refusal yields NO token at all — there is nothing to pass on.
        assert!(authorize_open(&s, &agents, "agent:coder", "m-stranger").is_err());

        let auth = authorize_open(&s, &agents, "agent:coder", "m-priya").unwrap();
        // (These read the PRIVATE fields — legal only because this test module
        // is a child of `grants`. No other module can do this.)
        assert_eq!(auth.agent.id, "coder");
        assert_eq!(auth.grant.project_path, "/repo/site");
        // The definition can only be moved out of a BrokeredRun, which can only
        // be made from the token.
        let def = brokered_agent(&auth, "Priya").into_definition();
        assert_eq!(def.project_path, "/repo/site");
    }
}
