//! One live Agent Client Protocol session, held independent of any client and served to any number of attached clients.
//!
//! A [`Hub`] is a state machine over JSON-RPC messages and does no I/O: the host gives it what arrives from the agent and
//! from each client and sends the [`Output`]s it returns over its own links. See `AGENTS.md` for the message shapes.

#![forbid(unsafe_code)]

extern crate alloc;

mod agent_side;
mod client;
mod client_side;
mod error;
mod hub;
mod log;
mod output;
mod permission;
mod policy;
mod protocol;
mod settings;
mod snapshot;

pub use client::{Client, ClientId, Cursor, CursorError};
pub use error::HubError;
pub use hub::{Builder, Hub};
pub use output::{Answerer, Note, Output, PermissionRequest};
pub use policy::{Action, AllowAll, Policy, Verdict};
pub use protocol::{ENDED, META_KEY, PERMISSION_CLOSED, PRESENCE, TURN_ENDED};
pub use settings::{NonDefault, Settings};
pub use snapshot::{LogSnapshot, SnapshotError};
