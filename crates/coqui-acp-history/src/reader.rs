//! The bounded streaming reader.

use std::io::{self, BufRead, BufReader, Read};

use sha2::{Digest, Sha256};

use crate::header::{Header, is_digest};
use crate::order::{Sequencer, Summary};
use crate::settings::{Bounds, ReaderSettings};
use crate::{HistoryError, Line, strict};

/// One body line as read: its parsed form and the text it was read from.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Entry {
    /// Its line number, the header being line 1.
    pub number: u64,
    /// What it says.
    pub line: Line,
    /// The line exactly as the file has it, without its line feed. Writing this text back, with a
    /// line feed, leaves a file unaltered.
    pub raw: String,
}

/// Sets a [`Reader`] up. Nothing is read until [`open`](Self::open).
#[derive(Debug)]
#[must_use]
pub struct ReaderBuilder<R> {
    source: R,
    settings: ReaderSettings,
}

impl<R: Read> ReaderBuilder<R> {
    /// All the settings at once, as a file or a peer filled them.
    pub fn settings(mut self, settings: ReaderSettings) -> Self {
        self.settings = settings;
        self
    }

    /// The sizes the history is held to.
    pub fn bounds(mut self, bounds: Bounds) -> Self {
        self.settings.bounds = bounds;
        self
    }

    /// Whether a digest in the header is checked at the end.
    pub fn verify_digest(mut self, verify: bool) -> Self {
        self.settings.verify_digest = verify;
        self
    }

    /// Reads and checks the header line.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Empty`] for no input; the header's refusals: [`HistoryError::UnknownFormat`],
    /// [`HistoryError::UnknownVersion`], [`HistoryError::Shape`] and the size bounds.
    pub fn open(self) -> Result<Reader<R>, HistoryError> {
        let mut reader = Reader {
            input: BufReader::new(self.source),
            settings: self.settings,
            header: None,
            header_raw: String::new(),
            sequencer: Sequencer::default(),
            hasher: Sha256::new(),
            number: 0,
            total: 0,
            buffer: Vec::new(),
            finished: false,
        };
        let Some((_, text)) = reader.next_text()? else {
            return Err(HistoryError::Empty);
        };
        let value = strict::parse(&text, 1, reader.settings.bounds.max_depth)?;
        reader.header = Some(Header::from_value(&value)?);
        reader.sequencer.header(text.len() as u64 + 1);
        reader.header_raw = text;
        Ok(reader)
    }
}

/// Reads a history line by line, holding it to its bounds and refusing what it does not know.
///
/// It reads no more than one line ahead of the caller and keeps no line past the one it returns.
/// After an error it returns nothing more.
#[derive(Debug)]
pub struct Reader<R> {
    input: BufReader<R>,
    settings: ReaderSettings,
    header: Option<Header>,
    header_raw: String,
    sequencer: Sequencer,
    hasher: Sha256,
    number: u64,
    total: u64,
    buffer: Vec<u8>,
    finished: bool,
}

impl<R: Read> Reader<R> {
    /// A reader of `source`, with the default settings until the builder says otherwise.
    pub fn builder(source: R) -> ReaderBuilder<R> {
        ReaderBuilder {
            source,
            settings: ReaderSettings::default(),
        }
    }

    /// The header.
    #[must_use]
    pub fn header(&self) -> &Header {
        self.header
            .as_ref()
            .expect("open reads the header before it returns a reader")
    }

    /// The header line exactly as the file has it, without its line feed.
    #[must_use]
    pub fn header_raw(&self) -> &str {
        &self.header_raw
    }

    /// What has been read so far.
    #[must_use]
    pub fn summary(&self) -> &Summary {
        self.sequencer.summary()
    }

    /// The next body line, or `None` at the end, after the digest (when the header has one and the
    /// settings verify it) has been checked.
    ///
    /// # Errors
    ///
    /// The first line the format refuses, or the digest mismatch at the end.
    pub fn next_entry(&mut self) -> Result<Option<Entry>, HistoryError> {
        if self.finished {
            return Ok(None);
        }
        let result = self.read_entry();
        if !matches!(result, Ok(Some(_))) {
            self.finished = true;
        }
        result
    }

    fn read_entry(&mut self) -> Result<Option<Entry>, HistoryError> {
        let Some((number, text)) = self.next_text()? else {
            return self.check_digest().map(|()| None);
        };
        self.hasher.update(text.as_bytes());
        self.hasher.update(b"\n");
        let value = strict::parse(&text, number, self.settings.bounds.max_depth)?;
        let line = Line::from_value(&value, number)?;
        self.sequencer
            .accept(number, text.len() as u64 + 1, &line.body)?;
        Ok(Some(Entry {
            number,
            line,
            raw: text,
        }))
    }

    fn check_digest(&mut self) -> Result<(), HistoryError> {
        let digest = self.header().digest.as_deref();
        if let Some(expected) = digest.filter(|_| self.settings.verify_digest) {
            debug_assert!(is_digest(expected));
            if body_digest(&self.hasher) != expected {
                return Err(HistoryError::DigestMismatch);
            }
        }
        Ok(())
    }

    /// The next line's text, checked for size, encoding and line ending.
    /// Writes the header and every line exactly as the file has them, line feeds included, and
    /// returns what was counted. The file is read to its end first line by line, so a history the
    /// format refuses stops the copy at the line it refuses.
    ///
    /// # Errors
    ///
    /// The first refusal of the reader, or the sink's error.
    pub fn copy_to<W: std::io::Write>(mut self, mut sink: W) -> Result<Summary, HistoryError> {
        sink.write_all(self.header_raw.as_bytes())?;
        sink.write_all(
            b"
",
        )?;
        while let Some(entry) = self.next_entry()? {
            sink.write_all(entry.raw.as_bytes())?;
            sink.write_all(
                b"
",
            )?;
        }
        sink.flush()?;
        Ok(self.sequencer.summary().clone())
    }

    fn next_text(&mut self) -> Result<Option<(u64, String)>, HistoryError> {
        let bounds = self.settings.bounds.clone();
        let number = self.number + 1;
        match read_line(&mut self.input, &mut self.buffer, bounds.max_line_bytes)? {
            Scan::End => return Ok(None),
            Scan::TooLong => {
                return Err(HistoryError::TooLong {
                    line: number,
                    limit: bounds.max_line_bytes,
                });
            }
            Scan::Unterminated => return Err(HistoryError::Truncated { line: number }),
            Scan::Line => {}
        }
        if number > bounds.max_lines {
            return Err(HistoryError::TooManyLines {
                limit: bounds.max_lines,
            });
        }
        self.total += self.buffer.len() as u64 + 1;
        if self.total > bounds.max_total_bytes {
            return Err(HistoryError::TooLarge {
                limit: bounds.max_total_bytes,
            });
        }
        self.number = number;
        let bytes = match (number, self.buffer.strip_prefix(b"\xEF\xBB\xBF")) {
            (1, Some(rest)) => rest,
            _ => &self.buffer[..],
        };
        if bytes.is_empty() {
            return Err(HistoryError::BlankLine { line: number });
        }
        if bytes.contains(&b'\r') {
            return Err(HistoryError::LineEnding { line: number });
        }
        let text =
            core::str::from_utf8(bytes).map_err(|_| HistoryError::NotUtf8 { line: number })?;
        Ok(Some((number, text.to_owned())))
    }
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Entry, HistoryError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().transpose()
    }
}

/// `sha256:` and the lower-case hexadecimal digest of what `hasher` has seen.
pub(crate) fn body_digest(hasher: &Sha256) -> String {
    format!(
        "sha256:{}",
        data_encoding::HEXLOWER.encode(&hasher.clone().finalize())
    )
}

#[derive(Debug)]
enum Scan {
    End,
    Line,
    Unterminated,
    TooLong,
}

/// Reads up to the next line feed into `buffer` (without it), never holding more than `limit`
/// bytes of the line.
fn read_line<R: BufRead>(input: &mut R, buffer: &mut Vec<u8>, limit: u64) -> io::Result<Scan> {
    buffer.clear();
    loop {
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if chunk.is_empty() {
            return Ok(if buffer.is_empty() {
                Scan::End
            } else {
                Scan::Unterminated
            });
        }
        let (take, found) = match chunk.iter().position(|b| *b == b'\n') {
            Some(at) => (at, true),
            None => (chunk.len(), false),
        };
        if (buffer.len() + take) as u64 > limit {
            return Ok(Scan::TooLong);
        }
        buffer.extend_from_slice(&chunk[..take]);
        input.consume(take + usize::from(found));
        if found {
            return Ok(Scan::Line);
        }
    }
}
