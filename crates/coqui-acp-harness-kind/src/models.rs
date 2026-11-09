//! What crosses the `acp_harness` kind besides the channel: plain data, the same in every language.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use guatiao::{FromValue, Schema, ToValue, Value};

/// A requirement that the harness's sessions only read and answer: no file is written and no
/// command runs.
pub const READ_ONLY: &str = "read_only";

/// A requirement that the harness's sessions run no command; they may still write files.
pub const NO_COMMANDS: &str = "no_commands";

/// A requirement that a session held to another requirement can later be resumed, on a connection
/// started without it, with the tools that connection allows.
pub const RESUME_WIDER: &str = "resume_wider";

/// A requirement that the harness load exactly the skills [`Requirement::detail`] lists (a list of
/// names) and no other.
pub const ONLY_SKILLS: &str = "only_skills";

/// A requirement that the interpreters [`Requirement::detail`] lists (a list of program names) run
/// inside the harness's own sandbox.
pub const SANDBOXED_SCRIPTS: &str = "sandboxed_scripts";

/// One thing a host holds a connection to. Each narrows what the harness may do; none widens it.
///
/// [`name`](Self::name) is an open vocabulary: this crate's constants name the part with a fixed
/// meaning, and a harness refuses a name it does not know.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct Requirement {
    /// What is required: [`READ_ONLY`], [`NO_COMMANDS`] and the others of this crate.
    pub name: String,
    /// What the requirement names, when it takes anything.
    pub detail: Option<Value>,
}

impl Requirement {
    /// The requirement `name`, with no detail.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: None,
        }
    }

    /// The requirement `name` about `detail`.
    #[must_use]
    pub fn about(name: impl Into<String>, detail: impl Into<Value>) -> Self {
        Self {
            name: name.into(),
            detail: Some(detail.into()),
        }
    }
}

/// What a harness's `initialize` will advertise, known before anything starts.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct Takes {
    /// Images in a prompt.
    #[map(default)]
    pub images: bool,
    /// Resources' content in a prompt.
    #[map(default)]
    pub embedded_context: bool,
    /// MCP servers reached over HTTP.
    #[map(default)]
    pub mcp_http: bool,
    /// MCP servers reached over server-sent events.
    #[map(default)]
    pub mcp_sse: bool,
    /// Loading a session with its history replayed.
    #[map(default)]
    pub load: bool,
    /// More directories than the working one.
    #[map(default)]
    pub additional_directories: bool,
    /// A system prompt appended to the harness's own.
    #[map(default)]
    pub system_prompt: bool,
}

impl Takes {
    /// Takes nothing beyond text prompts and stdio MCP servers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            images: false,
            embedded_context: false,
            mcp_http: false,
            mcp_sse: false,
            load: false,
            additional_directories: false,
            system_prompt: false,
        }
    }
}

/// Who a harness is, from [`AcpHarness::describe`](crate::AcpHarness::describe).
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct HarnessInfo {
    /// Its name for people, such as `Claude Code`.
    pub title: String,
    /// The ACP versions its `initialize` answers, lowest first.
    pub versions: Vec<u16>,
    /// What its `initialize` will advertise.
    #[map(default)]
    pub takes: Takes,
}

impl HarnessInfo {
    /// A harness named `title` for people, answering the ACP `versions`, taking nothing more.
    #[must_use]
    pub fn new(title: impl Into<String>, versions: Vec<u16>) -> Self {
        Self {
            title: title.into(),
            versions,
            takes: Takes::new(),
        }
    }
}

/// What a host starts one connection with: what the provider cannot learn from ACP.
#[derive(ToValue, FromValue, Schema, Clone, PartialEq, Default)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct StartRequest {
    /// Environment values for what the harness runs. They may hold credentials, so a provider logs and
    /// reports none of them.
    #[map(default)]
    #[schema(sensitive)]
    pub environment: BTreeMap<String, String>,
    /// The directories every session's `cwd` and other directories must lie in, on top of the
    /// provider's own configuration; empty adds none.
    #[map(default)]
    pub working_roots: Vec<String>,
    /// What the connection is held to, on top of the provider's own configuration.
    #[map(default)]
    pub requirements: Vec<Requirement>,
}

impl fmt::Debug for StartRequest {
    /// Names the environment and shows none of its values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartRequest")
            .field("environment", &self.environment.keys())
            .field("working_roots", &self.working_roots)
            .field("requirements", &self.requirements)
            .finish()
    }
}

impl StartRequest {
    /// A request adding nothing to the provider's configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// One model a harness says an account may use.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct ModelInfo {
    /// The harness's own identifier or alias for it.
    pub id: String,
    /// What the harness shows a person, when it has a name apart from the identifier.
    pub name: Option<String>,
    /// The harness's one-line description, when it gives one.
    pub description: Option<String>,
}

impl ModelInfo {
    /// The model `id`, with no name or description.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            description: None,
        }
    }
}

/// What a harness can say about the models an account may use, with no model turn. A harness that
/// cannot list says so; it never guesses.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct ModelListing {
    /// Whether the harness listed them; when not, `models` is empty.
    pub known: bool,
    /// Why it could not, when `known` is false.
    pub unknown_because: Option<String>,
    /// The models, in the harness's order.
    #[map(default)]
    pub models: Vec<ModelInfo>,
}

impl ModelListing {
    /// A listing the harness could not give, with the reason.
    #[must_use]
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            known: false,
            unknown_because: Some(reason.into()),
            models: Vec::new(),
        }
    }

    /// The models a harness listed.
    #[must_use]
    pub fn of(models: Vec<ModelInfo>) -> Self {
        Self {
            known: true,
            unknown_because: None,
            models,
        }
    }
}

/// The session whose record [`AcpHarness::session_record`](crate::AcpHarness::session_record) is
/// asked for.
#[derive(ToValue, FromValue, Schema, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct SessionRecordQuery {
    /// The ACP session id the harness issued.
    pub session_id: String,
    /// The directory the session worked in.
    pub working_directory: String,
    /// The environment its connection was started with.
    #[map(default)]
    #[schema(sensitive)]
    pub environment: BTreeMap<String, String>,
}

impl fmt::Debug for SessionRecordQuery {
    /// Names the environment and shows none of its values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRecordQuery")
            .field("session_id", &self.session_id)
            .field("working_directory", &self.working_directory)
            .field("environment", &self.environment.keys())
            .finish()
    }
}

impl SessionRecordQuery {
    /// The session `session_id`, which worked in `working_directory`, with no environment.
    #[must_use]
    pub fn new(session_id: impl Into<String>, working_directory: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            working_directory: working_directory.into(),
            environment: BTreeMap::new(),
        }
    }
}

/// Where a harness keeps its own record of one session, and in what format.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct SessionRecordFile {
    /// The file's absolute path.
    pub path: String,
    /// What the file holds, as a short name a reader knows (`claude-code-jsonl`,
    /// `codex-rollout-jsonl`).
    pub format: String,
}

impl SessionRecordFile {
    /// The record at `path`, in `format`.
    #[must_use]
    pub fn new(path: impl Into<String>, format: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            format: format.into(),
        }
    }
}
