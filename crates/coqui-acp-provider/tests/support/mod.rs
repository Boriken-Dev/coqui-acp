//! A provider that does as its test says, and a client at the JSON-RPC level for what the real one will not
//! send.
#![allow(dead_code, reason = "each test binary uses a part of this")]

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use coqui_acp_provider::guatiao::Status;
use coqui_acp_provider::guatiao::library::{Object, ProviderError};
use coqui_acp_provider::{
    AcpFacade, AgentEvent, AgentMessage, AgentProvider, AgentSession, COMPLETED_EVENT, Error,
    ResumeRequest, Settings, StartRequest, TEXT_EVENT, sdk,
};
use futures::SinkExt;
use futures::channel::mpsc as async_mpsc;
use serde_json::{Value, json};

/// How long a test waits for what should happen quickly.
pub const WAIT: Duration = Duration::from_secs(20);

/// A provider call a test can make misbehave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Start,
    Resume,
    Send,
    Next,
    Interrupt,
    Close,
}

/// How it misbehaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Blocks until the test calls [`Probe::release`].
    Hang,
    Panic,
    Fail,
    /// Takes [`SLOW`], then goes on.
    Slow,
    /// Takes [`SLOW`], then refuses.
    SlowFail,
}

/// How long a slow call takes.
pub const SLOW: Duration = Duration::from_millis(150);

/// A secret a panic or an environment holds, which no client may hear.
pub const SECRET: &str = "sk-do-not-show";

type Script = Box<dyn FnMut(&str) -> Vec<AgentEvent> + Send>;

/// What a test sets up, and what the provider saw.
pub struct Probe {
    faults: Mutex<Vec<(Call, Fault)>>,
    script: Mutex<Script>,
    /// What `interrupt` makes the provider report; `None` ignores it.
    pub on_interrupt: Mutex<Option<AgentEvent>>,
    pub starts: Mutex<Vec<StartRequest>>,
    pub resumes: Mutex<Vec<ResumeRequest>>,
    pub messages: Mutex<Vec<String>>,
    pub closed: AtomicUsize,
    pub interrupted: AtomicUsize,
    pub next_calls: AtomicUsize,
    /// `next_event` returns at once, ignoring its timeout.
    pub impatient: AtomicBool,
    /// Sessions report this identity.
    pub identity: Mutex<Option<String>>,
    /// Events a session has yet to report, shared by all sessions.
    pending: Mutex<VecDeque<AgentEvent>>,
    ready: Condvar,
    released: Mutex<bool>,
    release_signal: Condvar,
}

/// Answers each message with `echo: <text>`, then completes.
pub fn echo(text: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::new(TEXT_EVENT, Some(format!("echo: {text}"))),
        AgentEvent::new(COMPLETED_EVENT, None),
    ]
}

impl Probe {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            faults: Mutex::new(Vec::new()),
            script: Mutex::new(Box::new(echo)),
            on_interrupt: Mutex::new(Some(AgentEvent::new(COMPLETED_EVENT, None))),
            starts: Mutex::new(Vec::new()),
            resumes: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
            closed: AtomicUsize::new(0),
            interrupted: AtomicUsize::new(0),
            next_calls: AtomicUsize::new(0),
            impatient: AtomicBool::new(false),
            identity: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            released: Mutex::new(false),
            release_signal: Condvar::new(),
        })
    }

    /// The next `call` misbehaves, once.
    pub fn fault(&self, call: Call, fault: Fault) {
        self.faults.lock().unwrap().push((call, fault));
    }

    pub fn script(&self, script: impl FnMut(&str) -> Vec<AgentEvent> + Send + 'static) {
        *self.script.lock().unwrap() = Box::new(script);
    }

    /// Lets every hung call return.
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release_signal.notify_all();
    }

    pub fn report(&self, event: AgentEvent) {
        self.pending.lock().unwrap().push_back(event);
        self.ready.notify_all();
    }

    fn message(&self, text: &str) {
        self.messages.lock().unwrap().push(text.to_owned());
        let events = (self.script.lock().unwrap())(text);
        for event in events {
            self.report(event);
        }
    }

    /// Does what the test set up for `call`, if anything.
    fn enter(&self, call: Call) -> Result<(), ProviderError> {
        let fault = {
            let mut faults = self.faults.lock().unwrap();
            faults
                .iter()
                .position(|(when, _)| *when == call)
                .map(|at| faults.remove(at).1)
        };
        match fault {
            None => Ok(()),
            Some(Fault::Hang) => {
                let mut released = self.released.lock().unwrap();
                while !*released {
                    released = self.release_signal.wait(released).unwrap();
                }
                Ok(())
            }
            Some(Fault::Panic) => panic!("a provider bug that knows {SECRET}"),
            Some(Fault::Fail) => Err(ProviderError::new(
                Status::GUATIAO_ERR_BAD_VALUE,
                "the provider refused",
            )),
            Some(Fault::Slow) => {
                thread::sleep(SLOW);
                Ok(())
            }
            Some(Fault::SlowFail) => {
                thread::sleep(SLOW);
                Err(ProviderError::new(
                    Status::GUATIAO_ERR_BAD_VALUE,
                    "the provider refused",
                ))
            }
        }
    }
}

pub struct TestProvider(pub Arc<Probe>);

/// A session reports from what the test queued on the probe, until it is closed.
struct TestSession(Arc<Probe>, AtomicBool);

impl AgentProvider for TestProvider {
    fn start(&self, request: StartRequest) -> Result<Object<dyn AgentSession>, ProviderError> {
        self.0.enter(Call::Start)?;
        let prompt = request.prompt.clone();
        self.0.starts.lock().unwrap().push(request);
        self.0.message(&prompt);
        Ok(TestSession(self.0.clone(), AtomicBool::new(false)).into_object())
    }

    fn resume(&self, request: ResumeRequest) -> Result<Object<dyn AgentSession>, ProviderError> {
        self.0.enter(Call::Resume)?;
        self.0.resumes.lock().unwrap().push(request);
        Ok(TestSession(self.0.clone(), AtomicBool::new(false)).into_object())
    }
}

impl AgentSession for TestSession {
    fn session_id(&self) -> Result<Option<String>, ProviderError> {
        Ok(self.0.identity.lock().unwrap().clone())
    }

    fn send_message(&self, message: AgentMessage) -> Result<(), ProviderError> {
        self.0.enter(Call::Send)?;
        self.0.message(&message.text);
        Ok(())
    }

    fn next_event(&self, timeout_ms: i64) -> Result<Option<AgentEvent>, ProviderError> {
        self.0.next_calls.fetch_add(1, Ordering::SeqCst);
        self.0.enter(Call::Next)?;
        let pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.0.impatient.load(Ordering::SeqCst) {
            let mut pending = pending;
            return Ok(pending.pop_front());
        }
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap());
        let (mut pending, _) = self
            .0
            .ready
            .wait_timeout_while(pending, timeout, |pending| {
                pending.is_empty() && !self.1.load(Ordering::SeqCst)
            })
            .unwrap_or_else(PoisonError::into_inner);
        if self.1.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(pending.pop_front())
    }

    /// Reports what the test set up, then misbehaves if told to: an interrupt that ended its turn and has
    /// not returned yet.
    fn interrupt(&self) -> Result<(), ProviderError> {
        self.0.interrupted.fetch_add(1, Ordering::SeqCst);
        let event = self.0.on_interrupt.lock().unwrap().clone();
        if let Some(event) = event {
            self.0.report(event);
        }
        self.0.enter(Call::Interrupt)
    }

    fn close(&self) -> Result<(), ProviderError> {
        self.0.enter(Call::Close)?;
        self.1.store(true, Ordering::SeqCst);
        self.0.ready.notify_all();
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Limits short enough to run into, and a poll to keep the suite quick.
pub fn quick() -> Settings {
    let mut settings = Settings::default();
    settings.poll = Duration::from_millis(10);
    settings.start_timeout = Duration::from_millis(400);
    settings.call_timeout = Duration::from_millis(400);
    settings.cancel_grace = Duration::from_millis(200);
    settings.close_timeout = Duration::from_millis(400);
    settings
}

pub fn facade(probe: &Arc<Probe>, settings: Settings) -> AcpFacade {
    AcpFacade::new(Arc::new(TestProvider(probe.clone()))).with_settings(settings)
}

/// An absolute directory on this platform; the facade only checks that it is one.
pub fn cwd() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

/// Polls until `condition` holds, or fails the test naming `what`.
pub fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "never happened: {what}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

type AgentLines = sdk::Lines<
    futures::sink::SinkMapErr<
        async_mpsc::UnboundedSender<String>,
        fn(async_mpsc::SendError) -> io::Error,
    >,
    async_mpsc::UnboundedReceiver<io::Result<String>>,
>;

/// A client that writes JSON-RPC by hand, so a test sees every message and can send what `coqui_acp::Client`
/// never would.
pub struct Raw {
    to_agent: Option<async_mpsc::UnboundedSender<io::Result<String>>>,
    from_agent: mpsc::Receiver<(usize, Value)>,
    next_id: u64,
    /// Every notification seen so far.
    pub notifications: Vec<Value>,
    /// What those notifications took on the wire, in bytes.
    pub notification_bytes: usize,
    /// The longest line the facade wrote so far, in bytes.
    pub longest_line: usize,
    answers: Vec<Value>,
}

impl Raw {
    pub fn pair() -> (Self, AgentLines) {
        let (to_agent, agent_reads) = async_mpsc::unbounded::<io::Result<String>>();
        let (agent_writes, mut lines) = async_mpsc::unbounded::<String>();
        let (forward, from_agent) = mpsc::channel();
        thread::spawn(move || {
            while let Some(line) = futures::executor::block_on(futures::StreamExt::next(&mut lines))
            {
                let message: Value = serde_json::from_str(&line).expect("the facade writes JSON");
                if forward.send((line.len(), message)).is_err() {
                    return;
                }
            }
        });
        let outgoing = agent_writes.sink_map_err((|error| io::Error::other(error)) as fn(_) -> _);
        (
            Self {
                to_agent: Some(to_agent),
                from_agent,
                next_id: 0,
                notifications: Vec::new(),
                notification_bytes: 0,
                longest_line: 0,
                answers: Vec::new(),
            },
            sdk::Lines::new(outgoing, agent_reads),
        )
    }

    pub fn serve(facade: &AcpFacade) -> (Self, JoinHandle<Result<(), Error>>) {
        let (raw, lines) = Self::pair();
        let served = facade.serve(lines).expect("a thread starts");
        (raw, served)
    }

    fn write(&self, message: &Value) {
        let _ = self
            .to_agent
            .as_ref()
            .expect("the client is still connected")
            .unbounded_send(Ok(message.to_string()));
    }

    pub fn send(&mut self, method: &str, params: &Value) -> u64 {
        self.next_id += 1;
        self.write(
            &json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}),
        );
        self.next_id
    }

    pub fn notify(&self, method: &str, params: &Value) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Reads what has arrived until `done` says the last message was the one wanted. `None` on hanging up.
    fn read_until(&mut self, done: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.from_agent.recv_timeout(left) {
                Ok((length, message)) => {
                    self.longest_line = self.longest_line.max(length);
                    if message.get("id").is_none() {
                        self.notification_bytes += length;
                    }
                    if done(&message) {
                        return Some(message);
                    }
                    if message.get("id").is_some() {
                        self.answers.push(message);
                    } else {
                        self.notifications.push(message);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the facade went silent"),
            }
        }
    }

    /// The answer to request `id`, however many other messages come first.
    pub fn answer(&mut self, id: u64) -> Value {
        if let Some(at) = self
            .answers
            .iter()
            .position(|answer| answer["id"] == json!(id))
        {
            return self.answers.remove(at);
        }
        self.read_until(|message| message.get("id") == Some(&json!(id)))
            .expect("the facade answers before it goes")
    }

    /// The answer to request `id`, or `None` when the facade ended the connection first.
    pub fn answer_or_gone(&mut self, id: u64) -> Option<Value> {
        if let Some(at) = self
            .answers
            .iter()
            .position(|answer| answer["id"] == json!(id))
        {
            return Some(self.answers.remove(at));
        }
        self.read_until(|message| message.get("id") == Some(&json!(id)))
    }

    pub fn call(&mut self, method: &str, params: &Value) -> Value {
        let id = self.send(method, params);
        self.answer(id)
    }

    pub fn initialize(&mut self) -> Value {
        self.call("initialize", &json!({"protocolVersion": 1}))["result"].clone()
    }

    /// `initialize` and `session/new`: the session's identity.
    pub fn open(&mut self) -> String {
        self.initialize();
        self.new_session()
    }

    pub fn new_session(&mut self) -> String {
        let answer = self.call("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
        answer["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("no session: {answer}"))
            .to_owned()
    }

    pub fn prompt(&mut self, session: &str, text: &str) -> Value {
        self.call("session/prompt", &prompt_params(session, text))
    }

    /// Hangs up, and reads to the end of what the facade sent.
    pub fn hang_up(&mut self) {
        self.to_agent = None;
    }

    /// The `session/update` payloads seen so far, with the most recent arrivals read in.
    pub fn updates(&self) -> Vec<Value> {
        self.notifications
            .iter()
            .filter(|message| message["method"] == "session/update")
            .map(|message| message["params"]["update"].clone())
            .collect()
    }

    /// The text of every `agent_message_chunk` seen so far.
    pub fn text(&self) -> String {
        self.updates()
            .iter()
            .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
            .map(|update| update["content"]["text"].as_str().unwrap().to_owned())
            .collect()
    }
}

pub fn prompt_params(session: &str, text: &str) -> Value {
    json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]})
}

/// The JSON-RPC error's detail, as a client shows it.
pub fn detail(answer: &Value) -> &str {
    answer["error"]["data"]
        .as_str()
        .unwrap_or_else(|| panic!("no error detail: {answer}"))
}

/// A connection's thread, within the time a test waits.
pub fn joined(served: JoinHandle<Result<(), Error>>) -> Result<(), Error> {
    let deadline = std::time::Instant::now() + WAIT;
    while !served.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the connection did not end"
        );
        thread::sleep(Duration::from_millis(5));
    }
    served
        .join()
        .expect("the connection's thread did not panic")
}
