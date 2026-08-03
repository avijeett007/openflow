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

//!   replay_tests.rs — REAL captured agent frames replayed through the above
//!
//! On `replay_tests`: every wire assumption on this branch was originally
//! verified against `DESIGN-acp-agents.md`, which was written from prose rather
//! than from the schema the agents ship. That cost two bugs which a fully green
//! unit suite could not see, because the fixtures were written from the same
//! prose. `fixtures/real-agent-frames.jsonl` holds unedited `session/update`
//! and `session/request_permission` frames captured from live Kimi and Claude
//! Code, and `replay_tests` feeds them through THIS crate's own deserializers.
//! It is the only test here whose input we did not write.

pub mod client;
pub mod codec;
pub mod events;
pub mod permission;
pub mod protocol;
#[cfg(test)]
mod replay_tests;
