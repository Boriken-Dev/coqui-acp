//! An ACP agent that answers each prompt with its text, as an `acp_harness` library any Guatiao host
//! can load: the contract agent of `coqui_acp_api::contract`. This file is a `cdylib` and
//! nothing else, as a crate calling `providers!` has to be.
//!
//! It keeps every rule of the channel: `send` waits for room in a queue of a few messages and `close`
//! wakes it; one thread of its own answers, in order, and is the only caller of the sink; a sink
//! that refuses ends the stream. It starts no program, so it lets go of the spawner unused.
//!
//! With the setting `usage = "scripted"` it also reports usage, as the contract documents: `quota P`
//! reports two buckets, `limit` refuses the turn with a limit hit, and every turn spends fixed tokens.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{self, FromValue, Provider, Schema, Secret, Status, ToValue};
use coqui_acp_api::usage::{
    Bucket, Coverage, LIMIT_META_KEY, LimitHit, Refill, Source, Spend, Status as Level,
    USAGE_CAPABILITY, USAGE_METHOD, USAGE_NOTIFICATION, USAGE_VERSION, Unit, UsageQuery,
    UsageReport, UsageSupport,
};
use coqui_acp_api::{
    AcpHarness, Channel, ChannelSink, Environment, ExportPage, HarnessInfo, ImportReport,
    LINE_TOO_LARGE, ModelInfo, ModelListing, NO_COMMANDS, Narrowing, ProcessSpawner, READ_ONLY,
    Requirement, SeedPoint, SessionExport, SessionImport, StartRequest, WorkingRoots,
    is_session_id,
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

/// Whether an echo reports usage.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[map(crate = ::coqui_acp_api::guatiao, rename_all = "snake_case")]
enum EchoUsage {
    /// It reports none: the extension is not advertised and `usage` says so.
    #[default]
    None,
    /// It reports what its prompts script.
    Scripted,
}

/// How an echo is configured.
#[derive(FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
struct EchoSettings {
    /// Whether it reports usage.
    #[map(default)]
    usage: EchoUsage,
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

/// The newest report any connection of an instance made, by the account it ran on (`""` for none).
type Reports = Arc<Mutex<BTreeMap<String, UsageReport>>>;

/// Unix seconds now.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0)
}

/// What a scripted echo reports.
const SOURCES: [Source; 1] = [Source::Reported];

/// The tokens every turn of a scripted echo spends: input, output, cache read, cache write.
const SPENT: [u64; 4] = [100, 40, 20, 10];

/// The usage of one connection of a scripted echo.
struct UsageState {
    report: UsageReport,
    spends: BTreeMap<String, Spend>,
    /// Whether the client asked for `_coqui_acp/usage_update`.
    notify: bool,
    reports: Reports,
}

impl UsageState {
    fn new(account: Option<String>, reports: Reports) -> Self {
        let mut report = UsageReport::new();
        report.sources = SOURCES.to_vec();
        report.account = account;
        Self {
            report,
            spends: BTreeMap::new(),
            notify: false,
            reports,
        }
    }

    /// The report, with `session`'s spend when one is named.
    fn snapshot(&self, session: Option<&str>) -> UsageReport {
        let mut report = self.report.clone();
        report.spend = session.and_then(|session| self.spends.get(session).cloned());
        report
    }

    /// Something changed: a new revision, kept for the kind's `usage`, and the notification when
    /// the client asked.
    fn changed(&mut self, session: Option<&str>) -> Vec<Value> {
        self.report.revision += 1;
        self.report.observed_at = self
            .report
            .buckets
            .iter()
            .map(|bucket| bucket.observed_at)
            .max()
            .unwrap_or(0);
        let key = self.report.account.clone().unwrap_or_default();
        self.reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, self.report.clone());
        if !self.notify {
            return Vec::new();
        }
        let report = serde_json::to_value(self.snapshot(session)).expect("a report is JSON");
        vec![json!({
            "jsonrpc": "2.0",
            "method": USAGE_NOTIFICATION,
            "params": {"sessionId": session, "report": report},
        })]
    }

    /// `quota P`: the five-hour window used `P` thousandths, and a request bucket that refills
    /// continuously.
    fn quota(&mut self, permille: u64, session: &str) -> Vec<Value> {
        let at = now();
        let mut five = Bucket::new("five_hour");
        five.label = Some("5 hours".into());
        five.models = Coverage::All;
        five.unit = Unit::Permille;
        five.used = Some(permille);
        five.limit = Some(1000);
        five.refill = Refill::FixedWindow {
            length: Some(Duration::from_secs(5 * 3600)),
            resets_at: Some(at + 5 * 3600),
        };
        five.status = if permille >= 1000 {
            Level::Exhausted
        } else {
            Level::Ok
        };
        five.observed_at = at;
        let mut rpm = Bucket::new("rpm");
        rpm.models = Coverage::All;
        rpm.unit = Unit::Requests;
        rpm.limit = Some(60);
        rpm.remaining = Some(60);
        rpm.refill = Refill::TokenBucket {
            amount: 60,
            per: Duration::from_secs(60),
            max: 60,
        };
        rpm.status = Level::Ok;
        rpm.observed_at = at;
        self.report.buckets = vec![five, rpm];
        self.report.limit = None;
        self.changed(Some(session))
    }

    /// `limit`: the five-hour window is used up and the turn is refused.
    fn limit(&mut self, session: &str) -> (Vec<Value>, LimitHit) {
        let at = now();
        let mut hit = LimitHit::new(at);
        hit.bucket = Some("five_hour".into());
        hit.resets_at = Some(at + 3600);
        hit.retry_after = Some(Duration::from_secs(60));
        hit.text = Some("usage limit reached".into());
        if let Some(five) = self
            .report
            .buckets
            .iter_mut()
            .find(|bucket| bucket.id == "five_hour")
        {
            five.status = Level::Exhausted;
            five.used = Some(1000);
            five.observed_at = at;
        } else {
            let mut five = Bucket::new("five_hour");
            five.unit = Unit::Permille;
            five.used = Some(1000);
            five.limit = Some(1000);
            five.status = Level::Exhausted;
            five.observed_at = at;
            self.report.buckets.push(five);
        }
        self.report.limit = Some(hit.clone());
        (self.changed(Some(session)), hit)
    }

    /// A turn ended: its fixed spend is added to the session's totals.
    fn spent(&mut self, session: &str) -> Vec<Value> {
        let spend = self
            .spends
            .entry(session.to_owned())
            .or_insert_with(|| Spend::new(session));
        spend.input += SPENT[0];
        spend.output += SPENT[1];
        spend.cache_read += SPENT[2];
        spend.cache_write += SPENT[3];
        self.changed(Some(session))
    }
}

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
    /// What its connections reported, for `usage`.
    reports: Reports,
}

impl EchoHarness {
    fn configured(settings: EchoSettings) -> Self {
        Self {
            settings,
            sessions: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
            reports: Arc::default(),
        }
    }

    fn unconfigured() -> Self {
        Self::configured(EchoSettings {
            usage: EchoUsage::None,
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
        if self.settings.usage == EchoUsage::Scripted {
            info.usage = UsageSupport::new(SOURCES.to_vec(), true);
        }
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
            usage: (self.settings.usage == EchoUsage::Scripted)
                .then(|| UsageState::new(request.account, Arc::clone(&self.reports))),
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
            let width = line.len() + 1;
            if text.len().saturating_add(width) > limit {
                if end == start {
                    return Err(ProviderError::new(
                        Status::GUATIAO_ERR_BAD_VALUE,
                        &format!("{LINE_TOO_LARGE}{width} bytes, more than max_bytes {limit}"),
                    ));
                }
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

    fn usage(
        &self,
        query: UsageQuery,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<UsageReport, ProviderError> {
        drop(spawner);
        if self.settings.usage != EchoUsage::Scripted {
            return Ok(UsageReport::unsupported("this echo reports no usage"));
        }
        let held = self
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&query.account.clone().unwrap_or_default())
            .cloned();
        Ok(held.unwrap_or_else(|| {
            let mut report = UsageReport::new();
            report.sources = SOURCES.to_vec();
            report.account = query.account;
            report
        }))
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
    /// What a scripted echo reports; none for one that reports nothing.
    usage: Option<UsageState>,
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
                let mut capabilities = json!({
                    "loadSession": true,
                    "promptCapabilities": {},
                    "mcpCapabilities": {"http": false, "sse": false},
                });
                if let Some(usage) = &mut self.usage {
                    let asked = &params["clientCapabilities"]["_meta"][USAGE_CAPABILITY];
                    usage.notify = asked["notify"] == true;
                    if usage.report.account.is_none() {
                        usage.report.account = asked["account"].as_str().map(str::to_owned);
                    }
                    capabilities["_meta"] = json!({USAGE_CAPABILITY: {
                        "version": USAGE_VERSION,
                        "sources": ["reported"],
                        "spend": true,
                    }});
                }
                let answer = json!({
                    "protocolVersion": 1,
                    "agentCapabilities": capabilities,
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
            USAGE_METHOD if self.usage.is_some() => {
                let session = match params.get("sessionId") {
                    None | Some(Value::Null) => None,
                    Some(named) => Some(
                        named
                            .as_str()
                            .filter(|named| self.open.iter().any(|open| open == named))
                            .ok_or("no such session is open here")?,
                    ),
                };
                let report = self
                    .usage
                    .as_ref()
                    .map(|usage| usage.snapshot(session))
                    .expect("checked above");
                let report = serde_json::to_value(report).expect("a report is JSON");
                (vec![result(id, report)], false)
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
        if let Some(usage) = &mut self.usage {
            if let Some(permille) = said
                .strip_prefix("quota ")
                .and_then(|rest| rest.trim().parse::<u64>().ok())
            {
                let mut answers = usage.quota(permille, session);
                answers.extend(self.finish(
                    session,
                    id,
                    Some(&format!("echo: {said}")),
                    "end_turn",
                ));
                return (answers, false);
            }
            if said == "limit" {
                let (mut answers, hit) = usage.limit(session);
                let mut ended = self.finish(session, id, Some("echo: limit"), "refusal");
                if let Some(result) = ended.last_mut() {
                    result["result"]["_meta"] = json!({
                        LIMIT_META_KEY: serde_json::to_value(&hit).expect("a hit is JSON"),
                    });
                }
                answers.extend(ended);
                return (answers, false);
            }
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
    fn finish(
        &mut self,
        session: &str,
        prompt: &Value,
        said: Option<&str>,
        stop: &str,
    ) -> Vec<Value> {
        let mut messages = Vec::new();
        if matches!(stop, "end_turn" | "refusal")
            && let Some(usage) = &mut self.usage
        {
            messages.extend(usage.spent(session));
        }
        messages.extend(finish(session, prompt, said, stop));
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
