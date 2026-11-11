#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod error;
mod files;
mod host;
mod ledger;
mod mcp;
mod run;
mod scrub;
mod session;
mod settings;
mod tools;

pub use error::Error;
pub use host::{Action, Approval, Decision, Event, Frontend};
pub use run::{CommandLine, CommandOutput, CommandSpec, ProcessRunner, Runner};
pub use session::{Secrets, SecretsBuilder, SessionConfig};
pub use settings::{NonDefault, Settings};
pub use tools::{SessionTools, ToolDefinition, ToolResult};
