//! The relay transport seam.
//!
//! Mirrors `A2aTransport` (a2a.rs:651) and `ServiceTransport`
//! (service_sync.rs:173): the network lives behind a trait so the host loop's
//! logic — hello, dispatch, authorisation, backoff, session bookkeeping — is
//! unit-testable with a scripted in-memory transport and NO socket. Native
//! RPITIT, no `async_trait`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// Reconnect floor / ceiling. Faster than service_sync's 30s→5min: a dropped
/// relay socket leaves a teammate staring at a dead session.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Cap on a single dial. Without it an unreachable service blocks in
/// `connect_async` for the OS TCP timeout (~75s on macOS) — during which the
/// loop is uninterruptible, cannot observe a `stop()`, and the backoff schedule
/// is meaningless because one "attempt" outlasts the whole ladder.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

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

/// Prefix on a dial error the SERVICE refused, as opposed to one it never
/// answered. See [`dial_was_refused`].
pub const REFUSED_PREFIX: &str = "the service refused this device";

/// Whether a failed dial was **refused** (a bad/revoked credential) rather than
/// merely unreachable (an outage).
///
/// Established live, not from prose: `openflow-service` (`feat/relay-v0.2`)
/// answers the `GET /v2/relay/host` upgrade for a revoked *or* never-issued
/// device token with `HTTP/1.1 401 Unauthorized` and
/// `{"error":{"code":"unauthorized","message":"invalid or revoked token"}}`,
/// while a service that is down surfaces as `Connection refused (os error 61)`.
/// Before this, `WsConnector` flattened both into one string and `run_host_loop`
/// logged both at `warn` and retried both forever — so an owner whose device had
/// been revoked saw exactly what an owner with a flaky network saw.
///
/// **The retry itself is deliberately unchanged.** A 401 is not always
/// permanent (the service may be mid-restore, or the owner may be about to
/// re-pair), and giving up would replace a noisy log with a host that silently
/// never comes back. What changes is that the log now says which of the two it
/// is. Surfacing it in the UI belongs to the settings task, not here.
pub fn dial_was_refused(error: &str) -> bool {
    error.starts_with(REFUSED_PREFIX)
}

/// The host socket URL for a configured service base URL.
/// `GET /v2/relay/host` per DESIGN-relay-v02 §5.
pub fn relay_ws_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let b = if let Some(rest) = b.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = b.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        b.to_string()
    };
    format!("{b}/v2/relay/host")
}

/// One live host socket. Owned args keep the returned futures `'static + Send`.
pub trait RelayTransport: Send + Sync + 'static {
    fn send(&self, line: String) -> impl std::future::Future<Output = Result<(), String>> + Send;
    /// `None` when the socket closes.
    fn recv(&self) -> impl std::future::Future<Output = Option<String>> + Send;
    fn close(&self) -> impl std::future::Future<Output = ()> + Send;
}

/// Opens sockets. Injected so a test can count connect attempts — which is how
/// "sharing off ⇒ no socket opens" is asserted rather than asserted-about.
pub trait RelayConnector: Send + Sync + 'static {
    type Conn: RelayTransport;
    fn connect(&self) -> impl std::future::Future<Output = Result<Self::Conn, String>> + Send;
}

/// Production transport over `tokio-tungstenite`.
pub struct WsRelayTransport {
    tx: Mutex<futures_util::stream::SplitSink<WsStream, Message>>,
    rx: Mutex<futures_util::stream::SplitStream<WsStream>>,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl RelayTransport for WsRelayTransport {
    async fn send(&self, line: String) -> Result<(), String> {
        let mut tx = self.tx.lock().await;
        tx.send(Message::Text(line.into()))
            .await
            .map_err(|e| format!("relay send failed: {e}"))
    }

    async fn recv(&self) -> Option<String> {
        let mut rx = self.rx.lock().await;
        loop {
            match rx.next().await? {
                Ok(Message::Text(t)) => return Some(t.to_string()),
                Ok(Message::Binary(b)) => return Some(String::from_utf8_lossy(&b).to_string()),
                // Ping/Pong are handled by tungstenite; Close ends the socket.
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => {
                    log::warn!("relay: socket error: {e}");
                    return None;
                }
            }
        }
    }

    async fn close(&self) {
        let mut tx = self.tx.lock().await;
        let _ = tx.close().await;
    }
}

/// Dials `GET /v2/relay/host` with the EXISTING device token from the OS
/// keyring (scope "service", account "device_token"). No second credential.
pub struct WsConnector {
    pub url: String,
    pub token: String,
}

impl RelayConnector for WsConnector {
    type Conn = WsRelayTransport;

    async fn connect(&self) -> Result<WsRelayTransport, String> {
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("bad relay URL: {e}"))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.token)
                .parse()
                .map_err(|_| "bad device token".to_string())?,
        );
        let (stream, _resp) =
            tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
                .await
                .map_err(|_| {
                    format!(
                        "the relay socket did not open within {}s",
                        CONNECT_TIMEOUT.as_secs()
                    )
                })?
                .map_err(|e| match &e {
                    // The service ANSWERED and said no. Kept distinguishable
                    // from "nobody answered" — see `dial_was_refused`.
                    tokio_tungstenite::tungstenite::Error::Http(resp)
                        if resp.status().is_client_error() =>
                    {
                        format!("{REFUSED_PREFIX}'s token (HTTP {})", resp.status().as_u16())
                    }
                    _ => format!("could not open the relay socket: {e}"),
                })?;
        let (tx, rx) = stream.split();
        Ok(WsRelayTransport {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_upgrades_the_scheme_and_appends_the_host_route() {
        assert_eq!(
            relay_ws_url("https://flow.example.com"),
            "wss://flow.example.com/v2/relay/host"
        );
        assert_eq!(
            relay_ws_url("http://localhost:8080"),
            "ws://localhost:8080/v2/relay/host"
        );
        // Trailing slashes and stray whitespace behave like normalize_base_url.
        assert_eq!(
            relay_ws_url("  https://x.io/  "),
            "wss://x.io/v2/relay/host"
        );
        // Already-ws URLs pass through rather than being double-prefixed.
        assert_eq!(relay_ws_url("wss://x.io"), "wss://x.io/v2/relay/host");
    }

    #[test]
    fn a_refused_device_token_is_told_apart_from_an_unreachable_service() {
        // Both strings below are VERBATIM from a live dial against a real
        // openflow-service on `feat/relay-v0.2` (see
        // verification/shared-agents/RESULTS.md): the first from a device
        // revoked with `DELETE /v1/devices/{id}`, the second from a port with
        // nothing listening on it.
        assert!(dial_was_refused(
            "the service refused this device's token (HTTP 401)"
        ));
        assert!(!dial_was_refused(
            "could not open the relay socket: IO error: Connection refused (os error 61)"
        ));
        // A dial that timed out is an outage too, not a refusal.
        assert!(!dial_was_refused(
            "the relay socket did not open within 15s"
        ));
    }

    #[test]
    fn backoff_doubles_from_one_second_and_caps_at_a_minute() {
        // Faster than service_sync's 30s floor on purpose: a dropped relay
        // socket means a teammate is staring at a dead session, so recovery
        // should be seconds, not half a minute.
        assert_eq!(next_backoff(Duration::ZERO), BACKOFF_MIN);
        assert_eq!(next_backoff(Duration::from_secs(1)), Duration::from_secs(2));
        assert_eq!(
            next_backoff(Duration::from_secs(32)),
            Duration::from_secs(60)
        );
        assert_eq!(next_backoff(BACKOFF_MAX), BACKOFF_MAX);
    }
}
