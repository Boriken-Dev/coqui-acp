//! What every `acp_harness` provider is held to, from the host's side, against an echo: a harness
//! that answers `initialize`, `session/new` and `session/prompt` (with `echo: <text>`), holds to
//! `read_only` and `no_commands` and refuses any other requirement, keeps `session/new` inside the
//! working roots, and ends the connection itself after answering a prompt of `close`.
//!
//! Included with `#[path]` by every test that drives such a provider, in whatever language it is
//! written. A sink here never waits inside `receive` for its own host's `send`.

#![allow(
    dead_code,
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std, and each includer uses part of this file"
)]

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use coqui_acp_harness_kind::guatiao::Status;
use coqui_acp_harness_kind::guatiao::library::{Object, ProviderError};
use coqui_acp_harness_kind::{
    AcpHarness, Channel, ChannelSink, READ_ONLY, Requirement, StartRequest,
};
use serde_json::{Value, json};

/// How long an awaited call may take: generous, for an interpreter under load.
pub const PATIENCE: Duration = Duration::from_secs(30);
/// How long to watch for something that must not happen.
pub const GRACE: Duration = Duration::from_millis(500);

const GONE: u32 = 12;
const UNSUPPORTED: u32 = 13;

/// Runs every check against `harness`, naming it in each failure.
pub fn hold_the_contract(name: &str, harness: &dyn AcpHarness) {
    describes_itself(name, harness);
    speaks_acp(name, harness);
    holds_a_sender_back(name, harness);
    close_wakes_a_waiting_send(name, harness);
    close_reaches_an_idle_provider(name, harness);
    the_provider_closes_once(name, harness);
    a_sink_that_has_gone_ends_the_connection(name, harness);
    requirements_only_narrow(name, harness);
    working_roots_hold(name, harness);
    serves_the_client(name, harness);
}

#[derive(Default)]
struct Log {
    received: Vec<Vec<u8>>,
    closed: Option<u32>,
    held: bool,
    away: bool,
    destroyed: bool,
    broken: Vec<String>,
}

struct Shared {
    log: Mutex<Log>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The host's side of one connection: the sink handed to the provider, seen from the test.
pub struct Probe(Arc<Shared>);

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

    fn closed(&mut self, status: u32) {
        let mut log = self.0.lock();
        if log.closed.is_some() {
            log.broken.push(format!("closed({status}) came twice"));
        }
        log.closed = Some(status);
        drop(log);
        self.0.changed.notify_all();
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.0.lock().destroyed = true;
        self.0.changed.notify_all();
    }
}

impl Probe {
    /// A probe and the sink to hand over.
    pub fn new() -> (Self, Object<dyn ChannelSink>) {
        let shared = Arc::new(Shared {
            log: Mutex::new(Log::default()),
            changed: Condvar::new(),
        });
        let sink = Sink(Arc::clone(&shared)).into_object();
        (Self(shared), sink)
    }

    /// `receive` waits from now until [`release`](Self::release).
    pub fn hold(&self) {
        self.0.lock().held = true;
    }

    pub fn release(&self) {
        self.0.lock().held = false;
        self.0.changed.notify_all();
    }

    /// `receive` answers GONE from now on.
    pub fn go_away(&self) {
        self.0.lock().away = true;
    }

    fn wait_for<T>(&self, what: &str, mut done: impl FnMut(&Log) -> Option<T>) -> T {
        let deadline = Instant::now() + PATIENCE;
        let mut log = self.0.lock();
        loop {
            assert!(log.broken.is_empty(), "{:?}", log.broken);
            if let Some(found) = done(&log) {
                return found;
            }
            let now = Instant::now();
            assert!(now < deadline, "{what} did not come within {PATIENCE:?}");
            log = self
                .0
                .changed
                .wait_timeout(log, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The first `count` messages received, as JSON.
    pub fn messages(&self, count: usize) -> Vec<Value> {
        self.wait_for(&format!("{count} messages"), |log| {
            (log.received.len() >= count).then(|| {
                log.received[..count]
                    .iter()
                    .map(|bytes| {
                        serde_json::from_slice(bytes).unwrap_or_else(|_| {
                            panic!(
                                "a message is not one JSON value: {:?}",
                                String::from_utf8_lossy(bytes)
                            )
                        })
                    })
                    .collect()
            })
        })
    }

    /// The status of the provider's `closed`.
    pub fn closed(&self) -> u32 {
        self.wait_for("closed", |log| log.closed)
    }

    /// Waits for the sink to be destroyed.
    pub fn destroyed(&self) {
        self.wait_for("the sink's destruction", |log| log.destroyed.then_some(()));
    }

    fn count(&self) -> usize {
        self.0.lock().received.len()
    }
}

/// A JSON-RPC request as the bytes of one message.
pub fn request(id: u64, method: &str, params: Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        .to_string()
        .into_bytes()
}

fn initialize(id: u64) -> Vec<u8> {
    request(
        id,
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    )
}

fn prompt(id: u64, session: &str, text: &str) -> Vec<u8> {
    request(
        id,
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
    )
}

fn cwd() -> String {
    std::env::temp_dir()
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_owned()
}

fn start(
    name: &str,
    harness: &dyn AcpHarness,
    request: StartRequest,
) -> (Probe, Object<dyn Channel>) {
    let (probe, sink) = Probe::new();
    let channel = harness
        .start(request, sink)
        .unwrap_or_else(|e| panic!("{name}: start refused: {}", e.message()));
    (probe, channel)
}

fn status(error: &ProviderError) -> u32 {
    error.status.into()
}

fn describes_itself(name: &str, harness: &dyn AcpHarness) {
    let info = harness.describe().expect("described");
    assert!(!info.title.is_empty(), "{name}: no title");
    assert!(info.versions.contains(&1), "{name}: {info:?}");
}

fn speaks_acp(name: &str, harness: &dyn AcpHarness) {
    let (probe, channel) = start(name, harness, StartRequest::new());
    channel.send(&initialize(0)).expect("taken");
    channel
        .send(&request(
            1,
            "session/new",
            json!({"cwd": cwd(), "mcpServers": []}),
        ))
        .expect("taken");
    let answers = probe.messages(2);
    assert_eq!(answers[0]["id"], 0, "{name}: {answers:?}");
    assert_eq!(answers[0]["result"]["protocolVersion"], 1, "{name}");
    let session = answers[1]["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{name}: no session: {answers:?}"))
        .to_owned();
    channel.send(&prompt(2, &session, "hello")).expect("taken");
    channel
        .send(&request(3, "no/such", json!({})))
        .expect("taken");
    channel.send(b"not JSON").expect("taken");
    let answers = probe.messages(6);
    assert_eq!(answers[2]["method"], "session/update", "{name}");
    assert_eq!(
        answers[2]["params"]["update"]["content"]["text"], "echo: hello",
        "{name}"
    );
    assert_eq!(answers[3]["id"], 2, "{name}");
    assert_eq!(answers[3]["result"]["stopReason"], "end_turn", "{name}");
    assert_eq!(answers[4]["error"]["code"], -32601, "{name}");
    assert_eq!(answers[5]["error"]["code"], -32700, "{name}");
    assert_eq!(answers[5]["id"], Value::Null, "{name}");
    channel.close(0);
    assert_eq!(probe.closed(), 0, "{name}");
    probe.destroyed();
}

/// Sends `count` messages on a thread of its own, counting each one taken, until one is refused.
fn sender(
    channel: Arc<Object<dyn Channel>>,
    count: u64,
) -> (Arc<AtomicUsize>, std::thread::JoinHandle<Option<u32>>) {
    let taken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&taken);
    let thread = std::thread::spawn(move || {
        for id in 0..count {
            if let Err(e) = channel.send(&initialize(id)) {
                return Some(status(&e));
            }
            counted.fetch_add(1, Ordering::SeqCst);
        }
        None
    });
    (taken, thread)
}

/// Waits until `taken` stops growing, and answers where it stopped.
fn settled(taken: &AtomicUsize) -> usize {
    let mut last = taken.load(Ordering::SeqCst);
    loop {
        std::thread::sleep(GRACE);
        let now = taken.load(Ordering::SeqCst);
        if now == last {
            return now;
        }
        last = now;
    }
}

fn holds_a_sender_back(name: &str, harness: &dyn AcpHarness) {
    const MANY: u64 = 64;
    let (probe, channel) = start(name, harness, StartRequest::new());
    probe.hold();
    let channel = Arc::new(channel);
    let (taken, thread) = sender(Arc::clone(&channel), MANY);
    let held = settled(&taken);
    assert!(
        held < usize::try_from(MANY).expect("small"),
        "{name}: a held sink held no sender back"
    );
    assert!(probe.count() == 0, "{name}: a held sink took a message");
    probe.release();
    assert_eq!(thread.join().expect("the sender ends"), None, "{name}");
    let answers = probe.messages(usize::try_from(MANY).expect("small"));
    for (id, answer) in answers.iter().enumerate() {
        assert_eq!(answer["id"], id, "{name}: out of order");
    }
    channel.close(0);
    assert_eq!(probe.closed(), 0, "{name}");
}

fn close_wakes_a_waiting_send(name: &str, harness: &dyn AcpHarness) {
    let (probe, channel) = start(name, harness, StartRequest::new());
    probe.hold();
    let channel = Arc::new(channel);
    let (taken, thread) = sender(Arc::clone(&channel), 64);
    settled(&taken);
    let closing = Instant::now();
    channel.close(0);
    assert_eq!(
        thread.join().expect("the sender ends"),
        Some(GONE),
        "{name}: a send waiting for room was not answered GONE by close"
    );
    assert!(
        closing.elapsed() < PATIENCE,
        "{name}: close took too long to wake a send"
    );
    assert_eq!(
        channel.send(&initialize(99)).map_err(|e| status(&e)),
        Err(GONE),
        "{name}"
    );
    probe.release();
    assert_eq!(probe.closed(), 0, "{name}");
    probe.destroyed();
}

fn close_reaches_an_idle_provider(name: &str, harness: &dyn AcpHarness) {
    for code in [0, UNSUPPORTED] {
        let (probe, channel) = start(name, harness, StartRequest::new());
        channel.send(&initialize(0)).expect("taken");
        probe.messages(1);
        // The provider now waits for a message; close has to reach it there.
        std::thread::sleep(GRACE);
        channel.close(code);
        assert_eq!(probe.closed(), code, "{name}: the host's status");
        probe.destroyed();
        drop(channel);
    }
}

fn the_provider_closes_once(name: &str, harness: &dyn AcpHarness) {
    let (probe, channel) = start(name, harness, StartRequest::new());
    channel.send(&initialize(0)).expect("taken");
    channel
        .send(&request(
            1,
            "session/new",
            json!({"cwd": cwd(), "mcpServers": []}),
        ))
        .expect("taken");
    let session = probe.messages(2)[1]["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_owned();
    channel.send(&prompt(2, &session, "close")).expect("taken");
    assert_eq!(probe.closed(), 0, "{name}");
    assert_eq!(
        probe.messages(4)[3]["result"]["stopReason"],
        "end_turn",
        "{name}: the prompt was answered before closing"
    );
    let deadline = Instant::now() + PATIENCE;
    loop {
        match channel.send(&initialize(3)) {
            Err(e) => {
                assert_eq!(status(&e), GONE, "{name}");
                break;
            }
            Ok(()) => assert!(
                Instant::now() < deadline,
                "{name}: send was still taken after closed"
            ),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    channel.close(0);
    probe.destroyed();
}

fn a_sink_that_has_gone_ends_the_connection(name: &str, harness: &dyn AcpHarness) {
    let (probe, channel) = start(name, harness, StartRequest::new());
    probe.go_away();
    channel.send(&initialize(0)).expect("taken");
    assert_eq!(probe.closed(), GONE, "{name}");
    probe.destroyed();
}

fn requirements_only_narrow(name: &str, harness: &dyn AcpHarness) {
    let unknown = Requirement::new("no_such_requirement");
    assert!(
        harness
            .refusal(unknown.clone(), Default::default())
            .expect("answered")
            .is_some(),
        "{name}: a requirement it does not know was not refused"
    );
    assert_eq!(
        harness
            .refusal(Requirement::new(READ_ONLY), Default::default())
            .expect("answered"),
        None,
        "{name}"
    );

    let (probe, sink) = Probe::new();
    let mut asked = StartRequest::new();
    asked.requirements = vec![Requirement::new(READ_ONLY), unknown];
    let Err(refused) = harness.start(asked, sink) else {
        panic!("{name}: started, with a requirement it refuses");
    };
    assert_eq!(status(&refused), UNSUPPORTED, "{name}");
    probe.destroyed();

    let mut asked = StartRequest::new();
    asked.requirements = vec![Requirement::new(READ_ONLY)];
    let (probe, channel) = start(name, harness, asked);
    channel.close(0);
    assert_eq!(probe.closed(), 0, "{name}");
}

fn working_roots_hold(name: &str, harness: &dyn AcpHarness) {
    let mut asked = StartRequest::new();
    asked.working_roots = vec![cwd()];
    let (probe, channel) = start(name, harness, asked);
    channel.send(&initialize(0)).expect("taken");
    channel
        .send(&request(
            1,
            "session/new",
            json!({"cwd": cwd(), "mcpServers": []}),
        ))
        .expect("taken");
    // A sibling whose name starts with the root's is not inside it.
    channel
        .send(&request(
            2,
            "session/new",
            json!({"cwd": format!("{}-beside", cwd()), "mcpServers": []}),
        ))
        .expect("taken");
    let answers = probe.messages(3);
    assert!(
        answers[1]["result"]["sessionId"].is_string(),
        "{name}: {answers:?}"
    );
    assert_eq!(answers[2]["error"]["code"], -32602, "{name}: {answers:?}");
    channel.close(0);
    assert_eq!(probe.closed(), 0, "{name}");
}

/// The sink of [`serves_the_client`]: hands each message to the transport's reader.
struct Forward(Option<mpsc::Sender<Vec<u8>>>);

impl ChannelSink for Forward {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        self.0
            .as_ref()
            .and_then(|tx| tx.send(bytes.to_vec()).ok())
            .ok_or_else(|| ProviderError::new(Status::GUATIAO_ERR_GONE, "the client is gone"))
    }

    fn closed(&mut self, _status: u32) {
        self.0 = None;
    }
}

/// The agent's messages as lines, for a line transport.
struct Inbox {
    rx: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Read for Inbox {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            let Ok(mut message) = self.rx.recv() else {
                return Ok(0);
            };
            message.push(b'\n');
            self.pending = message;
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

/// The client's lines, each sent as one message.
struct Outbox {
    channel: Object<dyn Channel>,
    line: Vec<u8>,
}

impl Write for Outbox {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(buf);
        while let Some(end) = self.line.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            let message = line.trim_ascii();
            if !message.is_empty() {
                self.channel
                    .send(message)
                    .map_err(|e| io::Error::other(e.message().to_owned()))?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serves_the_client(name: &str, harness: &dyn AcpHarness) {
    let (tx, rx) = mpsc::channel();
    let channel = harness
        .start(StartRequest::new(), Forward(Some(tx)).into_object())
        .unwrap_or_else(|e| panic!("{name}: start refused: {}", e.message()));
    let inbox = Inbox {
        rx,
        pending: Vec::new(),
    };
    let outbox = Outbox {
        channel,
        line: Vec::new(),
    };
    let (transport, _) =
        coqui_acp::lines::over_io(inbox, outbox, 0, coqui_acp::lines::DEFAULT_MAX_LINE)
            .expect("threads start");
    let session = coqui_acp::Client::builder(cwd())
        .initialize_timeout(Some(PATIENCE))
        .request_timeout(Some(PATIENCE))
        .prompt("hello")
        .connect(transport)
        .unwrap_or_else(|e| panic!("{name}: the client could not open a session: {e}"));
    let mut text = String::new();
    loop {
        match session.next_event(PATIENCE) {
            Some(coqui_acp::Event::Text(more)) => text.push_str(&more),
            Some(coqui_acp::Event::Completed { .. }) => break,
            Some(coqui_acp::Event::Failed(why)) => panic!("{name}: the turn failed: {why}"),
            Some(_) => {}
            None => panic!("{name}: the turn did not end"),
        }
    }
    assert_eq!(text, "echo: hello", "{name}");
    session.close();
}
