//! Puts a stdio Agent Client Protocol agent on the network: a peer proves itself, then complete messages pass between it
//! and the agent's standard input and output, unread. What the agent is allowed to do stays with the agent, and whatever the
//! client allows stays with the client.
#![forbid(unsafe_code)]
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

pub mod agent;
pub mod auth;
pub mod pairing;
pub mod relay;

use core::fmt;

/// What ended a bridge.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The network channel failed before anything could be relayed.
    Channel(coqui_web::Failure),
    /// A peer failed its identity proof, or its proof could not be completed.
    Proof(String),
    /// The agent could not be started.
    Agent(std::io::Error),
    /// A key or identity file could not be used.
    Identity(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(failure) => write!(f, "the channel failed: {failure}"),
            Self::Proof(why) => write!(f, "the peer was not proven: {why}"),
            Self::Agent(error) => write!(f, "the agent could not start: {error}"),
            Self::Identity(why) => write!(f, "the identity could not be used: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<coqui_web::Failure> for Error {
    fn from(failure: coqui_web::Failure) -> Self {
        Self::Channel(failure)
    }
}
