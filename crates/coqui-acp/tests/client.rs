//! The client against a scripted agent, over a real JSON-RPC line transport. The conversations are shared with the Dart suite.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coqui_acp::{
    Answer, Authenticate, ChoiceOption, Client, Decider, Decision, DescribeLimits, Elicitation,
    ElicitationAnswer, Elicitor, Error, Event, Exchange, FieldKind, PermissionRequest, Session,
    SessionList,
};
use futures::SinkExt;
use futures::channel::mpsc as async_mpsc;
use serde_json::{Map, Value, json};

const WAIT: Duration = Duration::from_secs(10);

type Transport = coqui_acp::sdk::Lines<
    futures::sink::SinkMapErr<
        async_mpsc::UnboundedSender<String>,
        fn(async_mpsc::SendError) -> io::Error,
    >,
    async_mpsc::UnboundedReceiver<io::Result<String>>,
>;

/// The agent's end of a transport: lines in, lines out.
struct Agent {
    from_client: async_mpsc::UnboundedReceiver<String>,
    to_client: async_mpsc::UnboundedSender<io::Result<String>>,
    next_id: u64,
}

impl Agent {
    fn pair() -> (Self, Transport) {
        let (to_agent, from_client) = async_mpsc::unbounded::<String>();
        let (to_client, from_agent) = async_mpsc::unbounded::<io::Result<String>>();
        let outgoing = to_agent.sink_map_err((|error| io::Error::other(error)) as fn(_) -> _);
        (
            Self {
                from_client,
                to_client,
                next_id: 1000,
            },
            coqui_acp::sdk::Lines::new(outgoing, from_agent),
        )
    }

    /// The next message from the client; `None` when it hung up.
    fn read(&mut self) -> Option<Value> {
        let line = futures::executor::block_on(futures::StreamExt::next(&mut self.from_client))?;
        Some(serde_json::from_str(&line).expect("the client writes JSON"))
    }

    fn write(&self, message: &Value) {
        let _ = self.to_client.unbounded_send(Ok(message.to_string()));
    }

    fn reply(&self, id: &Value, result: &Value) {
        self.write(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn fail(&self, id: &Value, error: &Value) {
        self.write(&json!({"jsonrpc": "2.0", "id": id, "error": error}));
    }

    fn notify(&self, method: &str, params: &Value) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Asks the client something and returns the response's result.
    fn ask(&mut self, method: &str, params: &Value) -> Value {
        self.ask_raw(method, params)["result"].clone()
    }

    /// The whole answer, which may be an error.
    fn ask_raw(&mut self, method: &str, params: &Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.read().expect("the client answers");
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
            }
        }
    }
}

/// Every key of `subset` holds the same value in `value`, recursively.
fn holds(value: &Value, subset: &Value) -> bool {
    match (value, subset) {
        (Value::Object(have), Value::Object(want)) => want
            .iter()
            .all(|(key, wanted)| have.get(key).is_some_and(|got| holds(got, wanted))),
        _ => value == subset,
    }
}

struct Fixed(Decision);

impl Decider for Fixed {
    fn decide(&self, _request: &PermissionRequest) -> Decision {
        self.0.clone()
    }
}

/// Plays a conversation's steps as the agent, then answers `session/close` and whatever else arrives.
fn play(mut agent: Agent, steps: Vec<Value>) {
    let mut held = None;
    for step in steps {
        if let Some(expect) = step.get("expect") {
            let message = agent
                .read()
                .expect("the client sends what the script expects");
            assert_eq!(
                message["method"], expect["method"],
                "the message was {message}"
            );
            if let Some(params) = expect.get("params") {
                assert!(
                    holds(&message["params"], params),
                    "params {} should hold {params}",
                    message["params"]
                );
            }
            let id = message["id"].clone();
            if let Some(reply) = step.get("reply") {
                agent.reply(&id, reply);
            } else if let Some(error) = step.get("error") {
                agent.fail(&id, error);
            } else if step.get("hold").is_some() {
                held = Some(id);
            }
        } else if let Some(send) = step.get("send") {
            agent.notify(send["method"].as_str().unwrap(), &send["params"]);
        } else if let Some(ask) = step.get("ask") {
            let answer = agent.ask(ask["method"].as_str().unwrap(), &ask["params"]);
            assert!(
                holds(&answer, &step["answer"]),
                "answer {answer} should hold {}",
                step["answer"]
            );
        } else if let Some(result) = step.get("release") {
            agent.reply(&held.take().expect("a held request"), result);
        }
    }
    while let Some(message) = agent.read() {
        if message.get("id").is_some() && message.get("method").is_some() {
            agent.reply(&message["id"], &json!({}));
        }
    }
}

fn matches_event(event: &Event, want: &Value) -> bool {
    let (kind, got) = match event {
        Event::Text(text) => ("text", json!({"text": text})),
        Event::Tool { title, kind, .. } => ("tool", json!({"title": title, "kind": kind})),
        Event::Cost { amount, currency } => {
            ("cost", json!({"amount": amount, "currency": currency}))
        }
        Event::Permission { allowed, .. } => ("permission", json!({"allowed": allowed})),
        Event::Completed { stop } => ("completed", json!({"stop": stop})),
        Event::Failed(_) => ("failed", json!({})),
        Event::Cancelled => ("cancelled", json!({})),
        Event::Blocked(_) => ("blocked", json!({})),
        Event::OutputLost { lines, bytes } => {
            ("output_lost", json!({"lines": lines, "bytes": bytes}))
        }
        _ => ("other", json!({})),
    };
    let mut wanted: Map<String, Value> = want.as_object().unwrap().clone();
    wanted.remove("type").as_ref().and_then(Value::as_str) == Some(kind)
        && holds(&got, &Value::Object(wanted))
}

fn events_until(session: &Session, count: usize) -> Vec<Event> {
    let mut got = Vec::new();
    while got.len() < count {
        match session.next_event(WAIT) {
            Some(event) => got.push(event),
            None => panic!("after {got:?} no more events"),
        }
    }
    got
}

#[test]
fn every_v1_conversation_holds() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../vectors/conversations.json")).unwrap();
    let mut ran = 0;
    for conversation in vectors["conversations"].as_array().unwrap() {
        if conversation["version"] != 1 {
            continue;
        }
        ran += 1;
        let name = conversation["name"].as_str().unwrap();
        let (agent, transport) = Agent::pair();
        let steps = conversation["steps"].as_array().unwrap().clone();
        let actor = thread::spawn(move || play(agent, steps));
        let client = &conversation["client"];
        let decision = if client["decider"] == "allow" {
            Decision::Allow
        } else {
            Decision::Deny("no".to_owned())
        };
        let session = Client::builder(client["cwd"].as_str().unwrap())
            .prompt(client["prompt"].as_str().unwrap())
            .decider(Arc::new(Fixed(decision)))
            .connect(transport)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let want = conversation["events"].as_array().unwrap();
        let got = events_until(&session, want.len());
        for (event, wanted) in got.iter().zip(want) {
            assert!(
                matches_event(event, wanted),
                "{name}: {event:?} should match {wanted}"
            );
        }
        session.close();
        drop(session);
        actor
            .join()
            .unwrap_or_else(|_| panic!("{name}: the script failed"));
    }
    assert!(ran >= 8, "only {ran} v1 conversations ran");
}

/// Answers as a vector says, and keeps what it was asked.
struct Scripted {
    answer: ElicitationAnswer,
    asked: Mutex<Option<Elicitation>>,
}

impl Elicitor for Scripted {
    fn elicit(&self, request: &Elicitation) -> ElicitationAnswer {
        *self.asked.lock().unwrap() = Some(request.clone());
        self.answer.clone()
    }
}

/// An elicitation as `vectors/elicitation.json` writes what the elicitor is given.
fn normalized(request: &Elicitation) -> Value {
    fn options(options: &[ChoiceOption]) -> Value {
        options
            .iter()
            .map(|option| {
                let mut kept = json!({"value": option.value, "title": option.title});
                if let Some(description) = &option.description {
                    kept["description"] = json!(description);
                }
                kept
            })
            .collect()
    }
    let fields: Vec<Value> = request
        .fields
        .iter()
        .map(|field| {
            let mut kept = Map::new();
            kept.insert("name".to_owned(), json!(field.name));
            let mut put = |key: &str, value: Value| {
                if !value.is_null() {
                    kept.insert(key.to_owned(), value);
                }
            };
            let kind = match &field.kind {
                FieldKind::Text {
                    min_length,
                    max_length,
                    format,
                    default,
                } => {
                    put("minLength", json!(min_length));
                    put("maxLength", json!(max_length));
                    put("format", json!(format));
                    put("default", json!(default));
                    "text"
                }
                FieldKind::Choice {
                    options: offered,
                    default,
                } => {
                    put("options", options(offered));
                    put("default", json!(default));
                    "choice"
                }
                FieldKind::Choices {
                    options: offered,
                    min_items,
                    max_items,
                    default,
                } => {
                    put("options", options(offered));
                    put("minItems", json!(min_items));
                    put("maxItems", json!(max_items));
                    put("default", json!(default));
                    "choices"
                }
                FieldKind::Number {
                    minimum,
                    maximum,
                    default,
                } => {
                    put("minimum", json!(minimum));
                    put("maximum", json!(maximum));
                    put("default", json!(default));
                    "number"
                }
                FieldKind::Integer {
                    minimum,
                    maximum,
                    default,
                } => {
                    put("minimum", json!(minimum));
                    put("maximum", json!(maximum));
                    put("default", json!(default));
                    "integer"
                }
                FieldKind::Boolean { default } => {
                    put("default", json!(default));
                    "boolean"
                }
                _ => "unsupported",
            };
            put("type", json!(kind));
            put("required", json!(field.required));
            put("title", json!(field.title));
            put("description", json!(field.description));
            Value::Object(kept)
        })
        .collect();
    let mut asked =
        json!({"sessionId": request.session_id, "message": request.message, "fields": fields});
    for (key, value) in [
        ("toolCallId", &request.tool_call_id),
        ("title", &request.title),
        ("description", &request.description),
    ] {
        if let Some(value) = value {
            asked[key] = json!(value);
        }
    }
    asked
}

/// Equal as JSON, a number compared by its value whichever way it was written.
fn same(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Number(got), Value::Number(want)) => got.as_f64() == want.as_f64(),
        (Value::Array(got), Value::Array(want)) => {
            got.len() == want.len() && got.iter().zip(want).all(|(got, want)| same(got, want))
        }
        (Value::Object(got), Value::Object(want)) => {
            got.len() == want.len()
                && want
                    .iter()
                    .all(|(key, want)| got.get(key).is_some_and(|got| same(got, want)))
        }
        _ => got == want,
    }
}

#[test]
fn every_elicitation_vector_holds() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../vectors/elicitation.json")).unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap().to_owned();
        let (mut agent, transport) = Agent::pair();
        let params = case["params"].clone();
        let advertised = vectors["advertised"]["elicitation"].clone();
        let gives = case["elicitor"] == true;
        let named = name.clone();
        // The agent asks once the client has its session, as an agent asks during a turn.
        let (opened, open) = mpsc::channel::<()>();
        let actor = thread::spawn(move || {
            let init = agent.read().expect("initialize");
            let capability = &init["params"]["clientCapabilities"]["elicitation"];
            assert_eq!(
                *capability,
                if gives { advertised } else { Value::Null },
                "{named}"
            );
            agent.reply(
                &init["id"],
                &json!({"protocolVersion": 1, "agentCapabilities": {}}),
            );
            let new = agent.read().expect("session/new");
            agent.reply(&new["id"], &json!({"sessionId": "s1"}));
            open.recv().expect("the client opened its session");
            let said = agent.ask_raw("elicitation/create", &params);
            thread::spawn(move || while agent.read().is_some() {});
            said
        });
        let answer = match case["answer"].get("action").and_then(Value::as_str) {
            Some("accept") => {
                ElicitationAnswer::Accept(case["answer"]["content"].as_object().unwrap().clone())
            }
            Some("decline") => ElicitationAnswer::Decline,
            _ => ElicitationAnswer::Cancel,
        };
        let elicitor = Arc::new(Scripted {
            answer,
            asked: Mutex::new(None),
        });
        let mut builder = Client::builder("/w");
        if gives {
            builder = builder.elicitor(elicitor.clone());
        }
        if let Some(fields) = case["limits"]["fields"].as_u64() {
            builder = builder.max_elicitation_fields(usize::try_from(fields).unwrap());
        }
        if let Some(options) = case["limits"]["options"].as_u64() {
            builder = builder.max_elicitation_options(usize::try_from(options).unwrap());
        }
        let session = builder
            .connect(transport)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        opened.send(()).unwrap();
        let said = actor
            .join()
            .unwrap_or_else(|_| panic!("{name}: the agent failed"));
        match case["response"].get("error") {
            Some(error) => assert_eq!(said["error"]["code"], error["code"], "{name}: {said}"),
            None => assert!(
                same(&said["result"], &case["response"]["result"]),
                "{name}: {said}"
            ),
        }
        let asked = elicitor.asked.lock().unwrap().as_ref().map(normalized);
        match (&asked, &case["asked"]) {
            (None, Value::Null) => {}
            (Some(got), want) => assert!(same(got, want), "{name}: asked {got}"),
            (None, want) => panic!("{name}: the elicitor was not asked {want}"),
        }
        let mut blocked = false;
        while let Some(event) = session.next_event(Duration::from_millis(50)) {
            blocked |= matches!(event, Event::Blocked(_));
        }
        assert_eq!(blocked, case["blocked"] == true, "{name}");
        session.close();
    }
}

/// An agent answering by a closure, for what a script does not cover.
fn agent_with(
    answer: impl Fn(&str, &Value, &Agent) -> Option<Value> + Send + 'static,
) -> (Transport, Receiver<Value>) {
    let (mut agent, transport) = Agent::pair();
    let (seen, messages): (Sender<Value>, Receiver<Value>) = mpsc::channel();
    thread::spawn(move || {
        while let Some(message) = agent.read() {
            let _ = seen.send(message.clone());
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if let Some(id) = message.get("id") {
                match answer(method, &message["params"], &agent) {
                    Some(result) => agent.reply(id, &result),
                    None => agent.fail(id, &json!({"code": -32601, "message": "no such method"})),
                }
            }
        }
    });
    (transport, messages)
}

fn plain(method: &str) -> Option<Value> {
    match method {
        "initialize" => Some(json!({"protocolVersion": 1, "agentCapabilities": {}})),
        "session/new" => Some(json!({"sessionId": "s1"})),
        "session/prompt" => Some(json!({"stopReason": "end_turn"})),
        _ => Some(json!({})),
    }
}

#[test]
fn an_agent_of_another_version_is_refused() {
    let (transport, _) = agent_with(|method, _, _| match method {
        "initialize" => Some(json!({"protocolVersion": 2, "agentCapabilities": {}})),
        other => plain(other),
    });
    let error = Client::builder("/w").connect(transport).unwrap_err();
    // The SDK refuses a negotiated version it was not built for, before this library sees the answer.
    assert!(error.to_string().contains("version"), "{error:?}");
}

#[test]
fn an_agent_without_http_mcp_refuses_a_session_that_needs_it() {
    let (transport, _) = agent_with(|method, _, _| plain(method));
    let error = Client::builder("/w")
        .mcp_server(
            json!({"type": "http", "name": "tools", "url": "https://x.example/", "headers": []}),
        )
        .connect(transport)
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
}

#[test]
fn a_silent_initialize_times_out_with_its_name() {
    let (agent, transport) = Agent::pair();
    let error = Client::builder("/w")
        .initialize_timeout(Some(Duration::from_millis(100)))
        .connect(transport)
        .unwrap_err();
    assert_eq!(
        error,
        Error::Timeout {
            what: "initialize".to_owned(),
            limit: Duration::from_millis(100),
        }
    );
    assert_eq!(error.to_string(), "initialize: no answer within 100 ms");
    drop(agent);
}

/// A real adapter goes ahead with the tool on an error answer, so a request this client cannot read has to be refused as an outcome.
#[test]
fn a_permission_request_that_cannot_be_read_is_cancelled_never_an_error() {
    let (agent, transport) = Agent::pair();
    let answer = Arc::new(Mutex::new(None));
    let kept = answer.clone();
    thread::spawn(move || {
        let mut agent = agent;
        while let Some(message) = agent.read() {
            let id = message.get("id").cloned();
            match message.get("method").and_then(Value::as_str) {
                Some("session/prompt") => {
                    let said = agent.ask_raw(
                        "session/request_permission",
                        &json!({"sessionId": "s1", "toolCall": "not an object"}),
                    );
                    *kept.lock().unwrap() = Some(said);
                    agent.reply(id.as_ref().unwrap(), &json!({"stopReason": "end_turn"}));
                }
                Some(method) if id.is_some() => {
                    agent.reply(id.as_ref().unwrap(), &plain(method).unwrap());
                }
                _ => {}
            }
        }
    });
    let session = Client::builder("/w")
        .prompt("go")
        .decider(Arc::new(Fixed(Decision::Allow)))
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 2);
    assert!(matches!(&events[0], Event::Blocked(_)), "{events:?}");
    let said = answer.lock().unwrap().clone().unwrap();
    assert_eq!(said["result"]["outcome"]["outcome"], "cancelled", "{said}");
    assert!(said.get("error").is_none(), "{said}");
}

#[test]
fn model_and_effort_go_through_the_agents_options_and_a_gap_is_a_notice() {
    let (transport, messages) = agent_with(|method, _, _| match method {
        "session/new" => Some(json!({"sessionId": "s1", "configOptions": [
            {"id": "pick", "name": "Model", "category": "model", "type": "select", "currentValue": "a",
             "options": [{"value": "a", "name": "Alpha"}, {"value": "b", "name": "Beta"}]},
        ]})),
        other => plain(other),
    });
    let session = Client::builder("/w")
        .model("beta")
        .effort("high")
        .connect(transport)
        .unwrap();
    let event = session.next_event(WAIT).unwrap();
    assert!(
        matches!(&event, Event::Notice(text) if text.contains("effort")),
        "{event:?}"
    );
    let set: Vec<Value> = messages
        .try_iter()
        .filter(|m| m["method"] == "session/set_config_option")
        .collect();
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["params"]["configId"], "pick");
    assert_eq!(set[0]["params"]["value"], "b");
}

#[test]
fn a_resume_uses_session_resume_when_the_agent_offers_it() {
    let (transport, messages) = agent_with(|method, _, _| match method {
        "initialize" => Some(
            json!({"protocolVersion": 1, "agentCapabilities": {"sessionCapabilities": {"resume": {}}}}),
        ),
        other => plain(other),
    });
    let session = Client::builder("/w")
        .resume("old")
        .connect(transport)
        .unwrap();
    assert_eq!(session.id().as_deref(), Some("old"));
    assert!(
        messages
            .try_iter()
            .any(|m| m["method"] == "session/resume" && m["params"]["sessionId"] == "old")
    );
}

#[test]
fn a_resume_by_load_drops_what_the_agent_replays() {
    let (transport, _) = agent_with(|method, _, agent| match method {
        "initialize" => {
            Some(json!({"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}))
        }
        "session/load" => {
            agent.notify("session/update", &json!({"sessionId": "old", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "history"}}}));
            Some(json!({}))
        }
        "session/prompt" => {
            agent.notify("session/update", &json!({"sessionId": "old", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "new"}}}));
            Some(json!({"stopReason": "end_turn"}))
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .resume("old")
        .connect(transport)
        .unwrap();
    session.send("go").unwrap();
    let events = events_until(&session, 2);
    assert_eq!(events[0], Event::Text("new".to_owned()));
}

#[test]
fn an_explicit_load_delivers_the_replay() {
    let (transport, _) = agent_with(|method, _, agent| match method {
        "initialize" => {
            Some(json!({"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}))
        }
        "session/load" => {
            agent.notify("_runner/event", &json!({"seq": 1}));
            Some(json!({}))
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .load("old")
        .connect(transport)
        .unwrap();
    let event = session.next_event(WAIT).unwrap();
    assert_eq!(
        event,
        Event::Extension {
            method: "_runner/event".to_owned(),
            params: json!({"seq": 1})
        }
    );
}

#[test]
fn a_refused_kind_is_refused_without_asking_the_decider() {
    let outcome = Arc::new(Mutex::new(None));
    let (agent, transport) = Agent::pair();
    let kept = outcome.clone();
    thread::spawn(move || {
        let mut agent = agent;
        while let Some(message) = agent.read() {
            let id = message.get("id").cloned();
            match message.get("method").and_then(Value::as_str) {
                Some("session/prompt") => {
                    let answer = agent.ask("session/request_permission", &json!({
                        "sessionId": "s1",
                        "toolCall": {"toolCallId": "t1", "title": "Run", "kind": "execute", "status": "pending"},
                        "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"}, {"optionId": "no", "name": "No", "kind": "reject_once"}],
                    }));
                    *kept.lock().unwrap() = Some(answer);
                    agent.reply(id.as_ref().unwrap(), &json!({"stopReason": "end_turn"}));
                }
                Some(method) if id.is_some() => {
                    agent.reply(id.as_ref().unwrap(), &plain(method).unwrap())
                }
                _ => {}
            }
        }
    });
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .decider(Arc::new(Fixed(Decision::Allow)))
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 2);
    assert!(
        matches!(&events[0], Event::Permission { allowed: false, .. }),
        "{events:?}"
    );
    assert_eq!(
        outcome.lock().unwrap().as_ref().unwrap()["outcome"]["optionId"],
        "no"
    );
}

#[test]
fn a_refused_kind_that_runs_unasked_ends_the_session() {
    let (transport, messages) = agent_with(|method, _, agent| match method {
        "session/prompt" => {
            agent.notify("session/update", &json!({"sessionId": "s1", "update": {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run", "kind": "execute", "status": "in_progress"}}));
            None
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 2);
    assert!(
        matches!(&events[1], Event::Failed(text) if text.contains("without leave")),
        "{events:?}"
    );
    let until = Instant::now() + WAIT;
    let mut cancelled = false;
    while !cancelled && Instant::now() < until {
        cancelled = messages
            .recv_timeout(Duration::from_millis(200))
            .is_ok_and(|m| m["method"] == "session/cancel");
    }
    assert!(cancelled, "the agent should be told to stop");
}

#[test]
fn a_forgotten_tool_kind_is_said_once() {
    let (transport, _) = agent_with(|method, _, agent| match method {
        "session/prompt" => {
            for id in ["t1", "t2", "t3"] {
                agent.notify("session/update", &json!({"sessionId": "s1", "update": {"sessionUpdate": "tool_call", "toolCallId": id, "title": "Read", "kind": "read", "status": "pending"}}));
            }
            Some(json!({"stopReason": "end_turn"}))
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .prompt("go")
        .max_tool_kinds(1)
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 5);
    let notices = events
        .iter()
        .filter(|event| matches!(event, Event::Notice(text) if text.contains("1 tool")))
        .count();
    assert_eq!(notices, 1, "{events:?}");
    assert!(matches!(&events[4], Event::Completed { .. }), "{events:?}");
}

#[test]
fn a_refused_kind_the_session_cannot_keep_ends_it_and_is_still_refused() {
    let outcome = Arc::new(Mutex::new(None));
    let (agent, transport) = Agent::pair();
    let kept = outcome.clone();
    thread::spawn(move || {
        let mut agent = agent;
        while let Some(message) = agent.read() {
            let id = message.get("id").cloned();
            match message.get("method").and_then(Value::as_str) {
                Some("session/prompt") => {
                    agent.notify("session/update", &json!({"sessionId": "s1", "update": {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run", "kind": "execute", "status": "pending"}}));
                    let answer = agent.ask("session/request_permission", &json!({
                        "sessionId": "s1",
                        "toolCall": {"toolCallId": "t2", "title": "Run", "kind": "execute", "status": "pending"},
                        "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"}, {"optionId": "no", "name": "No", "kind": "reject_once"}],
                    }));
                    *kept.lock().unwrap() = Some(answer);
                    agent.reply(id.as_ref().unwrap(), &json!({"stopReason": "end_turn"}));
                }
                Some(method) if id.is_some() => {
                    agent.reply(id.as_ref().unwrap(), &plain(method).unwrap())
                }
                _ => {}
            }
        }
    });
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .max_tool_kinds(1)
        .decider(Arc::new(Fixed(Decision::Allow)))
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 3);
    assert!(
        matches!(&events[1], Event::Failed(text) if text.contains("refused kind")),
        "{events:?}"
    );
    assert!(
        matches!(&events[2], Event::Permission { allowed: false, .. }),
        "{events:?}"
    );
    let until = Instant::now() + WAIT;
    while outcome.lock().unwrap().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        outcome.lock().unwrap().as_ref().unwrap()["outcome"]["optionId"],
        "no"
    );
}

#[test]
fn turn_end_by_hands_turns_to_the_extension_and_ignores_standard_updates() {
    let (transport, _) = agent_with(|method, _, agent| match method {
        "initialize" => Some(
            json!({"protocolVersion": 1, "agentCapabilities": {"_meta": {"example/events": true}}}),
        ),
        "session/prompt" => {
            agent.notify("session/update", &json!({"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ignored"}}}));
            agent.notify("_example/event", &json!({"kind": "completed"}));
            Some(json!({"stopReason": "end_turn"}))
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .turn_end_by("example/events")
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert!(session.extension_active());
    let event = session.next_event(WAIT).unwrap();
    assert!(
        matches!(&event, Event::Extension { method, .. } if method == "_example/event"),
        "{event:?}"
    );
    assert_eq!(
        session.next_event(Duration::from_millis(200)),
        None,
        "no standard event should follow"
    );
}

#[test]
fn a_failed_prompt_under_an_extension_is_the_callers_to_judge() {
    let (transport, _) = agent_with(|method, _, _| match method {
        "initialize" => Some(
            json!({"protocolVersion": 1, "agentCapabilities": {"_meta": {"example/events": true}}}),
        ),
        "session/prompt" => None,
        other => plain(other),
    });
    let session = Client::builder("/w")
        .turn_end_by("example/events")
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::PromptFailed(_))
    ));
}

#[test]
fn notifications_and_cancel_reach_the_agent() {
    let (transport, messages) = agent_with(|method, _, _| plain(method));
    let session = Client::builder("/w").connect(transport).unwrap();
    session.notify("_example/ack", json!({"seq": 7})).unwrap();
    session.cancel().unwrap();
    let mut seen: VecDeque<Value> = VecDeque::new();
    let until = Instant::now() + WAIT;
    while seen.len() < 4 && Instant::now() < until {
        if let Ok(message) = messages.recv_timeout(Duration::from_millis(100)) {
            seen.push_back(message);
        }
    }
    assert!(
        seen.iter()
            .any(|m| m["method"] == "_example/ack" && m["params"]["seq"] == 7),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|m| m["method"] == "session/cancel"),
        "{seen:?}"
    );
}

#[test]
fn a_lost_connection_is_reported_once_and_on_close_runs() {
    let (mut agent, transport) = Agent::pair();
    let closed = Arc::new(Mutex::new(false));
    let flag = closed.clone();
    let actor = thread::spawn(move || {
        for _ in 0..2 {
            let message = agent.read().unwrap();
            agent.reply(
                &message["id"],
                &plain(message["method"].as_str().unwrap()).unwrap(),
            );
        }
    });
    let session = Client::builder("/w")
        .on_close(move || *flag.lock().unwrap() = true)
        .connect(transport)
        .unwrap();
    actor.join().unwrap();
    assert!(matches!(session.next_event(WAIT), Some(Event::Failed(_))));
    assert!(session.connection_lost());
    assert_eq!(session.next_event(Duration::from_millis(100)), None);
    session.close();
    assert!(*closed.lock().unwrap());
}

/// An agent that answers what opens a session, runs `at_prompt` on the first prompt, and leaves.
fn agent_leaving_at_prompt(
    capabilities: Value,
    at_prompt: impl FnOnce(&Agent, &Value) + Send + 'static,
) -> Transport {
    let (mut agent, transport) = Agent::pair();
    thread::spawn(move || {
        while let Some(message) = agent.read() {
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            match method.as_str() {
                "initialize" => agent.reply(
                    &message["id"],
                    &json!({"protocolVersion": 1, "agentCapabilities": capabilities}),
                ),
                "session/prompt" => {
                    at_prompt(&agent, &message["id"]);
                    return;
                }
                _ if message.get("id").is_some() => {
                    agent.reply(&message["id"], &plain(&method).unwrap());
                }
                _ => {}
            }
        }
    });
    transport
}

#[test]
fn a_lost_connection_after_a_cancelled_turn_is_no_failure() {
    let transport = agent_leaving_at_prompt(json!({}), |agent, id| {
        agent.reply(id, &json!({"stopReason": "cancelled"}));
    });
    let session = Client::builder("/w")
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert_eq!(session.next_event(WAIT), Some(Event::Cancelled));
    assert_eq!(session.next_event(Duration::from_millis(300)), None);
    assert!(session.connection_lost());
}

const EXTENSION: &str = "example/events";

/// Whether a notification of this test's extension ends a turn.
fn completed(method: &str, params: &Value) -> bool {
    method == "_example/event" && params["kind"] == "completed"
}

#[test]
fn a_notification_that_ends_a_turn_keeps_the_agent_leaving_from_being_a_failure() {
    let transport = agent_leaving_at_prompt(json!({"_meta": {EXTENSION: true}}), |agent, _| {
        agent.notify("_example/event", &json!({"kind": "completed"}));
    });
    let asked = Arc::new(Mutex::new(false));
    let told = asked.clone();
    let session = Client::builder("/w")
        .turn_end_by(EXTENSION)
        .ends_turn(completed)
        .disconnect_detail(move || {
            *told.lock().unwrap() = true;
            None
        })
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::Extension { .. })
    ));
    assert_eq!(session.next_event(Duration::from_millis(300)), None);
    assert!(session.connection_lost());
    assert!(
        !*asked.lock().unwrap(),
        "nothing is due, so nothing is asked"
    );
}

#[test]
fn a_notification_that_ends_nothing_leaves_the_agent_leaving_a_failure() {
    let transport = agent_leaving_at_prompt(json!({"_meta": {EXTENSION: true}}), |agent, _| {
        agent.notify("_example/event", &json!({"kind": "text"}));
    });
    let session = Client::builder("/w")
        .turn_end_by(EXTENSION)
        .ends_turn(completed)
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::Extension { .. })
    ));
    assert!(matches!(session.next_event(WAIT), Some(Event::Failed(_))));
}

#[test]
fn a_turn_after_one_an_extension_ended_can_still_fail() {
    let (mut agent, transport) = Agent::pair();
    let actor = thread::spawn(move || {
        let mut prompts = 0;
        while let Some(message) = agent.read() {
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            match method.as_str() {
                "initialize" => agent.reply(
                    &message["id"],
                    &json!({"protocolVersion": 1, "agentCapabilities": {"_meta": {EXTENSION: true}}}),
                ),
                "session/prompt" => {
                    prompts += 1;
                    if prompts == 2 {
                        return;
                    }
                    agent.notify("_example/event", &json!({"kind": "completed"}));
                }
                _ => agent.reply(&message["id"], &plain(&method).unwrap()),
            }
        }
    });
    let session = Client::builder("/w")
        .turn_end_by(EXTENSION)
        .ends_turn(completed)
        .prompt("one")
        .connect(transport)
        .unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::Extension { .. })
    ));
    session.send("two").unwrap();
    actor.join().unwrap();
    assert!(matches!(session.next_event(WAIT), Some(Event::Failed(_))));
}

/// The ending of the first turn arrives after the second was sent, and ends only the first.
#[test]
fn a_turn_sent_before_the_last_ones_ending_arrived_can_still_fail() {
    let (mut agent, transport) = Agent::pair();
    let actor = thread::spawn(move || {
        let mut prompts = 0;
        while let Some(message) = agent.read() {
            let method = message["method"].as_str().unwrap_or_default().to_owned();
            match method.as_str() {
                "initialize" => agent.reply(
                    &message["id"],
                    &json!({"protocolVersion": 1, "agentCapabilities": {"_meta": {EXTENSION: true}}}),
                ),
                "session/prompt" => {
                    prompts += 1;
                    if prompts == 2 {
                        agent.notify("_example/event", &json!({"kind": "completed"}));
                        return;
                    }
                }
                _ => agent.reply(&message["id"], &plain(&method).unwrap()),
            }
        }
    });
    let session = Client::builder("/w")
        .turn_end_by(EXTENSION)
        .ends_turn(completed)
        .prompt("one")
        .connect(transport)
        .unwrap();
    session.send("two").unwrap();
    actor.join().unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::Extension { .. })
    ));
    assert!(matches!(session.next_event(WAIT), Some(Event::Failed(_))));
}

#[test]
fn an_agent_with_no_such_extension_is_not_asked_what_ends_a_turn() {
    let transport = agent_leaving_at_prompt(json!({}), |agent, _| {
        agent.notify("_example/event", &json!({"kind": "completed"}));
    });
    let session = Client::builder("/w")
        .turn_end_by(EXTENSION)
        .ends_turn(completed)
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert!(matches!(
        session.next_event(WAIT),
        Some(Event::Extension { .. })
    ));
    assert!(matches!(session.next_event(WAIT), Some(Event::Failed(_))));
}

#[test]
fn a_lost_agents_failure_says_what_became_of_it() {
    let transport = agent_leaving_at_prompt(json!({}), |_, _| {});
    let session = Client::builder("/w")
        .disconnect_detail(|| Some("it exited with status 3".to_owned()))
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert_eq!(
        session.next_event(WAIT),
        Some(Event::Failed(
            "the agent closed its connection: it exited with status 3".to_owned()
        ))
    );
}

#[test]
fn a_lost_agent_with_nothing_to_say_fails_in_the_librarys_words() {
    let transport = agent_leaving_at_prompt(json!({}), |_, _| {});
    let session = Client::builder("/w")
        .disconnect_detail(|| None)
        .prompt("go")
        .connect(transport)
        .unwrap();
    assert_eq!(
        session.next_event(WAIT),
        Some(Event::Failed("the agent closed its connection".to_owned()))
    );
}

#[test]
fn a_reader_past_the_event_limit_ends_the_session() {
    let (transport, _) = agent_with(|method, _, agent| match method {
        "session/prompt" => {
            for n in 0..20 {
                agent.notify("_example/event", &json!({"n": n}));
            }
            Some(json!({"stopReason": "end_turn"}))
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .max_pending_events(5)
        .prompt("go")
        .connect(transport)
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    let mut failed = false;
    while let Some(event) = session.next_event(Duration::from_millis(200)) {
        failed |= matches!(&event, Event::Failed(text) if text.contains("limit"));
    }
    assert!(failed);
}

/// The texts of a turn whose agent sends `chunks`, read through a session `builder` makes.
fn texts_of(chunks: &'static [&'static str], builder: coqui_acp::Builder) -> Vec<String> {
    let (transport, _) = agent_with(move |method, _, agent| match method {
        "session/prompt" => {
            for chunk in chunks {
                agent.notify(
                    "session/update",
                    &json!({"sessionId": "s1", "update": {
                        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}}}),
                );
            }
            Some(json!({"stopReason": "end_turn"}))
        }
        other => plain(other),
    });
    let session = builder.prompt("go").connect(transport).unwrap();
    let mut texts = Vec::new();
    while let Some(event) = session.next_event(Duration::from_secs(5)) {
        match event {
            Event::Text(text) => texts.push(text),
            Event::Completed { .. } => break,
            other => panic!("{other:?}"),
        }
    }
    texts
}

#[test]
fn text_is_flushed_at_the_size_set_in_utf8_bytes() {
    assert_eq!(texts_of(&["é", "ab"], Client::builder("/w")), ["éab"]);
    assert_eq!(
        texts_of(&["é", "ab"], Client::builder("/w").text_flush(2)),
        ["é", "ab"]
    );
}

#[test]
fn exchange_makes_each_call_and_reports_each_failure() {
    let (transport, _) = agent_with(|method, _, _| match method {
        "initialize" | "authenticate" | "_known" => plain(method),
        _ => None,
    });
    let results = Exchange::new()
        .authenticate(Authenticate {
            method: "token".to_owned(),
            meta: Map::new(),
        })
        .initialize_timeout(Some(WAIT))
        .request_timeout(Some(WAIT))
        .run(
            transport,
            vec![
                ("_known".to_owned(), json!({})),
                ("_unknown".to_owned(), json!({})),
            ],
        )
        .unwrap();
    assert!(results[0].is_ok());
    assert_eq!(results[1].as_ref().unwrap_err().code, -32601);
}

#[test]
fn exchange_calls_itself_what_it_is_told_and_coqui_acp_otherwise() {
    let (transport, messages) = agent_with(|method, _, _| plain(method));
    Exchange::new()
        .client_info("ai6", "1.2.3")
        .run(transport, Vec::new())
        .unwrap();
    let initialize = messages.recv_timeout(WAIT).unwrap();
    assert_eq!(initialize["method"], "initialize", "{initialize}");
    assert_eq!(
        initialize["params"]["clientInfo"],
        json!({"name": "ai6", "version": "1.2.3"}),
        "{initialize}"
    );

    let (transport, messages) = agent_with(|method, _, _| plain(method));
    Exchange::default().run(transport, Vec::new()).unwrap();
    let initialize = messages.recv_timeout(WAIT).unwrap();
    assert_eq!(
        initialize["params"]["clientInfo"]["name"], "coqui-acp",
        "{initialize}"
    );
}

#[test]
fn an_exchange_with_no_answer_in_time_says_so_for_that_call() {
    // The agent answers only once the exchange has given up, however long a busy machine takes to get there.
    let (release, held) = mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let (transport, _) = agent_with(move |method, _, _| match method {
        "_slow" => {
            let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(300));
            Some(json!({}))
        }
        other => plain(other),
    });
    let results = Exchange::new()
        .request_timeout(Some(Duration::from_millis(50)))
        .run(transport, vec![("_slow".to_owned(), json!({}))])
        .unwrap();
    assert_eq!(results[0].as_ref().unwrap_err().code, -1);
    let _ = release.send(());
}

/// An outcome as `vectors/describe.json` writes it.
fn outcome<T>(answer: &Answer<T>, known: impl Fn(&T) -> Value) -> Value {
    match answer {
        Answer::Known(value) => json!({"known": known(value)}),
        Answer::Unknown(reason) => json!({"unknown": reason}),
        Answer::NotSupported => json!("not_supported"),
    }
}

/// The sessions outcome as `vectors/describe.json` writes it: `incomplete` only when the list stops short.
fn sessions_outcome(answer: &Answer<SessionList>) -> Value {
    let mut got = outcome(answer, |list| serde_json::to_value(&list.sessions).unwrap());
    if let Answer::Known(SessionList {
        incomplete: Some(why),
        ..
    }) = answer
    {
        got["incomplete"] = json!(why);
    }
    got
}

/// Whether `got` says why a list stops short as `want` does: the same words, or for `<prefix>: <reason>`, words that
/// start with `<prefix>: ` and hold the reason.
fn said_why(got: &str, want: &str) -> bool {
    got == want
        || want.split_once(": ").is_some_and(|(prefix, reason)| {
            got.starts_with(&format!("{prefix}: ")) && got.contains(reason)
        })
}

/// Whether `got` is the outcome `want` says: the same known value, said complete or incomplete for the same reason, a
/// reason holding its text, or not supported.
fn as_described(got: &Value, want: &Value) -> bool {
    match (got.get("unknown"), want.get("unknown")) {
        (Some(Value::String(reason)), Some(Value::String(text))) => reason.contains(text.as_str()),
        _ => {
            let why = match (got.get("incomplete"), want.get("incomplete")) {
                (None, None) => true,
                (Some(Value::String(got)), Some(Value::String(want))) => said_why(got, want),
                _ => false,
            };
            let rest = |value: &Value| {
                let mut value = value.clone();
                if let Some(fields) = value.as_object_mut() {
                    fields.remove("incomplete");
                }
                value
            };
            why && rest(got) == rest(want)
        }
    }
}

/// Plays `steps` as the agent, and fails if the client sends anything after them.
fn play_only(mut agent: Agent, steps: Vec<Value>) {
    for step in &steps {
        let expect = &step["expect"];
        let message = agent
            .read()
            .expect("the client sends what the script expects");
        assert_eq!(
            message["method"], expect["method"],
            "the message was {message}"
        );
        if let Some(params) = expect.get("params") {
            assert!(
                holds(&message["params"], params),
                "params {} should hold {params}",
                message["params"]
            );
        }
        if let Some(reply) = step.get("reply") {
            agent.reply(&message["id"], reply);
        } else if let Some(error) = step.get("error") {
            agent.fail(&message["id"], error);
        }
    }
    if let Some(message) = agent.read() {
        panic!("the client sent {message} after the script");
    }
}

fn describe_vectors() -> Value {
    serde_json::from_str(include_str!("../../../vectors/describe.json")).unwrap()
}

/// The default limits, with those `set` names replaced, as `vectors/describe.json` writes them.
fn limits_of(set: &Value) -> DescribeLimits {
    let mut limits = DescribeLimits::new();
    for (name, value) in set.as_object().into_iter().flatten() {
        let value = usize::try_from(value.as_u64().unwrap()).unwrap();
        limits = match name.as_str() {
            "options" => limits.options(value),
            "values" => limits.values(value),
            "modes" => limits.modes(value),
            "sessions" => limits.sessions(value),
            "text" => limits.text(value),
            "pages" => limits.pages(value),
            other => panic!("no limit {other}"),
        };
    }
    limits
}

#[test]
fn every_v1_description_holds() {
    let vectors = describe_vectors();
    let mut ran = 0;
    for case in vectors["cases"].as_array().unwrap() {
        if case["version"] != 1 {
            continue;
        }
        ran += 1;
        let name = case["name"].as_str().unwrap();
        let (agent, transport) = Agent::pair();
        let steps = case["steps"].as_array().unwrap().clone();
        let actor = thread::spawn(move || play_only(agent, steps));
        let description = Exchange::new()
            .describe_limits(limits_of(&case["limits"]))
            .describe(transport)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let want = &case["describe"];
        for (part, got) in [
            (
                "options",
                outcome(&description.options, |value| {
                    serde_json::to_value(value).unwrap()
                }),
            ),
            (
                "modes",
                outcome(&description.modes, |value| {
                    serde_json::to_value(value).unwrap()
                }),
            ),
            ("sessions", sessions_outcome(&description.sessions)),
        ] {
            assert!(
                as_described(&got, &want[part]),
                "{name}: {part} was {got}, wanted {}",
                want[part]
            );
        }
        actor
            .join()
            .unwrap_or_else(|_| panic!("{name}: the script failed"));
    }
    assert!(ran >= 13, "only {ran} v1 descriptions ran");
}

/// Text of `bytes` UTF-8 bytes, of two-byte characters, so a cut lands inside one.
fn long(bytes: usize) -> String {
    "é".repeat(bytes / 2) + if bytes % 2 == 1 { "x" } else { "" }
}

#[test]
fn a_description_is_bounded_as_the_vectors_say() {
    let limits = describe_vectors()["limits"].clone();
    assert_eq!(DescribeLimits::new(), limits_of(&limits));
    bounded_by(&limits, Exchange::new());
}

#[test]
fn a_description_is_bounded_by_the_limits_set() {
    let limits = json!({"options": 2, "values": 3, "modes": 2, "sessions": 3, "text": 16});
    bounded_by(&limits, Exchange::new().describe_limits(limits_of(&limits)));
}

#[test]
fn sessions_are_asked_for_ten_pages_at_most_by_default() {
    let (transport, seen) = agent_with(move |method, _, _| match method {
        "initialize" => Some(
            json!({"protocolVersion": 1, "agentCapabilities": {"sessionCapabilities": {"list": {}}}}),
        ),
        "session/list" => {
            Some(json!({"sessions": [{"sessionId": "s", "cwd": "/w"}], "nextCursor": "more"}))
        }
        other => plain(other),
    });
    let description = Exchange::new().describe(transport).unwrap();
    let Answer::Known(sessions) = &description.sessions else {
        panic!("{:?}", description.sessions);
    };
    assert_eq!(sessions.sessions.len(), 10);
    assert_eq!(
        sessions.incomplete.as_deref(),
        Some("the page limit was reached")
    );
    let asked = seen
        .try_iter()
        .filter(|message| message["method"] == "session/list")
        .count();
    assert_eq!(asked, 10);
}

/// Describes an agent whose every list is one past `limits`, which `exchange` must keep.
fn bounded_by(limits: &Value, exchange: Exchange) {
    let limit = |name: &str| usize::try_from(limits[name].as_u64().unwrap()).unwrap();
    let text = limit("text");
    let value = |id: String| json!({"value": id, "name": "v"});
    let mut options = vec![json!({
        "id": "many", "name": long(text + 11), "type": "select", "currentValue": "v0",
        "options": (0..=limit("values")).map(|at| value(format!("v{at}"))).collect::<Vec<_>>(),
    })];
    options.push(json!({"id": long(text + 1), "name": "too long to name", "type": "boolean", "currentValue": true}));
    options.extend((0..limit("options")).map(
        |at| json!({"id": format!("o{at}"), "name": "o", "type": "boolean", "currentValue": true}),
    ));
    let modes = (0..=limit("modes"))
        .map(|at| json!({"id": format!("m{at}"), "name": "m"}))
        .collect::<Vec<_>>();
    let sessions = (0..=limit("sessions"))
        .map(|at| json!({"sessionId": format!("s{at}"), "cwd": "/w"}))
        .collect::<Vec<_>>();
    let answer = json!({"configOptions": options, "modes": {"currentModeId": "m0", "availableModes": modes}});
    let (transport, _) = agent_with(move |method, _, _| match method {
        "initialize" => Some(json!({"protocolVersion": 1, "agentCapabilities": {
            "sessionCapabilities": {"list": {}}, "_meta": {coqui_acp::SESSION_OPTIONS_CAPABILITY: true}}})),
        "_coqui_acp/session_options" => Some(answer.clone()),
        "session/list" => Some(json!({"sessions": sessions})),
        other => plain(other),
    });
    let description = exchange.describe(transport).unwrap();
    let Answer::Known(options) = &description.options else {
        panic!("{:?}", description.options);
    };
    assert_eq!(options.len(), limit("options"));
    let first = serde_json::to_value(&options[0]).unwrap();
    assert_eq!(first["options"].as_array().unwrap().len(), limit("values"));
    let name = first["name"].as_str().unwrap();
    assert!(
        name.len() <= text && name.len() > text - 2,
        "{}",
        name.len()
    );
    assert_eq!(
        options[1].id.0.as_ref(),
        "o0",
        "an id too long to name is dropped"
    );
    let Answer::Known(modes) = &description.modes else {
        panic!("{:?}", description.modes);
    };
    assert_eq!(modes.available_modes.len(), limit("modes"));
    let Answer::Known(sessions) = &description.sessions else {
        panic!("{:?}", description.sessions);
    };
    assert_eq!(sessions.sessions.len(), limit("sessions"));
    assert_eq!(
        sessions.incomplete.as_deref(),
        Some("the sessions limit was reached"),
        "one session past the limit was left out"
    );
}

/// A transport whose writer is busy with a `session/cancel` line for `delay` without blocking the connection, and drops that line if
/// `killed` is set by then, as the pipe of an agent whose process `on_close` ended does.
fn slow_cancel_agent(
    answer: impl Fn(&str, &Value, &Agent) -> Option<Value> + Send + 'static,
    delay: Duration,
    killed: Arc<AtomicBool>,
) -> (
    impl coqui_acp::sdk::ConnectTo<coqui_acp::sdk::Client> + 'static,
    Receiver<Value>,
) {
    let (to_agent, from_client) = async_mpsc::unbounded::<String>();
    let (to_client, from_agent) = async_mpsc::unbounded::<io::Result<String>>();
    let outgoing = to_agent
        .sink_map_err((|error| io::Error::other(error)) as fn(_) -> _)
        .with(move |line: String| {
            let killed = killed.clone();
            async move {
                if line.contains("session/cancel") {
                    let (fire, fired) = futures::channel::oneshot::channel::<()>();
                    thread::spawn(move || {
                        thread::sleep(delay);
                        let _ = fire.send(());
                    });
                    let _ = fired.await;
                    if killed.load(Ordering::SeqCst) {
                        return Err(io::ErrorKind::BrokenPipe.into());
                    }
                }
                Ok::<_, io::Error>(line)
            }
        });
    let mut agent = Agent {
        from_client,
        to_client,
        next_id: 1000,
    };
    let (seen, messages): (Sender<Value>, Receiver<Value>) = mpsc::channel();
    thread::spawn(move || {
        while let Some(message) = agent.read() {
            let _ = seen.send(message.clone());
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if let Some(id) = message.get("id") {
                match answer(method, &message["params"], &agent) {
                    Some(result) => agent.reply(id, &result),
                    None => agent.fail(id, &json!({"code": -32601, "message": "no such method"})),
                }
            }
        }
    });
    (coqui_acp::sdk::Lines::new(outgoing, from_agent), messages)
}

fn runs_a_refused_tool(method: &str, agent: &Agent) -> Option<Value> {
    if method == "session/prompt" {
        agent.notify("session/update", &json!({"sessionId": "s1", "update": {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run", "kind": "execute", "status": "in_progress"}}));
        return None;
    }
    plain(method)
}

fn cancel_arrives(messages: &Receiver<Value>) -> bool {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if messages
            .recv_timeout(Duration::from_millis(50))
            .is_ok_and(|m| m["method"] == "session/cancel")
        {
            return true;
        }
    }
    false
}

#[test]
fn an_abort_tells_the_agent_to_stop_though_the_writer_is_slow() {
    let killed = Arc::new(AtomicBool::new(false));
    let (transport, messages) = slow_cancel_agent(
        |method, _, agent| runs_a_refused_tool(method, agent),
        Duration::from_millis(300),
        killed.clone(),
    );
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .close_on_drop(false)
        .on_close({
            let killed = killed.clone();
            move || killed.store(true, Ordering::SeqCst)
        })
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 2);
    assert!(matches!(&events[1], Event::Failed(_)), "{events:?}");
    drop(session);
    assert!(
        cancel_arrives(&messages),
        "the agent should be told to stop"
    );
}

#[test]
fn a_session_dropped_on_failed_still_tells_the_agent_to_stop() {
    let killed = Arc::new(AtomicBool::new(false));
    let (transport, messages) = slow_cancel_agent(
        |method, _, agent| runs_a_refused_tool(method, agent),
        Duration::from_millis(100),
        killed.clone(),
    );
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .on_close({
            let killed = killed.clone();
            move || killed.store(true, Ordering::SeqCst)
        })
        .connect(transport)
        .unwrap();
    loop {
        match session.next_event(WAIT) {
            Some(Event::Failed(_)) | None => break,
            Some(_) => {}
        }
    }
    drop(session);
    assert!(
        cancel_arrives(&messages),
        "the agent should be told to stop"
    );
}

#[test]
fn dropping_a_session_whose_agent_is_hung_returns_within_the_abort_wait() {
    let killed = Arc::new(AtomicBool::new(false));
    let (transport, _messages) = agent_with(|method, _, agent| match method {
        "session/prompt" => runs_a_refused_tool(method, agent),
        "_coqui/flush" | "session/close" => {
            thread::sleep(Duration::from_secs(30));
            plain(method)
        }
        other => plain(other),
    });
    let session = Client::builder("/w")
        .prompt("go")
        .refuse_kinds(["execute"])
        .close_grace(None)
        .abort_wait(Duration::from_millis(300))
        .on_close({
            let killed = killed.clone();
            move || killed.store(true, Ordering::SeqCst)
        })
        .connect(transport)
        .unwrap();
    let events = events_until(&session, 2);
    assert!(matches!(&events[1], Event::Failed(_)), "{events:?}");
    let started = Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(killed.load(Ordering::SeqCst), "on_close runs anyway");
}
