//! The client against a scripted agent, over a real JSON-RPC line transport. The conversations are shared with the Dart suite.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use coqui_acp::{
    Authenticate, Client, Decider, Decision, Error, Event, PermissionRequest, Session, exchange,
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
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.read().expect("the client answers");
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message["result"].clone();
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
        Event::Blocked(_) => ("blocked", json!({})),
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
    assert!(ran >= 6, "only {ran} v1 conversations ran");
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
    assert_eq!(error, Error::Timeout("initialize".to_owned()));
    drop(agent);
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

#[test]
fn exchange_makes_each_call_and_reports_each_failure() {
    let (transport, _) = agent_with(|method, _, _| match method {
        "initialize" | "authenticate" | "_known" => plain(method),
        _ => None,
    });
    let results = exchange(
        transport,
        Some(Authenticate {
            method: "token".to_owned(),
            meta: Map::new(),
        }),
        Some(WAIT),
        Some(WAIT),
        vec![
            ("_known".to_owned(), json!({})),
            ("_unknown".to_owned(), json!({})),
        ],
    )
    .unwrap();
    assert!(results[0].is_ok());
    assert_eq!(results[1].as_ref().unwrap_err().code, -32601);
}
