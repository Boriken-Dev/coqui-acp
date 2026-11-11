//! Pi as an ACP agent.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::io::Read;
use std::sync::OnceLock;

use coqui_acp_api::{Bucket, UsageSupport};
use coqui_acp_harness::{
    AgentInfo, ConfigOption, Conversation, EventSink, Harness, HarnessError, Launch, McpTransport,
    Open, OpenRequest, OptionValue, PlainSpawn, Spawn, Takes,
};

use crate::launch::{PiLaunch, SessionStart, valid_session_id};
use crate::session::{PiSession, PiSettings};
use crate::{Error, mode};
use coqui_acp_harness::mode::Mode;

/// The value of the model and effort options when Pi has chosen none.
pub const DEFAULT: &str = "default";

/// Pi served as an ACP agent: each session a `pi --mode rpc` process with one session.
///
/// A session's `additionalDirectories` are directories a write's old content may be read from for its diff; Pi itself
/// works wherever its tools are pointed.
#[derive(Clone)]
pub struct PiAgent {
    defaults: PiLaunch,
    spawn: Arc<dyn Spawn>,
    settings: PiSettings,
    version: Arc<OnceLock<Option<String>>>,
}

impl core::fmt::Debug for PiAgent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PiAgent")
            .field("defaults", &self.defaults)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Default for PiAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl PiAgent {
    /// Runs `pi` from `PATH` with its own defaults, as a plain process.
    #[must_use]
    pub fn new() -> Self {
        Self {
            defaults: PiLaunch::new("."),
            spawn: Arc::new(PlainSpawn),
            settings: PiSettings::new(),
            version: Arc::new(OnceLock::new()),
        }
    }

    /// What every session starts with; a session's own parts are added to it.
    #[must_use]
    pub fn defaults(mut self, defaults: PiLaunch) -> Self {
        self.defaults = defaults;
        self
    }

    /// How processes are started.
    #[must_use]
    pub fn spawn(mut self, spawn: Arc<dyn Spawn>) -> Self {
        self.spawn = spawn;
        self
    }

    /// The bounds and waits of each session.
    #[must_use]
    pub fn settings(mut self, settings: PiSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Pi's version, from `pi --version`, asked once; `None` when it does not answer within [`PiSettings::version_wait`].
    #[must_use]
    pub fn cli_version(&self) -> Option<String> {
        self.version
            .get_or_init(|| {
                if self.settings.version_wait.is_zero() {
                    return None;
                }
                let mut launch = Launch::new(self.defaults.executable.clone())
                    .arg("--version")
                    .clear_env(self.defaults.clear_env);
                launch.env.clone_from(&self.defaults.env);
                launch.remove_env.clone_from(&self.defaults.remove_env);
                let spawned = self.spawn.spawn(&launch).ok()?;
                let (sender, received) = std::sync::mpsc::channel();
                let mut stdout = spawned.stdout;
                let limit = self.settings.version_bytes;
                let _ = std::thread::Builder::new()
                    .name("coqui-acp-pi-version".into())
                    .spawn(move || {
                        let mut text = String::new();
                        let _ = stdout.by_ref().take(limit).read_to_string(&mut text);
                        let _ = sender.send(text);
                    });
                let text = received.recv_timeout(self.settings.version_wait).ok();
                let _ = spawned.process.kill();
                // `1.5.2`
                text?.split_whitespace().next().map(str::to_string)
            })
            .clone()
    }

    fn launch_for(&self, request: &OpenRequest) -> Result<PiLaunch, HarnessError> {
        let mut launch = self.defaults.clone();
        launch.cwd.clone_from(&request.cwd);
        launch
            .working_roots
            .extend(request.working_roots.iter().cloned());
        launch.session = match &request.open {
            Open::New => SessionStart::New,
            Open::Resume(id) | Open::Load(id) if !valid_session_id(id) => {
                return Err(HarnessError::Refused("that is not a Pi session id".into()));
            }
            Open::Resume(id) => SessionStart::Resume(id.clone()),
            Open::Load(id) => SessionStart::Load(id.clone()),
            _ => {
                return Err(HarnessError::Refused(
                    "that kind of opening is not supported".into(),
                ));
            }
        };
        if let Some(prompt) = &request.system_prompt {
            launch.system_prompt = Some(match launch.system_prompt.take() {
                Some(own) => format!("{own}\n\n{prompt}"),
                None => prompt.clone(),
            });
        }
        if let Some(server) = request.mcp_servers.first() {
            return Err(HarnessError::Refused(match &server.transport {
                McpTransport::Sse { .. } => "Pi takes no MCP servers from a client".to_string(),
                _ => "Pi reads its MCP servers from its own configuration, not from a client"
                    .to_string(),
            }));
        }
        launch
            .additional_directories
            .extend(request.additional_directories.iter().cloned());
        Ok(launch)
    }
}

impl Harness for PiAgent {
    fn info(&self) -> AgentInfo {
        let title = match self.cli_version() {
            Some(version) => format!("Pi {version}"),
            None => "Pi".to_string(),
        };
        AgentInfo::new("coqui-acp-pi", title, env!("CARGO_PKG_VERSION"))
    }

    /// Pi reports no usage allowances: only what each turn spent, as the conversation's own usage.
    fn usage(&self) -> UsageSupport {
        UsageSupport::none()
    }

    fn read_usage(&self) -> Option<Vec<Bucket>> {
        None
    }

    fn takes(&self) -> Takes {
        Takes::new()
            .images(true)
            .embedded_context(true)
            .load(true)
            .additional_directories(true)
            .system_prompt(true)
    }

    fn open(
        &self,
        request: OpenRequest,
        events: EventSink,
    ) -> Result<Box<dyn Conversation>, HarnessError> {
        let launch = self.launch_for(&request)?;
        let sink = events.clone();
        let mut settings = self.settings.clone();
        if settings.known_version.is_none() {
            settings.known_version = self.cli_version();
        }
        let session = PiSession::start(&launch, self.spawn.as_ref(), settings, move |event| {
            sink.send(event)
        })
        .map_err(HarnessError::from)?;
        let start = launch.start_mode();
        session.set_mode(start);
        let offered = mode::offered(
            Some(start),
            launch.modes.as_deref().unwrap_or(&[]),
            launch.allow_bypass,
        );
        Ok(Box::new(PiConversation { session, offered }))
    }
}

impl From<Error> for HarnessError {
    fn from(error: Error) -> Self {
        match error {
            Error::Refused(why) => Self::Refused(why),
            other => Self::Failed(other.to_string()),
        }
    }
}

struct PiConversation {
    session: PiSession,
    /// The modes that can be chosen.
    offered: Vec<Mode>,
}

impl Conversation for PiConversation {
    fn session_id(&self) -> String {
        self.session.session().to_string()
    }

    fn options(&self) -> Vec<ConfigOption> {
        let (model, level, levels) = self.session.choice();
        let known = self.session.models();
        let mut models: Vec<OptionValue> = known
            .iter()
            .map(|model| {
                let mut about = Vec::new();
                about.push(model.provider.clone());
                if model.reasoning {
                    about.push("reasoning".to_string());
                }
                if let Some(window) = model.context_window {
                    about.push(format!("{}K context", window / 1000));
                }
                OptionValue::new(model.id.clone(), model.name.clone(), Some(about.join(", ")))
            })
            .collect();
        let current = model.unwrap_or_else(|| DEFAULT.to_string());
        if !models.iter().any(|value| value.id == current) {
            let description = (current == DEFAULT).then(|| "Pi's own choice".to_string());
            models.push(OptionValue::new(
                current.clone(),
                current.clone(),
                description,
            ));
        }
        let level = level.unwrap_or_else(|| DEFAULT.to_string());
        let mut efforts: Vec<OptionValue> = levels
            .iter()
            .map(|level| OptionValue::new(level.clone(), level.clone(), None))
            .collect();
        if !efforts.iter().any(|value| value.id == level) {
            efforts.push(OptionValue::new(level.clone(), level.clone(), None));
        }
        vec![
            ConfigOption::new("model", "Model", current, models).category("model"),
            ConfigOption::new("effort", "Thinking", level, efforts).category("thought_level"),
            coqui_acp_harness::mode::option(self.session.mode(), &self.offered),
        ]
    }

    fn prompt(&self, content: Vec<coqui_acp_harness::event::Content>) -> Result<(), HarnessError> {
        self.session.prompt(&content).map_err(HarnessError::from)
    }

    fn interrupt(&self) -> Result<(), HarnessError> {
        self.session.interrupt().map_err(HarnessError::from)
    }

    fn set_option(&self, id: &str, value: &str) -> Result<Vec<ConfigOption>, HarnessError> {
        if id == coqui_acp_harness::mode::OPTION_ID {
            let chosen = coqui_acp_harness::mode::chosen(value, &self.offered)
                .map_err(HarnessError::Refused)?;
            self.session.set_mode(chosen);
            return Ok(self.options());
        }
        let offered = self.options();
        let option = offered
            .iter()
            .find(|option| option.id == id)
            .ok_or_else(|| HarnessError::Refused(format!("no option {id}")))?;
        if !option.values.iter().any(|known| known.id == value) {
            return Err(HarnessError::Refused(format!(
                "{value} is not a value of {id}"
            )));
        }
        if id == "model" {
            self.session.set_model(value).map_err(HarnessError::from)?;
        } else {
            self.session.set_effort(value).map_err(HarnessError::from)?;
        }
        Ok(self.options())
    }

    fn close(&self) {
        self.session.close();
    }
}
