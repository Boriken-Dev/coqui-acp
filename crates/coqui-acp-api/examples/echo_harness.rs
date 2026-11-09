//! An ACP agent that answers each prompt with its text, as an `acp_harness` library any Guatiao host
//! can load: the contract agent of `coqui_acp_api::contract`. This file is a `cdylib` and
//! nothing else, as a crate calling `providers!` has to be.
//!
//! It keeps every rule of the channel: `send` waits for room in a queue of a few messages and `close`
//! wakes it; one thread of its own answers, in order, and is the only caller of the sink; a sink
//! that refuses ends the stream. It starts no program, so it lets go of the spawner unused.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{self, FromValue, Provider, Schema, Secret, Status};
use coqui_acp_api::{
    AcpHarness, Channel, ChannelSink, Environment, ExportPage, HarnessInfo, ImportReport,
    ModelInfo, ModelListing, NO_COMMANDS, Narrowing, ProcessSpawner, READ_ONLY, Requirement,
    SeedPoint, SessionExport, SessionImport, StartRequest, WorkingRoots, is_session_id,
};
use serde_json::{Value, json};

/// How many messages wait for the sink before `send` waits too.
const QUEUE: usize = 4;

/// The requirements an echo holds to: it writes nothing and runs nothing.
const HOLDS: [&str; 2] = [READ_ONLY, NO_COMMANDS];

/// The threads this library started, joined when it is unloaded.
static THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// The library's say in being unloaded: busy while a thread of its own still runs.
extern "C" fn threads_done() -> Status {
    let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
    if threads.iter().any(|t| !t.is_finished()) {
        return Status::GUATIAO_ERR_BUSY;
    }
    for thread in threads.drain(..) {
        let _ = thread.join();
    }
    Status::GUATIAO_OK
}

/// How an echo is configured.
#[derive(FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
struct EchoSettings {
    /// Its name for people.
    #[map(default = String::from("Echo"))]
    title: String,
    /// The largest message it takes, in bytes.
    #[map(default = 1 << 20)]
    max_message_bytes: u64,
    /// Whether it imports and exports sessions; when not, it refuses both, saying why.
    #[map(default = true)]
    imports: bool,
}

/// The header of the history the echo writes and reads: the standard format's name and the one
/// version the echo knows. A history is this line, then one line per prompt and per update, each
/// in ACP's own shape.
const HISTORY_FORMAT: &str = "coqui-acp-history";
const HISTORY_VERSION: u64 = 1;

/// The digest a header gives its body: FNV-1a over each line and its line feed, `fnv1a64:` and 16
/// hexadecimal digits. Enough to see a body changed; not a defence against anyone.
fn digest_of<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.flat_map(|line| line.bytes().chain(core::iter::once(b'\n'))) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// A session's history as lines, the header first, carrying the body's digest.
fn history_lines(kept: &Kept) -> Vec<String> {
    let body: Vec<String> = kept.lines.iter().map(Value::to_string).collect();
    let digest = digest_of(body.iter().map(String::as_str));
    let header =
        json!({"format": HISTORY_FORMAT, "version": HISTORY_VERSION, "digest": digest}).to_string();
    core::iter::once(header).chain(body).collect()
}

/// A session the echo keeps: its own id, and what it said, as history lines.
#[derive(Default)]
struct Kept {
    #[allow(
        dead_code,
        reason = "the mapping a real harness resumes by; the echo has nothing to look up by it"
    )]
    own: String,
    lines: Vec<Value>,
}

type Store = Arc<Mutex<BTreeMap<String, Kept>>>;

fn lock(store: &Store) -> MutexGuard<'_, BTreeMap<String, Kept>> {
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The lines of `history` after its header; an empty history has none.
fn read_history(history: &str) -> Result<Vec<Value>, ProviderError> {
    let bad = |why: &str| ProviderError::new(Status::GUATIAO_ERR_BAD_VALUE, why);
    let mut lines = history.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return Ok(Vec::new());
    };
    let header: Value =
        serde_json::from_str(header).map_err(|_| bad("the history's header is not JSON"))?;
    if header["format"] != HISTORY_FORMAT || !header["version"].is_u64() {
        return Err(bad(
            "the history has no header naming its format and version",
        ));
    }
    if header["version"] != HISTORY_VERSION {
        return Err(ProviderError::new(
            Status::GUATIAO_ERR_UNSUPPORTED,
            "the echo reads version 1 of the history format only",
        ));
    }
    let lines: Vec<&str> = lines.collect();
    let given = header["digest"].as_str();
    if given.is_some_and(|digest| digest != digest_of(lines.iter().copied())) {
        return Err(bad("the history's body does not match its header's digest"));
    }
    lines
        .into_iter()
        .map(|line| {
            let line: Value =
                serde_json::from_str(line).map_err(|_| bad("a history line is not JSON"))?;
            match line["method"].as_str() {
                Some("session/prompt" | "session/update") => Ok(line),
                _ => Err(bad("a history line is neither a prompt nor an update")),
            }
        })
        .collect()
}

#[derive(Provider)]
#[provider(
    AcpHarness,
    id = "echo_harness",
    config = EchoSettings,
    new = EchoHarness::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
struct EchoHarness {
    settings: EchoSettings,
    /// Every session it keeps, by the id it reported: a UUID.
    sessions: Store,
    /// The next of its own ids.
    next: Arc<AtomicU64>,
}

impl EchoHarness {
    fn configured(settings: EchoSettings) -> Self {
        Self {
            settings,
            sessions: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    fn unconfigured() -> Self {
        Self::configured(EchoSettings {
            title: "Echo".into(),
            max_message_bytes: 1 << 20,
            imports: true,
        })
    }
}

impl EchoHarness {
    /// Refuses, with the reason, when this echo is configured to keep no session for a host.
    fn keeps_sessions(&self) -> Result<(), ProviderError> {
        if self.settings.imports {
            Ok(())
        } else {
            Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this echo is configured to import and export no session",
            ))
        }
    }
}

impl TryFrom<EchoSettings> for EchoHarness {
    type Error = ProviderError;

    fn try_from(settings: EchoSettings) -> Result<Self, ProviderError> {
        Ok(Self::configured(settings))
    }
}

/// Why the echo would refuse `requirements`, after reading them.
fn refusal_of(requirements: &[Requirement]) -> Result<Option<String>, ProviderError> {
    match Narrowing::read_all(requirements) {
        Ok(_) => Ok(requirements
            .iter()
            .find(|r| !HOLDS.contains(&r.name.as_str()))
            .map(|r| format!("the echo cannot hold `{}`", r.name))),
        Err(error) if error.status() == Status::GUATIAO_ERR_UNSUPPORTED => {
            Ok(Some(error.to_string()))
        }
        Err(error) => Err(error.into()),
    }
}

impl AcpHarness for EchoHarness {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        let mut info = HarnessInfo::new(self.settings.title.clone(), vec![1]);
        info.takes.load = true;
        info.takes.resume = true;
        info.takes.additional_directories = true;
        Ok(info)
    }

    fn start(
        &self,
        request: StartRequest,
        spawner: Object<dyn ProcessSpawner>,
        sink: Object<dyn ChannelSink>,
    ) -> Result<Object<dyn Channel>, ProviderError> {
        // The echo starts nothing.
        drop(spawner);
        if let Some(why) = refusal_of(&request.requirements)? {
            return Err(ProviderError::new(Status::GUATIAO_ERR_UNSUPPORTED, &why));
        }
        let queue = Arc::new(Queue::default());
        let limit = usize::try_from(self.settings.max_message_bytes).unwrap_or(usize::MAX);
        let agent = Agent {
            roots: WorkingRoots::new(request.working_roots),
            sessions: Arc::clone(&self.sessions),
            next: Arc::clone(&self.next),
            open: Vec::new(),
            reads_files: false,
            turns: BTreeMap::new(),
            asked: 0,
        };
        let thread_queue = Arc::clone(&queue);
        let thread = std::thread::Builder::new()
            .name("echo_harness".into())
            .spawn(move || deliver(sink, &thread_queue, agent))
            .map_err(|_| {
                ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "no thread to answer on")
            })?;
        let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
        let (done, running): (Vec<_>, Vec<_>) = threads.drain(..).partition(|t| t.is_finished());
        *threads = running;
        for finished in done {
            let _ = finished.join();
        }
        threads.push(thread);
        Ok(Connection { queue, limit }.into_object())
    }

    fn refusal(
        &self,
        requirements: Vec<Requirement>,
        _environment: Secret<Environment>,
    ) -> Result<Option<String>, ProviderError> {
        refusal_of(&requirements)
    }

    fn program(&self) -> Result<Option<String>, ProviderError> {
        Ok(None)
    }

    fn import_session(
        &self,
        session: SessionImport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ImportReport, ProviderError> {
        drop(spawner);
        self.keeps_sessions()?;
        if !is_session_id(&session.session_id) {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_BAD_VALUE,
                "the session id is not a UUID",
            ));
        }
        let lines = read_history(&session.history)?;
        let mut sessions = lock(&self.sessions);
        if sessions.contains_key(&session.session_id) {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_BAD_VALUE,
                "the id names a session already",
            ));
        }
        let own = format!("echo-{}", self.next.fetch_add(1, Ordering::SeqCst));
        sessions.insert(session.session_id, Kept { own, lines });
        let mut report = ImportReport::new();
        if !session.history.trim().is_empty() {
            report.seeded_from = Some(SeedPoint::line(2));
        }
        Ok(report)
    }

    /// A position is `<line>:<digest>`: the next line to give, and the digest of the body when the
    /// first page was given, so a session that has changed since refuses it.
    fn export_session(
        &self,
        export: SessionExport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ExportPage, ProviderError> {
        drop(spawner);
        self.keeps_sessions()?;
        let sessions = lock(&self.sessions);
        let kept = sessions.get(&export.session_id).ok_or_else(|| {
            ProviderError::new(Status::GUATIAO_ERR_NOT_FOUND, "no session has this id")
        })?;
        let lines = history_lines(kept);
        drop(sessions);
        let digest = digest_of(lines[1..].iter().map(String::as_str));
        let start = match &export.after {
            None => 0,
            Some(after) => after
                .split_once(':')
                .filter(|(_, given)| *given == digest)
                .and_then(|(line, _)| line.parse::<usize>().ok())
                .filter(|line| (1..lines.len()).contains(line))
                .ok_or_else(|| {
                    ProviderError::new(
                        Status::GUATIAO_ERR_BAD_VALUE,
                        "the position is not one the echo gave, or the session has changed since",
                    )
                })?,
        };
        let limit = usize::try_from(export.max_bytes).unwrap_or(usize::MAX);
        let mut text = String::new();
        let mut end = start;
        while let Some(line) = lines.get(end) {
            if end > start && text.len().saturating_add(line.len() + 1) > limit {
                break;
            }
            text.push_str(line);
            text.push('\n');
            end += 1;
        }
        let next = (end < lines.len()).then(|| format!("{end}:{digest}"));
        Ok(ExportPage::new(text, next))
    }

    fn models(&self, _environment: Secret<Environment>) -> Result<ModelListing, ProviderError> {
        Ok(ModelListing::of(vec![ModelInfo::new("echo")]))
    }
}

#[derive(Default)]
struct Inner {
    items: VecDeque<Vec<u8>>,
    /// Either side has ended the connection: `send` answers GONE from here on.
    closed: bool,
    /// The status it ends with, and why, once one side has given them.
    status: Option<(u32, String)>,
}

#[derive(Default)]
struct Queue {
    inner: Mutex<Inner>,
    /// A message was taken or the connection ended: `send` waits on it.
    space: Condvar,
    /// A message arrived or the connection ended: the thread waits on it.
    arrived: Condvar,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the connection with `status` and why, unless it has ended, dropping what is still
    /// queued.
    fn end(&self, status: u32, reason: &str) {
        let mut inner = self.lock();
        inner.closed = true;
        inner
            .status
            .get_or_insert_with(|| (status, reason.to_owned()));
        inner.items.clear();
        drop(inner);
        self.space.notify_all();
        self.arrived.notify_all();
    }
}

/// The host's end of one connection.
struct Connection {
    queue: Arc<Queue>,
    limit: usize,
}

impl Channel for Connection {
    fn send(&self, bytes: &[u8]) -> Result<(), ProviderError> {
        if bytes.len() > self.limit {
            const WHY: &str = "the message is larger than the echo takes";
            self.queue.end(Status::GUATIAO_ERR_BAD_VALUE.into(), WHY);
            return Err(ProviderError::new(Status::GUATIAO_ERR_BAD_VALUE, WHY));
        }
        let mut inner = self.queue.lock();
        loop {
            if inner.closed {
                return Err(ProviderError::new(
                    Status::GUATIAO_ERR_GONE,
                    "the connection is closed",
                ));
            }
            if inner.items.len() < QUEUE {
                break;
            }
            inner = self
                .queue
                .space
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        inner.items.push_back(bytes.to_vec());
        drop(inner);
        self.queue.arrived.notify_one();
        Ok(())
    }

    fn close(&self, status: u32, reason: &str) {
        let mut inner = self.queue.lock();
        if inner.closed {
            return;
        }
        inner.closed = true;
        inner.status = Some((status, reason.to_owned()));
        drop(inner);
        self.queue.space.notify_all();
        self.queue.arrived.notify_all();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close(Status::GUATIAO_OK.into(), "");
    }
}

/// The connection's thread, and the only caller of the sink.
fn deliver(mut sink: Object<dyn ChannelSink>, queue: &Queue, mut agent: Agent) {
    let send = |sink: &mut Object<dyn ChannelSink>, answers: Vec<Value>| {
        for answer in answers {
            sink.receive(answer.to_string().as_bytes())
                .map_err(|refused| (u32::from(refused.status), refused.message().to_owned()))?;
        }
        Ok::<(), (u32, String)>(())
    };
    let status = loop {
        let message = {
            let mut inner = queue.lock();
            loop {
                if let Some(message) = inner.items.pop_front() {
                    queue.space.notify_all();
                    break Some(message);
                }
                if inner.closed {
                    break None;
                }
                inner = queue
                    .arrived
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(message) = message else {
            // The host closed: every turn still running ends, answered, before `closed`, which
            // passes the host's reason on.
            let status = queue.lock().status.clone().unwrap_or((0, String::new()));
            match send(&mut sink, agent.end_every_turn()) {
                Ok(()) => break status,
                Err(refused) => break refused,
            }
        };
        let (answers, then_close) = agent.answer(&message);
        if let Err(refused) = send(&mut sink, answers) {
            queue.end(refused.0, &refused.1);
            break refused;
        }
        if then_close {
            queue.end(0, "");
            let _ = send(&mut sink, agent.end_every_turn());
            break (0, String::new());
        }
    };
    sink.closed(status.0, &status.1);
}

/// A turn waiting on something: a cancel, or the client's answer to the agent's own request.
enum Turn {
    /// Ends only on `session/cancel`.
    Waiting { prompt: Value },
    /// Waits for the answer to `session/request_permission`.
    Asking { prompt: Value, request: String },
    /// Waits for the answer to `fs/read_text_file`.
    Reading { prompt: Value, request: String },
}

impl Turn {
    fn prompt(&self) -> &Value {
        match self {
            Self::Waiting { prompt }
            | Self::Asking { prompt, .. }
            | Self::Reading { prompt, .. } => prompt,
        }
    }

    fn request(&self) -> Option<&str> {
        match self {
            Self::Waiting { .. } => None,
            Self::Asking { request, .. } | Self::Reading { request, .. } => Some(request),
        }
    }
}

/// The ACP side of one connection: what each message from the client is answered with.
struct Agent {
    roots: WorkingRoots,
    sessions: Store,
    next: Arc<AtomicU64>,
    /// The sessions open on this connection, by the id reported.
    open: Vec<String>,
    /// Whether the client offered `fs/read_text_file`.
    reads_files: bool,
    /// The turn running in each session, by its id.
    turns: BTreeMap<String, Turn>,
    /// How many requests the agent has made of the client.
    asked: u64,
}

impl Agent {
    /// The messages to send back, and whether the connection ends after them.
    fn answer(&mut self, bytes: &[u8]) -> (Vec<Value>, bool) {
        let Ok(message) = serde_json::from_slice::<Value>(bytes) else {
            return (vec![error(&Value::Null, -32700, "not JSON")], false);
        };
        let Some(object) = message.as_object() else {
            return (
                vec![error(&Value::Null, -32600, "not one JSON-RPC message")],
                false,
            );
        };
        let id = object.get("id");
        let Some(method) = object.get("method") else {
            return (self.answered(&message), false);
        };
        let Some(method) = method.as_str() else {
            return (
                vec![error(
                    id.unwrap_or(&Value::Null),
                    -32600,
                    "the method is not text",
                )],
                false,
            );
        };
        let params = object.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = id else {
            return (self.notified(method, &params), false);
        };
        if !params.is_object() {
            return (
                vec![error(id, -32602, "the params are not an object")],
                false,
            );
        }
        match self.requested(method, id, &params) {
            Ok(answers) => answers,
            Err(why) => (vec![error(id, -32602, &why)], false),
        }
    }

    fn requested(
        &mut self,
        method: &str,
        id: &Value,
        params: &Value,
    ) -> Result<(Vec<Value>, bool), String> {
        let text = |key: &str| {
            params[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("`{key}` is not text"))
        };
        Ok(match method {
            "initialize" => {
                self.reads_files = params["clientCapabilities"]["fs"]["readTextFile"] == true;
                let answer = json!({
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": true,
                        "promptCapabilities": {},
                        "mcpCapabilities": {"http": false, "sse": false},
                    },
                    "authMethods": [],
                    "agentInfo": {"name": "echo_harness", "title": "Echo", "version": "1.0.0"},
                });
                (vec![result(id, answer)], false)
            }
            "session/new" => {
                self.admit(&text("cwd")?, params)?;
                let session = uuid::Uuid::new_v4().to_string();
                let own = format!("echo-{}", self.next.fetch_add(1, Ordering::SeqCst));
                lock(&self.sessions).insert(
                    session.clone(),
                    Kept {
                        own,
                        lines: Vec::new(),
                    },
                );
                self.open.push(session.clone());
                (vec![result(id, json!({"sessionId": session}))], false)
            }
            "session/load" | "session/resume" => {
                self.admit(&text("cwd")?, params)?;
                let session = text("sessionId")?;
                let sessions = lock(&self.sessions);
                let kept = sessions.get(&session).ok_or("no session has this id")?;
                // `session/load` replays what the session said; `session/resume` does not.
                let mut answers: Vec<Value> = if method == "session/load" {
                    kept.lines
                        .iter()
                        .filter(|line| line["method"] == "session/update")
                        .map(|line| {
                            json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": {"sessionId": session, "update": line["params"]["update"]},
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                drop(sessions);
                self.open.push(session);
                answers.push(result(id, json!({})));
                (answers, false)
            }
            "session/prompt" => {
                let session = text("sessionId")?;
                let blocks = params["prompt"]
                    .as_array()
                    .ok_or("`prompt` is not a list")?;
                if !self.open.contains(&session) {
                    return Err("no session with this id is open here".into());
                }
                let said: String = blocks.iter().filter_map(|b| b["text"].as_str()).collect();
                self.keep(
                    &session,
                    json!({"method": "session/prompt", "params": {"prompt": blocks}}),
                );
                self.prompted(id, &session, &said)
            }
            _ => (vec![error(id, -32601, "method not found")], false),
        })
    }

    /// Refuses a session whose directories are not all inside the working roots.
    fn admit(&self, cwd: &str, params: &Value) -> Result<(), String> {
        let mut directories = vec![cwd.to_owned()];
        if let Some(more) = params.get("additionalDirectories") {
            let more = more
                .as_array()
                .ok_or("`additionalDirectories` is not a list")?;
            for dir in more {
                directories.push(dir.as_str().ok_or("a directory is not text")?.to_owned());
            }
        }
        for dir in &directories {
            self.roots.admits(dir).map_err(|why| why.to_string())?;
        }
        Ok(())
    }

    fn prompted(&mut self, id: &Value, session: &str, said: &str) -> (Vec<Value>, bool) {
        if said == "wait" {
            self.turns
                .insert(session.into(), Turn::Waiting { prompt: id.clone() });
            return (Vec::new(), false);
        }
        if said == "ask" {
            self.asked += 1;
            let request = format!("echo-ask-{}", self.asked);
            let asked = json!({
                "jsonrpc": "2.0",
                "id": request,
                "method": "session/request_permission",
                "params": {
                    "sessionId": session,
                    "toolCall": {"toolCallId": request, "title": "Echo asks"},
                    "options": [
                        {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                    ],
                },
            });
            self.turns.insert(
                session.into(),
                Turn::Asking {
                    prompt: id.clone(),
                    request,
                },
            );
            return (vec![asked], false);
        }
        if let Some(path) = said.strip_prefix("read ") {
            if !self.reads_files {
                return (
                    self.finish(session, id, Some("echo: no fs"), "end_turn"),
                    false,
                );
            }
            self.asked += 1;
            let request = format!("echo-read-{}", self.asked);
            let asked = json!({
                "jsonrpc": "2.0",
                "id": request,
                "method": "fs/read_text_file",
                "params": {"sessionId": session, "path": path},
            });
            self.turns.insert(
                session.into(),
                Turn::Reading {
                    prompt: id.clone(),
                    request,
                },
            );
            return (vec![asked], false);
        }
        (
            self.finish(session, id, Some(&format!("echo: {said}")), "end_turn"),
            said == "close",
        )
    }

    fn notified(&mut self, method: &str, params: &Value) -> Vec<Value> {
        if method != "session/cancel" {
            return Vec::new();
        }
        let Some(session) = params["sessionId"].as_str() else {
            return Vec::new();
        };
        match self.turns.remove(session) {
            Some(turn) => self.finish(session, turn.prompt(), None, "cancelled"),
            None => Vec::new(),
        }
    }

    /// The client's answer to one of the agent's own requests.
    fn answered(&mut self, message: &Value) -> Vec<Value> {
        let Some(id) = message["id"].as_str() else {
            return Vec::new();
        };
        let Some(session) = self
            .turns
            .iter()
            .find(|(_, turn)| turn.request() == Some(id))
            .map(|(session, _)| session.clone())
        else {
            return Vec::new();
        };
        let Some(turn) = self.turns.remove(&session) else {
            return Vec::new();
        };
        let outcome = &message["result"]["outcome"];
        match turn {
            Turn::Asking { prompt, .. } if outcome["outcome"] == "cancelled" => {
                self.finish(&session, &prompt, None, "cancelled")
            }
            Turn::Asking { prompt, .. } => {
                let allowed = outcome["outcome"] == "selected" && outcome["optionId"] == "allow";
                let said = if allowed {
                    "echo: allowed"
                } else {
                    "echo: refused"
                };
                self.finish(&session, &prompt, Some(said), "end_turn")
            }
            Turn::Reading { prompt, .. } => {
                let said = message["result"]["content"]
                    .as_str()
                    .map_or_else(|| "echo: refused".to_owned(), |c| format!("echo: {c}"));
                self.finish(&session, &prompt, Some(&said), "end_turn")
            }
            Turn::Waiting { .. } => Vec::new(),
        }
    }

    /// Ends every turn still running, as cancelled: the connection is ending.
    fn end_every_turn(&mut self) -> Vec<Value> {
        core::mem::take(&mut self.turns)
            .into_iter()
            .flat_map(|(session, turn)| self.finish(&session, turn.prompt(), None, "cancelled"))
            .collect()
    }

    /// A turn's end, with what the agent said kept in the session's history.
    fn finish(&self, session: &str, prompt: &Value, said: Option<&str>, stop: &str) -> Vec<Value> {
        let messages = finish(session, prompt, said, stop);
        for message in &messages {
            if message["method"] == "session/update" {
                self.keep(
                    session,
                    json!({"method": "session/update", "params": {"update": message["params"]["update"]}}),
                );
            }
        }
        messages
    }

    /// Adds one line to the history of `session`, when the echo keeps it.
    fn keep(&self, session: &str, line: Value) {
        if let Some(kept) = lock(&self.sessions).get_mut(session) {
            kept.lines.push(line);
        }
    }
}

/// A turn's end: what the agent says, if anything, then the prompt's answer.
fn finish(session: &str, prompt: &Value, said: Option<&str>, stop: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(said) = said {
        messages.push(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": said},
                },
            },
        }));
    }
    messages.push(result(prompt, json!({"stopReason": stop})));
    messages
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

guatiao::providers!(
    id = "echo_harness",
    version = env!("CARGO_PKG_VERSION"),
    providers = [EchoHarness],
    unload = threads_done
);
