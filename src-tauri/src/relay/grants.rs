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
#[allow(dead_code)] // called from offers_from_grants/authorize_open, and by agent_host.rs (a later task)
pub fn action_id_for(agent: &AgentDefinition) -> String {
    if agent.binding_id.trim().is_empty() {
        format!("agent:{}", agent.id)
    } else {
        agent.binding_id.clone()
    }
}

#[allow(dead_code)] // called from offers_from_grants/authorize_open, and by agent_host.rs (a later task)
fn find_agent<'a>(agents: &'a [AgentDefinition], agent_id: &str) -> Option<&'a AgentDefinition> {
    agents.iter().find(|a| a.id == agent_id)
}

/// The offer list to publish on connect. A grant that cannot actually run —
/// disabled agent, deleted agent, no project chosen, nobody allowed — is not
/// advertised, so a teammate never sees an offer that would be refused on open.
#[allow(dead_code)] // called by agent_host.rs on connect/hello (a later task)
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

#[allow(dead_code)] // constructed by authorize_open, matched by agent_host.rs (a later task)
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
    #[allow(dead_code)] // reported to the requester by agent_host.rs (a later task)
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

#[allow(dead_code)] // constructed by authorize_open, consumed by agent_host.rs (a later task)
#[derive(Debug)]
pub struct Authorized {
    pub agent: AgentDefinition,
    pub grant: ShareGrant,
}

/// Re-check an incoming `open` against the CURRENT settings. Called for every
/// session, however confident the relay was.
#[allow(dead_code)] // called by agent_host.rs on every incoming `open` (a later task)
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
#[allow(dead_code)] // called by agent_host.rs on a successful authorize_open (a later task)
pub fn brokered_agent(agent: &AgentDefinition, grant: &ShareGrant) -> AgentDefinition {
    let mut a = agent.clone();
    a.project_path = grant.project_path.clone();
    if !a.output_sinks.contains(&AgentOutputSink::Relay) {
        a.output_sinks.push(AgentOutputSink::Relay);
    }
    a
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
        let source = agent("coder", true);
        assert_eq!(source.project_path, "/home/me/personal");
        let g = grant("coder", "/repo/site", &["m-priya"]);

        let brokered = brokered_agent(&source, &g);
        // The grant's folder wins — project_path is NEVER inherited (DESIGN §3).
        assert_eq!(brokered.project_path, "/repo/site");
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Relay));
        // …and the stored agent is untouched, so a local hotkey run is unchanged.
        assert_eq!(source.project_path, "/home/me/personal");
        assert!(!source.output_sinks.contains(&AgentOutputSink::Relay));
    }

    #[test]
    fn the_brokered_clone_keeps_the_owners_own_sinks() {
        let mut source = agent("coder", true);
        source.output_sinks = vec![AgentOutputSink::Panel, AgentOutputSink::File];
        let brokered = brokered_agent(&source, &grant("coder", "/r", &["m"]));
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Panel));
        assert!(brokered.output_sinks.contains(&AgentOutputSink::File));
        assert!(brokered.output_sinks.contains(&AgentOutputSink::Relay));
        // Idempotent: applying twice must not duplicate the sink.
        let twice = brokered_agent(&brokered, &grant("coder", "/r", &["m"]));
        assert_eq!(
            twice
                .output_sinks
                .iter()
                .filter(|s| **s == AgentOutputSink::Relay)
                .count(),
            1
        );
    }
}
