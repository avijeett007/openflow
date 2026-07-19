//! OpenFlow Service — dictation transcript + usage sync worker.
//!
//! This is a *sibling* pipeline, never part of the core dictation path (hotkey →
//! capture → STT → cleanup → inject). It runs a single background tokio task that,
//! every 30s, reads NEW rows from the existing history SQLite (`history.db`,
//! READ-ONLY — the schema is never modified) and pushes them to a user-paired,
//! self-hosted OpenFlow Service per the frozen contract (DESIGN-openflow-service.md
//! §5/§8). It is fully dormant unless the user has paired and opted in:
//!
//!   * The task is only ever started when `service_enabled && !service_url.empty`.
//!   * Transcript TEXT is pushed ONLY when `service_sync_transcripts` is true — the
//!     hard privacy rule. Usage events (`service_sync_usage`) carry counts/durations
//!     only, never text.
//!   * Its cursor + device identity live in a SEPARATE `service_sync.db`, so the
//!     history database and the dictation loop are untouched.
//!
//! The HTTP calls sit behind the [`ServiceTransport`] trait so the batching /
//! backoff / payload logic is unit-testable with no network.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use log::{debug, warn};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use tauri::AppHandle;

/// Keyring scope/account the device token is stored under. Mirrors the existing
/// `keychain` pattern used for provider API keys — the token NEVER touches the
/// settings store on disk.
pub const KEYRING_SCOPE: &str = "service";
pub const KEYRING_ACCOUNT: &str = "device_token";

/// Max items per push batch (contract §8: batches ≤ 50).
pub const MAX_BATCH: usize = 50;

/// Backoff floor / ceiling on repeated push failure (contract §8: 30s → 5min cap).
pub const BACKOFF_MIN: Duration = Duration::from_secs(30);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Normal poll cadence between sync passes.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Pure data + logic (unit-tested; no I/O)
// ---------------------------------------------------------------------------

/// A minimal, read-only view of one `transcription_history` row that the sync
/// worker cares about. Deliberately decoupled from `HistoryEntry` so this module
/// never depends on the history manager's write path.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncRow {
    /// The history SQLite rowid. Stable and monotonic → used verbatim as the
    /// idempotency `client_id`, so re-sending a row is a server-side no-op.
    pub rowid: i64,
    /// Unix seconds (the history `timestamp` column).
    pub created_at: i64,
    /// The transcript text (STT output).
    pub text: String,
    /// Duration in milliseconds, if the source row carries one. The
    /// `transcription_history` table does not have a duration column today, so
    /// this is `None` in practice — the plumbing honors the contract's
    /// `value=duration_ms` for the day a duration becomes available.
    pub duration_ms: Option<i64>,
}

/// Stable idempotency key for a row: the history rowid as a string. Identical for
/// the transcript push and the paired usage event, so the service can dedupe both
/// on `(device_id, client_id)`.
pub fn client_id_for(rowid: i64) -> String {
    rowid.to_string()
}

/// Split rows into ordered batches of at most [`MAX_BATCH`], preserving order.
pub fn into_batches(rows: &[SyncRow]) -> Vec<Vec<SyncRow>> {
    rows.chunks(MAX_BATCH).map(|c| c.to_vec()).collect()
}

/// Next backoff after a failure: double the current delay, clamped to
/// [`BACKOFF_MIN`, `BACKOFF_MAX`]. A zero/sub-floor input starts at the floor.
pub fn next_backoff(current: Duration) -> Duration {
    if current < BACKOFF_MIN {
        return BACKOFF_MIN;
    }
    let doubled = current.saturating_mul(2);
    if doubled > BACKOFF_MAX {
        BACKOFF_MAX
    } else {
        doubled
    }
}

/// RFC3339 UTC rendering of a unix-seconds timestamp (contract: all timestamps
/// RFC3339 UTC). Falls back to the epoch for an out-of-range value rather than
/// ever panicking on the sync path.
pub fn to_rfc3339(unix_secs: i64) -> String {
    DateTime::from_timestamp(unix_secs, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .to_rfc3339()
}

// ---- wire payloads (contract §5) ----

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct TranscriptItem {
    pub client_id: String,
    pub created_at: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct TranscriptsBody {
    pub items: Vec<TranscriptItem>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct UsageItem {
    pub client_id: String,
    pub kind: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct UsageBody {
    pub items: Vec<UsageItem>,
}

/// Build the `/v1/transcripts` body for a batch (carries TEXT — only ever called
/// when `service_sync_transcripts` is on).
pub fn build_transcripts_body(rows: &[SyncRow]) -> TranscriptsBody {
    TranscriptsBody {
        items: rows
            .iter()
            .map(|r| TranscriptItem {
                client_id: client_id_for(r.rowid),
                created_at: to_rfc3339(r.created_at),
                text: r.text.clone(),
                duration_ms: r.duration_ms,
            })
            .collect(),
    }
}

/// Build the `/v1/usage` body for a batch. Contains NO transcript text — only the
/// stable client_id, kind, timestamp and (optional) duration value.
pub fn build_usage_body(rows: &[SyncRow]) -> UsageBody {
    UsageBody {
        items: rows
            .iter()
            .map(|r| UsageItem {
                client_id: client_id_for(r.rowid),
                kind: "dictation".to_string(),
                created_at: to_rfc3339(r.created_at),
                value: r.duration_ms,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// HTTP transport abstraction
// ---------------------------------------------------------------------------

/// The two write calls the worker makes, behind a trait so logic can be tested
/// without a network. Owned args keep the returned futures `'static` + `Send`.
pub trait ServiceTransport: Send + Sync + 'static {
    fn post_transcripts(
        &self,
        base_url: String,
        token: String,
        body: TranscriptsBody,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;

    fn post_usage(
        &self,
        base_url: String,
        token: String,
        body: UsageBody,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

/// Production transport backed by `reqwest`. Normalizes the base URL and maps
/// non-2xx / transport errors to a short string (logged, then retried).
pub struct HttpTransport {
    client: reqwest::Client,
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    async fn post_json<B: Serialize>(
        &self,
        base_url: &str,
        token: &str,
        path: &str,
        body: &B,
    ) -> Result<(), String> {
        let url = format!("{}{}", normalize_base_url(base_url), path);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let text = resp.text().await.unwrap_or_default();
            Err(format!(
                "HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            ))
        }
    }
}

impl ServiceTransport for HttpTransport {
    async fn post_transcripts(
        &self,
        base_url: String,
        token: String,
        body: TranscriptsBody,
    ) -> Result<(), String> {
        self.post_json(&base_url, &token, "/v1/transcripts", &body)
            .await
    }

    async fn post_usage(
        &self,
        base_url: String,
        token: String,
        body: UsageBody,
    ) -> Result<(), String> {
        self.post_json(&base_url, &token, "/v1/usage", &body).await
    }
}

/// Strip a trailing slash so `format!("{base}/v1/...")` never doubles up.
pub fn normalize_base_url(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Sync-state persistence (its OWN db — history.db is never written)
// ---------------------------------------------------------------------------

fn open_state_db(path: &PathBuf) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sync_state (
            key   TEXT PRIMARY KEY,
            value TEXT
        )",
        [],
    )?;
    Ok(conn)
}

fn state_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM sync_state WHERE key = ?1",
        [key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn state_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

const KEY_CURSOR: &str = "cursor";
const KEY_LAST_SYNC_AT: &str = "last_sync_at";
const KEY_DEVICE_ID: &str = "device_id";
const KEY_DEVICE_NAME: &str = "device_name";

/// A snapshot of sync progress for the status command.
#[derive(Clone, Debug, Default)]
pub struct SyncStateSnapshot {
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub last_sync_at: Option<i64>,
    pub cursor: i64,
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Owns the sync-state db path and the running flag. Managed in Tauri state; the
/// worker task is (re)started idempotently via [`Self::ensure_started`].
pub struct ServiceSyncManager {
    app: AppHandle,
    state_db_path: PathBuf,
    history_db_path: PathBuf,
    running: Arc<AtomicBool>,
}

impl ServiceSyncManager {
    pub fn new(app: &AppHandle) -> Self {
        // Best-effort resolve of the data dir; if it fails we fall back to a
        // relative path (the worker will simply log and never start syncing).
        let data_dir = crate::portable::app_data_dir(app).unwrap_or_else(|_| PathBuf::from("."));
        Self {
            app: app.clone(),
            state_db_path: data_dir.join("service_sync.db"),
            history_db_path: data_dir.join("history.db"),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Persist the device identity after a successful pairing (used by the status
    /// UI). The token itself goes to the keyring, never here.
    pub fn record_pairing(&self, device_id: &str, device_name: &str) {
        if let Ok(conn) = open_state_db(&self.state_db_path) {
            let _ = state_set(&conn, KEY_DEVICE_ID, device_id);
            let _ = state_set(&conn, KEY_DEVICE_NAME, device_name);
        }
    }

    /// Forget the paired device identity + cursor on unpair. Leaves the table so a
    /// later re-pair starts clean.
    pub fn clear_pairing(&self) {
        if let Ok(conn) = open_state_db(&self.state_db_path) {
            let _ = conn.execute("DELETE FROM sync_state", []);
        }
    }

    /// Read a snapshot of sync progress (for `service_status`).
    pub fn snapshot(&self) -> SyncStateSnapshot {
        let mut snap = SyncStateSnapshot::default();
        if let Ok(conn) = open_state_db(&self.state_db_path) {
            snap.device_id = state_get(&conn, KEY_DEVICE_ID);
            snap.device_name = state_get(&conn, KEY_DEVICE_NAME);
            snap.last_sync_at = state_get(&conn, KEY_LAST_SYNC_AT).and_then(|v| v.parse().ok());
            snap.cursor = state_get(&conn, KEY_CURSOR)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
        snap
    }

    /// Count history rows not yet synced (id > cursor). Read-only; returns 0 on
    /// any error so the status UI degrades gracefully.
    pub fn pending_count(&self) -> i64 {
        let cursor = self.snapshot().cursor;
        read_pending_count(&self.history_db_path, cursor).unwrap_or(0)
    }

    /// Stop the worker (unpair / disable). The loop observes the flag each pass.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Start the worker if it is not already running AND the feature is
    /// configured. Idempotent — safe to call from setup, after pairing, and on
    /// settings changes.
    pub fn ensure_started(&self) {
        let settings = crate::settings::get_settings(&self.app);
        if !settings.service_enabled || settings.service_url.trim().is_empty() {
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

        let app = self.app.clone();
        let state_db_path = self.state_db_path.clone();
        let history_db_path = self.history_db_path.clone();
        let running = self.running.clone();
        let transport = Arc::new(HttpTransport::new());

        tauri::async_runtime::spawn(async move {
            run_sync_loop(app, transport, state_db_path, history_db_path, running).await;
        });
    }
}

/// The background loop. Generic over the transport so tests can drive it without a
/// network (the app only ever instantiates it with [`HttpTransport`]).
async fn run_sync_loop<T: ServiceTransport>(
    app: AppHandle,
    transport: Arc<T>,
    state_db_path: PathBuf,
    history_db_path: PathBuf,
    running: Arc<AtomicBool>,
) {
    debug!("service_sync: worker started");
    let mut backoff: Option<Duration> = None;

    while running.load(Ordering::SeqCst) {
        let settings = crate::settings::get_settings(&app);

        // Feature turned off underneath us (unpair) → exit cleanly.
        if !settings.service_enabled || settings.service_url.trim().is_empty() {
            debug!("service_sync: feature disabled, worker exiting");
            break;
        }

        // Nothing to do unless at least one sync type is opted in. Sit idle.
        let want_transcripts = settings.service_sync_transcripts;
        let want_usage = settings.service_sync_usage;
        if !want_transcripts && !want_usage {
            sleep_interruptible(POLL_INTERVAL, &running).await;
            continue;
        }

        let token = match crate::keychain::get_api_key(KEYRING_SCOPE, KEYRING_ACCOUNT) {
            Some(t) if !t.is_empty() => t,
            _ => {
                // Paired flag set but no token (shouldn't happen) — back off.
                warn!("service_sync: enabled but no device token in keyring");
                sleep_interruptible(POLL_INTERVAL, &running).await;
                continue;
            }
        };

        match sync_once(
            transport.as_ref(),
            &settings.service_url,
            &token,
            &state_db_path,
            &history_db_path,
            want_transcripts,
            want_usage,
        )
        .await
        {
            Ok(pushed) => {
                if pushed > 0 {
                    debug!("service_sync: pushed {pushed} row(s)");
                }
                backoff = None;
                sleep_interruptible(POLL_INTERVAL, &running).await;
            }
            Err(e) => {
                let delay = next_backoff(backoff.unwrap_or(Duration::ZERO));
                backoff = Some(delay);
                warn!(
                    "service_sync: push failed ({e}); retrying in {}s",
                    delay.as_secs()
                );
                sleep_interruptible(delay, &running).await;
            }
        }
    }

    running.store(false, Ordering::SeqCst);
    debug!("service_sync: worker stopped");
}

/// One sync pass: read rows past the cursor, push in batches, advance the cursor
/// only on success. Returns the number of rows pushed. Never touches history.db
/// except to READ.
async fn sync_once<T: ServiceTransport>(
    transport: &T,
    base_url: &str,
    token: &str,
    state_db_path: &PathBuf,
    history_db_path: &PathBuf,
    want_transcripts: bool,
    want_usage: bool,
) -> Result<usize, String> {
    let cursor = {
        let conn = open_state_db(state_db_path).map_err(|e| format!("state db: {e}"))?;
        state_get(&conn, KEY_CURSOR)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };

    let rows = read_history_since(history_db_path, cursor, 500)
        .map_err(|e| format!("history read: {e}"))?;
    if rows.is_empty() {
        return Ok(0);
    }

    let mut pushed = 0usize;
    for batch in into_batches(&rows) {
        if batch.is_empty() {
            continue;
        }
        // Privacy rule: transcript TEXT is only ever sent when opted in.
        if want_transcripts {
            transport
                .post_transcripts(
                    base_url.to_string(),
                    token.to_string(),
                    build_transcripts_body(&batch),
                )
                .await?;
        }
        if want_usage {
            transport
                .post_usage(
                    base_url.to_string(),
                    token.to_string(),
                    build_usage_body(&batch),
                )
                .await?;
        }

        // Advance the cursor to the last row of this batch and stamp the sync
        // time. Only reached when the required post(s) above succeeded.
        let last_rowid = batch.last().map(|r| r.rowid).unwrap_or(cursor);
        let conn = open_state_db(state_db_path).map_err(|e| format!("state db: {e}"))?;
        state_set(&conn, KEY_CURSOR, &last_rowid.to_string())
            .map_err(|e| format!("cursor persist: {e}"))?;
        state_set(
            &conn,
            KEY_LAST_SYNC_AT,
            &chrono::Utc::now().timestamp().to_string(),
        )
        .map_err(|e| format!("last_sync persist: {e}"))?;
        pushed += batch.len();
    }

    Ok(pushed)
}

/// READ-ONLY query of the existing history db for rows past the cursor.
fn read_history_since(
    history_db_path: &PathBuf,
    cursor: i64,
    limit: i64,
) -> rusqlite::Result<Vec<SyncRow>> {
    let conn = Connection::open_with_flags(
        history_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, transcription_text
         FROM transcription_history
         WHERE id > ?1
         ORDER BY id ASC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![cursor, limit], |row| {
            Ok(SyncRow {
                rowid: row.get(0)?,
                created_at: row.get(1)?,
                text: row.get(2)?,
                duration_ms: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// READ-ONLY count of history rows past the cursor (for status pending_count).
fn read_pending_count(history_db_path: &PathBuf, cursor: i64) -> rusqlite::Result<i64> {
    let conn = Connection::open_with_flags(
        history_db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.query_row(
        "SELECT COUNT(*) FROM transcription_history WHERE id > ?1",
        [cursor],
        |row| row.get(0),
    )
}

/// Sleep up to `dur`, waking early (in ~1s slices) if `running` is cleared, so an
/// unpair takes effect promptly rather than after a full poll interval.
async fn sleep_interruptible(dur: Duration, running: &Arc<AtomicBool>) {
    let mut remaining = dur;
    let step = Duration::from_secs(1);
    while remaining > Duration::ZERO {
        if !running.load(Ordering::SeqCst) {
            return;
        }
        let this = remaining.min(step);
        tokio::time::sleep(this).await;
        remaining = remaining.saturating_sub(this);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rowid: i64) -> SyncRow {
        SyncRow {
            rowid,
            created_at: 1_600_000_000 + rowid,
            text: format!("row {rowid}"),
            duration_ms: None,
        }
    }

    #[test]
    fn batching_splits_into_chunks_of_50() {
        let rows: Vec<SyncRow> = (1..=125).map(row).collect();
        let batches = into_batches(&rows);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 50);
        assert_eq!(batches[1].len(), 50);
        assert_eq!(batches[2].len(), 25);
        // Order is preserved end to end.
        assert_eq!(batches[0][0].rowid, 1);
        assert_eq!(batches[2][24].rowid, 125);
    }

    #[test]
    fn batching_handles_empty_and_exact_boundary() {
        assert!(into_batches(&[]).is_empty());
        let exact: Vec<SyncRow> = (1..=50).map(row).collect();
        let b = into_batches(&exact);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].len(), 50);
        let plus_one: Vec<SyncRow> = (1..=51).map(row).collect();
        let b2 = into_batches(&plus_one);
        assert_eq!(b2.len(), 2);
        assert_eq!(b2[1].len(), 1);
    }

    #[test]
    fn client_id_is_the_rowid_and_stable() {
        assert_eq!(client_id_for(42), "42");
        // Same row → same client_id every time (idempotency guarantee).
        assert_eq!(client_id_for(42), client_id_for(42));
        // The transcript push and the usage push for a row share the client_id.
        let r = row(7);
        let t = build_transcripts_body(std::slice::from_ref(&r));
        let u = build_usage_body(std::slice::from_ref(&r));
        assert_eq!(t.items[0].client_id, "7");
        assert_eq!(u.items[0].client_id, "7");
        assert_eq!(t.items[0].client_id, u.items[0].client_id);
    }

    #[test]
    fn backoff_progression_doubles_and_caps() {
        // Starts at the 30s floor from zero.
        let d0 = next_backoff(Duration::ZERO);
        assert_eq!(d0, BACKOFF_MIN);
        // Doubles: 30 → 60 → 120 → 240 → 300 (cap), then stays at 300.
        let d1 = next_backoff(d0);
        assert_eq!(d1, Duration::from_secs(60));
        let d2 = next_backoff(d1);
        assert_eq!(d2, Duration::from_secs(120));
        let d3 = next_backoff(d2);
        assert_eq!(d3, Duration::from_secs(240));
        let d4 = next_backoff(d3);
        assert_eq!(d4, BACKOFF_MAX); // capped at 300
        let d5 = next_backoff(d4);
        assert_eq!(d5, BACKOFF_MAX); // stays capped
    }

    #[test]
    fn usage_body_never_contains_text() {
        let rows: Vec<SyncRow> = (1..=3).map(row).collect();
        let usage = build_usage_body(&rows);
        let json = serde_json::to_string(&usage).unwrap();
        // The transcript text ("row 1" etc.) must NOT appear in a usage payload.
        assert!(!json.contains("row 1"));
        assert!(json.contains("\"kind\":\"dictation\""));
        assert_eq!(usage.items.len(), 3);
    }

    #[test]
    fn transcript_body_carries_text_and_rfc3339_timestamp() {
        let r = SyncRow {
            rowid: 5,
            created_at: 1_600_000_000,
            text: "hello world".to_string(),
            duration_ms: Some(4200),
        };
        let body = build_transcripts_body(std::slice::from_ref(&r));
        assert_eq!(body.items[0].text, "hello world");
        assert_eq!(body.items[0].client_id, "5");
        assert_eq!(body.items[0].duration_ms, Some(4200));
        // RFC3339 UTC (2020-09-13T12:26:40+00:00).
        assert!(body.items[0].created_at.starts_with("2020-09-13T12:26:40"));
    }

    #[test]
    fn duration_is_omitted_from_json_when_absent() {
        let body = build_transcripts_body(std::slice::from_ref(&row(1)));
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("duration_ms"));
    }

    #[test]
    fn normalize_base_url_strips_trailing_slash() {
        assert_eq!(normalize_base_url("https://x.io/"), "https://x.io");
        assert_eq!(normalize_base_url("https://x.io"), "https://x.io");
        assert_eq!(normalize_base_url("  https://x.io/  "), "https://x.io");
    }
}
