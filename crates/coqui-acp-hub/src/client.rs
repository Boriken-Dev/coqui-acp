use core::fmt;
use core::str::FromStr;

/// A client's identity: stable and opaque, chosen by the host, never by the client.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(String);

impl ClientId {
    /// The identity as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ClientId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for ClientId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// Who a connected client is, as its host knows it. The hub carries these fields and shows them to the other clients and
/// the policy; it checks none of them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Client {
    /// The stable identity every message of this client is attributed to.
    pub id: ClientId,
    /// A name a person reads (a device, a user).
    pub label: String,
    /// What kind of client this is: `operator`, `station`, `runner`, whatever the host's policy distinguishes.
    pub role: String,
    /// The person or compartment the host maps this client to, for its policy and for attributing prompts.
    pub subject: Option<String>,
}

impl Client {
    /// A client with this identity, labelled by it, of role `client`, with no subject.
    #[must_use]
    pub fn new(id: impl Into<ClientId>) -> Self {
        let id = id.into();
        Self {
            label: id.0.clone(),
            id,
            role: "client".to_owned(),
            subject: None,
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Sets the role.
    #[must_use]
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = role.into();
        self
    }

    /// Sets the subject.
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// What the other attached clients are shown. The subject is the host's mapping of the client to a person or
    /// compartment, for its policy and audit, so it is never shown to them.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "id": self.id.as_str(), "label": self.label, "role": self.role })
    }
}

/// A position in the session's update log: opaque, monotonic, and the same on every reconnect. It is carried as the
/// decimal string `_meta["dev.boriken.coqui.acp/hub"].seq` of every update and, to resume, as `.cursor` of a
/// `session/load` or `session/resume` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Cursor(pub(crate) u64);

impl Cursor {
    /// Before the first update: replaying from here is replaying everything retained.
    pub const START: Cursor = Cursor(0);
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A cursor that is not one the hub issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorError;

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a cursor")
    }
}

impl core::error::Error for CursorError {}

impl FromStr for Cursor {
    type Err = CursorError;
    fn from_str(text: &str) -> Result<Self, CursorError> {
        text.parse().map(Cursor).map_err(|_| CursorError)
    }
}
