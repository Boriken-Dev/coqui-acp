//! Reading one line of JSON strictly: no repeated keys, and a nesting bound.

use core::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use crate::HistoryError;

/// The message of the error a repeated key raises inside the parser.
const DUPLICATE: &str = "duplicate key";

/// A JSON value that refuses an object with a repeated key.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = Strict;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(Value::Bool(v)))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
                Number::from_f64(v)
                    .map(|n| Strict(Value::Number(n)))
                    .ok_or_else(|| E::custom("number out of range"))
            }

            fn visit_str<E>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(Value::String(v.to_owned())))
            }

            fn visit_string<E>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(Value::String(v)))
            }

            fn visit_unit<E>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
                let mut items = Vec::new();
                while let Some(Strict(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Strict(Value::Array(items)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
                let mut object = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    let Strict(value) = map.next_value()?;
                    if object.insert(key, value).is_some() {
                        return Err(de::Error::custom(DUPLICATE));
                    }
                }
                Ok(Strict(Value::Object(object)))
            }
        }

        deserializer.deserialize_any(V)
    }
}

/// Parses `text` as one JSON value no deeper than `max_depth`, with no object repeating a key.
pub(crate) fn parse(text: &str, line: u64, max_depth: u64) -> Result<Value, HistoryError> {
    let Strict(value) = serde_json::from_str::<Strict>(text).map_err(|error| {
        let message = error.to_string();
        if message.starts_with(DUPLICATE) {
            HistoryError::DuplicateKey { line }
        } else if message.starts_with("recursion limit") {
            HistoryError::TooDeep {
                line,
                limit: max_depth.min(128),
            }
        } else {
            HistoryError::Syntax {
                line,
                column: error.column() as u64,
            }
        }
    })?;
    if depth(&value) > max_depth {
        return Err(HistoryError::TooDeep {
            line,
            limit: max_depth,
        });
    }
    Ok(value)
}

/// The nesting of `value`: a scalar is 0, a container one more than its deepest child. Walks with
/// an explicit stack, since the depth is what is in question.
pub(crate) fn depth(value: &Value) -> u64 {
    let mut deepest = 0;
    let mut stack = vec![(value, 0_u64)];
    while let Some((value, level)) = stack.pop() {
        match value {
            Value::Array(items) => {
                deepest = deepest.max(level + 1);
                stack.extend(items.iter().map(|item| (item, level + 1)));
            }
            Value::Object(map) => {
                deepest = deepest.max(level + 1);
                stack.extend(map.values().map(|item| (item, level + 1)));
            }
            _ => {}
        }
    }
    deepest
}
