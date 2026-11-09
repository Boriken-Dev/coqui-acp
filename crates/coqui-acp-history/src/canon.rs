//! The canonical spelling of a line: compact, UTF-8 left as it is, the keys of a line's own
//! envelope in a fixed order and every other object's keys sorted by code point.
//!
//! It does not depend on the build: `serde_json`'s object order changes with its
//! `preserve_order` feature, which another crate of a build may turn on.

use serde_json::Value;

use crate::strict::depth;

/// The deepest value written; the reader's parser stops at the same depth.
const MAX_WRITE_DEPTH: u64 = 128;

/// `value` in canonical spelling, its top-level keys first in the order `top` gives and the rest
/// sorted. `None` when the value nests deeper than the parser would read back.
pub(crate) fn to_string(value: &Value, top: &[&str]) -> Option<String> {
    if depth(value) > MAX_WRITE_DEPTH {
        return None;
    }
    let mut out = String::new();
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|key| {
                let place = top
                    .iter()
                    .position(|first| first == key)
                    .unwrap_or(top.len());
                (place, key.as_str())
            });
            write_object(&mut out, map, &keys);
        }
        other => write(&mut out, other),
    }
    Some(out)
}

fn write_object(out: &mut String, map: &serde_json::Map<String, Value>, keys: &[&String]) {
    out.push('{');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        string(out, key);
        out.push(':');
        if let Some(value) = map.get(key.as_str()) {
            write(out, value);
        }
    }
    out.push('}');
}

fn write(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(text) => string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            write_object(out, map, &keys);
        }
    }
}

fn string(out: &mut String, text: &str) {
    // A string always serialises.
    out.push_str(&serde_json::to_string(text).unwrap_or_default());
}
