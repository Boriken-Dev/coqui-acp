use alloc::collections::VecDeque;

use serde_json::Value;

use crate::client::Cursor;
use crate::protocol::{fields, stamp};
use crate::settings::Settings;

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

impl Log {
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
        while self.entries.len() > settings.max_log_entries || self.bytes > settings.max_log_bytes {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.size;
        }
        message
    }

    pub fn head(&self) -> Cursor {
        Cursor(self.last)
    }

    /// True when entries after `after` were dropped, so a replay from there has a gap.
    pub fn truncated_after(&self, after: u64) -> bool {
        let first = self.entries.front().map_or(self.last + 1, |e| e.seq);
        after + 1 < first
    }

    pub fn after(&self, after: u64) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.seq > after)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
