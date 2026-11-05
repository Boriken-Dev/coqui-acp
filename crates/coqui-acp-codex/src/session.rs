//! A running Codex app server: its process, its JSON-RPC stream, and one thread on it.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use std::io::{self, Write};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;

use coqui_acp_harness::event::{Content, Event, Severity, context_block};
use coqui_acp_harness::lines::{self, Line, Tail};
use coqui_acp_harness::{Process, Spawn, exit_text, version};
use serde_json::{Value, json};

use crate::launch::{CodexLaunch, ThreadStart};
use crate::parse::{Parser, Writer, id_text};
use crate::{Error, TESTED_VERSIONS};

/// The bounds and waits of a session. Every one is a setting with a default, and any value is taken.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodexSettings {
    /// The longest line of the app server's output read; past it the session ends. Default 64 MiB.
    pub max_line: u64,
    /// How many of the last lines that were not protocol a failure quotes. Default 8.
    pub tail_lines: usize,
    /// The bytes of each of those lines. Default 512.
    pub tail_line_bytes: usize,
    /// How long the app server may take to answer `initialize`. Default 20 s.
    pub initialize_timeout: Duration,
    /// How long it may take to answer any other request: `thread/start`, `thread/resume`, `turn/start`,
    /// `model/list`. Default 30 s.
    pub request_timeout: Duration,
    /// How long the end of the output waits for the exit status. Default 1 s.
    pub exit_wait: Duration,
    /// The items kept for their deltas and results. Default 4096.
    pub max_tools: usize,
    /// The pages of `model/list` asked for. Default 10.
    pub max_model_pages: usize,
    /// The earlier turns a load replays, the latest kept. Default 1000.
    pub max_history_turns: usize,
}

impl CodexSettings {
    /// The defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_line: 64 * 1024 * 1024,
            tail_lines: 8,
            tail_line_bytes: 512,
            initialize_timeout: Duration::from_secs(20),
            request_timeout: Duration::from_secs(30),
            exit_wait: Duration::from_secs(1),
            max_tools: 4096,
            max_model_pages: 10,
            max_history_turns: 1000,
        }
    }
}

impl Default for CodexSettings {
    fn default() -> Self {
        Self::new()
    }
}

/// A model Codex offers, from `model/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Model {
    /// The id a turn names.
    pub id: String,
    /// Its name for people.
    pub name: String,
    /// What it is.
    pub description: Option<String>,
    /// The reasoning efforts it takes.
    pub efforts: Vec<String>,
    /// Its own effort when none is named.
    pub default_effort: Option<String>,
    /// Whether Codex uses it when none is named.
    pub is_default: bool,
}

/// The models of a `model/list` result, those Codex hides left out.
#[must_use]
pub fn models_of(result: &Value) -> Vec<Model> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| model.get("hidden") != Some(&Value::Bool(true)))
        .filter_map(|model| {
            let id = model
                .get("model")
                .and_then(Value::as_str)
                .or_else(|| model.get("id").and_then(Value::as_str))?;
            Some(Model {
                id: id.to_string(),
                name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                description: model
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                efforts: model
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|effort| {
                        effort
                            .get("reasoningEffort")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect(),
                default_effort: model
                    .get("defaultReasoningEffort")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                is_default: model.get("isDefault").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect()
}

/// Codex's version in its `initialize` answer's `userAgent` (`<client>/<version> (…)`).
#[must_use]
pub fn version_of(answer: &Value) -> Option<String> {
    let agent = answer.get("userAgent")?.as_str()?;
    let (_, rest) = agent.split_once('/')?;
    Some(rest.split_whitespace().next()?.to_string())
}

type Waiting = Arc<Mutex<BTreeMap<String, mpsc::Sender<Result<Value, String>>>>>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running Codex app server with one thread open. Its events go to the sink given to [`CodexSession::start`], from
/// a thread of its own; every method may be called from any thread.
pub struct CodexSession {
    thread: String,
    writer: Writer,
    process: Arc<dyn Process>,
    parser: Arc<Mutex<Parser>>,
    waiting: Waiting,
    requests: AtomicU64,
    settings: CodexSettings,
    closed: Arc<AtomicBool>,
    version: Option<String>,
    models: Vec<Model>,
    choice: Mutex<(Option<String>, Option<String>)>,
    titled: AtomicBool,
    sink: Arc<dyn Fn(Event) -> bool + Send + Sync>,
}

impl fmt::Debug for CodexSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexSession")
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

impl CodexSession {
    /// Starts `codex app-server` as `launch` says, through `spawn`; initializes it; lists its models; sets the skill
    /// roots of `launch`'s directories; and starts or resumes its thread, checking that a resumed thread is the one
    /// asked for. For [`ThreadStart::Load`] the thread's earlier turns go to `sink` as history first. A version Codex
    /// was not tested with is an [`Event::Notice`], never a refusal. Events reach `sink` until it answers `false`;
    /// the output's end is one [`Event::Exited`].
    ///
    /// # Errors
    ///
    /// [`Error::Spawn`] when it could not start; [`Error::Timeout`] or [`Error::Harness`] when a request failed, and
    /// the app server has been ended.
    pub fn start(
        launch: &CodexLaunch,
        spawn: &dyn Spawn,
        settings: CodexSettings,
        sink: impl Fn(Event) -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let sink: Arc<dyn Fn(Event) -> bool + Send + Sync> = Arc::new(sink);
        let spawned = spawn.spawn(&launch.launch()).map_err(Error::Spawn)?;
        let stdin = Arc::new(Mutex::new(spawned.stdin));
        let writer: Writer = Arc::new(move |value: &Value| {
            let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
            line.push(b'\n');
            let mut stdin = locked(&stdin);
            // One write per line, so a line written from another thread cannot land inside it.
            stdin.write_all(&line)?;
            stdin.flush()
        });
        let process: Arc<dyn Process> = Arc::from(spawned.process);
        let parser = Arc::new(Mutex::new(Parser::new(
            Arc::clone(&writer),
            settings.max_tools,
        )));
        let waiting: Waiting = Arc::new(Mutex::new(BTreeMap::new()));
        let tail = Arc::new(Mutex::new(Tail::new(
            settings.tail_lines,
            settings.tail_line_bytes,
        )));
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(stderr) = spawned.stderr {
            let tail = Arc::clone(&tail);
            let max_line = settings.max_line;
            thread::Builder::new()
                .name("coqui-acp-codex-stderr".into())
                .spawn(move || lines::read_diagnostics(stderr, max_line, &tail))
                .map_err(Error::Spawn)?;
        }
        {
            let parser = Arc::clone(&parser);
            let waiting = Arc::clone(&waiting);
            let process = Arc::clone(&process);
            let closed = Arc::clone(&closed);
            let sink = Arc::clone(&sink);
            let stdout = spawned.stdout;
            let max_line = settings.max_line;
            let exit_wait = settings.exit_wait;
            thread::Builder::new()
                .name("coqui-acp-codex-stdout".into())
                .spawn(move || {
                    let mut open = true;
                    let read = lines::read_lines(stdout, max_line, |line| {
                        match line {
                            Line::Json(value) => {
                                let (events, answers) = {
                                    let mut parser = locked(&parser);
                                    (parser.line(&value), parser.answers())
                                };
                                for (id, answer) in answers {
                                    if let Some(waiter) = locked(&waiting).remove(&id) {
                                        let _ = waiter.send(answer);
                                    }
                                }
                                for event in events {
                                    open = open && sink(event);
                                }
                            }
                            Line::Text(text) => {
                                locked(&tail).push(&text);
                                open = open && sink(Event::Output(text));
                            }
                            Line::TooLong(bytes) => {
                                locked(&tail).push(&format!(
                                    "a line of {bytes} bytes or more, past the limit"
                                ));
                                let _ = process.kill();
                            }
                        }
                        open
                    });
                    let how = exit_text(process.as_ref(), exit_wait);
                    let mut reason =
                        format!("Codex app-server {how} before completing the session");
                    if let Err(error) = read {
                        reason.push_str(&format!(" (its output could not be read: {error})"));
                    }
                    let tail = locked(&tail);
                    if !tail.is_empty() {
                        reason.push_str("; its last output:\n");
                        reason.push_str(&tail.text());
                    }
                    drop(tail);
                    for (_, waiter) in core::mem::take(&mut *locked(&waiting)) {
                        let _ = waiter.send(Err(reason.clone()));
                    }
                    if closed.load(Ordering::SeqCst) || !open {
                        return;
                    }
                    sink(Event::Exited(reason));
                })
                .map_err(Error::Spawn)?;
        }
        let mut session = Self {
            thread: String::new(),
            writer,
            process,
            parser,
            waiting,
            requests: AtomicU64::new(0),
            settings,
            closed,
            version: None,
            models: Vec::new(),
            choice: Mutex::new((launch.model.clone(), launch.effort.clone())),
            titled: AtomicBool::new(false),
            sink,
        };
        if let Err(error) = session.open(launch) {
            session.close();
            return Err(error);
        }
        Ok(session)
    }

    fn open(&mut self, launch: &CodexLaunch) -> Result<(), Error> {
        let initialized = self.request(
            "initialize",
            json!({"clientInfo": {"name": "coqui-acp-codex", "title": "coqui-acp-codex", "version": env!("CARGO_PKG_VERSION")},
                   "capabilities": {}}),
            self.settings.initialize_timeout,
        )?;
        self.version = version_of(&initialized);
        self.notify("initialized", json!({}))?;
        let found = self.version.clone().unwrap_or_default();
        if let Some(warning) = version::untested("Codex", &found, TESTED_VERSIONS) {
            (self.sink)(Event::Notice {
                severity: Severity::Warning,
                title: warning,
            });
        }
        let mut cursor: Option<String> = None;
        for _ in 0..self.settings.max_model_pages {
            let mut params = json!({});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let page = self.request("model/list", params, self.settings.request_timeout)?;
            self.models.extend(models_of(&page));
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|next| !next.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        let roots = launch.skill_roots();
        if !roots.is_empty() {
            self.request(
                "skills/extraRoots/set",
                json!({"extraRoots": roots}),
                self.settings.request_timeout,
            )?;
        }
        let (method, wanted) = match &launch.thread {
            ThreadStart::New => ("thread/start", None),
            ThreadStart::Resume(id) | ThreadStart::Load(id) => ("thread/resume", Some(id.clone())),
        };
        let opened = self.request(
            method,
            launch.thread_params(),
            self.settings.request_timeout,
        )?;
        let thread = opened
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Harness("Codex named no thread".into()))?;
        if let Some(wanted) = wanted
            && wanted != thread
        {
            return Err(Error::Harness(format!(
                "Codex resumed thread {thread}, not the one requested"
            )));
        }
        self.thread = thread.to_string();
        if matches!(launch.thread, ThreadStart::Load(_)) {
            let turns = opened
                .pointer("/thread/turns")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let skip = turns.len().saturating_sub(self.settings.max_history_turns);
            let writer: Writer =
                Arc::new(|_| Err(io::Error::other("a replayed turn asks nothing")));
            let mut history = Parser::new(writer, self.settings.max_tools);
            if skip > 0 {
                (self.sink)(Event::Notice {
                    severity: Severity::Warning,
                    title: format!(
                        "Only the last {} turns of this thread were replayed",
                        self.settings.max_history_turns
                    ),
                });
            }
            for turn in turns.iter().skip(skip) {
                for event in history.history(turn) {
                    (self.sink)(event);
                }
            }
        }
        Ok(())
    }

    fn write(&self, message: &Value) -> Result<(), Error> {
        (self.writer)(message)
            .map_err(|error| Error::Harness(format!("Codex could not be written to: {error}")))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        self.write(&json!({"method": method, "params": params}))
    }

    /// Sends one request and waits up to `limit` for its answer.
    fn request(&self, method: &str, params: Value, limit: Duration) -> Result<Value, Error> {
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let id = id_text(&json!(number));
        let (answer, answered) = mpsc::channel();
        locked(&self.waiting).insert(id.clone(), answer);
        if let Err(error) = self.write(&json!({"id": number, "method": method, "params": params})) {
            locked(&self.waiting).remove(&id);
            return Err(error);
        }
        let result = answered.recv_timeout(limit);
        locked(&self.waiting).remove(&id);
        match result {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(refusal)) => Err(Error::Harness(format!("Codex refused {method}: {refusal}"))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout {
                what: "a request",
                limit,
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::Harness("Codex ended before it answered".into()))
            }
        }
    }

    /// The thread's id.
    #[must_use]
    pub fn thread(&self) -> &str {
        &self.thread
    }

    /// Codex's version, from its answer to `initialize`.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The models Codex offers.
    #[must_use]
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// The model and effort the next turn names, when chosen.
    #[must_use]
    pub fn choice(&self) -> (Option<String>, Option<String>) {
        locked(&self.choice).clone()
    }

    /// Sets the model the turns that follow name; `None` leaves Codex's own.
    pub fn set_model(&self, model: Option<&str>) {
        locked(&self.choice).0 = model.map(str::to_string);
    }

    /// Sets the reasoning effort the turns that follow name; `None` leaves the model's own.
    pub fn set_effort(&self, effort: Option<&str>) {
        locked(&self.choice).1 = effort.map(str::to_string);
    }

    /// Starts a turn with a message of the person's: text, images, links and resources, with the model and effort
    /// chosen. Returns once Codex has taken it.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a resource Codex cannot read; [`Error::Harness`] or [`Error::Timeout`] when Codex did
    /// not take the turn.
    pub fn prompt(&self, content: &[Content]) -> Result<(), Error> {
        let input = user_input(content)?;
        let (model, effort) = self.choice();
        let mut params = json!({"threadId": self.thread, "input": input});
        if let Some(model) = model {
            params["model"] = json!(model);
        }
        if let Some(effort) = effort {
            params["effort"] = json!(effort);
        }
        let started = self.request("turn/start", params, self.settings.request_timeout)?;
        if let Some(turn) = started.pointer("/turn/id").and_then(Value::as_str) {
            let mut parser = locked(&self.parser);
            if parser.turn().is_none() {
                parser.started(turn);
            }
        }
        if !self.titled.swap(true, Ordering::SeqCst)
            && let Some(title) = content.iter().find_map(|block| match block {
                Content::Text(text) => text.lines().find(|line| !line.trim().is_empty()),
                _ => None,
            })
        {
            (self.sink)(Event::Title(
                coqui_acp_harness::lines::cut(title.trim(), 80).to_string(),
            ));
        }
        Ok(())
    }

    /// Interrupts the running turn (`turn/interrupt`), which then ends as cancelled; nothing when none runs. Its answer
    /// is not awaited.
    ///
    /// # Errors
    ///
    /// [`Error::Harness`] when it could not be written.
    pub fn interrupt(&self) -> Result<(), Error> {
        let turn = {
            let mut parser = locked(&self.parser);
            let Some(turn) = parser.turn().map(str::to_string) else {
                return Ok(());
            };
            parser.interrupting();
            turn
        };
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        self.write(&json!({"id": number, "method": "turn/interrupt", "params": {"threadId": self.thread, "turnId": turn}}))
    }

    /// Ends the app server and everything it started.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.process.kill();
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// A prompt as Codex's turn input: text, an image as a data URL, a link as a mention, and a text resource in a
/// `<context>` block. A resource's bytes are taken only for an image.
///
/// # Errors
///
/// [`Error::Refused`] for a resource of bytes that is not an image.
pub fn user_input(content: &[Content]) -> Result<Vec<Value>, Error> {
    let text = |text: String| json!({"type": "text", "text": text, "text_elements": []});
    let mut input = Vec::new();
    for block in content {
        input.push(match block {
            Content::Text(said) => text(said.clone()),
            Content::Image { mime_type, data } => {
                json!({"type": "image", "url": format!("data:{mime_type};base64,{data}")})
            }
            Content::ResourceLink { uri, name } => {
                let name = if name.is_empty() { uri } else { name };
                text(format!("[@{name}]({uri})"))
            }
            Content::Resource {
                uri,
                text: Some(said),
                ..
            } => text(context_block(uri, said)),
            Content::Resource {
                mime_type: Some(mime_type),
                blob: Some(data),
                ..
            } if mime_type.starts_with("image/") => {
                json!({"type": "image", "url": format!("data:{mime_type};base64,{data}")})
            }
            Content::Resource { .. } => {
                return Err(Error::Refused(
                    "Codex reads a resource's bytes only for an image".into(),
                ));
            }
            _ => return Err(Error::Refused("Codex does not take that content".into())),
        });
    }
    Ok(input)
}
