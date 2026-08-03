//! The pure allow/deny/ask decision. No I/O, no Tauri — so the whole matrix is
//! table-testable. DESIGN-acp-agents.md §8.
//!
//! Consumed by `managers::agent_run::run_acp_turn`, which feeds `decide` the
//! agent's offered options and never invents one of its own.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::acp::protocol::PermissionOptionWire;

/// What to do when an ACP agent calls `session/request_permission`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Type, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpPermissionPolicy {
    /// Prompt the user in the run panel. Default.
    #[default]
    Ask,
    /// Auto-allow `edit` tool calls; prompt for everything else. Approximates
    /// the pre-ACP `--permission-mode acceptEdits` behaviour.
    AutoEdits,
    /// Auto-allow everything. Opt-in, surfaced with a warning in the UI.
    AutoAll,
}

/// Answers the user gave during THIS session that outlive a single request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionOverride {
    pub allow_all: bool,
    pub denied_kinds: Vec<String>,
}

pub struct PolicyInput {
    pub policy: AcpPermissionPolicy,
    pub tool_kind: String,
    pub session_override: Option<SessionOverride>,
    pub options: Vec<PermissionOptionWire>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow { option_id: String, automatic: bool },
    Deny { option_id: String, automatic: bool },
    Ask,
}

/// Choose an option id the AGENT offered. We never invent one.
pub fn pick_option(options: &[PermissionOptionWire], want_allow: bool) -> Option<String> {
    let (once, always) = if want_allow {
        ("allow_once", "allow_always")
    } else {
        ("reject_once", "reject_always")
    };
    // Prefer the one-shot form: an automatic decision must not silently grant or
    // withhold a persistent one on the user's behalf.
    options
        .iter()
        .find(|o| o.kind == once)
        .or_else(|| options.iter().find(|o| o.kind == always))
        .map(|o| o.option_id.clone())
}

pub fn decide(input: &PolicyInput) -> PermissionDecision {
    // 1. A deny the user already gave this session is the strongest signal.
    if let Some(ov) = &input.session_override {
        if ov.denied_kinds.iter().any(|k| k == &input.tool_kind) {
            return match pick_option(&input.options, false) {
                Some(option_id) => PermissionDecision::Deny {
                    option_id,
                    automatic: true,
                },
                None => PermissionDecision::Ask,
            };
        }
        if ov.allow_all {
            return match pick_option(&input.options, true) {
                Some(option_id) => PermissionDecision::Allow {
                    option_id,
                    automatic: true,
                },
                None => PermissionDecision::Ask,
            };
        }
    }

    // 2. Policy.
    let auto_allow = match input.policy {
        AcpPermissionPolicy::Ask => false,
        AcpPermissionPolicy::AutoEdits => input.tool_kind == "edit",
        AcpPermissionPolicy::AutoAll => true,
    };
    if !auto_allow {
        return PermissionDecision::Ask;
    }
    match pick_option(&input.options, true) {
        Some(option_id) => PermissionDecision::Allow {
            option_id,
            automatic: true,
        },
        // The agent offered no allow option — asking is the only honest answer.
        None => PermissionDecision::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<PermissionOptionWire> {
        vec![
            PermissionOptionWire {
                option_id: "a1".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            },
            PermissionOptionWire {
                option_id: "aa".into(),
                name: "Always allow".into(),
                kind: "allow_always".into(),
            },
            PermissionOptionWire {
                option_id: "d1".into(),
                name: "Deny".into(),
                kind: "reject_once".into(),
            },
        ]
    }

    fn input(policy: AcpPermissionPolicy, kind: &str) -> PolicyInput {
        PolicyInput {
            policy,
            tool_kind: kind.into(),
            session_override: None,
            options: opts(),
        }
    }

    #[test]
    fn ask_policy_always_asks() {
        for kind in ["edit", "execute", "read", "delete", "whatever"] {
            assert!(matches!(
                decide(&input(AcpPermissionPolicy::Ask, kind)),
                PermissionDecision::Ask
            ));
        }
    }

    #[test]
    fn auto_all_allows_every_kind_automatically() {
        for kind in ["edit", "execute", "read", "delete"] {
            match decide(&input(AcpPermissionPolicy::AutoAll, kind)) {
                PermissionDecision::Allow { automatic, .. } => assert!(automatic),
                d => panic!("expected automatic Allow for {kind}, got {d:?}"),
            }
        }
    }

    #[test]
    fn auto_edits_allows_only_edit_and_asks_for_the_rest() {
        match decide(&input(AcpPermissionPolicy::AutoEdits, "edit")) {
            PermissionDecision::Allow { automatic, .. } => assert!(automatic),
            d => panic!("expected Allow, got {d:?}"),
        }
        for kind in ["execute", "delete", "fetch"] {
            assert!(
                matches!(
                    decide(&input(AcpPermissionPolicy::AutoEdits, kind)),
                    PermissionDecision::Ask
                ),
                "kind {kind} must ask"
            );
        }
    }

    #[test]
    fn session_allow_all_override_beats_ask_policy() {
        let mut i = input(AcpPermissionPolicy::Ask, "execute");
        i.session_override = Some(SessionOverride {
            allow_all: true,
            denied_kinds: vec![],
        });
        match decide(&i) {
            PermissionDecision::Allow { automatic, .. } => assert!(automatic),
            d => panic!("expected Allow, got {d:?}"),
        }
    }

    #[test]
    fn session_denied_kind_beats_auto_all() {
        let mut i = input(AcpPermissionPolicy::AutoAll, "execute");
        i.session_override = Some(SessionOverride {
            allow_all: false,
            denied_kinds: vec!["execute".into()],
        });
        // A deny_always answer is a stronger signal than a permissive policy.
        match decide(&i) {
            PermissionDecision::Deny { automatic, .. } => assert!(automatic),
            d => panic!("expected Deny, got {d:?}"),
        }
    }

    #[test]
    fn session_deny_beats_session_allow_all_not_just_policy() {
        // Both override signals fire on the SAME request: a persistent deny the
        // user already gave for "execute", plus a stale allow_all flag. Deny
        // must win — this is the case that pins the ordering of the two `if`s
        // inside the override block, not just "override beats policy".
        let mut i = input(AcpPermissionPolicy::Ask, "execute");
        i.session_override = Some(SessionOverride {
            allow_all: true,
            denied_kinds: vec!["execute".into()],
        });
        match decide(&i) {
            PermissionDecision::Deny { automatic, .. } => assert!(automatic),
            d => panic!(
                "expected Deny (a persistent deny must not be overridden by a stale allow_all), got {d:?}"
            ),
        }
    }

    #[test]
    fn session_deny_falls_back_to_ask_when_agent_offers_no_reject_option() {
        let mut i = input(AcpPermissionPolicy::Ask, "execute");
        i.session_override = Some(SessionOverride {
            allow_all: false,
            denied_kinds: vec!["execute".into()],
        });
        i.options = vec![PermissionOptionWire {
            option_id: "a1".into(),
            name: "Allow".into(),
            kind: "allow_once".into(),
        }];
        assert!(matches!(decide(&i), PermissionDecision::Ask));
    }

    #[test]
    fn session_allow_all_falls_back_to_ask_when_agent_offers_no_allow_option() {
        let mut i = input(AcpPermissionPolicy::Ask, "execute");
        i.session_override = Some(SessionOverride {
            allow_all: true,
            denied_kinds: vec![],
        });
        i.options = vec![PermissionOptionWire {
            option_id: "d1".into(),
            name: "Deny".into(),
            kind: "reject_once".into(),
        }];
        assert!(matches!(decide(&i), PermissionDecision::Ask));
    }

    #[test]
    fn falls_back_to_ask_when_the_agent_offers_no_allow_option() {
        let mut i = input(AcpPermissionPolicy::AutoAll, "edit");
        i.options = vec![PermissionOptionWire {
            option_id: "d".into(),
            name: "Deny".into(),
            kind: "reject_once".into(),
        }];
        // We never invent a button the agent did not offer — DESIGN §5.
        assert!(matches!(decide(&i), PermissionDecision::Ask));
    }

    #[test]
    fn pick_option_prefers_once_over_always() {
        let id = pick_option(&opts(), true).unwrap();
        assert_eq!(
            id, "a1",
            "auto-approval must not silently grant a persistent always"
        );
    }
}
