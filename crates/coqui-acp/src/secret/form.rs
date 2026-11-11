//! A secret form: a JSON Schema object whose properties are the questions, with the secret ones marked `writeOnly` or `x-sensitive`.

use super::error::Error;
use crate::elicit::{self, Elicitation, Field, FieldKind};
use alloc::string::String;
use alloc::vec::Vec;
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose};
use serde_json::{Map, Value};

/// How much of a form is taken. A form past any limit is refused. Every value from 0 up is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Questions in one form. Default 64.
    pub fields: usize,
    /// Choices of one question. Default 256.
    pub options: usize,
    /// The schema, written as JSON, in bytes. Default 64 KiB.
    pub schema_bytes: usize,
    /// A field's name, in bytes. Default 128.
    pub name_bytes: usize,
    /// One secret value, in bytes. Default 64 KiB.
    pub value_bytes: usize,
}

impl Limits {
    /// The defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fields: 64,
            options: 256,
            schema_bytes: 64 * 1024,
            name_bytes: 128,
            value_bytes: 64 * 1024,
        }
    }

    /// Sets the most questions in one form.
    #[must_use]
    pub const fn fields(mut self, fields: usize) -> Self {
        self.fields = fields;
        self
    }

    /// Sets the most choices of one question.
    #[must_use]
    pub const fn options(mut self, options: usize) -> Self {
        self.options = options;
        self
    }

    /// Sets the most bytes of a schema written as JSON.
    #[must_use]
    pub const fn schema_bytes(mut self, bytes: usize) -> Self {
        self.schema_bytes = bytes;
        self
    }

    /// Sets the most bytes of a field's name.
    #[must_use]
    pub const fn name_bytes(mut self, bytes: usize) -> Self {
        self.name_bytes = bytes;
        self
    }

    /// Sets the most bytes of one secret value.
    #[must_use]
    pub const fn value_bytes(mut self, bytes: usize) -> Self {
        self.value_bytes = bytes;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}

/// One question of a secret form.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FormField {
    /// The question as a plain form has it: name, title, kind, bounds, whether an answer requires it.
    pub field: Field,
    /// Whether the value is a secret: it travels sealed, and no one in between reads it.
    pub secret: bool,
    /// The schema's `contentEncoding`, such as `base64` for bytes; a frontend picks how to prompt for or show the value from it.
    pub encoding: Option<String>,
    /// The schema's `contentMediaType`.
    pub media_type: Option<String>,
}

/// A form read from a schema, the questions in the schema's order.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Form {
    /// The form's title.
    pub title: Option<String>,
    /// The form's description.
    pub description: Option<String>,
    /// Its questions.
    pub fields: Vec<FormField>,
    limits: Limits,
}

/// Standard base64, with or without its padding.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

impl Form {
    /// Reads a schema, or says which rule it breaks. The schema is the agent's, untrusted.
    ///
    /// # Errors
    /// [`Error::Schema`] for a schema that is not an object with properties, one over a limit, a name that is empty, long or holds a
    /// control character, a property that is no object or whose type no control can show, a `required` that is no list of the
    /// properties' names, and a secret field that is a choice, a flag or a list, or carries a `default`, `const`, `enum`,
    /// `oneOf` or `examples` (the form would put a value in the agent's text).
    pub fn parse(schema: &Value, limits: &Limits) -> Result<Self, Error> {
        let schema = schema
            .as_object()
            .ok_or(Error::Schema("the schema is not an object"))?;
        if serde_json::to_vec(schema).map_or(usize::MAX, |bytes| bytes.len()) > limits.schema_bytes
        {
            return Err(Error::Schema("the schema is larger than the limit"));
        }
        if schema
            .get("type")
            .is_some_and(|kind| kind.as_str() != Some("object"))
        {
            return Err(Error::Schema("the schema's type is not object"));
        }
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or(Error::Schema("the schema has no properties"))?;
        if properties.is_empty() {
            return Err(Error::Schema("the schema asks nothing"));
        }
        if properties.len() > limits.fields {
            return Err(Error::Schema(
                "the schema has more questions than the limit",
            ));
        }
        let required: Vec<&str> = match schema.get("required") {
            None => Vec::new(),
            Some(Value::Array(names)) => names
                .iter()
                .map(|name| {
                    name.as_str()
                        .ok_or(Error::Schema("required holds a name that is not text"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(Error::Schema("required is not a list")),
        };
        if required.iter().any(|name| !properties.contains_key(*name)) {
            return Err(Error::Schema("required names a property the schema lacks"));
        }
        let mut fields = Vec::with_capacity(properties.len());
        for (name, property) in properties {
            if name.is_empty()
                || name.len() > limits.name_bytes
                || name.chars().any(char::is_control)
            {
                return Err(Error::Schema(
                    "a property's name is empty, too long or holds a control character",
                ));
            }
            let property = property
                .as_object()
                .ok_or(Error::Schema("a property is not an object"))?;
            let kind = elicit::kind_of(property);
            if let FieldKind::Choice { options, .. } | FieldKind::Choices { options, .. } = &kind
                && options.len() > limits.options
            {
                return Err(Error::Schema("a question has more choices than the limit"));
            }
            if matches!(kind, FieldKind::Unsupported { .. }) {
                return Err(Error::Schema(
                    "a property's type is one no control can show",
                ));
            }
            let secret = flag(property, "writeOnly") || flag(property, "x-sensitive");
            if secret {
                if !matches!(
                    kind,
                    FieldKind::Text { .. } | FieldKind::Integer { .. } | FieldKind::Number { .. }
                ) {
                    return Err(Error::Schema("a secret field is not text or a number"));
                }
                if ["default", "const", "enum", "oneOf", "examples"]
                    .iter()
                    .any(|key| property.contains_key(*key))
                {
                    return Err(Error::Schema(
                        "a secret field carries a value or a choice in the schema",
                    ));
                }
            }
            fields.push(FormField {
                field: Field {
                    name: name.clone(),
                    title: elicit::text(property, "title"),
                    description: elicit::text(property, "description"),
                    required: required.contains(&name.as_str()),
                    kind,
                },
                secret,
                encoding: elicit::text(property, "contentEncoding"),
                media_type: elicit::text(property, "contentMediaType"),
            });
        }
        Ok(Self {
            title: elicit::text(schema, "title"),
            description: elicit::text(schema, "description"),
            fields,
            limits: *limits,
        })
    }

    /// The limits the form was read under.
    #[must_use]
    pub const fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The question named `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FormField> {
        self.fields.iter().find(|field| field.field.name == name)
    }

    /// Whether `values` are an answer to the form's plain questions: no name that is not one, none that is secret, every required one
    /// present, each of its field's type and within its bounds.
    ///
    /// # Errors
    /// [`Error::Field`].
    pub fn check_plain(&self, values: &Map<String, Value>) -> Result<(), Error> {
        for name in values.keys() {
            match self.field(name) {
                None => return Err(Error::Field("a value for a question the form lacks")),
                Some(field) if field.secret => {
                    return Err(Error::Field("a secret field holds a plain value"));
                }
                Some(_) => {}
            }
        }
        let plain = Elicitation {
            session_id: String::new(),
            tool_call_id: None,
            message: String::new(),
            title: None,
            description: None,
            fields: self
                .fields
                .iter()
                .filter(|field| !field.secret)
                .map(|field| field.field.clone())
                .collect(),
        };
        elicit::fits(&plain, values)
            .map_err(|_| Error::Field("a plain value does not fit its question"))
    }

    /// Whether `names` are the secret questions an answer sealed: none that is not one, every required one present.
    ///
    /// # Errors
    /// [`Error::Field`].
    pub fn check_sealed<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Result<(), Error> {
        let given: Vec<&str> = names.into_iter().collect();
        for name in &given {
            if !self.field(name).is_some_and(|field| field.secret) {
                return Err(Error::Field(
                    "a sealed value for a question that is not secret",
                ));
            }
        }
        if self.fields.iter().any(|field| {
            field.secret && field.field.required && !given.contains(&field.field.name.as_str())
        }) {
            return Err(Error::Field("a required secret question has no value"));
        }
        Ok(())
    }

    /// Whether `value` fits secret field `name`: UTF-8 text of its length, a whole number or a number as decimal text within its
    /// bounds, and base64 when the schema says `contentEncoding: base64`.
    ///
    /// # Errors
    /// [`Error::Field`].
    pub fn check_secret(&self, name: &str, value: &[u8]) -> Result<(), Error> {
        let field = self
            .field(name)
            .filter(|field| field.secret)
            .ok_or(Error::Field("not a secret question of the form"))?;
        if value.len() > self.limits.value_bytes {
            return Err(Error::TooLarge("a secret value"));
        }
        let text =
            core::str::from_utf8(value).map_err(|_| Error::Field("a secret value is not text"))?;
        let fit = match &field.field.kind {
            FieldKind::Text {
                min_length,
                max_length,
                ..
            } => {
                let length = text.chars().count() as u64;
                min_length.is_none_or(|least| length >= least)
                    && max_length.is_none_or(|most| length <= most)
                    && match field.encoding.as_deref() {
                        Some("base64") => BASE64.decode(text).is_ok(),
                        Some("base64url") => general_purpose::URL_SAFE_NO_PAD
                            .decode(text.trim_end_matches('='))
                            .is_ok(),
                        _ => true,
                    }
            }
            FieldKind::Integer {
                minimum, maximum, ..
            } => text.parse::<i64>().is_ok_and(|at| {
                minimum.is_none_or(|least| at >= least) && maximum.is_none_or(|most| at <= most)
            }),
            FieldKind::Number {
                minimum, maximum, ..
            } => text.parse::<f64>().is_ok_and(|at| {
                at.is_finite()
                    && minimum.is_none_or(|least| at >= least)
                    && maximum.is_none_or(|most| at <= most)
            }),
            _ => false,
        };
        if fit {
            Ok(())
        } else {
            Err(Error::Field("a secret value does not fit its question"))
        }
    }
}

fn flag(property: &Map<String, Value>, key: &str) -> bool {
    property.get(key).and_then(Value::as_bool) == Some(true)
}
