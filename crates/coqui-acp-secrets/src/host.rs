use core::time::Duration;
use std::path::PathBuf;

use zeroize::Zeroizing;

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

    /// Whether the person can answer now. A request that needs them while this is false is answered at once with the status
    /// `no_one_present` and nothing is put to the frontend; a request the frontend can answer from what it holds
    /// ([`Frontend::recall`]) is answered all the same. The default is true.
    fn attended(&self) -> bool {
        true
    }

    /// The longest a request waits for the person, whatever the agent asks for. When it is given, the tool states it in its schema
    /// (`wait_seconds` has this as its maximum) and a request that names no wait waits this long. The default is none: the
    /// frontend alone decides how long a request waits.
    fn longest_wait(&self) -> Option<Duration> {
        None
    }

    /// As [`Frontend::request`], but gives up after `wait`: `Ok(None)` when the person had not answered by then, and the question
    /// is withdrawn from their screen. A frontend that gives a [`Frontend::longest_wait`], or is asked to wait less, overrides this;
    /// the default ignores `wait` and waits as `request` does.
    ///
    /// # Errors
    /// [`Error::Frontend`] when the frontend or the link to it fails.
    fn request_within(
        &self,
        params: &RequestParams,
        wait: Duration,
    ) -> Result<Option<RequestReply>, Error> {
        let _ = wait;
        self.request(params).map(Some)
    }

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

    /// Puts an [`Approval`] to the person and returns their answer, as a permission card does: allow once, allow for the session, or
    /// no. It is asked in every mode except [`Mode::Bypass`], which [`Frontend::mode`] reports and which approves without asking, so
    /// an implementation answers from the person, not from the mode. Show the whole approval: the command, the program by full path
    /// and what it is a link to, the folder, and the notice that the folder's scripts, hooks and configuration belong to the agent.
    ///
    /// # Errors
    /// [`Error::Frontend`].
    fn approve(&self, approval: &Approval) -> Result<Decision, Error>;

    /// The session's permission mode now. A secret's use is a permission like any other: in [`Mode::Bypass`] (Full access) it is
    /// approved without asking, in any other mode `approve` is asked. Read at every use, so a change of mode applies at once. A host
    /// that has a permission decider answers from the same one here; the default is [`Mode::Ask`].
    fn mode(&self) -> Mode {
        Mode::Ask
    }

    /// The value the frontend already holds for the secret question `field`, so a request whose every question is a secret one it
    /// holds is answered without putting anything to the person. A frontend that remembers what the person typed, or reads a vault
    /// of theirs, answers here; the default holds nothing. It is called on the thread of the tool call and must not wait on the
    /// person.
    fn recall(&self, _field: &str) -> Option<Zeroizing<Vec<u8>>> {
        None
    }

    /// The ways the person may choose to have a secret they type kept, the first being what the form starts on. When this is not
    /// empty the form carries one more question, [`KEEP_FIELD`], offering them, and the answer is handed to
    /// [`Frontend::remember`] with each value typed. The default offers none.
    fn keeps(&self) -> Vec<Keep> {
        Vec::new()
    }

    /// A secret the person typed, with the id of the [`Keep`] they chose. Not called when the form offered none. A failure is the
    /// frontend's to report: the value is already the session's.
    fn remember(&self, _field: &str, _value: &[u8], _keep: &str) {}
}

/// The name of the question the form carries when the frontend offers [`Keep`]s. An agent's own question of that name is refused.
pub const KEEP_FIELD: &str = "_keep";

/// One way a typed secret may be kept, as the form offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Keep {
    /// What the frontend is told when the person picks it.
    pub id: String,
    /// What the form calls it.
    pub label: String,
}

impl Keep {
    /// The way `id`, called `label`.
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// How the session's permission mode treats a secret's use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// The person is asked, with allow once and allow for the session.
    Ask,
    /// Approved without asking, as the harness's own tools are in Full access. Like a session allowance, it trusts the contents of
    /// the folder the command runs in.
    Bypass,
}

/// What every approval of a command says about its folder.
pub const FOLDER_NOTICE: &str = "The scripts, hooks and configuration in this folder belong to the agent, which can change them: \
    whatever this command runs from there can read the secret.";

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
    /// The command exactly as it was given, unescaped: what "for the session" is keyed on, so two commands that read alike are two.
    pub(crate) raw: String,
    /// What the person must be told besides the action: for a command, [`FOLDER_NOTICE`]. Show it.
    pub notice: Option<&'static str>,
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
        /// Whether the file is under the session's roots. The host's agent is assumed to read and change files there with its own
        /// tools, whatever the file's mode, so a secret written there is the agent's to read.
        under_roots: bool,
    },
    /// Run a command with the secret in an environment variable. Whoever runs it can read the value.
    RunCommand {
        /// The variable that holds the secret.
        variable: String,
        /// The command in full (see [`CommandLine::display`](crate::CommandLine::display)).
        command: String,
        /// The program that will start, by full path: for shell text the shell. What the command names is resolved on a search path
        /// that leads nowhere under the session's roots; a program named by a path is shown as the file that path is.
        program: PathBuf,
        /// What `program` is a link to, when that is another file: it is started under the name it was found by, never its target's.
        target: Option<PathBuf>,
        /// Where it starts.
        cwd: PathBuf,
    },
    /// Run a command and show its output to the person as a secret.
    ShowCommand {
        /// The command in full.
        command: String,
        /// The program that will start, by full path.
        program: PathBuf,
        /// What `program` is a link to, when that is another file.
        target: Option<PathBuf>,
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
            "{}\u{0}{}\u{0}{}\u{0}{:?}",
            self.handle.as_deref().unwrap_or(""),
            self.secret,
            self.raw,
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
    /// An approval was not asked because the session's mode approves it.
    Bypassed {
        /// The session.
        session_id: String,
        /// What was approved.
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
    /// A request was answered from what the frontend already held, without asking the person.
    Recalled {
        /// The session.
        session_id: String,
        /// The handle.
        handle: String,
        /// The field it answers.
        secret: String,
    },
    /// A session ended and its secrets were cleared.
    Ended {
        /// The session.
        session_id: String,
        /// How many secrets it held.
        cleared: usize,
    },
}
