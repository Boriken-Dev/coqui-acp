use alloc::collections::VecDeque;
use alloc::vec::Vec;

use serde_json::Value;

use crate::client::Cursor;
use crate::protocol::{fields, stamp};
use crate::settings::Settings;
use crate::snapshot::{LogSnapshot, SnapshotError, seq_of};

pub(crate) struct Entry {
    pub seq: u64,
    pub message: Value,
    size: usize,
}

/// The append-only, bounded record of everything fanned out to clients.
#[derive(Default)]
pub(crate) struct Log {
    entries: VecDeque<Entry>,
    last: u64,
    bytes: usize,
}

/// Why a cursor cannot be replayed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gap {
    /// The cursor is newer than anything the log has issued: it came from a log that is gone.
    Ahead { head: u64 },
    /// Entries after the cursor were dropped by the bounds.
    Dropped { first: u64 },
}

impl Log {
    /// A log holding `snapshot`, whose next cursor follows the snapshot's head.
    pub fn restore(snapshot: LogSnapshot) -> Result<Self, SnapshotError> {
        let mut log = Self {
            entries: VecDeque::new(),
            last: snapshot.head.0,
            bytes: 0,
        };
        let mut previous = 0;
        for (n, message) in snapshot.entries.into_iter().enumerate() {
            let seq = seq_of(&message).ok_or(SnapshotError::NoCursor(n))?;
            if seq <= previous {
                return Err(SnapshotError::OutOfOrder(n));
            }
            if seq > log.last {
                return Err(SnapshotError::PastHead(n));
            }
            previous = seq;
            let size = message.to_string().len();
            log.bytes += size;
            log.entries.push_back(Entry { seq, message, size });
        }
        Ok(log)
    }

    /// Stamps the message with its sequence number, keeps it and returns the stamped message.
    pub fn push(&mut self, mut message: Value, settings: &Settings) -> Value {
        self.last += 1;
        stamp(
            &mut message,
            fields([("seq", Value::String(self.last.to_string()))]),
        );
        let size = message.to_string().len();
        self.bytes += size;
        self.entries.push_back(Entry {
            seq: self.last,
            message: message.clone(),
            size,
        });
        self.trim(settings);
        message
    }

    /// Drops the oldest entries until the bounds hold.
    pub fn trim(&mut self, settings: &Settings) {
        while self.entries.len() > settings.max_log_entries || self.bytes > settings.max_log_bytes {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.size;
        }
    }

    pub fn head(&self) -> Cursor {
        Cursor(self.last)
    }

    /// True when entries after `after` were dropped, so a replay from there has a gap.
    pub fn truncated_after(&self, after: u64) -> bool {
        let first = self.entries.front().map_or(self.last + 1, |e| e.seq);
        after + 1 < first
    }

    /// Whether a replay after `after` is impossible: the cursor is past the log's head, or the entries that followed it
    /// are gone. Cursor 0 is the start, which is always replayable (from the first entry kept).
    pub fn gap(&self, after: u64) -> Option<Gap> {
        if after == 0 {
            return None;
        }
        if after > self.last {
            return Some(Gap::Ahead { head: self.last });
        }
        let first = self.entries.front().map_or(self.last + 1, |e| e.seq);
        (after + 1 < first).then_some(Gap::Dropped { first })
    }

    pub fn after(&self, after: u64) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.seq > after)
    }

    pub fn snapshot(&self) -> LogSnapshot {
        LogSnapshot::new(
            self.head(),
            self.entries
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>(),
        )
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
