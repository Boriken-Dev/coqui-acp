use core::fmt;

use crate::client::ClientId;

/// Why the hub would not take a call.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HubError {
    /// The policy refused the client; the reason is the policy's.
    Refused(String),
    /// A client with this identity is already connected.
    Duplicate(ClientId),
    /// No client with this identity is connected.
    UnknownClient(ClientId),
    /// No permission request with this number is pending.
    UnknownRequest(u64),
    /// The answer is not a permission outcome the request offers.
    InvalidAnswer,
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::Duplicate(_) => f.write_str("a client with this identity is already connected"),
            Self::UnknownClient(_) => f.write_str("no such client is connected"),
            Self::UnknownRequest(_) => f.write_str("no such permission request is pending"),
            Self::InvalidAnswer => f.write_str("not an outcome the permission request offers"),
        }
    }
}

impl core::error::Error for HubError {}
