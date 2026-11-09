//! What crosses the `acp_harness` kind besides the channel and the spawner: plain data, the same in
//! every language.
//!
//! What a host sends (`StartRequest`, `Requirement`, `SessionRecordQuery`) refuses a key it does
//! not declare, so a field a newer host adds is never silently dropped by an older provider. What
//! a provider answers is read leniently.

use alloc::string::String;
use alloc::vec::Vec;

use guatiao::{FromValue, Schema, Secret, ToValue};

use crate::environment::Environment;
use crate::requirement::Requirement;

/// What a harness's `initialize` will advertise, known before anything starts. `initialize`'s own
/// answer governs; this is what a host shows before it has one.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct Takes {
    /// Images in a prompt.
    #[map(default)]
    pub images: bool,
    /// Audio in a prompt.
    #[map(default)]
    pub audio: bool,
    /// Resources' content in a prompt.
    #[map(default)]
    pub embedded_context: bool,
    /// MCP servers reached over HTTP.
    #[map(default)]
    pub mcp_http: bool,
    /// MCP servers reached over server-sent events.
    #[map(default)]
    pub mcp_sse: bool,
    /// `session/load`: a session reopened with its history replayed.
    #[map(default)]
    pub load: bool,
    /// `session/resume`: a session reopened without its history replayed.
    #[map(default)]
    pub resume: bool,
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
            audio: false,
            embedded_context: false,
            mcp_http: false,
            mcp_sse: false,
            load: false,
            resume: false,
            additional_directories: false,
            system_prompt: false,
        }
    }
}

/// Who a harness is, from [`AcpHarness::describe`](crate::AcpHarness::describe), as configured.
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
    /// Whether a session held to requirements can be reopened (`session/load` or `session/resume`)
    /// on a later connection started with fewer, and then have what that connection allows.
    #[map(default)]
    pub resumes_wider: bool,
}

impl HarnessInfo {
    /// A harness named `title` for people, answering the ACP `versions`, taking nothing more.
    #[must_use]
    pub fn new(title: impl Into<String>, versions: Vec<u16>) -> Self {
        Self {
            title: title.into(),
            versions,
            takes: Takes::new(),
            resumes_wider: false,
        }
    }
}

/// What a host starts one connection with: what the provider cannot learn from ACP, on top of the
/// provider's own configuration. Everything in it only narrows, except `session_id`, which names.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Default, PartialEq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct StartRequest {
    /// Variables for what the harness runs, over its configuration's. A provider shows none of
    /// them in an answer, a refusal, an error or a log.
    #[map(default)]
    pub environment: Secret<Environment>,
    /// The directories every session's `cwd` and additional directories must lie in, compared as
    /// [`WorkingRoots`](crate::WorkingRoots) says; empty adds none.
    #[map(default)]
    pub working_roots: Vec<String>,
    /// What the connection is held to. A name may come once.
    #[map(default)]
    pub requirements: Vec<Requirement>,
    /// The id the connection's first `session/new` creates its session under, chosen by the host:
    /// a UUID as [`is_session_id`](crate::is_session_id) spells it.
    pub session_id: Option<String>,
}

impl StartRequest {
    /// A request adding nothing to the provider's configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A request to fill in, starting from [`new`](Self::new).
    pub fn builder() -> StartRequestBuilder {
        StartRequestBuilder(Self::new())
    }
}

/// Builds a [`StartRequest`]; from [`StartRequest::builder`].
#[derive(Debug, Clone)]
#[must_use]
pub struct StartRequestBuilder(StartRequest);

impl StartRequestBuilder {
    /// The variables for what the harness runs.
    pub fn environment(mut self, environment: Environment) -> Self {
        self.0.environment = Secret::new(environment);
        self
    }

    /// Adds a working root.
    pub fn working_root(mut self, root: impl Into<String>) -> Self {
        self.0.working_roots.push(root.into());
        self
    }

    /// Adds a requirement.
    pub fn requirement(mut self, requirement: impl Into<Requirement>) -> Self {
        self.0.requirements.push(requirement.into());
        self
    }

    /// The id of the connection's first new session.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.0.session_id = Some(id.into());
        self
    }

    /// The request.
    #[must_use]
    pub fn build(self) -> StartRequest {
        self.0
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
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao, deny_unknown_fields)]
#[non_exhaustive]
pub struct SessionRecordQuery {
    /// The session's id as the provider reported it: a UUID.
    pub session_id: String,
    /// The directory the session worked in.
    pub working_directory: String,
    /// The variables its connection was started with.
    #[map(default)]
    pub environment: Secret<Environment>,
}

impl SessionRecordQuery {
    /// The session `session_id`, which worked in `working_directory`, with no environment.
    #[must_use]
    pub fn new(session_id: impl Into<String>, working_directory: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            working_directory: working_directory.into(),
            environment: Secret::default(),
        }
    }

    /// The same query, with the variables its connection was started with.
    #[must_use]
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environment = Secret::new(environment);
        self
    }
}

/// Where a harness keeps its own record of one session, and in what format.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct SessionRecordFile {
    /// The file's absolute path, where the harness's agent runs.
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

/// A setting of a configured harness that differs from its default in a way its settings' schema
/// cannot show, as plain data for the host to show or log; never refused for it.
#[derive(ToValue, FromValue, Schema, Debug, Clone, PartialEq, Eq)]
#[map(crate = ::guatiao)]
#[non_exhaustive]
pub struct NonDefault {
    /// The setting's name, as the provider spells it (`allowed_tools[2]`).
    pub setting: String,
    /// Its value, as text; never a credential.
    pub value: String,
    /// The default value, as text.
    pub default: String,
}

impl NonDefault {
    /// A non-default `setting`, now `value`, against `default`.
    #[must_use]
    pub fn new(
        setting: impl Into<String>,
        value: impl Into<String>,
        default: impl Into<String>,
    ) -> Self {
        Self {
            setting: setting.into(),
            value: value.into(),
            default: default.into(),
        }
    }
}
