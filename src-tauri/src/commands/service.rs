//! OpenFlow Service — pairing / status / connection-test Tauri commands.
//!
//! Additive & dormant-by-default: none of this runs until the user pairs a
//! self-hosted service. The device token lives in the OS keyring (never the
//! settings store); the URL + opt-in flags live in settings. See the frozen
//! contract DESIGN-openflow-service.md §5 (API) and §8 (desktop integration).

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager};

use crate::managers::service_sync::{ServiceSyncManager, KEYRING_ACCOUNT, KEYRING_SCOPE};
use crate::settings::{get_settings, write_settings};

/// Body POSTed to `/v1/pair`. Pure struct so its construction is unit-testable.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct PairRequest {
    pub setup_token: String,
    pub device_name: String,
    pub platform: String,
    pub app_version: String,
}

/// Construct the pairing request body from the device's own identity. Kept as a
/// free function (no I/O) so a test can assert the exact wire shape.
pub fn build_pair_request(
    setup_token: &str,
    device_name: &str,
    platform: &str,
    app_version: &str,
) -> PairRequest {
    PairRequest {
        setup_token: setup_token.trim().to_string(),
        device_name: device_name.to_string(),
        platform: platform.to_string(),
        app_version: app_version.to_string(),
    }
}

/// `201` response from `/v1/pair`.
#[derive(Deserialize, Debug)]
struct PairResponse {
    device_id: String,
    device_token: String,
}

/// Returned to the UI after a successful pairing.
#[derive(Serialize, Debug, Clone, Type)]
pub struct PairedDeviceInfo {
    pub device_id: String,
    pub device_name: String,
    pub url: String,
}

/// Status snapshot for the settings UI.
#[derive(Serialize, Debug, Clone, Type)]
pub struct ServiceStatus {
    /// A URL is configured (feature is at least half-set-up).
    pub configured: bool,
    /// A device is paired and the integration is active.
    pub enabled: bool,
    pub url: String,
    pub sync_transcripts: bool,
    pub sync_usage: bool,
    pub paired_device_name: Option<String>,
    /// Unix seconds of the last successful push, if any.
    pub last_sync_at: Option<i64>,
    /// History rows not yet synced (informational).
    pub pending_count: Option<i64>,
}

/// `/v1/info` response.
#[derive(Deserialize, Debug)]
struct InfoResponse {
    version: String,
    #[serde(default)]
    edition: String,
    #[serde(default)]
    modules: InfoModules,
    #[serde(default)]
    #[allow(dead_code)]
    server_time: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct InfoModules {
    #[serde(default)]
    stt: bool,
    #[serde(default)]
    llm: bool,
    #[serde(default)]
    memory: bool,
}

/// Typed result of a connection test / info fetch.
#[derive(Serialize, Debug, Clone, Type)]
pub struct ServiceInfo {
    pub version: String,
    pub edition: String,
    pub module_stt: bool,
    pub module_llm: bool,
    pub module_memory: bool,
}

fn normalize(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

/// Map a non-success pairing status to a friendly, user-facing message.
fn pair_error_for_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "Pairing rejected: the setup token is wrong or expired.".to_string(),
        404 => "No OpenFlow Service found at that URL. Check the address.".to_string(),
        409 => "This device is already paired with the service.".to_string(),
        429 => "Too many pairing attempts. Wait a minute and try again.".to_string(),
        500..=599 => "The service had an internal error. Try again shortly.".to_string(),
        _ => format!("Pairing failed (HTTP {}).", status.as_u16()),
    }
}

/// Pair this device with a self-hosted OpenFlow Service.
///
/// On `201`: store the returned `device_token` in the OS keyring, persist the URL,
/// set `service_enabled = true`, record the device identity, and start the sync
/// worker. On any error: friendly message, nothing persisted.
#[tauri::command]
#[specta::specta]
pub async fn pair_service(
    app: AppHandle,
    url: String,
    setup_token: String,
) -> Result<PairedDeviceInfo, String> {
    let base = normalize(&url);
    if base.is_empty() {
        return Err("Enter the service URL.".to_string());
    }
    if setup_token.trim().is_empty() {
        return Err("Enter the setup token from your service.".to_string());
    }

    let device_name = tauri_plugin_os::hostname();
    let platform = tauri_plugin_os::platform().to_string();
    let app_version = app.package_info().version.to_string();
    let body = build_pair_request(&setup_token, &device_name, &platform, &app_version);

    let resp = http_client()
        .post(format!("{base}/v1/pair"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Could not reach the service: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(pair_error_for_status(status));
    }

    let parsed: PairResponse = resp
        .json()
        .await
        .map_err(|e| format!("The service returned an unexpected response: {e}"))?;

    // Token → keyring ONLY (never the settings store).
    crate::keychain::set_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT, &parsed.device_token)
        .map_err(|e| format!("Could not store the device token securely: {e}"))?;

    // Persist URL + enable; the token is intentionally absent from settings.
    let mut settings = get_settings(&app);
    settings.service_url = base.clone();
    settings.service_enabled = true;
    write_settings(&app, settings);

    // Record identity + kick the worker (idempotent).
    let manager = app.state::<Arc<ServiceSyncManager>>();
    manager.record_pairing(&parsed.device_id, &device_name);
    manager.ensure_started();

    Ok(PairedDeviceInfo {
        device_id: parsed.device_id,
        device_name,
        url: base,
    })
}

/// Unpair: best-effort revoke on the server, then remove the local token and
/// disable the feature. Always succeeds locally even if the server is unreachable.
#[tauri::command]
#[specta::specta]
pub async fn unpair_service(app: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app);
    let base = normalize(&settings.service_url);
    let token = crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT).unwrap_or_default();
    let device_id = {
        let manager = app.state::<Arc<ServiceSyncManager>>();
        manager.snapshot().device_id
    };

    // Best-effort server-side revoke (DELETE /v1/devices/{self}). Ignore failures.
    if !base.is_empty() && !token.is_empty() {
        if let Some(id) = device_id {
            let _ = http_client()
                .delete(format!("{base}/v1/devices/{id}"))
                .bearer_auth(&token)
                .send()
                .await;
        }
    }

    // Stop the worker, forget the token, clear local state, disable the feature.
    {
        let manager = app.state::<Arc<ServiceSyncManager>>();
        manager.stop();
        manager.clear_pairing();
    }
    let _ = crate::keychain::delete_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT);

    let mut settings = get_settings(&app);
    settings.service_enabled = false;
    settings.service_sync_transcripts = false;
    settings.service_sync_usage = false;
    // Keep the URL so the field stays pre-filled for a quick re-pair.
    write_settings(&app, settings);

    Ok(())
}

/// Current status for the settings UI.
#[tauri::command]
#[specta::specta]
pub fn service_status(app: AppHandle) -> Result<ServiceStatus, String> {
    let settings = get_settings(&app);
    let manager = app.state::<Arc<ServiceSyncManager>>();
    let snap = manager.snapshot();
    let configured = !settings.service_url.trim().is_empty();

    Ok(ServiceStatus {
        configured,
        enabled: settings.service_enabled,
        url: settings.service_url.clone(),
        sync_transcripts: settings.service_sync_transcripts,
        sync_usage: settings.service_sync_usage,
        paired_device_name: snap.device_name,
        last_sync_at: snap.last_sync_at,
        pending_count: if settings.service_enabled {
            Some(manager.pending_count())
        } else {
            None
        },
    })
}

/// Test the connection with the stored token: `GET /v1/info`.
#[tauri::command]
#[specta::specta]
pub async fn test_service_connection(app: AppHandle) -> Result<ServiceInfo, String> {
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
        .get(format!("{base}/v1/info"))
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

    let info: InfoResponse = resp
        .json()
        .await
        .map_err(|e| format!("Unexpected response from the service: {e}"))?;

    Ok(ServiceInfo {
        version: info.version,
        edition: if info.edition.is_empty() {
            "community".to_string()
        } else {
            info.edition
        },
        module_stt: info.modules.stt,
        module_llm: info.modules.llm,
        module_memory: info.modules.memory,
    })
}

/// Toggle transcript sync (privacy-critical opt-in). Also nudges the worker so a
/// freshly-enabled sync starts promptly.
#[tauri::command]
#[specta::specta]
pub fn set_service_sync_transcripts(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.service_sync_transcripts = enabled;
    write_settings(&app, settings);
    app.state::<Arc<ServiceSyncManager>>().ensure_started();
    Ok(())
}

/// Toggle usage-event sync (counts/durations only — never text).
#[tauri::command]
#[specta::specta]
pub fn set_service_sync_usage(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.service_sync_usage = enabled;
    write_settings(&app, settings);
    app.state::<Arc<ServiceSyncManager>>().ensure_started();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_request_body_is_constructed_verbatim() {
        let req = build_pair_request("  tok-123  ", "My-Mac", "macos", "0.14.0");
        // Setup token is trimmed; identity fields pass through untouched.
        assert_eq!(req.setup_token, "tok-123");
        assert_eq!(req.device_name, "My-Mac");
        assert_eq!(req.platform, "macos");
        assert_eq!(req.app_version, "0.14.0");
    }

    #[test]
    fn pair_request_serializes_to_the_contract_shape() {
        let req = build_pair_request("tok", "host", "windows", "1.2.3");
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["setup_token"], "tok");
        assert_eq!(json["device_name"], "host");
        assert_eq!(json["platform"], "windows");
        assert_eq!(json["app_version"], "1.2.3");
        // Exactly the four contract fields, nothing extra.
        assert_eq!(json.as_object().unwrap().len(), 4);
    }

    #[test]
    fn pair_error_messages_map_common_statuses() {
        use reqwest::StatusCode;
        assert!(pair_error_for_status(StatusCode::UNAUTHORIZED).contains("setup token"));
        assert!(pair_error_for_status(StatusCode::NOT_FOUND).contains("No OpenFlow Service"));
        assert!(pair_error_for_status(StatusCode::TOO_MANY_REQUESTS).contains("Too many"));
        assert!(pair_error_for_status(StatusCode::CONFLICT).contains("already paired"));
    }
}
