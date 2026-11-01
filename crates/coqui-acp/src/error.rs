use alloc::string::String;
use core::fmt;

/// Why a session could not open, or an operation failed. Variants hold the agent's own words.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Error {
    /// The agent refused, or answered with something unreadable.
    Refused(String),
    /// The agent lacks what this session needs.
    Unsupported(String),
    /// A deadline passed; the text names the operation.
    Timeout(String),
    /// The connection ended.
    Closed,
    /// The operating system would not start a thread.
    Thread,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) | Self::Unsupported(why) => f.write_str(why),
            Self::Timeout(what) => write!(f, "{what}: no answer in time"),
            Self::Closed => f.write_str("the connection is closed"),
            Self::Thread => f.write_str("could not start a thread"),
        }
    }
}

impl core::error::Error for Error {}
