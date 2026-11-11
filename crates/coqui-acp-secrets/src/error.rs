use core::fmt;

use coqui_acp::secret::Error as SecretError;

/// Why a tool call, or a session operation, did not happen. No variant holds a secret value: each names which rule refused, and the
/// text reaches the agent as it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The session is not open, or has ended.
    UnknownSession,
    /// The session is already open.
    SessionOpen,
    /// The frontend did not offer this method, so the session has no such tool.
    NotOffered(&'static str),
    /// The handle names no secret of this session: never made, already expired, or the session ended.
    UnknownHandle,
    /// Another request or show of this session is still waiting for the person.
    Busy,
    /// A limit of the settings refused it.
    Limit(&'static str),
    /// The arguments of a tool call are not in the shape the tool takes.
    Arguments(&'static str),
    /// The person said no, or the frontend would not take the question.
    Refused(&'static str),
    /// A path the rules refuse: outside the session's roots, a link, a parent that is not there.
    Path(&'static str),
    /// A command that cannot be run, ran out of time, or failed.
    Command(&'static str),
    /// The frontend or the transport to it failed.
    Frontend(&'static str),
    /// The request id was used already; a request is answered once.
    Replayed,
    /// The extension's own refusal: a form, a key binding, a ciphertext.
    Secret(SecretError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSession => f.write_str("the session is not open"),
            Self::SessionOpen => f.write_str("the session is already open"),
            Self::NotOffered(what) => write!(f, "the person's frontend does not take {what}"),
            Self::UnknownHandle => f.write_str("no such secret handle in this session"),
            Self::Busy => f.write_str("another secret prompt of this session is still open"),
            Self::Limit(what) => write!(f, "over a limit: {what}"),
            Self::Arguments(what) => write!(f, "bad arguments: {what}"),
            Self::Refused(what) => write!(f, "refused: {what}"),
            Self::Path(what) => write!(f, "path refused: {what}"),
            Self::Command(what) => write!(f, "command: {what}"),
            Self::Frontend(what) => write!(f, "frontend: {what}"),
            Self::Replayed => f.write_str("the request id was already used"),
            Self::Secret(error) => write!(f, "{error}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Secret(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SecretError> for Error {
    fn from(error: SecretError) -> Self {
        Self::Secret(error)
    }
}
