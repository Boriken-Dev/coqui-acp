#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod kind;
mod models;

pub use guatiao;
pub use guatiao_channel::{Channel, ChannelSink, ChannelSinkVtable, ChannelVtable};
pub use kind::{AcpHarness, AcpHarnessVtable};
pub use models::{
    HarnessInfo, ModelInfo, ModelListing, NO_COMMANDS, ONLY_SKILLS, READ_ONLY, RESUME_WIDER,
    Requirement, SANDBOXED_SCRIPTS, SessionRecordFile, SessionRecordQuery, StartRequest, Takes,
};
