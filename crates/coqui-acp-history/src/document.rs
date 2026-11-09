//! A whole history in memory.

use core::str::FromStr;
use std::io::{Read, Write};

use sha2::{Digest, Sha256};

use crate::header::Header;
use crate::order::Summary;
use crate::reader::{Reader, body_digest};
use crate::seed::Seed;
use crate::{HistoryError, Line, Writer};

/// A header and the lines after it.
///
/// Reading then writing a document gives the same lines in this crate's canonical spelling: keys
/// sorted inside ACP payloads, numbers past the range of `i64` and `u64` as `f64`. A file that must
/// stay byte for byte as it is goes through [`Reader`] and [`Entry::raw`](crate::Entry::raw)
/// instead.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Document {
    /// The first line.
    pub header: Header,
    /// The lines after it, the first being line 2.
    pub lines: Vec<Line>,
}

impl Document {
    /// A document with a `header` and no lines.
    #[must_use]
    pub fn new(header: Header) -> Self {
        Self {
            header,
            lines: Vec::new(),
        }
    }

    /// Reads every line `reader` has.
    ///
    /// # Errors
    ///
    /// The first refusal the reader makes.
    pub fn from_reader<R: Read>(mut reader: Reader<R>) -> Result<Self, HistoryError> {
        let mut lines = Vec::new();
        while let Some(entry) = reader.next_entry()? {
            lines.push(entry.line);
        }
        Ok(Self {
            header: reader.header().clone(),
            lines,
        })
    }

    /// Reads a history held in bytes, with the default settings.
    ///
    /// # Errors
    ///
    /// The first refusal the reader makes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, HistoryError> {
        Self::from_reader(Reader::builder(bytes).open()?)
    }

    /// Checks the lines against the order rules and counts them.
    ///
    /// # Errors
    ///
    /// The first line out of order, as the writer would refuse it.
    pub fn check(&self) -> Result<Summary, HistoryError> {
        let mut writer = Writer::builder(std::io::sink()).start(&self.header)?;
        for line in &self.lines {
            writer.write(line)?;
        }
        Ok(writer.finish()?.1)
    }

    /// Writes the document, checking each line as the writer does.
    ///
    /// # Errors
    ///
    /// The first line the format refuses, or the sink's error. The sink may hold the lines before
    /// it; write to memory first where that matters.
    pub fn to_writer<W: Write>(&self, sink: W) -> Result<(), HistoryError> {
        let mut writer = Writer::builder(sink).start(&self.header)?;
        for line in &self.lines {
            writer.write(line)?;
        }
        writer.finish().map(|_| ())
    }

    /// The document as bytes.
    ///
    /// # Errors
    ///
    /// The first line the format refuses.
    pub fn to_vec(&self) -> Result<Vec<u8>, HistoryError> {
        let mut bytes = Vec::new();
        self.to_writer(&mut bytes)?;
        Ok(bytes)
    }

    /// The document as text.
    ///
    /// # Errors
    ///
    /// The first line the format refuses.
    pub fn to_string(&self) -> Result<String, HistoryError> {
        String::from_utf8(self.to_vec()?).map_err(|_| HistoryError::Shape {
            line: 0,
            pointer: String::new(),
            expected: "UTF-8",
        })
    }

    /// Sets the header's digest to the body's, so a reader can tell the body was not changed.
    ///
    /// # Errors
    ///
    /// The first line the format refuses.
    pub fn seal(&mut self) -> Result<(), HistoryError> {
        let mut hasher = Sha256::new();
        for line in &self.lines {
            hasher.update(line.to_json()?.as_bytes());
            hasher.update(b"\n");
        }
        self.header.digest = Some(body_digest(&hasher));
        Ok(())
    }

    /// What a new session is seeded from: the latest compaction summary and the lines after it.
    #[must_use]
    pub fn seed(&self) -> Seed<'_> {
        Seed::of(&self.lines)
    }
}

impl FromStr for Document {
    type Err = HistoryError;

    /// Reads a history held in text, with the default settings.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_slice(text.as_bytes())
    }
}
