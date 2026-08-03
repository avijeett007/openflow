//! ACP (Agent Client Protocol) client. OpenFlow is the CLIENT; the agent binary
//! is the server, spawned as a long-lived child speaking newline-delimited
//! JSON-RPC 2.0 over stdio. Contrast `a2a.rs`, where we are an HTTP client of a
//! remote server and calls only ever flow outward: here the agent calls US
//! (permission requests), so the client is bidirectional.
//!
//! Layering (each file has one job, none depend on Tauri or on a process):
//!   protocol.rs   — wire types
//!   codec.rs      — JSON-RPC envelope encode + inbound frame classification
//!   events.rs     — SessionUpdate -> RunEvent (the C2 wire contract) + text lines
//!   permission.rs — the pure allow/deny/ask decision
//!   client.rs     — transport trait, pending-request map, demux loop

// client.rs and permission.rs are added by later tasks in this plan;
// protocol.rs (wire types), codec.rs (envelope handling) and events.rs
// (RunEvent vocabulary + mapping) exist so far.
pub mod codec;
pub mod events;
pub mod protocol;
