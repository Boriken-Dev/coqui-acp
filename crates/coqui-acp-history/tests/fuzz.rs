//! Arbitrary input gives a value or an error, never a panic.

mod support;

use coqui_acp_history::{Bounds, Document, Line, Reader, ReaderSettings};
use support::document;

/// A small deterministic generator (xorshift), so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn settings() -> ReaderSettings {
    let mut settings = ReaderSettings::default();
    let mut bounds = Bounds::default();
    bounds.max_line_bytes = 4096;
    bounds.max_lines = 64;
    bounds.max_total_bytes = 1 << 16;
    settings.bounds = bounds;
    settings
}

fn read(bytes: &[u8]) -> Result<usize, String> {
    let mut reader = Reader::builder(bytes)
        .settings(settings())
        .open()
        .map_err(|e| e.code().to_owned())?;
    let mut n = 0;
    while reader
        .next_entry()
        .map_err(|e| e.code().to_owned())?
        .is_some()
    {
        n += 1;
    }
    Ok(n)
}

#[test]
fn random_bytes_are_read_or_refused_never_a_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..2000 {
        let len = rng.below(300);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = read(&bytes);
    }
}

#[test]
fn a_valid_history_with_a_byte_changed_is_read_or_refused() {
    let mut doc = document(3);
    doc.lines.push(Line::compaction("summary"));
    let original = doc.to_vec().unwrap();
    assert!(read(&original).is_ok());
    let mut rng = Rng(42);
    let mut refused = 0;
    for _ in 0..4000 {
        let mut bytes = original.clone();
        let at = rng.below(bytes.len());
        match rng.below(3) {
            0 => bytes[at] = rng.next() as u8,
            1 => {
                bytes.remove(at);
            }
            _ => bytes.truncate(at),
        }
        if read(&bytes).is_err() {
            refused += 1;
        }
        let _ = Document::from_slice(&bytes);
    }
    assert!(refused > 1000, "most edits break a line: {refused}");
}

#[test]
fn deeply_nested_input_is_refused_in_bounds() {
    let header = document(0).to_string().unwrap();
    for depth in [10, 200, 100_000] {
        let nested = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let line = format!(
            "{{\"method\":\"session/update\",\"params\":{{\"update\":{{\"sessionUpdate\":\"x\",\"a\":{nested}}}}}}}\n"
        );
        let text = format!("{header}{line}");
        let mut bounds = Bounds::default();
        bounds.max_line_bytes = 1 << 20;
        let result = Reader::builder(text.as_bytes())
            .bounds(bounds)
            .open()
            .and_then(|mut r| r.next_entry());
        if depth > 60 {
            assert_eq!(result.unwrap_err().code(), "too_deep", "depth {depth}");
        } else {
            assert!(result.is_ok());
        }
    }
}
