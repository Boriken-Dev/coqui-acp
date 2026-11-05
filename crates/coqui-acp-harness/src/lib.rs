#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

pub mod acp;
pub mod event;
pub mod lines;
pub mod process;
pub mod version;

pub use acp::{
    AgentInfo, ConfigOption, Conversation, EventSink, Harness, HarnessError, McpServer,
    McpTransport, Open, OpenRequest, OptionValue, Settings, Takes, serve,
};
pub use event::{Event, Outcome, PermissionRequest, Responder};
pub use process::{Exit, Launch, PlainSpawn, Process, Spawn, Spawned};
