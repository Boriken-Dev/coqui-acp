//! The ACP agent against a scripted harness: lifetimes, cancels, bounds and permissions.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_harness::event::{
    Event, OptionKind, Outcome, PermissionOption, PermissionRequest, Responder, Stop, ToolCall,
    TurnEnd,
};
use coqui_acp_harness::{
    AgentInfo, ConfigOption, Conversation, EventSink, Harness, HarnessError, OpenRequest, Settings,
    Takes, serve,
};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A turn ends one second after it starts, cancelled if interrupted meanwhile.
    Slow,
    /// A turn asks one permission and ends once it is answered.
    Asks,
    /// A turn sends as many chunks as it can.
    Floods,
    /// A turn never ends.
    Hangs,
    /// A turn asks one permission with a description and no tool title.
    AsksInDetail,
    /// A turn asks five permissions at once and ends once each is answered.
    AsksFive,
}

#[derive(Clone)]
struct Fake {
    mode: Mode,
    open_delay: Duration,
    opened: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<String>>>,
    sent: Arc<AtomicUsize>,
    /// An interrupt does not return for this long.
    stuck_interrupt: Duration,
    /// Chunks the opening sends before the session's answer.
    open_chunks: usize,
}

impl Fake {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            open_delay: Duration::ZERO,
            opened: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            log: Arc::new(Mutex::new(Vec::new())),
            sent: Arc::new(AtomicUsize::new(0)),
            stuck_interrupt: Duration::ZERO,
            open_chunks: 0,
        }
    }
}

struct FakeConversation {
    fake: Fake,
    id: String,
    sink: EventSink,
    interrupted: Arc<AtomicBool>,
}

impl Drop for FakeConversation {
    fn drop(&mut self) {
        self.fake.dropped.store(true, Ordering::SeqCst);
    }
}

impl Harness for Fake {
    fn info(&self) -> AgentInfo {
        AgentInfo::new("fake", "Fake", "0")
    }

    fn takes(&self) -> Takes {
        Takes::new().additional_directories(true)
    }

    fn open(
        &self,
        _request: OpenRequest,
        events: EventSink,
    ) -> Result<Box<dyn Conversation>, HarnessError> {
        std::thread::sleep(self.open_delay);
        for _ in 0..self.open_chunks {
            events.send(Event::MessageChunk {
                message: "early".into(),
                text: "x".into(),
            });
        }
        let number = self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeConversation {
            fake: self.clone(),
            id: format!("session-{number}"),
            sink: events,
            interrupted: Arc::new(AtomicBool::new(false)),
        }))
    }
}

impl Conversation for FakeConversation {
    fn session_id(&self) -> String {
        self.id.clone()
    }

    fn options(&self) -> Vec<ConfigOption> {
        Vec::new()
    }

    fn prompt(&self, _content: Vec<coqui_acp_harness::event::Content>) -> Result<(), HarnessError> {
        self.fake.log.lock().unwrap().push("prompt".into());
        let sink = self.sink.clone();
        let interrupted = Arc::clone(&self.interrupted);
        let fake = self.fake.clone();
        std::thread::spawn(move || match fake.mode {
            Mode::Slow => {
                std::thread::sleep(Duration::from_secs(1));
                let stop = if interrupted.load(Ordering::SeqCst) {
                    Stop::Cancelled
                } else {
                    Stop::EndTurn
                };
                sink.send(Event::TurnEnd(TurnEnd::Stopped(stop)));
            }
            Mode::Asks => {
                let (answered, outcome) = mpsc::channel();
                let responder = Responder::new(move |chosen| {
                    let _ = answered.send(chosen);
                    Ok(())
                });
                let options = vec![
                    PermissionOption::new("yes", "Allow", OptionKind::AllowOnce),
                    PermissionOption::new("no", "Reject", OptionKind::RejectOnce),
                ];
                sink.send(Event::Permission(PermissionRequest::new(
                    ToolCall::new("t1"),
                    "Edit",
                    None,
                    options,
                    responder,
                )));
                let chosen = outcome
                    .recv()
                    .map_or_else(|_| "dropped".to_string(), |chosen| format!("{chosen:?}"));
                fake.log.lock().unwrap().push(format!("answered {chosen}"));
                sink.send(Event::TurnEnd(TurnEnd::Stopped(Stop::EndTurn)));
            }
            Mode::Floods => {
                for number in 0..200_000 {
                    let event = Event::MessageChunk {
                        message: "m".into(),
                        text: "x".repeat(64),
                    };
                    if !sink.send(event) {
                        return;
                    }
                    fake.sent.store(number + 1, Ordering::SeqCst);
                }
            }
            Mode::Hangs => {}
            Mode::AsksInDetail => {
                let (answered, outcome) = mpsc::channel();
                let responder = Responder::new(move |chosen| {
                    let _ = answered.send(chosen);
                    Ok(())
                });
                sink.send(Event::Permission(PermissionRequest::new(
                    ToolCall::new("t1"),
                    "Write under /x",
                    Some("Because of y".into()),
                    vec![PermissionOption::new("yes", "Allow", OptionKind::AllowOnce)],
                    responder,
                )));
                let _ = outcome.recv();
                sink.send(Event::TurnEnd(TurnEnd::Stopped(Stop::EndTurn)));
            }
            Mode::AsksFive => {
                let (answered, outcomes) = mpsc::channel();
                for number in 0..5 {
                    let answered = answered.clone();
                    let responder = Responder::new(move |chosen| {
                        let _ = answered.send(chosen);
                        Ok(())
                    });
                    sink.send(Event::Permission(PermissionRequest::new(
                        ToolCall::new(format!("t{number}")),
                        "Edit",
                        None,
                        vec![PermissionOption::new("yes", "Allow", OptionKind::AllowOnce)],
                        responder,
                    )));
                }
                for _ in 0..5 {
                    let chosen = outcomes.recv();
                    fake.log
                        .lock()
                        .unwrap()
                        .push(format!("answered {chosen:?}"));
                }
                sink.send(Event::TurnEnd(TurnEnd::Stopped(Stop::EndTurn)));
            }
        });
        Ok(())
    }

    fn interrupt(&self) -> Result<(), HarnessError> {
        self.fake.log.lock().unwrap().push("interrupt".into());
        self.interrupted.store(true, Ordering::SeqCst);
        std::thread::sleep(self.fake.stuck_interrupt);
        Ok(())
    }

    fn set_option(&self, _id: &str, _value: &str) -> Result<Vec<ConfigOption>, HarnessError> {
        Ok(Vec::new())
    }

    fn close(&self) {
        self.fake.log.lock().unwrap().push("close".into());
    }
}

/// A client by hand, over pipes to a `serve` on a thread of its own.
struct Client {
    to_agent: std::io::PipeWriter,
    from_agent: Receiver<Value>,
    next: u64,
    seen: Vec<Value>,
    /// Set, the client stops reading what the agent writes.
    paused: Arc<AtomicBool>,
}

impl Client {
    fn serve(harness: Fake, settings: Settings) -> Self {
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || serve(Arc::new(harness), settings, agent_reads, agent_writes));
        let (sender, from_agent) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let pausing = Arc::clone(&paused);
        std::thread::spawn(move || {
            let mut lines = BufReader::new(client_reads).lines();
            loop {
                while pausing.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let Some(Ok(line)) = lines.next() else { return };
                if sender.send(serde_json::from_str(&line).unwrap()).is_err() {
                    return;
                }
            }
        });
        Self {
            to_agent: client_writes,
            from_agent,
            next: 0,
            seen: Vec::new(),
            paused,
        }
    }

    fn send(&mut self, message: Value) -> u64 {
        self.next += 1;
        let mut message = message;
        message["jsonrpc"] = json!("2.0");
        if message.get("method").is_some()
            && message.get("id").is_none()
            && message.get("notification").is_none()
        {
            message["id"] = json!(self.next);
        }
        if let Some(object) = message.as_object_mut() {
            object.remove("notification");
        }
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.to_agent.write_all(&line).unwrap();
        self.next
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.send(json!({"method": method, "params": params}))
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"method": method, "params": params, "notification": true}));
    }

    /// The next message that satisfies `wanted`, keeping the others in `seen`.
    fn until(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        if let Some(at) = self.seen.iter().position(&wanted) {
            return self.seen.remove(at);
        }
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let message = self
                .from_agent
                .recv_timeout(left)
                .expect("the message did not come");
            if wanted(&message) {
                return message;
            }
            self.seen.push(message);
        }
    }

    fn answer(&mut self, id: u64) -> Value {
        self.until(|message| message["id"] == id && message.get("method").is_none())
    }

    fn open(&mut self, version: u64) -> String {
        let initialize = self.request("initialize", json!({"protocolVersion": version}));
        self.answer(initialize);
        let new = self.request("session/new", json!({"cwd": cwd()}));
        let answer = self.answer(new);
        answer["result"]["sessionId"]
            .as_str()
            .expect("a session")
            .to_string()
    }
}

fn cwd() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn idle(message: &Value) -> bool {
    message["method"] == "session/update"
        && message["params"]["update"]["sessionUpdate"] == "state_update"
        && message["params"]["update"]["state"] == "idle"
}

#[test]
fn a_closed_session_lets_go_of_its_conversation() {
    let fake = Fake::new(Mode::Slow);
    let dropped = Arc::clone(&fake.dropped);
    let mut client = Client::serve(fake, Settings::new());
    let session = client.open(2);
    let close = client.request("session/close", json!({"sessionId": session}));
    client.answer(close);
    let until = Instant::now() + Duration::from_secs(5);
    while !dropped.load(Ordering::SeqCst) {
        assert!(Instant::now() < until, "the conversation was never dropped");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_cancel_right_after_a_prompt_interrupts_that_turn_after_it_was_sent() {
    let fake = Fake::new(Mode::Slow);
    let log = Arc::clone(&fake.log);
    let mut client = Client::serve(fake, Settings::new());
    let session = client.open(2);
    // A large prompt takes the agent a moment to read; the cancel is on the next line.
    let big = "x".repeat(4 * 1024 * 1024);
    client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": big}]}),
    );
    client.notify("session/cancel", json!({"sessionId": session}));
    let ended = client.until(idle);
    assert_eq!(ended["params"]["update"]["stopReason"], "cancelled");
    let log = log.lock().unwrap().clone();
    let prompted = log
        .iter()
        .position(|entry| entry == "prompt")
        .expect("prompted");
    let interrupted = log
        .iter()
        .position(|entry| entry == "interrupt")
        .expect("the cancel was lost");
    assert!(
        prompted < interrupted,
        "the interrupt reached the harness before the prompt: {log:?}"
    );
}

#[test]
fn opening_at_once_never_goes_past_the_session_limit() {
    let mut fake = Fake::new(Mode::Slow);
    fake.open_delay = Duration::from_millis(300);
    let opened = Arc::clone(&fake.opened);
    let mut settings = Settings::new();
    settings.max_sessions = 2;
    let mut client = Client::serve(fake, settings);
    let initialize = client.request("initialize", json!({"protocolVersion": 2}));
    client.answer(initialize);
    let asked: Vec<u64> = (0..10)
        .map(|_| client.request("session/new", json!({"cwd": cwd()})))
        .collect();
    let opened_ok = asked
        .into_iter()
        .filter(|id| client.answer(*id).get("result").is_some())
        .count();
    assert_eq!(opened_ok, 2);
    assert!(
        opened.load(Ordering::SeqCst) <= 2,
        "the harness was asked {} times",
        opened.load(Ordering::SeqCst)
    );
}

#[test]
fn a_client_that_does_not_read_holds_back_the_harness() {
    let fake = Fake::new(Mode::Floods);
    let sent = Arc::clone(&fake.sent);
    let mut settings = Settings::new();
    settings.max_queued_events = 100;
    let mut client = Client::serve(fake, settings);
    let session = client.open(2);
    // Nothing more is read: the pipe fills, then the queue.
    client.paused.store(true, Ordering::SeqCst);
    client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    std::thread::sleep(Duration::from_secs(2));
    let queued = sent.load(Ordering::SeqCst);
    assert!(
        queued < 50_000,
        "{queued} events were taken from a harness whose client reads nothing"
    );
}

#[test]
fn a_permission_left_open_by_a_cancel_is_answered_cancelled_whatever_comes_later() {
    let fake = Fake::new(Mode::Asks);
    let log = Arc::clone(&fake.log);
    let mut settings = Settings::new();
    settings.cancel_grace = Duration::from_millis(200);
    let mut client = Client::serve(fake, settings);
    let session = client.open(2);
    client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    let asked = client.until(|message| message["method"] == "session/request_permission");
    client.notify("session/cancel", json!({"sessionId": session}));
    client.until(idle);
    client.send(json!({"id": asked["id"], "result": {"outcome": {"outcome": "selected", "optionId": "yes"}}}));
    let until = Instant::now() + Duration::from_secs(5);
    let answered = loop {
        if let Some(entry) = log
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.starts_with("answered"))
        {
            break entry.clone();
        }
        assert!(Instant::now() < until, "the harness was never answered");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(answered, "answered Cancelled");
}

#[test]
fn a_version_1_prompt_running_when_its_session_closes_is_answered_cancelled() {
    let fake = Fake::new(Mode::Hangs);
    let mut client = Client::serve(fake, Settings::new());
    let session = client.open(1);
    let prompt = client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    let close = client.request("session/close", json!({"sessionId": session}));
    client.answer(close);
    let answer = client.answer(prompt);
    assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
}

#[test]
fn what_a_client_may_choose_can_be_narrowed_by_the_host() {
    let mut settings = Settings::new();
    settings.client_mcp_servers = false;
    settings.working_roots = vec![std::env::temp_dir()];
    let mut client = Client::serve(Fake::new(Mode::Slow), settings);
    let initialize = client.request("initialize", json!({"protocolVersion": 2}));
    client.answer(initialize);
    let command = std::env::temp_dir()
        .join("server")
        .to_string_lossy()
        .into_owned();
    let server = client.request(
        "session/new",
        json!({"cwd": cwd(), "mcpServers": [{"type": "stdio", "name": "a", "command": command, "args": [], "env": []}]}),
    );
    assert_eq!(
        client.answer(server)["error"]["code"],
        -32602,
        "MCP servers from the client are off"
    );
    let elsewhere = if cfg!(windows) { "C:\\Windows" } else { "/etc" };
    let outside = client.request("session/new", json!({"cwd": elsewhere}));
    assert_eq!(
        client.answer(outside)["error"]["code"],
        -32602,
        "outside the working roots"
    );
    let directory = client.request(
        "session/new",
        json!({"cwd": cwd(), "additionalDirectories": [elsewhere]}),
    );
    assert_eq!(
        client.answer(directory)["error"]["code"],
        -32602,
        "a directory outside the working roots"
    );
    let inside = client.request("session/new", json!({"cwd": cwd()}));
    assert!(client.answer(inside).get("result").is_some());
}

#[test]
fn a_network_path_is_refused_unless_the_host_allows_it() {
    let mut client = Client::serve(Fake::new(Mode::Slow), Settings::new());
    let initialize = client.request("initialize", json!({"protocolVersion": 2}));
    client.answer(initialize);
    for path in [
        "\\\\host\\share\\w",
        "//host/share/w",
        "\\\\?\\C:\\w",
        "\\\\.\\pipe\\x",
    ] {
        let new = client.request("session/new", json!({"cwd": path}));
        assert_eq!(client.answer(new)["error"]["code"], -32602, "{path}");
    }
}

#[test]
fn a_responder_dropped_unanswered_answers_cancelled() {
    let (answered, outcome) = mpsc::channel();
    let responder = Responder::new(move |chosen| {
        let _ = answered.send(chosen);
        Ok(())
    });
    drop(responder);
    assert_eq!(outcome.try_recv(), Ok(Outcome::Cancelled));
}

#[test]
fn a_version_1_question_keeps_its_title_and_description() {
    let mut client = Client::serve(Fake::new(Mode::AsksInDetail), Settings::new());
    let session = client.open(1);
    client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    let asked = client.until(|message| message["method"] == "session/request_permission");
    let said = asked["params"]["toolCall"].to_string();
    assert!(said.contains("Write under /x"), "{said}");
    assert!(said.contains("Because of y"), "{said}");
    assert_eq!(asked["params"]["toolCall"]["title"], "Write under /x");
}

#[test]
fn an_interrupt_that_does_not_return_holds_up_nothing_else() {
    let mut fake = Fake::new(Mode::Hangs);
    fake.stuck_interrupt = Duration::from_secs(30);
    let mut client = Client::serve(fake, Settings::new());
    let session = client.open(2);
    let prompt = client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    client.answer(prompt);
    client.notify("session/cancel", json!({"sessionId": session}));
    let started = Instant::now();
    let close = client.request("session/close", json!({"sessionId": session}));
    client.answer(close);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the connection waited {:?}",
        started.elapsed()
    );
}

#[test]
fn questions_past_the_limit_are_answered_cancelled_without_being_asked() {
    let fake = Fake::new(Mode::AsksFive);
    let log = Arc::clone(&fake.log);
    let mut settings = Settings::new();
    settings.max_open_questions = 2;
    let mut client = Client::serve(fake, settings);
    let session = client.open(2);
    client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    let until = Instant::now() + Duration::from_secs(10);
    while log
        .lock()
        .unwrap()
        .iter()
        .filter(|entry| entry.contains("Cancelled"))
        .count()
        < 3
    {
        assert!(Instant::now() < until, "{:?}", log.lock().unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(message) = client.from_agent.try_recv() {
        client.seen.push(message);
    }
    let asked = client
        .seen
        .iter()
        .filter(|message| message["method"] == "session/request_permission")
        .count();
    assert_eq!(asked, 2);
}

#[test]
fn what_comes_before_a_sessions_answer_is_bounded_and_says_so() {
    let mut fake = Fake::new(Mode::Slow);
    fake.open_chunks = 100;
    let mut settings = Settings::new();
    settings.max_queued_events = 8;
    let mut client = Client::serve(fake, settings);
    let session = client.open(2);
    let prompt = client.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}),
    );
    client.answer(prompt);
    client.until(idle);
    let chunks = client
        .seen
        .iter()
        .filter(|message| message["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .count();
    assert_eq!(chunks, 8);
    assert!(
        client
            .seen
            .iter()
            .any(|message| message["params"]["update"]["sessionUpdate"] == "notice"),
        "{:?}",
        client.seen
    );
}

#[test]
fn a_servers_debug_shows_no_credential_of_its_address_or_arguments() {
    let http = coqui_acp_harness::McpServer::new(
        "docs",
        coqui_acp_harness::McpTransport::Http {
            url: "https://user:hunter2@docs.example/mcp?key=hunter2".into(),
            headers: vec![],
        },
    );
    let stdio = coqui_acp_harness::McpServer::new(
        "files",
        coqui_acp_harness::McpTransport::Stdio {
            command: "/bin/files".into(),
            args: vec!["--token=hunter2".into()],
            env: vec![],
        },
    );
    for shown in [
        format!("{http:?}"),
        format!("{:?}", http.transport),
        format!("{stdio:?}"),
        format!("{:?}", stdio.transport),
    ] {
        assert!(!shown.contains("hunter2"), "{shown}");
    }
    assert!(format!("{http:?}").contains("docs.example"));
    assert!(format!("{stdio:?}").contains("/bin/files"));
}
