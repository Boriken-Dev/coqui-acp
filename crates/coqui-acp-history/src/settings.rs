//! Settings as guatiao values: one description for parsing, validation, defaults and docs.

use serde::Serialize;

use guatiao::{FromValue, Schema, ToValue};

/// The longest line a reader takes by default, in bytes.
pub const DEFAULT_MAX_LINE_BYTES: u64 = 16 << 20;
/// The most lines a reader takes by default, the header included.
pub const DEFAULT_MAX_LINES: u64 = 5_000_000;
/// The most bytes a reader takes by default.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 4 << 30;
/// The deepest nesting a reader takes by default.
pub const DEFAULT_MAX_DEPTH: u64 = 64;

/// The sizes a reader and a writer hold a history to. Every one is a setting with no floor and no
/// ceiling the crate adds; a looser value is reported by [`non_defaults`].
///
/// Nesting is also bounded by the JSON parser, which stops at 128 levels whatever is set here.
#[derive(ToValue, FromValue, Schema, Serialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct Bounds {
    /// The longest line, in bytes, without its line feed.
    #[map(default = DEFAULT_MAX_LINE_BYTES)]
    pub max_line_bytes: u64,
    /// The most lines, the header included.
    #[map(default = DEFAULT_MAX_LINES)]
    pub max_lines: u64,
    /// The most bytes in all.
    #[map(default = DEFAULT_MAX_TOTAL_BYTES)]
    pub max_total_bytes: u64,
    /// The deepest nesting of arrays and objects in a line.
    #[map(default = DEFAULT_MAX_DEPTH)]
    pub max_depth: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_lines: DEFAULT_MAX_LINES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// How a [`Reader`](crate::Reader) behaves.
#[derive(ToValue, FromValue, Schema, Serialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct ReaderSettings {
    /// The sizes the history is held to.
    #[map(default)]
    pub bounds: Bounds,
    /// Whether a digest in the header is checked against the body once the reader reaches the
    /// end. Off, a history whose body was changed is read as it is.
    #[map(default = true)]
    pub verify_digest: bool,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            bounds: Bounds::default(),
            verify_digest: true,
        }
    }
}

/// How a [`Writer`](crate::Writer) behaves.
#[derive(ToValue, FromValue, Schema, Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct WriterSettings {
    /// The sizes the history is held to: a line past them is refused, never cut.
    #[map(default)]
    pub bounds: Bounds,
}

/// What a [`Redactor`](crate::Redactor) removes. Both lists empty removes nothing.
#[derive(ToValue, FromValue, Schema, Serialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct RedactionSettings {
    /// Text removed wherever it appears, as it is spelled. These may be credentials.
    #[map(default)]
    #[schema(sensitive)]
    pub values: Vec<String>,
    /// Regular expressions (the `regex` crate's syntax, which matches in linear time) whose
    /// matches are removed.
    #[map(default)]
    pub patterns: Vec<String>,
    /// What replaces each removal.
    #[map(default = String::from(DEFAULT_REPLACEMENT))]
    pub replacement: String,
}

/// What a removal leaves by default.
pub const DEFAULT_REPLACEMENT: &str = "[redacted]";

impl Default for RedactionSettings {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            patterns: Vec::new(),
            replacement: String::from(DEFAULT_REPLACEMENT),
        }
    }
}

/// A setting that is not at its default.
#[derive(ToValue, FromValue, Schema, Serialize, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct NonDefault {
    /// The setting's key path (`bounds.max_line_bytes`).
    pub setting: String,
    /// Its value, as JSON text; a [`RedactionSettings::values`] entry is never shown.
    pub value: String,
    /// The default, as JSON text.
    pub default: String,
}

/// Every setting of `settings` that is not at its default, by key path, in key order.
///
/// A list is one setting; the values of [`RedactionSettings`] are named and not shown.
#[must_use]
pub fn non_defaults<T: Serialize + Default>(settings: &T) -> Vec<NonDefault> {
    let (Ok(now), Ok(base)) = (
        serde_json::to_value(settings),
        serde_json::to_value(T::default()),
    ) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    compare("", &now, &base, &mut found);
    found
}

fn compare(
    path: &str,
    now: &serde_json::Value,
    base: &serde_json::Value,
    out: &mut Vec<NonDefault>,
) {
    use serde_json::Value;
    if let (Value::Object(now), Value::Object(base)) = (now, base) {
        for (key, value) in now {
            let at = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match base.get(key) {
                Some(default) => compare(&at, value, default, out),
                None => out.push(NonDefault {
                    setting: at,
                    value: value.to_string(),
                    default: String::from("null"),
                }),
            }
        }
    } else if now != base {
        let hidden = path == "values";
        out.push(NonDefault {
            setting: path.to_owned(),
            value: if hidden {
                String::from("[not shown]")
            } else {
                now.to_string()
            },
            default: base.to_string(),
        });
    }
}
