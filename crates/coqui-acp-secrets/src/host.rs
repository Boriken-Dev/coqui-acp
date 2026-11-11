use std::path::PathBuf;

use coqui_acp::secret::{
    Capability, IdentityId, RequestParams, RequestReply, ShowDeliver, ShowDone, ShowOpen,
    ShowOpened,
};

use crate::Error;

/// The frontend of a session's owner: the one party the host ever asks. A host gives a session this and nothing else, so a request
/// is never sent to another attached client; the key binding of every answer is checked against [`Frontend::identity`] too, so an
/// answer from any other identity is refused.
///
/// The methods block until the person answers. They run on the thread that called the tool.
pub trait Frontend: Send + Sync {
    /// The identity the owner's frontend proves with: the only one whose answers are taken.
    fn identity(&self) -> IdentityId;

    /// What the frontend offered when it connected. A tool whose method it did not offer is not served.
    fn capability(&self) -> Capability;

    /// Sends `_coqui_acp/secret_request` and waits for the person.
    ///
    /// # Errors
    /// [`Error::Frontend`] when the frontend or the link to it fails.
    fn request(&self, params: &RequestParams) -> Result<RequestReply, Error>;

    /// Sends `_coqui_acp/secret_show_open`.
    ///
    /// # Errors
    /// [`Error::Frontend`].
    fn show_open(&self, open: &ShowOpen) -> Result<ShowOpened, Error>;

    /// Sends `_coqui_acp/secret_show_deliver` and waits until the person has seen the values and closed them.
    ///
    /// # Errors
    /// [`Error::Frontend`].
    fn show_deliver(&self, deliver: &ShowDeliver) -> Result<ShowDone, Error>;

    /// Puts an [`Approval`] to the person and returns their answer. It is asked for every use and every command-based show whatever
    /// the harness's permission mode is, so an implementation must not answer it from that mode: Full access and Bypass do not
    /// approve a secret's use.
    ///
    /// # Errors
    /// [`Error::Frontend`].
    fn approve(&self, approval: &Approval) -> Result<Decision, Error>;
}

/// What the person is asked to allow.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Approval {
    /// The session.
    pub session_id: String,
    /// The secret's handle, for a use or a show of a handle; the person is shown `secret` instead.
    pub handle: Option<String>,
    /// The secret's name: the form field it was entered as, or the field a show displays it in.
    pub secret: String,
    /// What would be done with it.
    pub action: Action,
}

/// What an [`Approval`] is for. Each shows the person everything that is done: a path or a command in full.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Write the secret to a file.
    WriteFile {
        /// The file, resolved.
        path: PathBuf,
        /// The mode it is made with.
        mode: u32,
        /// Whether this platform applies `mode` (Unix does; Windows files take their folder's access list).
        mode_applied: bool,
        /// Whether an existing file is replaced.
        overwrite: bool,
        /// Whether the file is under the agent's roots: its own tools can read it back, whatever its mode.
        readable_by_agent: bool,
    },
    /// Run a command with the secret in an environment variable. Whoever runs it can read the value.
    RunCommand {
        /// The variable that holds the secret.
        variable: String,
        /// The command in full (see [`CommandLine::display`](crate::CommandLine::display)).
        command: String,
        /// Where it starts.
        cwd: PathBuf,
    },
    /// Run a command and show its output to the person as a secret.
    ShowCommand {
        /// The command in full.
        command: String,
        /// Where it starts.
        cwd: PathBuf,
    },
    /// Show the person the host's environment variable of this name.
    ShowEnv {
        /// The variable.
        name: String,
    },
}

impl Approval {
    /// What "allow this for the rest of the session" remembers: the exact action for the exact secret.
    pub(crate) fn key(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{:?}",
            self.handle.as_deref().unwrap_or(""),
            self.secret,
            self.action
        )
    }
}

/// The person's answer to an [`Approval`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// No.
    Deny,
    /// Yes, this once.
    Once,
    /// Yes, and the same action for the same secret for the rest of the session without asking again.
    Session,
}

/// Something that happened, for a host's own audit log. It names handles, fields, paths and commands, and never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A secret was entered and kept under a handle.
    Stored {
        /// The session.
        session_id: String,
        /// The handle.
        handle: String,
        /// The field it was entered as.
        secret: String,
    },
    /// An approval was asked.
    Asked {
        /// The session.
        session_id: String,
        /// What was asked.
        action: Action,
    },
    /// An approval was answered.
    Answered {
        /// The session.
        session_id: String,
        /// The answer, or `session` when it was already allowed for the session.
        decision: Decision,
        /// Whether the answer came from an earlier "for the session".
        remembered: bool,
    },
    /// A file was written.
    Wrote {
        /// The session.
        session_id: String,
        /// The file.
        path: PathBuf,
    },
    /// A command was run.
    Ran {
        /// The session.
        session_id: String,
        /// The command in full.
        command: String,
    },
    /// A value was shown to the person.
    Shown {
        /// The session.
        session_id: String,
    },
    /// A session ended and its secrets were cleared.
    Ended {
        /// The session.
        session_id: String,
        /// How many secrets it held.
        cleared: usize,
    },
}
