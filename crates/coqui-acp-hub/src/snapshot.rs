use alloc::vec::Vec;
use core::fmt;

use serde_json::{Value, json};

use crate::client::Cursor;
use crate::protocol::META_KEY;

/// The hub's update log in a form a host can keep and hand back to a hub that starts later (after its own restart), so
/// the cursors clients hold stay valid.
///
/// `entries` are the messages exactly as the hub fanned them out, each stamped with its own `seq`; `head` is the newest
/// cursor ever issued, which may be past the last entry when the bounds dropped it.
#[derive(Clone, Debug, PartialEq)]
pub struct LogSnapshot {
    /// The newest cursor issued: the next update gets the one after it.
    pub head: Cursor,
    /// The kept updates, oldest first.
    pub entries: Vec<Value>,
}

/// A log that cannot be restored, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotError {
    /// An entry carries no `seq`, or one that is not a cursor.
    NoCursor(usize),
    /// An entry's cursor is not after the one before it.
    OutOfOrder(usize),
    /// An entry's cursor is past `head`.
    PastHead(usize),
    /// The text is not a snapshot.
    Malformed(&'static str),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCursor(n) => write!(f, "log entry {n} carries no cursor"),
            Self::OutOfOrder(n) => write!(f, "log entry {n} is not after the entry before it"),
            Self::PastHead(n) => write!(f, "log entry {n} is past the log's head"),
            Self::Malformed(what) => write!(f, "not a log snapshot: {what}"),
        }
    }
}

impl core::error::Error for SnapshotError {}

impl LogSnapshot {
    /// A snapshot of a log whose newest cursor is `head`.
    #[must_use]
    pub fn new(head: Cursor, entries: Vec<Value>) -> Self {
        Self { head, entries }
    }

    /// `{ "head": "<cursor>", "entries": [...] }`, with the cursor as the decimal text clients see.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({ "head": self.head.to_string(), "entries": self.entries })
    }

    /// Reads what `to_json` wrote. The entries are checked when a hub is built from it.
    ///
    /// # Errors
    /// `SnapshotError::Malformed` when the value has no `head` cursor or no `entries` array.
    pub fn from_json(value: &Value) -> Result<Self, SnapshotError> {
        let head = value
            .get("head")
            .and_then(Value::as_str)
            .and_then(|h| h.parse::<Cursor>().ok())
            .ok_or(SnapshotError::Malformed("no head cursor"))?;
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or(SnapshotError::Malformed("no entries"))?
            .clone();
        Ok(Self { head, entries })
    }
}

/// The cursor a logged message was stamped with.
pub(crate) fn seq_of(message: &Value) -> Option<u64> {
    message
        .pointer("/params/_meta")?
        .get(META_KEY)?
        .get("seq")?
        .as_str()?
        .parse()
        .ok()
}

/// The hub's number for the prompt a logged message belongs to: stamped on an echoed prompt, a field of a turn's end.
pub(crate) fn prompt_of(message: &Value) -> Option<u64> {
    message
        .pointer("/params/_meta")
        .and_then(|m| m.get(META_KEY))
        .and_then(|own| own.get("prompt"))
        .or_else(|| message.pointer("/params/prompt"))?
        .as_u64()
}
