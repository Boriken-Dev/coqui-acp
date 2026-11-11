//! Output scrubbing: a backstop that replaces a secret, and the encodings an accident is likely to produce, with its handle.
//!
//! It is not a guarantee. A reversed, split, re-encoded (rot13, gzip, a different hash) or otherwise transformed value passes it.

use alloc::borrow::Cow;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use data_encoding::{HEXLOWER, HEXUPPER};
use serde_json::Value;
use zeroize::Zeroizing;

/// The forms of a set of secrets that are looked for, each with the handle that replaces it. The patterns are secrets themselves and
/// are zeroed when the scrubber is dropped.
pub(crate) struct Scrubber {
    patterns: Vec<(Zeroizing<Vec<u8>>, String)>,
}

impl Scrubber {
    /// `secrets` pairs each handle with the value it stands for.
    pub(crate) fn new<'a>(
        secrets: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        min_piece: usize,
    ) -> Self {
        let mut patterns: Vec<(Zeroizing<Vec<u8>>, String)> = Vec::new();
        for (handle, value) in secrets {
            for form in forms(value, min_piece) {
                if form.is_empty() || patterns.iter().any(|(known, _)| **known == *form) {
                    continue;
                }
                patterns.push((form, handle.to_owned()));
            }
        }
        // The longest first, so a form that holds a shorter one is replaced whole.
        patterns.sort_by_key(|(pattern, _)| core::cmp::Reverse(pattern.len()));
        Self { patterns }
    }

    /// The longest form looked for, in bytes.
    pub(crate) fn longest(&self) -> usize {
        self.patterns
            .first()
            .map_or(0, |(pattern, _)| pattern.len())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// The bytes with every form replaced by its handle.
    pub(crate) fn scrub_bytes(&self, input: &[u8]) -> Vec<u8> {
        if self.patterns.is_empty() {
            return input.to_vec();
        }
        let mut out = Vec::with_capacity(input.len());
        let mut at = 0;
        'next: while at < input.len() {
            for (pattern, handle) in &self.patterns {
                if input[at..].starts_with(pattern) {
                    out.extend_from_slice(handle.as_bytes());
                    at += pattern.len();
                    continue 'next;
                }
            }
            out.push(input[at]);
            at += 1;
        }
        out
    }

    /// Output as text: scrubbed as bytes first, so a form that is not valid text on its own is still found, and then read as UTF-8
    /// with anything invalid replaced.
    pub(crate) fn scrub_output(&self, input: &[u8]) -> String {
        String::from_utf8_lossy(&self.scrub_bytes(input)).into_owned()
    }

    pub(crate) fn scrub_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.is_empty() {
            return Cow::Borrowed(text);
        }
        Cow::Owned(self.scrub_output(text.as_bytes()))
    }

    /// Every string and every key of a JSON value, scrubbed.
    pub(crate) fn scrub_value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.scrub_text(text).into_owned(),
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub_value(item)),
            Value::Object(map) => {
                let old = core::mem::take(map);
                for (key, mut item) in old {
                    self.scrub_value(&mut item);
                    map.insert(self.scrub_text(&key).into_owned(), item);
                }
            }
            _ => {}
        }
    }
}

fn zeroizing(bytes: impl Into<Vec<u8>>) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(bytes.into())
}

/// Every form of `value` that is looked for.
fn forms(value: &[u8], min_piece: usize) -> Vec<Zeroizing<Vec<u8>>> {
    let mut out = vec![zeroizing(value)];
    // Base64, standard and URL alphabets, the whole value padded or not.
    for text in [
        STANDARD.encode(value),
        STANDARD_NO_PAD.encode(value),
        URL_SAFE.encode(value),
        URL_SAFE_NO_PAD.encode(value),
    ] {
        out.push(zeroizing(text));
    }
    // Inside a longer base64 text the value starts at any of three alignments: what is stable of each is looked for.
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for lead in 0..3usize {
            let mut padded = Zeroizing::new(vec![0u8; lead]);
            padded.extend_from_slice(value);
            let text = Zeroizing::new(engine.encode(&*padded));
            // The first characters mix in the bytes before the value, and the last one the bytes after it.
            let from = (lead * 8).div_ceil(6);
            let to = if padded.len() % 3 == 0 {
                text.len()
            } else {
                text.len().saturating_sub(1)
            };
            if to > from && to - from >= min_piece {
                out.push(zeroizing(&text.as_bytes()[from..to]));
            }
        }
    }
    out.push(zeroizing(HEXLOWER.encode(value)));
    out.push(zeroizing(HEXUPPER.encode(value)));
    for upper in [true, false] {
        for plus in [false, true] {
            out.push(zeroizing(percent(value, upper, plus)));
        }
    }
    if let Ok(text) = core::str::from_utf8(value) {
        for ascii in [false, true] {
            for slash in [false, true] {
                out.push(zeroizing(json_escape(text, ascii, slash)));
            }
        }
    }
    out
}

/// Percent-encoding: only the unreserved characters stay; a space is `+` for a form.
fn percent(value: &[u8], upper: bool, plus: bool) -> String {
    let digits: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut out = String::with_capacity(value.len() * 3);
    for &byte in value {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else if plus && byte == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(char::from(digits[usize::from(byte >> 4)]));
            out.push(char::from(digits[usize::from(byte & 15)]));
        }
    }
    out
}

/// The text as it is written inside a JSON string, optionally with every non-ASCII character as `\u` escapes and `/` as `\/`.
fn json_escape(text: &str, ascii: bool, slash: bool) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '/' if slash => out.push_str("\\/"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c if ascii && !c.is_ascii() => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrubber(value: &str) -> Scrubber {
        Scrubber::new([("secret_h1", value.as_bytes())], 8)
    }

    #[test]
    fn the_value_and_its_forms_are_replaced() {
        let value = "p@ss w/rd+é\"x";
        let s = scrubber(value);
        for form in [
            value.to_owned(),
            STANDARD.encode(value),
            URL_SAFE_NO_PAD.encode(value),
            HEXLOWER.encode(value.as_bytes()),
            HEXUPPER.encode(value.as_bytes()),
            percent(value.as_bytes(), true, false),
            percent(value.as_bytes(), false, true),
            json_escape(value, true, true),
        ] {
            let text = format!("before {form} after");
            assert_eq!(s.scrub_text(&text), "before secret_h1 after", "{form}");
        }
    }

    #[test]
    fn base64_inside_a_longer_text_is_found_at_every_alignment() {
        let value = "hunter2-hunter2";
        let s = scrubber(value);
        for lead in ["", "a", "ab", "abc"] {
            for tail in ["", "z", "zz"] {
                let whole = STANDARD.encode(format!("{lead}{value}{tail}"));
                let out = s.scrub_text(&whole);
                assert!(
                    out.contains("secret_h1"),
                    "lead {lead:?} tail {tail:?}: {out}"
                );
            }
        }
    }

    #[test]
    fn unrelated_text_is_left_alone() {
        let s = scrubber("correct horse");
        assert_eq!(s.scrub_text("nothing to see"), "nothing to see");
    }

    #[test]
    fn a_json_value_is_scrubbed_in_keys_and_strings() {
        let s = scrubber("swordfish");
        let mut value = serde_json::json!({"swordfish": ["x swordfish y", 3]});
        s.scrub_value(&mut value);
        assert_eq!(
            value,
            serde_json::json!({"secret_h1": ["x secret_h1 y", 3]})
        );
    }
}
