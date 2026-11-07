//! A running Claude Code: its process, its stream, and what is written to it.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use std::io::{self, Write};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use coqui_acp_harness::event::{Content, Event, Severity, context_block};
use coqui_acp_harness::lines::{self, Tail};
use coqui_acp_harness::stream::Waiting;
use coqui_acp_harness::{LineHook, Process, Spawn, Step, Stream, exit_text, line_writer, version};
use serde_json::{Value, json};

use crate::launch::ClaudeLaunch;
use crate::parse::{self, Parser, ParserLimits, Writer};
use crate::{Error, TESTED_VERSIONS};

/// The bounds and waits of a session. Every one is a setting with a default, and any value is taken.
#[derive(Clone)]
#[non_exhaustive]
pub struct ClaudeSettings {
    /// The longest line of Claude Code's output read; past it the stream ends. Default 64 MiB.
    pub max_line: u64,
    /// How many of the last lines that were not protocol a failure quotes. Default 8.
    pub tail_lines: usize,
    /// The bytes of each of those lines. Default 512.
    pub tail_line_bytes: usize,
    /// How long Claude Code may take to answer `initialize`. Default 30 s.
    pub initialize_timeout: Duration,
    /// How long it may take to answer a change of model or effort. Default 30 s.
    pub control_timeout: Duration,
    /// How long the end of the output waits for the exit status. Default 1 s.
    pub exit_wait: Duration,
    /// The tool calls kept for their results. Default 4096.
    pub max_tools: usize,
    /// The largest transcript replayed by a load. Default 64 MiB.
    pub max_transcript_bytes: u64,
    /// What the parser keeps and cuts.
    pub parser: ParserLimits,
    /// How often Claude Code is asked whether it has exited while its output stays open. Default 100 ms.
    pub exit_poll: Duration,
    /// How long a close waits for it to have ended, as the caller may remove its directory next. Default 2 s.
    pub close_wait: Duration,
    /// The lines waiting to be written to it; past it a write fails, rather than wait for a process that has stopped
    /// reading. Default 1024.
    pub max_queued_writes: usize,
    /// The bytes of a session's title, from its first prompt. Default 80.
    pub title_bytes: usize,
    /// How long `claude --version` may take; 0 does not ask. Default 10 s.
    pub version_wait: Duration,
    /// How much of its answer is read. Default 4096.
    pub version_bytes: u64,
    /// The host's check of each line Claude Code writes, before anything reads it; `false` ends Claude Code.
    pub on_line: Option<LineHook>,
}

impl fmt::Debug for ClaudeSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeSettings")
            .field("max_line", &self.max_line)
            .field("tail_lines", &self.tail_lines)
            .field("tail_line_bytes", &self.tail_line_bytes)
            .field("initialize_timeout", &self.initialize_timeout)
            .field("control_timeout", &self.control_timeout)
            .field("exit_wait", &self.exit_wait)
            .field("max_tools", &self.max_tools)
            .field("max_transcript_bytes", &self.max_transcript_bytes)
            .field("parser", &self.parser)
            .field("exit_poll", &self.exit_poll)
            .field("close_wait", &self.close_wait)
            .field("max_queued_writes", &self.max_queued_writes)
            .field("title_bytes", &self.title_bytes)
            .field("version_wait", &self.version_wait)
            .field("version_bytes", &self.version_bytes)
            .field("on_line", &self.on_line.is_some())
            .finish()
    }
}

impl ClaudeSettings {
    /// The defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_line: 64 * 1024 * 1024,
            tail_lines: 8,
            tail_line_bytes: 512,
            initialize_timeout: Duration::from_secs(30),
            control_timeout: Duration::from_secs(30),
            exit_wait: Duration::from_secs(1),
            max_tools: 4096,
            max_transcript_bytes: 64 * 1024 * 1024,
            parser: ParserLimits::new(),
            exit_poll: Duration::from_millis(100),
            close_wait: Duration::from_secs(2),
            max_queued_writes: 1024,
            title_bytes: 80,
            version_wait: Duration::from_secs(10),
            version_bytes: 4096,
            on_line: None,
        }
    }
}

impl Default for ClaudeSettings {
    fn default() -> Self {
        Self::new()
    }
}

/// A model Claude Code offers the account, from its answer to `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Model {
    /// The id a launch or `set_model` names, such as `sonnet` or `default`.
    pub id: String,
    /// Its name for people.
    pub name: String,
    /// What it is, with the model it resolves to when that differs from its id.
    pub description: Option<String>,
    /// The efforts it takes; empty when it takes none.
    pub efforts: Vec<String>,
}

/// The models of an `initialize` answer, `None` when it lists none at all, as distinct from an empty list. The answer's
/// `account` is never read.
#[must_use]
pub fn models_of(answer: &Value) -> Option<Vec<Model>> {
    let listed = answer.get("models")?.as_array()?;
    Some(
        listed
            .iter()
            .filter_map(|model| {
                let id = model.get("value").and_then(Value::as_str)?;
                let resolved = model
                    .get("resolvedModel")
                    .and_then(Value::as_str)
                    .filter(|resolved| *resolved != id);
                let description = match (model.get("description").and_then(Value::as_str), resolved)
                {
                    (Some(text), Some(resolved)) => {
                        Some(format!("{text} (resolves to {resolved})"))
                    }
                    (Some(text), None) => Some(text.to_string()),
                    (None, Some(resolved)) => Some(format!("resolves to {resolved}")),
                    (None, None) => None,
                };
                let efforts = if model.get("supportsEffort").and_then(Value::as_bool) == Some(true)
                {
                    model
                        .get("supportedEffortLevels")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                } else {
                    Vec::new()
                };
                Some(Model {
                    id: id.to_string(),
                    name: model
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_string(),
                    description,
                    efforts,
                })
            })
            .collect(),
    )
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running Claude Code in stream-json mode. Its events go to the sink given to [`ClaudeSession::start`], from a
/// thread of its own; every method may be called from any thread.
pub struct ClaudeSession {
    id: Option<String>,
    writer: Writer,
    process: Arc<dyn Process>,
    parser: Arc<Mutex<Parser>>,
    waiting: Waiting,
    ended: Arc<OnceLock<String>>,
    requests: AtomicU64,
    answer: Value,
    settings: ClaudeSettings,
    closed: Arc<AtomicBool>,
    titled: AtomicBool,
    sink: Arc<dyn Fn(Event) -> bool + Send + Sync>,
    /// The MCP configuration's file, removed with the session.
    mcp_file: Option<PrivateFile>,
}

impl fmt::Debug for ClaudeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ClaudeSession {
    /// Starts Claude Code as `launch` says, through `spawn`, and asks it to `initialize`, which it answers with no
    /// model turn: its models, commands and version. Its events go to `sink` until it ends, the last being
    /// [`Event::Exited`]; a sink that answers `false` is not called again. A version Claude Code was not tested with
    /// is an [`Event::Notice`], never a refusal.
    ///
    /// # Errors
    ///
    /// [`Error::Spawn`] when it could not start; [`Error::Timeout`] or [`Error::Harness`] when it did not answer
    /// `initialize`, and it has been ended.
    pub fn start(
        launch: &ClaudeLaunch,
        spawn: &dyn Spawn,
        settings: ClaudeSettings,
        sink: impl Fn(Event) -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let sink: Arc<dyn Fn(Event) -> bool + Send + Sync> = Arc::new(sink);
        let mut launch = launch.clone();
        let mcp_file = if launch.mcp_servers.is_empty() || launch.mcp_config_file.is_some() {
            None
        } else {
            let file =
                PrivateFile::write(&crate::launch::mcp_config(&launch.mcp_servers).to_string())
                    .map_err(Error::Spawn)?;
            launch.mcp_config_file = Some(file.0.clone());
            Some(file)
        };
        let spawned = spawn.spawn(&launch.launch()).map_err(Error::Spawn)?;
        let writer: Writer = line_writer(
            spawned.stdin,
            settings.max_queued_writes,
            "coqui-acp-claude-stdin",
        )
        .map_err(Error::Spawn)?;
        let process: Arc<dyn Process> = Arc::from(spawned.process);
        let parser = Arc::new(Mutex::new(Parser::with_limits(
            Arc::clone(&writer),
            settings.max_tools,
            settings.parser,
        )));
        let tail = Arc::new(Mutex::new(Tail::new(
            settings.tail_lines,
            settings.tail_line_bytes,
        )));
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(stderr) = spawned.stderr {
            let tail = Arc::clone(&tail);
            let max_line = settings.max_line;
            thread::Builder::new()
                .name("coqui-acp-claude-stderr".into())
                .spawn(move || lines::read_diagnostics(stderr, max_line, &tail))
                .map_err(Error::Spawn)?;
        }
        let mut stream = Stream::new(
            "Claude Code",
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
        {
            let reading = Arc::clone(&parser);
            let ending = Arc::clone(&parser);
            let expected = launch.session_id().map(str::to_string);
            stream
                .read(
                    spawned.stdout,
                    "coqui-acp-claude-stdout",
                    move |value| {
                        let (events, answers) = {
                            let mut parser = locked(&reading);
                            (parser.line(value), parser.answers())
                        };
                        let mut told = Vec::with_capacity(events.len());
                        for event in events {
                            let wrong = match (&event, &expected) {
                                (Event::SessionId(reported), Some(expected))
                                    if reported != expected =>
                                {
                                    Some(format!(
                                        "Claude Code reported session {reported}, not the one requested"
                                    ))
                                }
                                _ => None,
                            };
                            told.push(event);
                            // What follows belongs to another session: nothing of it is told, but its id is.
                            if let Some(why) = wrong {
                                return Step::stopping(told, answers, why);
                            }
                        }
                        Step::new(told, answers)
                    },
                    move || {
                        locked(&ending)
                            .end_of_output()
                            .map(Event::TurnEnd)
                            .into_iter()
                            .collect()
                    },
                )
                .map_err(Error::Spawn)?;
        }
        let mut session = Self {
            id: launch.session_id().map(str::to_string),
            writer,
            process,
            parser,
            waiting,
            ended,
            requests: AtomicU64::new(0),
            answer: Value::Null,
            settings,
            closed,
            titled: AtomicBool::new(false),
            sink,
            mcp_file,
        };
        session.answer = match session.control(
            json!({"subtype": "initialize"}),
            session.settings.initialize_timeout,
        ) {
            Ok(answer) => answer,
            Err(error) => {
                session.close();
                return Err(error);
            }
        };
        let commands = parse::commands(&session.answer);
        if !commands.is_empty() {
            (session.sink)(Event::Commands(commands));
        }
        let found = session.version().unwrap_or_default();
        if let Some(warning) = version::untested("Claude Code", &found, TESTED_VERSIONS) {
            (session.sink)(Event::Notice {
                severity: Severity::Warning,
                title: warning,
            });
        }
        Ok(session)
    }

    /// The session's id: the one asked for, or the one Claude Code chose, once it has said it.
    #[must_use]
    pub fn id(&self) -> Option<String> {
        self.id
            .clone()
            .or_else(|| locked(&self.parser).session().map(str::to_string))
    }

    /// Claude Code's version, from its answer to `initialize`.
    #[must_use]
    pub fn version(&self) -> Option<String> {
        self.answer
            .get("claude_code_version")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| locked(&self.parser).version().map(str::to_string))
    }

    /// The models Claude Code offers, from its answer to `initialize`; `None` when it listed none at all.
    #[must_use]
    pub fn models(&self) -> Option<Vec<Model>> {
        models_of(&self.answer)
    }

    /// Sends one control request and waits up to `limit` for its answer.
    fn control(&self, request: Value, limit: Duration) -> Result<Value, Error> {
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("coqui-acp-{number}");
        let (answer, answered) = mpsc::channel();
        locked(&self.waiting).insert(id.clone(), answer);
        if let Some(reason) = self.ended.get() {
            locked(&self.waiting).remove(&id);
            return Err(Error::Harness(reason.clone()));
        }
        if let Err(error) =
            (self.writer)(&json!({"type": "control_request", "request_id": id, "request": request}))
        {
            locked(&self.waiting).remove(&id);
            return Err(Error::Harness(format!(
                "Claude Code could not be written to: {error}"
            )));
        }
        let result = answered.recv_timeout(limit);
        locked(&self.waiting).remove(&id);
        match result {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(refusal)) => Err(Error::Harness(refusal)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout {
                what: "a control request",
                limit,
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Harness(
                "Claude Code ended before it answered".into(),
            )),
        }
    }

    /// Sends a message of the person's: text, images, links and resources.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a resource Claude Code cannot read, [`Error::Harness`] when it could not be written.
    pub fn prompt(&self, content: &[Content]) -> Result<(), Error> {
        let blocks = user_blocks(content)?;
        locked(&self.parser).prompting();
        (self.writer)(&json!({"type": "user", "message": {"role": "user", "content": blocks}}))
            .map_err(|error| {
                Error::Harness(format!("Claude Code could not be written to: {error}"))
            })?;
        if !self.titled.swap(true, Ordering::SeqCst)
            && let Some(title) = content.iter().find_map(|block| match block {
                Content::Text(text) => text.lines().find(|line| !line.trim().is_empty()),
                _ => None,
            })
        {
            (self.sink)(Event::Title(
                coqui_acp_harness::lines::cut(title.trim(), self.settings.title_bytes).to_string(),
            ));
        }
        Ok(())
    }

    /// Interrupts the running turn with an `interrupt` control request, as the Agent SDK does; the turn then ends as
    /// cancelled. Its answer is not awaited.
    ///
    /// # Errors
    ///
    /// [`Error::Harness`] when it could not be written.
    pub fn interrupt(&self) -> Result<(), Error> {
        locked(&self.parser).interrupting();
        let number = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        (self.writer)(
            &json!({"type": "control_request", "request_id": format!("coqui-acp-{number}"),
            "request": {"subtype": "interrupt"}}),
        )
        .map_err(|error| Error::Harness(format!("Claude Code could not be written to: {error}")))
    }

    /// The permission mode Claude Code started in, from its answer to `initialize`.
    #[must_use]
    pub fn permission_mode(&self) -> Option<String> {
        self.answer
            .get("current_permission_mode")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Switches the permission mode (`default`, `acceptEdits`, `plan`, `auto`, `dontAsk` or `bypassPermissions`) for
    /// what follows, a running turn included. `bypassPermissions` only when the session was started allowing it.
    ///
    /// # Errors
    ///
    /// Claude Code refused the mode or did not answer in time.
    pub fn set_permission_mode(&self, mode: &str) -> Result<(), Error> {
        self.control(
            json!({"subtype": "set_permission_mode", "mode": mode}),
            self.settings.control_timeout,
        )
        .map(|_| ())
    }

    /// Changes the model for the turns that follow.
    ///
    /// # Errors
    ///
    /// Claude Code refused or did not answer in time.
    pub fn set_model(&self, model: &str) -> Result<(), Error> {
        self.control(
            json!({"subtype": "set_model", "model": model}),
            self.settings.control_timeout,
        )
        .map(|_| ())
    }

    /// Changes the effort for the turns that follow; `None` returns to Claude Code's own.
    ///
    /// # Errors
    ///
    /// Claude Code refused or did not answer in time.
    pub fn set_effort(&self, effort: Option<&str>) -> Result<(), Error> {
        self.control(
            json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": effort}}),
            self.settings.control_timeout,
        )
        .map(|_| ())
    }

    /// Ends Claude Code and everything it started, and waits up to `close_wait` for it to have gone.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.process.kill();
            let _ = exit_text(self.process.as_ref(), self.settings.close_wait);
        }
        if let Some(file) = &self.mcp_file {
            let _ = std::fs::remove_file(&file.0);
        }
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// A prompt as Claude Code's message content: text, base64 images, a link as a mention, and a text resource in a
/// `<context>` block. A resource's bytes are taken only for an image.
///
/// # Errors
///
/// [`Error::Refused`] for a resource of bytes that is not an image.
pub fn user_blocks(content: &[Content]) -> Result<Vec<Value>, Error> {
    let mut blocks = Vec::new();
    for block in content {
        blocks.push(match block {
            Content::Text(text) => json!({"type": "text", "text": text}),
            Content::Image { mime_type, data } => image(mime_type, data),
            Content::ResourceLink { uri, name } => {
                let name = if name.is_empty() { uri } else { name };
                json!({"type": "text", "text": format!("[@{name}]({uri})")})
            }
            Content::Resource {
                uri,
                text: Some(text),
                ..
            } => {
                json!({"type": "text", "text": context_block(uri, text)})
            }
            Content::Resource {
                mime_type: Some(mime_type),
                blob: Some(data),
                ..
            } if mime_type.starts_with("image/") => image(mime_type, data),
            Content::Resource { .. } => {
                return Err(Error::Refused(
                    "Claude Code reads a resource's bytes only for an image".into(),
                ));
            }
            _ => {
                return Err(Error::Refused(
                    "Claude Code does not take that content".into(),
                ));
            }
        });
    }
    Ok(blocks)
}

/// A file only this user can read, removed when dropped: the MCP configuration, which may hold credentials. On Unix its
/// mode says so; on Windows the file takes the access of the temporary directory, by default this user's own.
#[derive(Debug)]
struct PrivateFile(std::path::PathBuf);

impl PrivateFile {
    fn write(text: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "coqui-acp-claude-mcp-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = Self(path);
        options.open(&file.0)?.write_all(text.as_bytes())?;
        Ok(file)
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn image(mime_type: &str, data: &str) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": mime_type, "data": data}})
}
