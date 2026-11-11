//! A running `pi --mode rpc`: its process, its JSONL stream, and one session on it.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use std::io;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use coqui_acp_harness::event::{Command, Content, Event, Severity, context_block};
use coqui_acp_harness::lines::{self, Tail};
use coqui_acp_harness::mode::Mode;
use coqui_acp_harness::stream::Waiting;
use coqui_acp_harness::{LineHook, Process, Spawn, Step, Stream, exit_text, line_writer, version};
use serde_json::{Value, json};

use crate::gate::Gate;
use crate::launch::{PiLaunch, SessionStart, valid_session_id};
use crate::mode::Policy;
use crate::parse::{Parser, ParserLimits, Writer};
use crate::{Error, TESTED_VERSIONS};

/// The bounds and waits of a session. Every one is a setting with a default, and any value is taken.
#[derive(Clone)]
#[non_exhaustive]
pub struct PiSettings {
    /// The longest line of Pi's output read; past it the session ends. Default 64 MiB.
    pub max_line: u64,
    /// How many of the last lines that were not protocol a failure quotes. Default 8.
    pub tail_lines: usize,
    /// The bytes of each of those lines. Default 512.
    pub tail_line_bytes: usize,
    /// How long Pi may take to answer the first command, which waits for it to start. Default 30 s.
    pub initialize_timeout: Duration,
    /// How long it may take to answer any other command. Default 30 s.
    pub request_timeout: Duration,
    /// How long it may take to answer `prompt`, which a command of an extension answers when it is done. Default 5 min.
    pub prompt_timeout: Duration,
    /// How long the end of the output waits for the exit status. Default 1 s.
    pub exit_wait: Duration,
    /// The tool calls followed at once. Default 4096.
    pub max_tools: usize,
    /// The earlier messages a load replays, the latest kept. Default 5000.
    pub max_history_messages: usize,
    /// What the parser keeps and cuts.
    pub parser: ParserLimits,
    /// Whether to ask for the models and thinking levels when starting. Default on.
    pub list_models: bool,
    /// How often Pi is asked whether it has exited while its output stays open. Default 100 ms.
    pub exit_poll: Duration,
    /// How long a close waits for it to have ended, as the caller may remove its directory next. Default 2 s.
    pub close_wait: Duration,
    /// The lines waiting to be written to it; past it a write fails. Default 1024.
    pub max_queued_writes: usize,
    /// The bytes of a session's title, from its first prompt. Default 80.
    pub title_bytes: usize,
    /// How long `pi --version` may take; 0 does not ask. Default 10 s.
    pub version_wait: Duration,
    /// How much of its answer is read. Default 4096.
    pub version_bytes: u64,
    /// The version of Pi, when the caller has asked: one this crate was not tested with is a warning.
    pub known_version: Option<String>,
    /// The host's check of each line Pi writes, before anything reads it; `false` ends Pi.
    pub on_line: Option<LineHook>,
}

impl fmt::Debug for PiSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PiSettings")
            .field("max_line", &self.max_line)
            .field("initialize_timeout", &self.initialize_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("prompt_timeout", &self.prompt_timeout)
            .field("max_tools", &self.max_tools)
            .field("list_models", &self.list_models)
            .field("known_version", &self.known_version)
            .field("on_line", &self.on_line.is_some())
            .finish_non_exhaustive()
    }
}

impl PiSettings {
    /// The defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_line: 64 * 1024 * 1024,
            tail_lines: 8,
            tail_line_bytes: 512,
            initialize_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
            prompt_timeout: Duration::from_secs(300),
            exit_wait: Duration::from_secs(1),
            max_tools: 4096,
            max_history_messages: 5000,
            parser: ParserLimits::new(),
            list_models: true,
            exit_poll: Duration::from_millis(100),
            close_wait: Duration::from_secs(2),
            max_queued_writes: 1024,
            title_bytes: 80,
            version_wait: Duration::from_secs(10),
            version_bytes: 4096,
            known_version: None,
            on_line: None,
        }
    }
}

impl Default for PiSettings {
    fn default() -> Self {
        Self::new()
    }
}

/// A model Pi offers, from `get_available_models`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Model {
    /// What a client names it by: the provider and the model's own id, `provider/id`.
    pub id: String,
    /// The provider.
    pub provider: String,
    /// The model's own id, which `set_model` names.
    pub model: String,
    /// Its name for people.
    pub name: String,
    /// Whether it reasons, so takes thinking levels.
    pub reasoning: bool,
    /// Whether it takes images.
    pub images: bool,
    /// Its context window, in tokens.
    pub context_window: Option<u64>,
    /// Whether Pi would use it with none named. Set only by [`PiSession::read_models`].
    pub is_default: bool,
    /// The thinking levels it takes, in Pi's order: listed only for the model Pi would use, empty for the others.
    pub efforts: Vec<String>,
    /// The level Pi starts that model at.
    pub default_effort: Option<String>,
}

fn model_of(model: &Value) -> Option<Model> {
    let provider = model.get("provider")?.as_str()?;
    let id = model.get("id")?.as_str()?;
    Some(Model {
        id: format!("{provider}/{id}"),
        provider: provider.to_string(),
        model: id.to_string(),
        name: model
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        reasoning: model.get("reasoning").and_then(Value::as_bool) == Some(true),
        images: model
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "image")),
        context_window: model.get("contextWindow").and_then(Value::as_u64),
        is_default: false,
        efforts: Vec::new(),
        default_effort: None,
    })
}

/// The models of a `get_available_models` answer (`{models: [..]}`); Pi's placeholder for "nothing configured" is left out.
#[must_use]
pub fn models_of(data: &Value) -> Vec<Model> {
    data.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(model_of)
        .filter(|model| model.provider != "unknown")
        .collect()
}

/// What the session knows of its model and thinking level.
#[derive(Debug, Default)]
struct Choice {
    models: Vec<Model>,
    /// The model in use, `provider/id`.
    model: Option<String>,
    level: Option<String>,
    levels: Vec<String>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running `pi --mode rpc` with one session open. Its events go to the sink given to [`PiSession::start`], from a thread
/// of its own; every method may be called from any thread.
pub struct PiSession {
    session: String,
    writer: Writer,
    process: Arc<dyn Process>,
    parser: Arc<Mutex<Parser>>,
    policy: Arc<Mutex<Policy>>,
    waiting: Waiting,
    ended: Arc<OnceLock<String>>,
    requests: AtomicU64,
    settings: PiSettings,
    closed: Arc<AtomicBool>,
    choice: Arc<Mutex<Choice>>,
    titled: AtomicBool,
    sink: Arc<dyn Fn(Event) -> bool + Send + Sync>,
    /// Removes the gate's directory once Pi has ended: declared last, so it is dropped after the close.
    gate_command: Option<String>,
    _gate: Option<Gate>,
}

impl fmt::Debug for PiSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PiSession")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl PiSession {
    /// Starts `pi --mode rpc` as `launch` says, through `spawn`, with the permission gate loaded; reads its state; lists
    /// its models and thinking levels and its commands; and checks that the session it opened is the one asked for. For
    /// [`SessionStart::Load`] the session's earlier messages go to `sink` as history first. A version Pi was not tested
    /// with is an [`Event::Notice`], never a refusal. Events reach `sink` until it answers `false`; the output's end is
    /// one [`Event::Exited`].
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a model or thinking level that cannot be an argument; [`Error::Spawn`] when it could not
    /// start; [`Error::Timeout`] or [`Error::Harness`] when a command failed, and Pi has been ended.
    pub fn start(
        launch: &PiLaunch,
        spawn: &dyn Spawn,
        settings: PiSettings,
        sink: impl Fn(Event) -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let mut session = Self::connect(launch, spawn, settings, sink, false)?;
        if let Err(error) = session.open(launch) {
            session.close();
            return Err(error);
        }
        Ok(session)
    }

    /// Lists the models Pi has credentials for with no turn: starts `pi --mode rpc --no-session` as `launch` says, with no
    /// gate, asks, and ends it. An empty list is Pi having none configured.
    ///
    /// # Errors
    ///
    /// As [`PiSession::start`] up to the first command.
    pub fn read_models(
        launch: &PiLaunch,
        spawn: &dyn Spawn,
        settings: PiSettings,
    ) -> Result<Vec<Model>, Error> {
        let session = Self::connect(launch, spawn, settings, |_| true, true)?;
        let listed = session.read_listing();
        session.close();
        listed
    }

    /// The models, with the model Pi would use marked and, for that one alone, its thinking levels: Pi lists levels only for
    /// the model in use, and a switch per model would be a command per entry.
    fn read_listing(&self) -> Result<Vec<Model>, Error> {
        let state = self.request("get_state", json!({}), self.settings.initialize_timeout)?;
        let data = self.request(
            "get_available_models",
            json!({}),
            self.settings.request_timeout,
        )?;
        let mut models = models_of(&data);
        let current = state
            .get("model")
            .and_then(model_of)
            .map(|model| model.id)
            .filter(|id| models.iter().any(|model| &model.id == id));
        if let Some(current) = current
            && let Some(model) = models.iter_mut().find(|model| model.id == current)
        {
            model.is_default = true;
            model.default_effort = state
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Ok(levels) = self.request(
                "get_available_thinking_levels",
                json!({}),
                self.settings.request_timeout,
            ) {
                model.efforts = levels
                    .get("levels")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|level| level.as_str().map(str::to_string))
                    .collect();
            }
        }
        Ok(models)
    }

    /// Starts the process and its readers, with no command sent yet. `ephemeral` leaves the gate out and keeps no session.
    fn connect(
        launch: &PiLaunch,
        spawn: &dyn Spawn,
        settings: PiSettings,
        sink: impl Fn(Event) -> bool + Send + Sync + 'static,
        ephemeral: bool,
    ) -> Result<Self, Error> {
        if launch.start_mode() == Mode::Bypass && !launch.allow_bypass {
            return Err(Error::Refused(
                "the Bypass mode needs the launch to allow it".into(),
            ));
        }
        if !ephemeral && let Some(root) = launch.credential_root() {
            return Err(Error::Refused(format!(
                "the working directory {} holds the places credentials are kept in; a search from there would read them                  (allow_credential_roots lets a launch do it)",
                root.display()
            )));
        }
        for (what, value) in [("model", &launch.model), ("thinking level", &launch.effort)] {
            if let Some(value) = value
                && (value.starts_with('-') || value.chars().any(char::is_control))
            {
                return Err(Error::Refused(format!("that is not a Pi {what}")));
            }
        }
        if let SessionStart::Resume(id) | SessionStart::Load(id) = &launch.session
            && !valid_session_id(id)
        {
            return Err(Error::Refused("that is not a Pi session id".into()));
        }
        let sink: Arc<dyn Fn(Event) -> bool + Send + Sync> = Arc::new(sink);
        let gate = if ephemeral {
            None
        } else {
            Some(
                Gate::write(launch.gate_dir.as_deref(), launch.system_prompt.as_deref())
                    .map_err(Error::Spawn)?,
            )
        };
        let mut command = launch.launch_guarded(
            gate.as_ref().map(Gate::extension),
            gate.as_ref().and_then(Gate::prompt),
            gate.as_ref().map(Gate::guard),
        )?;
        if ephemeral {
            command = command.arg("--no-session");
        }
        let spawned = spawn.spawn(&command).map_err(Error::Spawn)?;
        let process: Arc<dyn Process> = Arc::from(spawned.process);
        // Past here Pi runs: a failure ends it, since no session exists yet to do so on drop.
        let ended_with = |error: io::Error| {
            let _ = process.kill();
            Error::Spawn(error)
        };
        let writer: Writer = line_writer(
            spawned.stdin,
            settings.max_queued_writes,
            "coqui-acp-pi-stdin",
        )
        .map_err(ended_with)?;
        let policy = Arc::new(Mutex::new(Policy::new(
            launch.start_mode(),
            launch.ask_for_reads,
        )));
        let parser = Arc::new(Mutex::new(
            Parser::with_limits(Arc::clone(&writer), settings.max_tools, settings.parser)
                .with_policy(Arc::clone(&policy))
                .with_roots(&launch.cwd, launch.roots())
                .with_protected(launch.agent_dirs())
                .with_home(launch.home()),
        ));
        let tail = Arc::new(Mutex::new(Tail::new(
            settings.tail_lines,
            settings.tail_line_bytes,
        )));
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(stderr) = spawned.stderr {
            let tail = Arc::clone(&tail);
            let max_line = settings.max_line;
            thread::Builder::new()
                .name("coqui-acp-pi-stderr".into())
                .spawn(move || lines::read_diagnostics(stderr, max_line, &tail))
                .map_err(ended_with)?;
        }
        let mut stream = Stream::new(
            "Pi",
            Arc::clone(&process),
            tail,
            Arc::clone(&closed),
            Arc::clone(&sink),
        );
        stream.max_line = settings.max_line;
        stream.exit_poll = settings.exit_poll;
        stream.exit_wait = settings.exit_wait;
        stream.on_line.clone_from(&settings.on_line);
        let waiting = Arc::clone(&stream.waiting);
        let ended = Arc::clone(&stream.ended);
        let choice = Arc::new(Mutex::new(Choice::default()));
        {
            let parser = Arc::clone(&parser);
            let choice = Arc::clone(&choice);
            stream
                .read(
                    spawned.stdout,
                    "coqui-acp-pi-stdout",
                    move |value| {
                        if value.get("type").and_then(Value::as_str)
                            == Some("thinking_level_changed")
                            && let Some(level) = value.get("level").and_then(Value::as_str)
                        {
                            locked(&choice).level = Some(level.to_string());
                        }
                        let mut parser = locked(&parser);
                        let events = parser.line(value);
                        Step::new(events, parser.answers())
                    },
                    Vec::new,
                )
                .map_err(ended_with)?;
        }
        Ok(Self {
            session: String::new(),
            writer,
            process,
            parser,
            policy,
            waiting,
            ended,
            requests: AtomicU64::new(0),
            settings,
            closed,
            choice,
            titled: AtomicBool::new(false),
            sink,
            gate_command: gate.as_ref().map(Gate::command),
            _gate: gate,
        })
    }

    fn open(&mut self, launch: &PiLaunch) -> Result<(), Error> {
        let state = self.request("get_state", json!({}), self.settings.initialize_timeout)?;
        let session = state
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Harness("Pi named no session".into()))?;
        if let SessionStart::Resume(wanted) | SessionStart::Load(wanted) = &launch.session
            && wanted != session
        {
            return Err(Error::Harness(format!(
                "Pi opened session {session}, not the one requested"
            )));
        }
        self.session = session.to_string();
        if let Some(found) = self.settings.known_version.clone()
            && let Some(warning) = version::untested("Pi", &found, TESTED_VERSIONS)
        {
            (self.sink)(Event::Notice {
                severity: Severity::Warning,
                title: warning,
            });
        }
        self.apply_state(&state);
        if self.settings.list_models {
            self.list_models();
        }
        self.read_commands()?;
        if matches!(launch.session, SessionStart::Load(_)) {
            self.replay()?;
        }
        Ok(())
    }

    /// The model, level and window of a `get_state` answer.
    fn apply_state(&self, state: &Value) {
        let model = state.get("model").filter(|model| model.is_object());
        let known = model
            .and_then(model_of)
            .filter(|model| model.provider != "unknown");
        let mut choice = locked(&self.choice);
        choice.model = known.as_ref().map(|model| model.id.clone());
        choice.level = state
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .map(str::to_string);
        locked(&self.parser).set_context_window(known.and_then(|model| model.context_window));
    }

    /// Asks for the models and the current model's thinking levels. Their being unknown stops nothing: a notice says so.
    fn list_models(&self) {
        let listed = self.request(
            "get_available_models",
            json!({}),
            self.settings.request_timeout,
        );
        match listed {
            Ok(data) => locked(&self.choice).models = models_of(&data),
            Err(error) => {
                let _ = (self.sink)(Event::Notice {
                    severity: Severity::Warning,
                    title: format!("Pi's models are not known: {error}"),
                });
            }
        }
        self.read_levels();
    }

    fn read_levels(&self) {
        if let Ok(data) = self.request(
            "get_available_thinking_levels",
            json!({}),
            self.settings.request_timeout,
        ) {
            locked(&self.choice).levels = data
                .get("levels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| level.as_str().map(str::to_string))
                .collect();
        }
    }

    /// The commands of Pi's extensions, prompt templates and skills, which a prompt of `/name` runs.
    ///
    /// # Errors
    ///
    /// [`Error::Harness`] when the session has a gate and Pi does not list the command the gate registers: Pi did not load
    /// it (or could not be asked), and nothing it would run is gated.
    fn read_commands(&self) -> Result<(), Error> {
        let listed = self.request("get_commands", json!({}), self.settings.request_timeout);
        if let Some(gate) = &self.gate_command {
            let loaded = listed.as_ref().is_ok_and(|data| {
                data.get("commands")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|command| command.get("name").and_then(Value::as_str) == Some(gate))
            });
            if !loaded {
                return Err(Error::Harness(
                    "Pi did not load the permission gate, so it is not started".into(),
                ));
            }
        }
        let Ok(data) = listed else {
            return Ok(());
        };
        let commands: Vec<Command> = data
            .get("commands")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|command| {
                let name = command.get("name")?.as_str()?;
                if self.gate_command.as_deref() == Some(name) {
                    return None;
                }
                let description = command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Some(Command::new(name, description, None))
            })
            .collect();
        if !commands.is_empty() {
            (self.sink)(Event::Commands(commands));
        }
        Ok(())
    }

    /// The session's earlier messages, as history.
    fn replay(&self) -> Result<(), Error> {
        let data = self.request("get_messages", json!({}), self.settings.request_timeout)?;
        let messages = data
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let skip = messages
            .len()
            .saturating_sub(self.settings.max_history_messages);
        if skip > 0 {
            (self.sink)(Event::Notice {
                severity: Severity::Warning,
                title: format!(
                    "Only the last {} messages of this session were replayed",
                    self.settings.max_history_messages
                ),
            });
        }
        let writer: Writer = Arc::new(|_| Err(io::Error::other("a replayed message asks nothing")));
        let mut history =
            Parser::with_limits(writer, self.settings.max_tools, self.settings.parser);
        for event in history.history(&messages[skip..]) {
            (self.sink)(event);
        }
        Ok(())
    }

    fn write(&self, message: &Value) -> Result<(), Error> {
        (self.writer)(message)
            .map_err(|error| Error::Harness(format!("Pi could not be written to: {error}")))
    }

    fn next_id(&self) -> String {
        (self.requests.fetch_add(1, Ordering::SeqCst) + 1).to_string()
    }

    /// Sends one command of Pi's RPC protocol, `fields` beside its `type`, and waits up to `limit` for its response.
    ///
    /// # Errors
    ///
    /// [`Error::Harness`] when Pi refused or could not be written to, [`Error::Timeout`] when it did not answer.
    pub fn request(&self, command: &str, fields: Value, limit: Duration) -> Result<Value, Error> {
        let id = self.next_id();
        let (answer, answered) = mpsc::channel();
        locked(&self.waiting).insert(id.clone(), answer);
        if let Some(reason) = self.ended.get() {
            locked(&self.waiting).remove(&id);
            return Err(Error::Harness(reason.clone()));
        }
        let mut line = json!({"id": id, "type": command});
        if let (Some(line), Some(fields)) = (line.as_object_mut(), fields.as_object()) {
            line.extend(fields.clone());
        }
        if let Err(error) = self.write(&line) {
            locked(&self.waiting).remove(&id);
            return Err(error);
        }
        let result = answered.recv_timeout(limit);
        locked(&self.waiting).remove(&id);
        match result {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(refusal)) => Err(Error::Harness(refusal)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout {
                what: "a command",
                limit,
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::Harness("Pi ended before it answered".into()))
            }
        }
    }

    /// The session's id.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The models Pi offers.
    #[must_use]
    pub fn models(&self) -> Vec<Model> {
        locked(&self.choice).models.clone()
    }

    /// The model in use (`provider/id`; none while Pi has none configured), the thinking level, and the levels the model
    /// takes.
    #[must_use]
    pub fn choice(&self) -> (Option<String>, Option<String>, Vec<String>) {
        let choice = locked(&self.choice);
        (
            choice.model.clone(),
            choice.level.clone(),
            choice.levels.clone(),
        )
    }

    /// The mode the calls that follow are decided in.
    #[must_use]
    pub fn mode(&self) -> Mode {
        locked(&self.policy).mode()
    }

    /// Puts the calls that follow in `mode`.
    pub fn set_mode(&self, mode: Mode) {
        locked(&self.policy).set_mode(mode);
    }

    /// Switches the model to `id` (`provider/id`, one of [`PiSession::models`]); the thinking level may change with it.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a model Pi does not offer; [`Error::Harness`] or [`Error::Timeout`] when Pi refused.
    pub fn set_model(&self, id: &str) -> Result<(), Error> {
        let model = locked(&self.choice)
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
            .ok_or_else(|| Error::Refused(format!("{id} is not a model Pi offers")))?;
        self.request(
            "set_model",
            json!({"provider": model.provider, "modelId": model.model}),
            self.settings.request_timeout,
        )?;
        let state = self.request("get_state", json!({}), self.settings.request_timeout)?;
        self.apply_state(&state);
        self.read_levels();
        Ok(())
    }

    /// Sets the thinking level to one of the model's.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a level the model does not take (Pi clamps one silently); [`Error::Harness`] or
    /// [`Error::Timeout`] when Pi refused.
    pub fn set_effort(&self, level: &str) -> Result<(), Error> {
        if !locked(&self.choice)
            .levels
            .iter()
            .any(|known| known == level)
        {
            return Err(Error::Refused(format!(
                "{level} is not a level this model takes"
            )));
        }
        self.request(
            "set_thinking_level",
            json!({"level": level}),
            self.settings.request_timeout,
        )?;
        locked(&self.choice).level = Some(level.to_string());
        Ok(())
    }

    /// Starts a run with a message of the person's: text, images, links and resources. Returns once Pi has taken it.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a resource Pi cannot read; [`Error::Harness`] or [`Error::Timeout`] when Pi did not take the
    /// prompt (with no credential it says so).
    pub fn prompt(&self, content: &[Content]) -> Result<(), Error> {
        let (message, images) = user_input(content)?;
        let mut fields = json!({"message": message});
        if !images.is_empty() {
            fields["images"] = Value::Array(images);
        }
        // Told before the prompt is sent: a run that ends at once must not outrun it.
        if !self.titled.swap(true, Ordering::SeqCst)
            && let Some(title) = content.iter().find_map(|block| match block {
                Content::Text(text) => text.lines().find(|line| !line.trim().is_empty()),
                _ => None,
            })
        {
            (self.sink)(Event::Title(
                lines::cut(title.trim(), self.settings.title_bytes).to_string(),
            ));
        }
        locked(&self.parser).prompting();
        let taken = self
            .request("prompt", fields, self.settings.prompt_timeout)
            .inspect_err(|_| locked(&self.parser).abandon())?;
        if taken.get("disposition").and_then(Value::as_str) == Some("handled") {
            let events = locked(&self.parser).handled();
            for event in events {
                (self.sink)(event);
            }
        }
        Ok(())
    }

    /// Stops the run: the dialogs the person was shown are answered cancelled, since Pi's abort does not end an open one,
    /// then Pi is told to abort (and to stop retrying), and the run ends as cancelled. Nothing when none runs. Pi's
    /// answer is not awaited.
    ///
    /// # Errors
    ///
    /// [`Error::Harness`] when it could not be written.
    pub fn interrupt(&self) -> Result<(), Error> {
        let (running, retrying) = {
            let mut parser = locked(&self.parser);
            let retrying = parser.retrying();
            (parser.interrupting(), retrying)
        };
        if !running {
            return Ok(());
        }
        if retrying {
            self.write(&json!({"id": self.next_id(), "type": "abort_retry"}))?;
        }
        self.write(&json!({"id": self.next_id(), "type": "abort"}))
    }

    /// Ends Pi and everything it started, and waits up to `close_wait` for it to have gone.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.process.kill();
            let _ = exit_text(self.process.as_ref(), self.settings.close_wait);
        }
    }
}

impl Drop for PiSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// A prompt as Pi's `prompt` fields: the text blocks joined, an image as an `images` entry, a link as a mention and a text
/// resource in a `<context>` block. A resource's bytes are taken only for an image.
///
/// # Errors
///
/// [`Error::Refused`] for a resource of bytes that is not an image.
pub fn user_input(content: &[Content]) -> Result<(String, Vec<Value>), Error> {
    let mut parts: Vec<String> = Vec::new();
    let mut images = Vec::new();
    let image = |mime_type: &String, data: &String| json!({"type": "image", "data": data, "mimeType": mime_type});
    for block in content {
        match block {
            Content::Text(said) => parts.push(said.clone()),
            Content::Image { mime_type, data } => images.push(image(mime_type, data)),
            Content::ResourceLink { uri, name } => {
                let name = if name.is_empty() { uri } else { name };
                parts.push(format!("[@{name}]({uri})"));
            }
            Content::Resource {
                uri,
                text: Some(said),
                ..
            } => parts.push(context_block(uri, said)),
            Content::Resource {
                mime_type: Some(mime_type),
                blob: Some(data),
                ..
            } if mime_type.starts_with("image/") => images.push(image(mime_type, data)),
            Content::Resource { .. } => {
                return Err(Error::Refused(
                    "Pi reads a resource's bytes only for an image".into(),
                ));
            }
            _ => return Err(Error::Refused("Pi does not take that content".into())),
        }
    }
    Ok((parts.join("\n\n"), images))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_is_text_joined_and_images_apart() {
        let (message, images) = user_input(&[
            Content::Text("one".into()),
            Content::Image {
                mime_type: "image/png".into(),
                data: "AAA".into(),
            },
            Content::ResourceLink {
                uri: "file:///x".into(),
                name: "x".into(),
            },
        ])
        .unwrap();
        assert_eq!(message, "one\n\n[@x](file:///x)");
        assert_eq!(
            images,
            vec![json!({"type": "image", "data": "AAA", "mimeType": "image/png"})]
        );
    }

    #[test]
    fn bytes_that_are_not_an_image_are_refused() {
        let blob = Content::Resource {
            uri: "file:///x".into(),
            mime_type: Some("application/zip".into()),
            text: None,
            blob: Some("AAA".into()),
        };
        assert!(matches!(user_input(&[blob]), Err(Error::Refused(_))));
    }

    #[test]
    fn the_placeholder_model_is_not_listed() {
        let data = json!({"models": [
            {"id": "unknown", "provider": "unknown", "name": "x"},
            {"id": "m", "provider": "p", "name": "M", "reasoning": true, "input": ["text", "image"], "contextWindow": 1000},
        ]});
        let models = models_of(&data);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "p/m");
        assert!(models[0].reasoning && models[0].images);
    }
}
