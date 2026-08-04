//! C2 shared agents — status, grant and member commands for the Sharing
//! settings UI (Task 8) and the run panel (Task 9).
//!
//! Additive & dormant-by-default, mirroring `commands/service.rs`: none of
//! this does anything until the owner turns sharing on, and every command
//! that writes settings goes through `settings::write_settings`, which
//! already re-publishes the host's offers on every write (Task 4) — no
//! command here calls the host manager a second time to do that.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};

use crate::managers::agent_host::AgentHostManager;
use crate::managers::service_sync::{KEYRING_ACCOUNT, KEYRING_SCOPE};
use crate::relay::grants::offers_from_grants;
use crate::settings::{get_settings, write_settings, ShareGrant};

fn normalize(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

/// Status snapshot for the Sharing settings UI and the run panel.
#[derive(Serialize, Debug, Clone, Type)]
pub struct SharingStatus {
    /// The master switch (`settings.sharing.enabled`).
    pub enabled: bool,
    /// A device is paired with a self-hosted service (`settings.service_enabled`).
    pub service_paired: bool,
    /// Best-effort: this device is believed to be bound to a service member,
    /// derived from the most recent dial's outcome. `true` until a dial is
    /// actually refused with the specific "not bound to a member" answer
    /// (spec gap #3) — there is no endpoint that answers this directly, so
    /// an owner who has never dialled yet reads as a member rather than as
    /// definitely-not-one.
    pub is_member: bool,
    /// A live, published connection to the relay exists right now.
    pub connected: bool,
    /// How many offers the current settings would publish (or are publishing).
    pub offer_count: u32,
    /// How many teammate sessions are live right now.
    pub active_sessions: u32,
    /// The most recent dial/publish failure, verbatim, if any.
    pub last_error: Option<String>,
}

/// A member of the paired service, as shown to the owner when picking who a
/// grant is for.
#[derive(Serialize, Debug, Clone, Type, PartialEq)]
pub struct ServiceMember {
    pub member_id: String,
    pub display_name: String,
    pub role: String,
    /// A revoked member must not be offerable in the grants UI even though
    /// the service still lists them (Task 8 filters on this).
    pub revoked: bool,
}

/// One row of the `GET /v2/members` response, before the wire's split
/// `revoked_at` timestamp collapses to a plain `bool`.
#[derive(Deserialize, Debug)]
struct RawMember {
    member_id: String,
    display_name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    revoked_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct MembersResponse {
    // No `#[serde(default)]`: `members` is the field that DEFINES this as a
    // `/v2/members` response (mirrors `InfoResponse::version` in
    // `commands/service.rs`). A body missing it entirely is a different
    // shape — a proxy error page, an older/different API — and must surface
    // as "unexpected response", not silently read as "zero members".
    members: Vec<RawMember>,
}

/// Parse the `GET /v2/members` response body into the UI shape. A free,
/// no-I/O function so the v2 shape is a unit test rather than a live round
/// trip — mirrors why `build_pair_request` in `commands/service.rs` is split
/// out the same way.
pub fn parse_members(raw: &serde_json::Value) -> Result<Vec<ServiceMember>, String> {
    let parsed: MembersResponse = serde_json::from_value(raw.clone())
        .map_err(|e| format!("Unexpected response from the service: {e}"))?;
    Ok(parsed
        .members
        .into_iter()
        .map(|m| ServiceMember {
            member_id: m.member_id,
            display_name: m.display_name,
            role: m.role,
            revoked: m.revoked_at.is_some(),
        })
        .collect())
}

/// Reject a grant a teammate could never actually use rather than let it save
/// silently and never be offered (`relay::grants::offers_from_grants` drops
/// exactly these two cases without saying why). Checked in this order so the
/// first message a user sees is the most fundamental thing missing.
pub fn validate_grants(grants: &[ShareGrant]) -> Result<(), String> {
    let mut seen_ids: Vec<&str> = Vec::new();
    for grant in grants {
        if grant.agent_id.trim().is_empty() {
            return Err("Choose an agent to share.".to_string());
        }
        if grant.project_path.trim().is_empty() {
            return Err("Choose a folder for the shared agent to run in.".to_string());
        }
        if grant.allowed_members.is_empty()
            || grant.allowed_members.iter().all(|m| m.trim().is_empty())
        {
            return Err("Allow at least one teammate to run the shared agent.".to_string());
        }
        // A blank member id is nobody: `Requester::member_id` defaults to `""`,
        // so an entry that is blank (or whitespace) would be an entry an
        // anonymous `open` could try to match. `authorize_open` refuses those
        // too — this stops one being stored in the first place.
        if grant.allowed_members.iter().any(|m| m.trim().is_empty()) {
            return Err("A teammate entry is blank — remove it and pick a teammate.".to_string());
        }
        // The grant's id is what the published `action_id` is keyed on
        // (`relay::grants::action_id_for_grant`). Two grants sharing one id
        // would collapse back into a single offer, which is exactly the bug
        // the id exists to prevent, so it is rejected at the save boundary
        // rather than discovered as a teammate running in the wrong folder.
        let id = grant.id.trim();
        if id.is_empty() {
            return Err("This grant has no id. Remove it and add it again.".to_string());
        }
        if seen_ids.contains(&id) {
            return Err("Two grants share an id. Remove one and add it again.".to_string());
        }
        seen_ids.push(id);
    }
    Ok(())
}

/// Whether the last dial failure was specifically the "not bound to a
/// member" refusal (`relay::transport::refused_message(403)`). `true`
/// (assume bound) for `None` or any other error — this is advisory, not a
/// security decision; the real authorization re-check is
/// `relay::grants::authorize_open` on the live socket.
fn is_member_from(last_error: &Option<String>) -> bool {
    !last_error
        .as_deref()
        .is_some_and(|e| e.contains("not bound to a member"))
}

/// Current status for the Sharing settings UI.
#[tauri::command]
#[specta::specta]
pub fn sharing_status(app: AppHandle) -> Result<SharingStatus, String> {
    let settings = get_settings(&app);
    let host = app.state::<Arc<AgentHostManager>>();
    let last_error = host.last_error();
    let offer_count = offers_from_grants(&settings.sharing, &settings.agents).len() as u32;

    Ok(SharingStatus {
        enabled: settings.sharing.enabled,
        service_paired: settings.service_enabled,
        is_member: is_member_from(&last_error),
        connected: host.is_connected(),
        offer_count,
        active_sessions: host.active_sessions(),
        last_error,
    })
}

/// The master sharing switch. `write_settings` republishes (or tears down)
/// the host on its own — see the module doc comment.
#[tauri::command]
#[specta::specta]
pub fn set_sharing_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.sharing.enabled = enabled;
    write_settings(&app, settings);
    Ok(())
}

/// Replace the whole grant list. Validated at this boundary — see
/// `validate_grants` — so a grant with no folder or nobody allowed is a
/// rejected save, not a silent one that is never offered.
#[tauri::command]
#[specta::specta]
pub fn set_share_grants(app: AppHandle, grants: Vec<ShareGrant>) -> Result<(), String> {
    validate_grants(&grants)?;
    let mut settings = get_settings(&app);
    settings.sharing.grants = grants;
    write_settings(&app, settings);
    Ok(())
}

/// List the paired service's members: `GET /v2/members`.
#[tauri::command]
#[specta::specta]
pub async fn list_service_members(app: AppHandle) -> Result<Vec<ServiceMember>, String> {
    let settings = get_settings(&app);
    let base = normalize(&settings.service_url);
    if base.is_empty() {
        return Err("No service URL configured.".to_string());
    }
    let token = crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
    if token.is_empty() {
        return Err("This device is not paired yet.".to_string());
    }

    let resp = http_client()
        .get(format!("{base}/v2/members"))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("Could not reach the service: {e}"))?;

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("The service rejected this device's token. Try re-pairing.".to_string());
    }
    if !status.is_success() {
        return Err(format!("The service returned HTTP {}.", status.as_u16()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Unexpected response from the service: {e}"))?;
    parse_members(&body)
}

/// Redeem a teammate invite: `POST /v2/invites/redeem`. Binds THIS device's
/// existing token to a member (spec gap #3) — it does not mint or change any
/// token, so unlike `pair_service` it has nothing to persist locally and
/// nothing to nudge: a host loop that has been retrying with a 403 dials
/// again on its own next backoff step with the SAME token, which the service
/// now accepts. Returns the new member id.
#[tauri::command]
#[specta::specta]
pub async fn redeem_service_invite(app: AppHandle, code: String) -> Result<String, String> {
    let settings = get_settings(&app);
    let base = normalize(&settings.service_url);
    if base.is_empty() {
        return Err("No service URL configured.".to_string());
    }
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Err("Enter the invite code.".to_string());
    }
    let token = crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
    if token.is_empty() {
        return Err("This device is not paired yet.".to_string());
    }

    let resp = http_client()
        .post(format!("{base}/v2/invites/redeem"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "code": trimmed }))
        .send()
        .await
        .map_err(|e| format!("Could not reach the service: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 => "The service rejected this device's token. Try re-pairing.".to_string(),
            404 => "That invite code was not found or has already been used.".to_string(),
            409 => "This device is already bound to a member.".to_string(),
            other => format!("Redeeming the invite failed (HTTP {other})."),
        });
    }

    #[derive(Deserialize)]
    struct RedeemResponse {
        member_id: String,
    }
    let parsed: RedeemResponse = resp
        .json()
        .await
        .map_err(|e| format!("The service returned an unexpected response: {e}"))?;

    Ok(parsed.member_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(id: &str, agent_id: &str, project: &str, members: &[&str]) -> ShareGrant {
        ShareGrant {
            id: id.into(),
            agent_id: agent_id.into(),
            project_path: project.into(),
            allowed_members: members.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn grants_are_validated_before_they_are_persisted() {
        // A grant with no folder or nobody allowed is not an error the user
        // should discover as silence — it is rejected at the command
        // boundary.
        assert!(validate_grants(&[grant("g1", "coder", "/repo/site", &["m"])]).is_ok());

        let no_project = validate_grants(&[grant("g1", "coder", "  ", &["m"])]);
        assert!(no_project.unwrap_err().contains("folder"));

        let nobody = validate_grants(&[grant("g1", "coder", "/r", &[])]);
        assert!(nobody.unwrap_err().contains("teammate"));

        let no_agent = validate_grants(&[ShareGrant::default()]);
        assert!(no_agent.unwrap_err().contains("agent"));
    }

    #[test]
    fn validate_grants_checks_every_grant_not_just_the_first() {
        let ok_then_bad = validate_grants(&[
            grant("g1", "coder", "/repo/site", &["m"]),
            grant("g2", "writer", "", &["m"]),
        ]);
        assert!(ok_then_bad.unwrap_err().contains("folder"));
    }

    #[test]
    fn a_grant_without_a_usable_id_is_refused() {
        // The id is what `relay::grants::action_id_for_grant` keys the published
        // offer on, so a blank or duplicated one collapses two grants back into
        // one offer — the bug the id exists to prevent. Two grants for the SAME
        // agent in different folders are explicitly fine.
        //
        // The single production edit that makes this fail: deleting the id
        // checks from `validate_grants` (both refusals below become `Ok`).
        assert!(validate_grants(&[
            grant("g1", "coder", "/repo/acme-client", &["m-priya"]),
            grant("g2", "coder", "/repo/public-website", &["m-priya", "m-sam"]),
        ])
        .is_ok());

        let blank = validate_grants(&[grant("  ", "coder", "/repo/site", &["m"])]);
        assert!(blank.unwrap_err().contains("id"), "a blank id is refused");

        let duplicated = validate_grants(&[
            grant("g1", "coder", "/repo/acme-client", &["m"]),
            grant("g1", "coder", "/repo/public-website", &["m"]),
        ]);
        assert!(duplicated.unwrap_err().contains("id"));
    }

    #[test]
    fn a_blank_teammate_entry_is_refused() {
        // `Requester::member_id` defaults to `""`, so a stored blank entry is
        // an entry an anonymous `open` could try to match. `authorize_open`
        // refuses those on the live socket; this stops one being saved at all.
        //
        // The single production edit that makes this fail: removing the
        // blank-entry check from `validate_grants`.
        // A list of nothing but blanks is the same as no list at all, and says
        // so — not "one of your entries is blank".
        let blank_only = validate_grants(&[grant("g1", "coder", "/r", &["  "])]);
        assert!(blank_only.unwrap_err().contains("at least one"));

        let blank_alongside_a_real_one = validate_grants(&[grant("g1", "coder", "/r", &["", "m"])]);
        assert!(blank_alongside_a_real_one.unwrap_err().contains("blank"));
    }

    #[test]
    fn members_are_parsed_from_the_v2_shape() {
        let raw = serde_json::json!({"members":[
            {"member_id":"m1","display_name":"Priya","role":"member","revoked_at":null},
            {"member_id":"m2","display_name":"Sam","role":"owner","revoked_at":"2026-08-01T00:00:00Z"}
        ]});
        let parsed = parse_members(&raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].display_name, "Priya");
        assert!(!parsed[0].revoked);
        assert!(parsed[1].revoked, "a revoked member must not be offerable");
    }

    #[test]
    fn parse_members_rejects_a_shape_that_is_not_the_v2_members_envelope() {
        // `members` missing entirely (a differently-shaped body: a proxy error
        // page, an older API) must not be read as "zero members" — that would
        // show the owner an empty team when the real problem is a wrong URL
        // or an incompatible service. The single production edit this test
        // catches: adding `#[serde(default)]` back onto `MembersResponse::members`.
        let raw = serde_json::json!({"items": []});
        let err = parse_members(&raw).unwrap_err();
        assert!(err.contains("Unexpected response"), "got: {err}");
    }

    #[test]
    fn parse_members_accepts_an_empty_but_present_members_list() {
        // The legitimate "this service has no teammates yet" case must still
        // work — only a MISSING `members` key is treated as malformed.
        let raw = serde_json::json!({"members": []});
        assert_eq!(parse_members(&raw).unwrap(), Vec::new());
    }

    #[test]
    fn is_member_reads_true_unless_the_403_refusal_was_seen() {
        assert!(is_member_from(&None), "nothing dialled yet ⇒ assume bound");
        assert!(is_member_from(&Some(
            "could not reach the service".to_string()
        )));
        assert!(!is_member_from(&Some(
            "the relay refused this device: HTTP 403, its device token is \
             valid but not allowed to host — this device is probably not \
             bound to a member yet; redeem an invite"
                .to_string()
        )));
    }
}
