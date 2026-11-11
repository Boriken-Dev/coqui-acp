#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

pub mod accounts;
mod agent;
mod gate;
mod launch;
pub mod mode;
pub mod parse;
mod provider;
mod session;

use alloc::string::String;
use core::fmt;
use core::time::Duration;
use std::io;

pub use agent::{DEFAULT, PiAgent};
pub use coqui_acp_harness::{self as harness, Launch, PlainSpawn, Spawn, Spawned};
pub use gate::{GATE_SOURCE, Gate};
pub use launch::{PiLaunch, SessionStart, valid_session_id};
pub use parse::{Parser, ParserLimits};
pub use provider::{PiProvider, ProviderSettings};
pub use session::{Model, PiSession, PiSettings, models_of, user_input};

/// The Pi versions this crate was tested with. Another one runs, with a warning.
pub const TESTED_VERSIONS: &[&str] = &["1.5.2"];

/// What went wrong with Pi.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// It could not be started.
    Spawn(io::Error),
    /// The request asks for what it cannot do.
    Refused(String),
    /// It did not answer in time.
    Timeout {
        /// What was waited for.
        what: &'static str,
        /// How long.
        limit: Duration,
    },
    /// It refused, failed or is gone, in its own words when it gave any.
    Harness(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "Pi could not be started: {error}"),
            Self::Refused(why) | Self::Harness(why) => f.write_str(why),
            Self::Timeout { what, limit } => {
                write!(f, "Pi did not answer {what} within {limit:?}")
            }
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}
