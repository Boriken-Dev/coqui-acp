//! Puts a stdio Agent Client Protocol agent on the network: a peer proves itself, then complete messages pass between it
//! and the agent's standard input and output, unread. What the agent is allowed to do stays with the agent, and whatever the
//! client allows stays with the client. Proof, pairing and discovery are `coqui_agentic_peer`'s.
#![forbid(unsafe_code)]
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

pub mod agent;
pub mod relay;
