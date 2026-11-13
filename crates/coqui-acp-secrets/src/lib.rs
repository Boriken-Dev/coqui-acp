#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod error;
mod files;
mod host;
mod ledger;
mod mcp;
mod program;
mod run;
mod scrub;
mod session;
mod settings;
mod text;
mod tools;

pub use error::Error;
pub use host::{Action, Approval, Decision, Event, FOLDER_NOTICE, Frontend, Mode};
pub use run::{
    CommandLine, CommandOutput, CommandSpec, Launch, Process, ProcessRunner, Runner, Started,
    Starter, set_starter,
};
pub use session::{Secrets, SecretsBuilder, SessionConfig};
pub use settings::{NonDefault, Settings};
pub use tools::{SessionTools, ToolDefinition, ToolResult};
