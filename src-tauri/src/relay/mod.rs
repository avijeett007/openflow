//! The C2 relay client. OpenFlow is the HOST: it dials out to the community's
//! openflow-service, publishes what it is willing to run, and answers brokered
//! sessions. Contrast `a2a.rs` (we are an HTTP client of a remote agent) and
//! `acp/` (we are a client of a local child process): here a third party asks
//! US to run something, so the trust decision lives on this side.
//!
//! Layering — no file here depends on Tauri, on a process, or on a socket:
//!   protocol.rs  — the wire contract (envelopes + HostFrame)
//!   grants.rs    — the host-side authorisation re-check + offer publication
//!   transport.rs — the transport trait + its WebSocket implementation

pub mod grants;
pub mod protocol;
pub mod transport;
