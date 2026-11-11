//! The extension's messages: what the host sends the frontend for a `request` or a `show`, and what it answers.

use super::error::Error;
use super::seal::{Recipient, Sealed};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use serde_json::{Map, Value};

/// The key of `_meta` in which a frontend says it takes the extension (`clientCapabilities._meta`, or `capabilities._meta` in ACP
/// version 2), and so a host serves secret tools only to a client that offers it.
pub const CAPABILITY: &str = "dev.boriken.coqui.acp/secrets";
/// The request that asks the person for values, some of them secret: the host sends it to the session's owner's frontend.
pub const REQUEST_METHOD: &str = "_coqui/secret/request";
/// The request that shows the person values, some of them secret.
pub const SHOW_METHOD: &str = "_coqui/secret/show";
/// The extension's version, which `info` and every binding carry in their labels.
pub const VERSION: u64 = 1;

/// What a frontend offers, as it writes it under [`CAPABILITY`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capability {
    /// It answers [`REQUEST_METHOD`].
    pub request: bool,
    /// It answers [`SHOW_METHOD`].
    pub show: bool,
    /// The binding methods it verifies, as `request` needs, and makes, as `show` needs.
    pub bindings: Vec<String>,
}

impl Capability {
    /// Both methods and both binding methods.
    #[must_use]
    pub fn all() -> Self {
        Self {
            request: true,
            show: true,
            bindings: alloc::vec![super::BINDING_ECDSA.into(), super::BINDING_DH.into()],
        }
    }

    /// `{"version": 1, "methods": ["request", "show"], "bindings": [..]}`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut methods = Vec::new();
        if self.request {
            methods.push(Value::from("request"));
        }
        if self.show {
            methods.push(Value::from("show"));
        }
        let mut map = Map::new();
        map.insert("version".into(), Value::from(VERSION));
        map.insert("methods".into(), Value::Array(methods));
        map.insert(
            "bindings".into(),
            Value::Array(self.bindings.iter().cloned().map(Value::String).collect()),
        );
        Value::Object(map)
    }

    /// Reads the capability from a `_meta` object, or `None` when it is absent, of another version, or offers no method: a host then
    /// serves no secret tools to that client.
    #[must_use]
    pub fn from_meta(meta: &Map<String, Value>) -> Option<Self> {
        let map = meta.get(CAPABILITY)?.as_object()?;
        if map.get("version")?.as_u64()? != VERSION {
            return None;
        }
        let methods = map.get("methods")?.as_array()?;
        let has = |name: &str| methods.iter().any(|method| method.as_str() == Some(name));
        let bindings = map
            .get("bindings")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|binding| binding.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let capability = Self {
            request: has("request"),
            show: has("show"),
            bindings,
        };
        (capability.request || capability.show).then_some(capability)
    }
}

fn object<'a>(value: &'a Value, what: &'static str) -> Result<&'a Map<String, Value>, Error> {
    value.as_object().ok_or(Error::Malformed(what))
}

fn text(map: &Map<String, Value>, key: &str, what: &'static str) -> Result<String, Error> {
    map.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(Error::Malformed(what))
}

fn optional_form(map: &Map<String, Value>) -> Result<Option<Value>, Error> {
    match map.get("form") {
        None | Some(Value::Null) => Ok(None),
        Some(form @ Value::Object(_)) => Ok(Some(form.clone())),
        Some(_) => Err(Error::Malformed("the form overlay is not an object")),
    }
}

/// `_coqui/secret/request`'s parameters: the host asks the person for values, and sends the key to seal the secret ones to.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestParams {
    /// The session the request belongs to.
    pub session_id: String,
    /// The request's id, 1 to 128 bytes, unique among the requests of a host.
    pub request_id: String,
    /// What the agent says it needs the values for. Its words, untrusted.
    pub message: String,
    /// The questions, as a JSON Schema object; the secret ones are `writeOnly` or `x-sensitive`. Read it with [`Form::parse`](super::Form::parse).
    pub schema: Value,
    /// A guatiao-intake form document laying the questions out, when there is one. Opaque to this crate.
    pub form: Option<Value>,
    /// Where the secret values are sealed to.
    pub recipient: Recipient,
}

impl RequestParams {
    /// The parameters as JSON.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("sessionId".into(), Value::String(self.session_id.clone()));
        map.insert("requestId".into(), Value::String(self.request_id.clone()));
        map.insert("message".into(), Value::String(self.message.clone()));
        map.insert("schema".into(), self.schema.clone());
        if let Some(form) = &self.form {
            map.insert("form".into(), form.clone());
        }
        map.insert("recipient".into(), self.recipient.to_value());
        Value::Object(map)
    }

    /// Reads the parameters.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = object(value, "the request is not an object")?;
        Ok(Self {
            session_id: text(map, "sessionId", "the request has no session")?,
            request_id: text(map, "requestId", "the request has no id")?,
            message: text(map, "message", "the request has no message")?,
            schema: map
                .get("schema")
                .cloned()
                .ok_or(Error::Malformed("the request has no schema"))?,
            form: optional_form(map)?,
            recipient: Recipient::from_value(
                map.get("recipient")
                    .ok_or(Error::Malformed("the request has no recipient"))?,
            )?,
        })
    }
}

/// What the person answered to a request: values by field, a secret one sealed.
#[derive(Clone, Debug, PartialEq)]
pub enum RequestReply {
    /// They filled in the form.
    Accept {
        /// The plain answers.
        plain: Map<String, Value>,
        /// The secret answers, sealed to the request's key.
        sealed: BTreeMap<String, Sealed>,
    },
    /// They refused to answer.
    Decline,
    /// They dismissed it.
    Cancel,
}

impl RequestReply {
    /// `{"action": "accept", "values": {..}}`, `{"action": "decline"}` or `{"action": "cancel"}`. A sealed value is
    /// `{"enc", "ct"}`; which fields are sealed is the form's, not the value's.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Accept { plain, sealed } => {
                map.insert("action".into(), "accept".into());
                let mut values = plain.clone();
                for (name, value) in sealed {
                    values.insert(name.clone(), value.to_value());
                }
                map.insert("values".into(), Value::Object(values));
            }
            Self::Decline => {
                map.insert("action".into(), "decline".into());
            }
            Self::Cancel => {
                map.insert("action".into(), "cancel".into());
            }
        }
        Value::Object(map)
    }

    /// Reads an answer. `secret` says which fields are secret (the form's): those must be sealed values, and no other may be one.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value, is_secret: impl Fn(&str) -> bool) -> Result<Self, Error> {
        let map = object(value, "the answer is not an object")?;
        match map.get("action").and_then(Value::as_str) {
            Some("decline") => Ok(Self::Decline),
            Some("cancel") => Ok(Self::Cancel),
            Some("accept") => {
                let values = object(
                    map.get("values")
                        .ok_or(Error::Malformed("the answer has no values"))?,
                    "the values are not an object",
                )?;
                let (mut plain, mut sealed) = (Map::new(), BTreeMap::new());
                for (name, value) in values {
                    if is_secret(name) {
                        sealed.insert(name.clone(), Sealed::from_value(value)?);
                    } else {
                        plain.insert(name.clone(), value.clone());
                    }
                }
                Ok(Self::Accept { plain, sealed })
            }
            _ => Err(Error::Malformed(
                "the answer's action is not accept, decline or cancel",
            )),
        }
    }
}

/// `_coqui/secret/show` with `"step": "open"`: the host tells the frontend what it will show, without the secret values, and asks
/// for the key to seal them to.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowOpen {
    /// The session the show belongs to.
    pub session_id: String,
    /// The show's id.
    pub request_id: String,
    /// What the agent says the values are.
    pub message: String,
    /// The values' schema, in which the secret ones are marked.
    pub schema: Value,
    /// A form overlay, when there is one.
    pub form: Option<Value>,
    /// The values that are not secret.
    pub values: Map<String, Value>,
}

impl ShowOpen {
    /// The parameters as JSON, with `"step": "open"`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("step".into(), "open".into());
        map.insert("sessionId".into(), Value::String(self.session_id.clone()));
        map.insert("requestId".into(), Value::String(self.request_id.clone()));
        map.insert("message".into(), Value::String(self.message.clone()));
        map.insert("schema".into(), self.schema.clone());
        if let Some(form) = &self.form {
            map.insert("form".into(), form.clone());
        }
        map.insert("values".into(), Value::Object(self.values.clone()));
        Value::Object(map)
    }

    /// Reads the parameters of an `open` step.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = object(value, "the show is not an object")?;
        if map.get("step").and_then(Value::as_str) != Some("open") {
            return Err(Error::Malformed("the show's step is not open"));
        }
        Ok(Self {
            session_id: text(map, "sessionId", "the show has no session")?,
            request_id: text(map, "requestId", "the show has no id")?,
            message: text(map, "message", "the show has no message")?,
            schema: map
                .get("schema")
                .cloned()
                .ok_or(Error::Malformed("the show has no schema"))?,
            form: optional_form(map)?,
            values: object(
                map.get("values")
                    .ok_or(Error::Malformed("the show has no values"))?,
                "the values are not an object",
            )?
            .clone(),
        })
    }
}

/// The answer to an `open` step.
#[derive(Clone, Debug, PartialEq)]
pub enum ShowOpened {
    /// The frontend will show it: the key to seal the secret values to.
    Ready(Recipient),
    /// The person refused to see it.
    Decline,
    /// They dismissed it.
    Cancel,
}

impl ShowOpened {
    /// `{"action": "ready", "recipient": ..}`, `{"action": "decline"}` or `{"action": "cancel"}`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Ready(recipient) => {
                map.insert("action".into(), "ready".into());
                map.insert("recipient".into(), recipient.to_value());
            }
            Self::Decline => {
                map.insert("action".into(), "decline".into());
            }
            Self::Cancel => {
                map.insert("action".into(), "cancel".into());
            }
        }
        Value::Object(map)
    }

    /// Reads the answer.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = object(value, "the answer is not an object")?;
        match map.get("action").and_then(Value::as_str) {
            Some("ready") => Ok(Self::Ready(Recipient::from_value(
                map.get("recipient")
                    .ok_or(Error::Malformed("the answer has no recipient"))?,
            )?)),
            Some("decline") => Ok(Self::Decline),
            Some("cancel") => Ok(Self::Cancel),
            _ => Err(Error::Malformed(
                "the answer's action is not ready, decline or cancel",
            )),
        }
    }
}

/// `_coqui/secret/show` with `"step": "deliver"`: the secret values, sealed to the key the frontend gave for this show.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowDeliver {
    /// The session the show belongs to.
    pub session_id: String,
    /// The show's id, as the `open` step gave it.
    pub request_id: String,
    /// The secret values, by field.
    pub sealed: BTreeMap<String, Sealed>,
}

impl ShowDeliver {
    /// The parameters as JSON, with `"step": "deliver"`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("step".into(), "deliver".into());
        map.insert("sessionId".into(), Value::String(self.session_id.clone()));
        map.insert("requestId".into(), Value::String(self.request_id.clone()));
        let values: Map<String, Value> = self
            .sealed
            .iter()
            .map(|(name, value)| (name.clone(), value.to_value()))
            .collect();
        map.insert("values".into(), Value::Object(values));
        Value::Object(map)
    }

    /// Reads the parameters of a `deliver` step.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        let map = object(value, "the show is not an object")?;
        if map.get("step").and_then(Value::as_str) != Some("deliver") {
            return Err(Error::Malformed("the show's step is not deliver"));
        }
        let values = object(
            map.get("values")
                .ok_or(Error::Malformed("the show has no values"))?,
            "the values are not an object",
        )?;
        Ok(Self {
            session_id: text(map, "sessionId", "the show has no session")?,
            request_id: text(map, "requestId", "the show has no id")?,
            sealed: values
                .iter()
                .map(|(name, value)| Ok((name.clone(), Sealed::from_value(value)?)))
                .collect::<Result<_, Error>>()?,
        })
    }
}

/// The answer to a `deliver` step, given when the person has seen the values and closed them, or refused to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowDone {
    /// They saw them.
    Shown,
    /// They dismissed it unseen.
    Cancel,
}

impl ShowDone {
    /// `{"action": "shown"}` or `{"action": "cancel"}`.
    #[must_use]
    pub fn to_value(self) -> Value {
        let mut map = Map::new();
        map.insert(
            "action".into(),
            if self == Self::Shown {
                "shown"
            } else {
                "cancel"
            }
            .into(),
        );
        Value::Object(map)
    }

    /// Reads the answer.
    ///
    /// # Errors
    /// [`Error::Malformed`].
    pub fn from_value(value: &Value) -> Result<Self, Error> {
        match object(value, "the answer is not an object")?
            .get("action")
            .and_then(Value::as_str)
        {
            Some("shown") => Ok(Self::Shown),
            Some("cancel") => Ok(Self::Cancel),
            _ => Err(Error::Malformed(
                "the answer's action is not shown or cancel",
            )),
        }
    }
}
