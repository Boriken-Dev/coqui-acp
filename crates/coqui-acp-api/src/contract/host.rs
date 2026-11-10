//! The host's side of a connection under test: the sink, spawner and keeper handed to `start`, and
//! the spawner and prompter handed to a login, seen from the suite.
//!
//! The spawner is virtual: it starts no program, answers every `spawn` with a process that does
//! nothing until it is killed, and records what it was asked, so the suite can see which
//! variables a provider cleared and which files it wrote. Its file methods work on a real
//! directory the suite grants.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use guatiao::library::{Object, ProviderError};
use guatiao::{Secret, Status};
use guatiao_channel::{Channel, ChannelSink};
use serde_json::{Value, json};

use super::checks::CANARY;
use super::json;
use crate::spawner::{
    FileInfo, LinkState, ProcessExit, ProcessSpawner, READ_TIMED_OUT, SpawnSpec, SpawnedProcess,
    SpawnerInfo, TerminalSize,
};
use crate::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, LoginAnswer, LoginForm, LoginPrompter,
    OpenUrl, StartRequest,
};

/// How long an awaited call may take: generous, for an interpreter under load.
pub(super) const PATIENCE: Duration = Duration::from_secs(30);
/// How long to watch for something that must not happen.
pub(super) const GRACE: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Log {
    received: Vec<Vec<u8>>,
    closed: Option<u32>,
    /// The reason `closed` gave.
    reason: String,
    held: bool,
    away: bool,
    sink_destroyed: bool,
    spawner_destroyed: bool,
    keeper_destroyed: bool,
    prompter_destroyed: bool,
    /// Whether the spawner was destroyed when `closed` came.
    spawner_gone_at_close: Option<bool>,
    /// Whether the keeper was destroyed when `closed` came.
    keeper_gone_at_close: Option<bool>,
    broken: Vec<String>,
    /// Every spec the spawner was asked to start.
    specs: Vec<SpawnSpec>,
    /// For each process it started, whether it was killed or destroyed.
    processes: Vec<Arc<AtomicBool>>,
    /// Every file written through the spawner, as `(path, content)`.
    writes: Vec<(String, Vec<u8>)>,
    renewed: Vec<AccountRenewal>,
    lost: Vec<AccountLoss>,
    /// Everything the provider sent the person through the prompter, as text.
    said: Vec<String>,
    /// How many `say`, `open_url` and `ask` calls the prompter took.
    prompts: usize,
    asked: usize,
}

pub(super) struct Shared {
    log: Mutex<Log>,
    changed: Condvar,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(Log::default()),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Sink(Arc<Shared>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let mut log = self.0.lock();
        if log.closed.is_some() {
            log.broken.push("receive came after closed".into());
        }
        while log.held {
            log = self
                .0
                .changed
                .wait(log)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let answer = if log.away {
            Err(ProviderError::new(
                Status::GUATIAO_ERR_GONE,
                "the host takes nothing more",
            ))
        } else {
            log.received.push(bytes.to_vec());
            Ok(())
        };
        drop(log);
        self.0.changed.notify_all();
        answer
    }

    fn closed(&mut self, status: u32, reason: &str) {
        let mut log = self.0.lock();
        if log.closed.is_some() {
            log.broken.push(format!("closed({status}) came twice"));
        }
        log.closed = Some(status);
        log.reason = reason.to_owned();
        log.spawner_gone_at_close = Some(log.spawner_destroyed);
        log.keeper_gone_at_close = Some(log.keeper_destroyed);
        drop(log);
        self.0.changed.notify_all();
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.0.lock().sink_destroyed = true;
        self.0.changed.notify_all();
    }
}

fn refuse(status: Status, why: &str) -> ProviderError {
    ProviderError::new(status, why)
}

/// A process that does nothing until it is killed or destroyed.
struct Idle(Arc<AtomicBool>);

impl Idle {
    /// Waits up to `timeout_ms` for the kill, in small steps; whether it came.
    fn killed_within(&self, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if self.0.load(Ordering::SeqCst) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }
}

impl SpawnedProcess for Idle {
    fn write(&self, _bytes: &[u8]) -> Result<(), ProviderError> {
        if self.0.load(Ordering::SeqCst) {
            return Err(refuse(Status::GUATIAO_ERR_GONE, "the process has ended"));
        }
        Ok(())
    }

    fn close_input(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn read(&self, _dst: &mut [u8], timeout_ms: u64) -> Result<i64, ProviderError> {
        Ok(if self.killed_within(timeout_ms) {
            0
        } else {
            READ_TIMED_OUT
        })
    }

    fn read_error(&self, _dst: &mut [u8], _timeout_ms: u64) -> Result<i64, ProviderError> {
        Ok(0)
    }

    fn resize(&self, _size: TerminalSize) -> Result<(), ProviderError> {
        Err(refuse(Status::GUATIAO_ERR_UNSUPPORTED, "no terminal"))
    }

    fn wait(&self, timeout_ms: u64) -> Result<ProcessExit, ProviderError> {
        Ok(if self.killed_within(timeout_ms) {
            ProcessExit::ended(None, Some(9))
        } else {
            ProcessExit::running()
        })
    }

    fn kill(&self) -> Result<(), ProviderError> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn link(&self) -> Result<LinkState, ProviderError> {
        Ok(LinkState::up())
    }
}

impl Drop for Idle {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A relative path inside `root`, or the refusal the file methods owe: an absolute path, a `..`
/// part and a link leading out are all `GUATIAO_ERR_NOT_FOUND`.
pub(super) fn confined(root: &Path, path: &str) -> Result<PathBuf, ProviderError> {
    let outside = || refuse(Status::GUATIAO_ERR_NOT_FOUND, "no such path in the granted directory");
    let drive = path.as_bytes().get(1) == Some(&b':');
    if path.is_empty()
        || path.contains('\0')
        || drive
        || path.starts_with(['/', '\\'])
        || Path::new(path).is_absolute()
    {
        return Err(outside());
    }
    let mut full = root.to_path_buf();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err(outside()),
            part => full.push(part),
        }
    }
    let base = std::fs::canonicalize(root).map_err(|_| outside())?;
    let mut nearest = full.clone();
    while !nearest.exists() {
        if !nearest.pop() {
            return Err(outside());
        }
    }
    let nearest = std::fs::canonicalize(&nearest).map_err(|_| outside())?;
    if nearest.starts_with(&base) {
        Ok(full)
    } else {
        Err(outside())
    }
}

/// A spawner that starts only idle processes, records what it is asked, and serves files in a
/// directory when it was given one. It says when it is destroyed.
struct Spawner {
    shared: Arc<Shared>,
    root: Option<PathBuf>,
}

impl Spawner {
    fn files(&self) -> Result<&Path, ProviderError> {
        self.root.as_deref().ok_or_else(|| {
            refuse(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this spawner has no private files",
            )
        })
    }
}

impl ProcessSpawner for Spawner {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        let mut info = SpawnerInfo::new(std::env::consts::OS, std::env::consts::ARCH, true);
        info.files = self.root.is_some();
        Ok(info)
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        let killed = Arc::new(AtomicBool::new(false));
        {
            let mut log = self.shared.lock();
            log.specs.push(spec);
            log.processes.push(Arc::clone(&killed));
        }
        self.shared.changed.notify_all();
        Ok(Idle(killed).into_object())
    }

    fn write_file(
        &self,
        path: &str,
        bytes: Secret<Vec<u8>>,
        private: bool,
    ) -> Result<(), ProviderError> {
        let root = self.files()?;
        let full = confined(root, path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| refuse(Status::GUATIAO_ERR_INTERNAL, "cannot make the directory"))?;
        }
        std::fs::write(&full, bytes.expose())
            .map_err(|_| refuse(Status::GUATIAO_ERR_INTERNAL, "cannot write the file"))?;
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        let _ = private;
        self.shared
            .lock()
            .writes
            .push((path.to_owned(), bytes.expose().clone()));
        self.shared.changed.notify_all();
        Ok(())
    }

    fn read_file(&self, path: &str, max_bytes: u64) -> Result<Secret<Vec<u8>>, ProviderError> {
        let full = confined(self.files()?, path)?;
        let meta = std::fs::metadata(&full)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .ok_or_else(|| refuse(Status::GUATIAO_ERR_NOT_FOUND, "no such file"))?;
        if meta.len() > max_bytes {
            return Err(refuse(Status::GUATIAO_ERR_BAD_VALUE, "the file is too large"));
        }
        std::fs::read(&full)
            .map(Secret::new)
            .map_err(|_| refuse(Status::GUATIAO_ERR_NOT_FOUND, "no such file"))
    }

    fn file_info(&self, path: &str) -> Result<Option<FileInfo>, ProviderError> {
        let full = confined(self.files()?, path)?;
        let Ok(meta) = std::fs::metadata(&full) else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Ok(None);
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX));
        Ok(Some(FileInfo::new(meta.len(), modified)))
    }

    fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        let full = confined(self.files()?, path)?;
        match std::fs::remove_file(&full) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(refuse(Status::GUATIAO_ERR_INTERNAL, "cannot remove the file")),
        }
    }
}

impl Drop for Spawner {
    fn drop(&mut self) {
        self.shared.lock().spawner_destroyed = true;
        self.shared.changed.notify_all();
    }
}

/// A keeper that records what it is told, and says when it is destroyed.
struct Keeper(Arc<Shared>);

impl AccountKeeper for Keeper {
    fn renewed(&self, renewal: AccountRenewal) -> Result<(), ProviderError> {
        self.0.lock().renewed.push(renewal);
        self.0.changed.notify_all();
        Ok(())
    }

    fn lost(&self, loss: AccountLoss) -> Result<(), ProviderError> {
        self.0.lock().lost.push(loss);
        self.0.changed.notify_all();
        Ok(())
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.0.lock().keeper_destroyed = true;
        self.0.changed.notify_all();
    }
}

/// How the person at a prompter behaves.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Script {
    /// The prompter call (counting `say`, `open_url` and `ask`, from 0) at which the person
    /// cancels: that call and every later one answer GONE.
    pub cancel_at_call: Option<usize>,
    /// How long after the prompter was made the person cancels.
    pub cancel_after: Option<Duration>,
    /// How long an `ask` takes the person to answer.
    pub ask_delay: Duration,
}

/// A prompter that answers every form with canary secrets, records what it was shown, and says when
/// it is destroyed.
struct Prompter {
    shared: Arc<Shared>,
    script: Script,
    since: Instant,
    cancelled: AtomicBool,
}

impl Prompter {
    fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return true;
        }
        if self
            .script
            .cancel_after
            .is_some_and(|after| self.since.elapsed() >= after)
        {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Counts a call, and refuses it with GONE once the person has cancelled.
    fn call(&self, shown: String) -> Result<(), ProviderError> {
        let mut log = self.shared.lock();
        let index = log.prompts;
        log.prompts += 1;
        log.said.push(shown);
        drop(log);
        self.shared.changed.notify_all();
        if self.script.cancel_at_call.is_some_and(|at| index >= at) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        if self.is_cancelled() {
            return Err(refuse(Status::GUATIAO_ERR_GONE, "the person cancelled"));
        }
        Ok(())
    }
}

impl LoginPrompter for Prompter {
    fn say(&self, text: &str) -> Result<(), ProviderError> {
        self.call(text.to_owned())
    }

    fn open_url(&self, open: OpenUrl) -> Result<(), ProviderError> {
        self.call(format!("{} {:?}", open.url, open.user_code))
    }

    fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        self.call(format!("{} {:?}", form.title, form.schema))?;
        self.shared.lock().asked += 1;
        std::thread::sleep(self.script.ask_delay);
        let schema = json::to_json(&form.schema);
        let mut values = serde_json::Map::new();
        let mut secrets = crate::Credentials::new();
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, property) in properties {
                let secret = property.get("writeOnly") == Some(&json!(true))
                    || property.get("x-sensitive") == Some(&json!(true));
                if secret {
                    secrets.insert(name.clone(), format!("{CANARY}-login-{name}"));
                } else {
                    values.insert(name.clone(), json!("contract"));
                }
            }
        }
        let values = json::from_json(&Value::Object(values))
            .map_err(|e| refuse(Status::GUATIAO_ERR_INTERNAL, &e))?;
        Ok(LoginAnswer::new(values, secrets))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(self.is_cancelled())
    }
}

impl Drop for Prompter {
    fn drop(&mut self) {
        self.shared.lock().prompter_destroyed = true;
        self.shared.changed.notify_all();
    }
}

/// What the suite sees of the objects it handed a call.
pub(super) struct Watch(Arc<Shared>);

impl Watch {
    /// Whether the spawner and the keeper (and the prompter, when one was handed) are destroyed.
    pub(super) fn destroyed(&self) -> bool {
        let log = self.0.lock();
        log.spawner_destroyed && log.keeper_destroyed
    }

    /// Whether the spawner handed over alone is destroyed.
    pub(super) fn spawner_destroyed(&self) -> bool {
        self.0.lock().spawner_destroyed
    }

    /// Whether the prompter is destroyed.
    pub(super) fn prompter_destroyed(&self) -> bool {
        self.0.lock().prompter_destroyed
    }

    /// The specs the spawner was asked to start.
    pub(super) fn specs(&self) -> Vec<SpawnSpec> {
        self.0.lock().specs.clone()
    }

    /// Whether every process the spawner started was killed or destroyed.
    pub(super) fn all_killed(&self) -> bool {
        self.0
            .lock()
            .processes
            .iter()
            .all(|killed| killed.load(Ordering::SeqCst))
    }

    /// How many processes the spawner started.
    pub(super) fn started(&self) -> usize {
        self.0.lock().processes.len()
    }

    /// The files written through the spawner, as `(path, content)`.
    pub(super) fn writes(&self) -> Vec<(String, Vec<u8>)> {
        self.0.lock().writes.clone()
    }

    /// What the keeper was told.
    pub(super) fn renewed(&self) -> Vec<AccountRenewal> {
        self.0.lock().renewed.clone()
    }

    pub(super) fn lost(&self) -> Vec<AccountLoss> {
        self.0.lock().lost.clone()
    }

    /// Everything shown through the prompter, as text.
    pub(super) fn said(&self) -> String {
        self.0.lock().said.join("\n")
    }

    /// How many forms the prompter took.
    pub(super) fn asked(&self) -> usize {
        self.0.lock().asked
    }

    /// How many prompter calls were made.
    pub(super) fn prompts(&self) -> usize {
        self.0.lock().prompts
    }

    /// Every channel message received so far, as text.
    pub(super) fn traffic(&self) -> String {
        let log = self.0.lock();
        let mut text: String = log
            .received
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str(&log.reason);
        text
    }

    /// Waits up to `patience` for `done` to hold of the log's keeper events.
    pub(super) fn wait_for(
        &self,
        patience: Duration,
        mut done: impl FnMut(&[AccountRenewal], &[AccountLoss]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + patience;
        let mut log = self.0.lock();
        loop {
            if done(&log.renewed, &log.lost) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            log = self
                .0
                .changed
                .wait_timeout(log, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// One connection under test: what the provider sent, read in order.
pub(super) struct Conn {
    shared: Arc<Shared>,
    pub(super) channel: Option<Arc<Object<dyn Channel>>>,
    read: usize,
}

/// The objects a call is handed, and the log they write to.
pub(super) struct Handed {
    pub(super) shared: Arc<Shared>,
    pub(super) spawner: Object<dyn ProcessSpawner>,
    pub(super) sink: Object<dyn ChannelSink>,
    pub(super) keeper: Object<dyn AccountKeeper>,
}

/// The pieces `start` is handed, and the log they write to; the spawner serves files in `root`.
pub(super) fn handed_in(root: Option<&Path>) -> Handed {
    let shared = Shared::new();
    let spawner = Spawner {
        shared: Arc::clone(&shared),
        root: root.map(Path::to_path_buf),
    }
    .into_object();
    let sink = Sink(Arc::clone(&shared)).into_object();
    let keeper = Keeper(Arc::clone(&shared)).into_object();
    Handed {
        shared,
        spawner,
        sink,
        keeper,
    }
}

/// The pieces `start` is handed, with a spawner that has no files.
pub(super) fn handed() -> Handed {
    handed_in(None)
}

/// A spawner and a keeper to hand to one call, and their watch; the spawner serves files in `root`.
pub(super) fn spawner_in(
    root: Option<&Path>,
) -> (Watch, Object<dyn ProcessSpawner>, Object<dyn AccountKeeper>) {
    let handed = handed_in(root);
    drop(handed.sink);
    (Watch(handed.shared), handed.spawner, handed.keeper)
}

/// A spawner with no files and a keeper to hand to one call, and their watch.
pub(super) fn spawner() -> (Watch, Object<dyn ProcessSpawner>, Object<dyn AccountKeeper>) {
    spawner_in(None)
}

/// A spawner, a prompter and the watch for a login; the spawner serves files in `root`.
pub(super) fn login_in(
    root: &Path,
    script: Script,
) -> (Watch, Object<dyn ProcessSpawner>, Object<dyn LoginPrompter>) {
    let handed = handed_in(Some(root));
    drop(handed.sink);
    drop(handed.keeper);
    let prompter = Prompter {
        shared: Arc::clone(&handed.shared),
        script,
        since: Instant::now(),
        cancelled: AtomicBool::new(false),
    }
    .into_object();
    (Watch(handed.shared), handed.spawner, prompter)
}

/// Starts a connection, or answers the refusal and whether every handed object was destroyed. The
/// spawner serves files in `root`.
pub(super) fn start_in(
    harness: &dyn AcpHarness,
    request: StartRequest,
    root: Option<&Path>,
) -> Result<Conn, (ProviderError, Conn)> {
    let Handed {
        shared,
        spawner,
        sink,
        keeper,
    } = handed_in(root);
    match harness.start(request, spawner, sink, keeper) {
        Ok(channel) => Ok(Conn {
            shared,
            channel: Some(Arc::new(channel)),
            read: 0,
        }),
        Err(refused) => Err((
            refused,
            Conn {
                shared,
                channel: None,
                read: 0,
            },
        )),
    }
}

/// Starts a connection, with a spawner that has no files.
pub(super) fn start(
    harness: &dyn AcpHarness,
    request: StartRequest,
) -> Result<Conn, (ProviderError, Conn)> {
    start_in(harness, request, None)
}

/// Starts a connection that must start.
pub(super) fn open(harness: &dyn AcpHarness, request: StartRequest) -> Result<Conn, String> {
    start(harness, request).map_err(|(e, _)| format!("start refused: {}", e.message()))
}

/// Starts a connection that must start, on a spawner serving files in `root`.
pub(super) fn open_in(
    harness: &dyn AcpHarness,
    request: StartRequest,
    root: &Path,
) -> Result<Conn, String> {
    start_in(harness, request, Some(root)).map_err(|(e, _)| format!("start refused: {}", e.message()))
}

pub(super) fn status(error: &ProviderError) -> u32 {
    error.status.into()
}

impl Conn {
    fn wait_for<T>(
        &self,
        what: &str,
        patience: Duration,
        mut done: impl FnMut(&Log) -> Option<T>,
    ) -> Result<Option<T>, String> {
        let deadline = Instant::now() + patience;
        let mut log = self.shared.lock();
        loop {
            if !log.broken.is_empty() {
                return Err(format!("the channel's rules broke: {:?}", log.broken));
            }
            if let Some(found) = done(&log) {
                return Ok(Some(found));
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = what;
                return Ok(None);
            }
            log = self
                .shared
                .changed
                .wait_timeout(log, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn must<T>(&self, what: &str, done: impl FnMut(&Log) -> Option<T>) -> Result<T, String> {
        self.wait_for(what, PATIENCE, done)?
            .ok_or_else(|| format!("{what} did not come within {PATIENCE:?}"))
    }

    pub(super) fn channel(&self) -> &Object<dyn Channel> {
        self.channel
            .as_deref()
            .expect("a started connection has a channel")
    }

    pub(super) fn shared_channel(&self) -> Arc<Object<dyn Channel>> {
        Arc::clone(
            self.channel
                .as_ref()
                .expect("a started connection has a channel"),
        )
    }

    /// Sends one message, which must be taken.
    pub(super) fn send(&self, message: &Value) -> Result<(), String> {
        self.channel()
            .send(message.to_string().as_bytes())
            .map_err(|e| format!("a send was refused: {}", e.message()))
    }

    /// Sends bytes as they are, which must be taken.
    pub(super) fn send_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        self.channel()
            .send(bytes)
            .map_err(|e| format!("a send was refused: {}", e.message()))
    }

    /// The next message the provider sent, as JSON.
    pub(super) fn next(&mut self) -> Result<Value, String> {
        let at = self.read;
        let bytes = self.must("a message from the provider", |log| {
            log.received.get(at).cloned()
        })?;
        self.read += 1;
        serde_json::from_slice(&bytes).map_err(|_| {
            format!(
                "a message is not one JSON value: {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }

    /// The next message that answers request `id`, skipping notifications; an agent's request in
    /// between is an error.
    pub(super) fn answer(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let message = self.next()?;
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return Ok(message);
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                return Err(format!(
                    "the agent asked {} while request {id} waited",
                    message["method"]
                ));
            }
        }
    }

    /// Whether nothing more arrives within [`GRACE`].
    pub(super) fn quiet(&self) -> Result<bool, String> {
        let at = self.read;
        Ok(self
            .wait_for("nothing", GRACE, |log| {
                (log.received.len() > at).then_some(())
            })?
            .is_none())
    }

    /// The status of the provider's `closed`.
    pub(super) fn closed(&self) -> Result<u32, String> {
        self.must("closed", |log| log.closed)
    }

    /// The reason the provider's `closed` gave, once it has come.
    pub(super) fn reason(&self) -> Result<String, String> {
        self.closed()?;
        Ok(self.shared.lock().reason.clone())
    }

    /// Whether the spawner was destroyed by the time `closed` came.
    pub(super) fn spawner_gone_at_close(&self) -> Result<bool, String> {
        self.must("closed", |log| log.spawner_gone_at_close)
    }

    /// Whether the keeper was destroyed by the time `closed` came.
    pub(super) fn keeper_gone_at_close(&self) -> Result<bool, String> {
        self.must("closed", |log| log.keeper_gone_at_close)
    }

    /// Waits for every handed object to be destroyed.
    pub(super) fn destroyed(&self) -> Result<(), String> {
        self.must("the sink's destruction", |log| {
            log.sink_destroyed.then_some(())
        })?;
        self.must("the spawner's destruction", |log| {
            log.spawner_destroyed.then_some(())
        })?;
        self.must("the keeper's destruction", |log| {
            log.keeper_destroyed.then_some(())
        })
    }

    /// What the suite sees of the objects this connection was handed.
    pub(super) fn watch(&self) -> Watch {
        Watch(Arc::clone(&self.shared))
    }

    /// `receive` waits from now until [`release`](Self::release).
    pub(super) fn hold(&self) {
        self.shared.lock().held = true;
    }

    pub(super) fn release(&self) {
        self.shared.lock().held = false;
        self.shared.changed.notify_all();
    }

    /// `receive` answers GONE from now on.
    pub(super) fn go_away(&self) {
        self.shared.lock().away = true;
    }

    pub(super) fn received(&self) -> usize {
        self.shared.lock().received.len()
    }

    /// Every message received so far that is a request of the agent's, by method.
    pub(super) fn asked(&self, method: &str) -> bool {
        self.shared.lock().received.iter().any(|bytes| {
            serde_json::from_slice::<Value>(bytes)
                .is_ok_and(|m| m.get("method") == Some(&json!(method)))
        })
    }

    /// Closes from the host's side with `code`, and checks the provider's `closed` follows with it
    /// and that both objects go.
    pub(super) fn close_with(&self, code: u32) -> Result<(), String> {
        self.channel().close(
            code,
            if code == 0 {
                ""
            } else {
                "the contract gives up"
            },
        );
        let closed = self.closed()?;
        if closed != code {
            return Err(format!(
                "closed({closed}) after the host closed with {code}"
            ));
        }
        if !self.spawner_gone_at_close()? {
            return Err("the spawner was still alive when the provider called closed".to_owned());
        }
        if !self.keeper_gone_at_close()? {
            return Err("the keeper was still alive when the provider called closed".to_owned());
        }
        self.destroyed()
    }
}
