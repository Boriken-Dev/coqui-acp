//! A provider served as an ACP agent, driven by the real `coqui_acp::Client` where it can say what is meant.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use coqui_acp::{Authenticate, Client, Event, RpcFailure, Session, exchange};
use coqui_acp_provider::{
    AcpFacade, AgentEvent, BLOCKED_EVENT, CANCELLED_EVENT, COMPLETED_EVENT, DENIED_EVENT,
    EVENT_NOTIFICATION, EVENTS_CAPABILITY, FAILED_EVENT, FacadeAuth, LAUNCH_META_KEY, LIMIT_EVENT,
    QuotaReport, TEXT_EVENT, TOOL_EVENT, TURN_CANCELLED, TokenUsage, UsageLimit,
};
use serde_json::{Map, Value, json};
use support::{Probe, Raw, WAIT, cwd, detail, eventually, facade, joined, prompt_params, quick};

/// Connects a real client to a facade over `probe` and sends `prompt`.
fn connect(probe: &Arc<Probe>, prompt: &str) -> Session {
    let (agent, client) = coqui_acp::duplex();
    facade(probe, quick())
        .serve(agent)
        .expect("a thread starts");
    Client::builder(cwd())
        .prompt(prompt)
        .connect(client)
        .expect("the facade opens a session")
}

/// The events up to and including whatever ends the turn.
fn turn(session: &Session) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        let event = session
            .next_event(WAIT)
            .unwrap_or_else(|| panic!("the turn did not end: {events:?}"));
        let ended = matches!(
            event,
            Event::Completed { .. } | Event::Failed(_) | Event::Blocked(_)
        ) || format!("{event:?}").starts_with("Cancelled");
        events.push(event);
        if ended {
            return events;
        }
    }
}

fn text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn completed() -> Event {
    Event::Completed {
        stop: "end_turn".to_owned(),
    }
}

#[test]
fn the_real_client_opens_a_session_and_gets_its_prompt_answered() {
    let probe = Probe::new();
    let session = connect(&probe, "hello");
    let events = turn(&session);
    assert_eq!(text(&events), "echo: hello");
    assert_eq!(events.last(), Some(&completed()));
    assert!(session.id().is_some());
    session.close();
}

#[test]
fn a_second_prompt_goes_to_the_session_already_running() {
    let probe = Probe::new();
    let session = connect(&probe, "one");
    turn(&session);
    session.send("two").unwrap();
    let events = turn(&session);
    assert_eq!(text(&events), "echo: two");
    assert_eq!(probe.starts.lock().unwrap().len(), 1, "started once");
    assert_eq!(*probe.messages.lock().unwrap(), ["one", "two"]);
    session.close();
}

#[test]
fn cancel_reaches_the_provider_and_the_turn_ends_cancelled() {
    let probe = Probe::new();
    let mut first = true;
    probe.script(move |text| {
        if first {
            first = false;
            vec![AgentEvent::new(TEXT_EVENT, Some("working".to_owned()))]
        } else {
            support::echo(text)
        }
    });
    let session = connect(&probe, "long job");
    // Text is gathered until something follows it, so wait on what the provider did with the prompt.
    eventually("the provider's text is read", || {
        probe.next_calls.load(Ordering::SeqCst) > 2
    });
    session.cancel().unwrap();
    let events = turn(&session);
    assert_eq!(text(&events), "working");
    assert_eq!(probe.interrupted.load(Ordering::SeqCst), 1);
    let last = events.last().unwrap();
    assert!(
        !matches!(last, Event::Completed { .. } | Event::Failed(_)),
        "a cancelled turn is neither finished nor failed: {last:?}"
    );
    session.send("next").unwrap();
    assert_eq!(text(&turn(&session)), "echo: next", "the session goes on");
    session.close();
}

#[test]
fn a_cancel_turn_answers_cancelled_whatever_the_provider_says_it_was() {
    for ending in [
        AgentEvent::new(COMPLETED_EVENT, None),
        AgentEvent::new(CANCELLED_EVENT, None),
        AgentEvent::new(FAILED_EVENT, Some("killed by the interrupt".to_owned())),
        AgentEvent::new(LIMIT_EVENT, None),
    ] {
        let probe = Probe::new();
        *probe.on_interrupt.lock().unwrap() = Some(ending.clone());
        probe.script(|_| Vec::new());
        let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
        let session = raw.open();
        let prompt = raw.send("session/prompt", &prompt_params(&session, "go"));
        raw.notify("session/cancel", &json!({"sessionId": session}));
        let answer = raw.answer(prompt);
        assert_eq!(
            answer["result"]["stopReason"], "cancelled",
            "{ending:?}: {answer}"
        );
    }
}

#[test]
fn close_answers_after_the_provider_has_closed() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "hi");
    let answer = raw.call("session/close", &json!({"sessionId": session}));
    assert_eq!(answer["result"], json!({}), "{answer}");
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
    let answer = raw.prompt(&session, "again");
    assert!(answer["error"].is_object(), "{answer}");
}

#[test]
fn close_while_a_turn_runs_answers_the_prompt_too() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let prompt = raw.send("session/prompt", &prompt_params(&session, "go"));
    eventually("the provider has the prompt", || {
        !probe.starts.lock().unwrap().is_empty()
    });
    let close = raw.send("session/close", &json!({"sessionId": session}));
    assert_eq!(raw.answer(prompt)["result"]["stopReason"], "cancelled");
    assert_eq!(raw.answer(close)["result"], json!({}));
}

#[test]
fn what_acp_has_no_field_for_reaches_the_provider_through_the_launch_meta() {
    let probe = Probe::new();
    let mut settings = quick();
    settings
        .environment
        .insert("BASE".to_owned(), "from the host".to_owned());
    settings
        .environment
        .insert("CHOSEN".to_owned(), "from the host".to_owned());
    settings.client_environment = vec!["CHOSEN".to_owned()];
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    raw.initialize();
    let answer = raw.call(
        "session/new",
        &json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {
            "context": ["first", "second"],
            "environment": {"CHOSEN": "from the client"},
            "model": "m",
            "effort": "high",
            "meta": {"nested": {"a": [1, true, null, "x"]}},
        }}}),
    );
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    raw.prompt(&session, "hi");
    let starts = probe.starts.lock().unwrap();
    let start = &starts[0];
    assert_eq!(start.prompt, "hi");
    assert_eq!(start.working_directory, cwd());
    assert_eq!(start.context, ["first", "second"]);
    assert_eq!(start.environment["BASE"], "from the host");
    assert_eq!(start.environment["CHOSEN"], "from the client");
    assert_eq!(start.model.as_deref(), Some("m"));
    assert_eq!(start.effort.as_deref(), Some("high"));
    let meta = start.meta.as_ref().expect("meta crosses");
    let meta: Value = guatiao_serde::adapt::from_value(meta).expect("it reads back");
    assert_eq!(meta, json!({"nested": {"a": [1, true, null, "x"]}}));
}

#[test]
fn a_real_client_sets_the_launch_meta_with_its_builder() {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, quick())
        .serve(agent)
        .expect("a thread starts");
    let session = Client::builder(cwd())
        .meta(LAUNCH_META_KEY, json!({"model": "from-meta"}))
        .prompt("hi")
        .connect(client)
        .unwrap();
    turn(&session);
    assert_eq!(
        probe.starts.lock().unwrap()[0].model.as_deref(),
        Some("from-meta")
    );
    session.close();
}

#[test]
fn each_event_becomes_what_acp_has_for_it() {
    let probe = Probe::new();
    probe.script(|_| {
        let mut tool = AgentEvent::new(TOOL_EVENT, Some("Read a file".to_owned()));
        tool.tools.push("Read".to_owned());
        let mut cost = AgentEvent::new("usage", None);
        cost.usage = Some(TokenUsage {
            input: 1,
            output: 2,
            cost_micro_usd: 1_500_000,
            cumulative: true,
            ..TokenUsage::default()
        });
        vec![
            AgentEvent::new(TEXT_EVENT, Some("before".to_owned())),
            tool,
            cost,
            AgentEvent::new(DENIED_EVENT, Some("Bash, WebFetch".to_owned())),
            AgentEvent::new(COMPLETED_EVENT, None),
        ]
    });
    let session = connect(&probe, "go");
    let events = turn(&session);
    assert_eq!(events[0], Event::Text("before".to_owned()));
    let tools: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Tool {
                title, name, kind, ..
            } => Some((title.as_str(), name.as_deref().unwrap(), kind.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        [
            ("Read a file", "Read", "other"),
            ("Bash", "Bash", "other"),
            ("WebFetch", "WebFetch", "other"),
        ]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Cost { amount, currency } if (*amount - 1.5).abs() < 1e-9 && currency == "USD"
    )));
    assert_eq!(events.last(), Some(&completed()));
    session.close();
}

#[test]
fn a_tool_call_the_turn_started_ends_with_the_turn() {
    for (ending, status) in [
        (AgentEvent::new(COMPLETED_EVENT, None), "completed"),
        (
            AgentEvent::new(FAILED_EVENT, Some("no".to_owned())),
            "failed",
        ),
    ] {
        let probe = Probe::new();
        probe.script(move |_| {
            let mut tool = AgentEvent::new(TOOL_EVENT, None);
            tool.tools.push("Read".to_owned());
            vec![tool, ending.clone()]
        });
        let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
        let session = raw.open();
        raw.prompt(&session, "go");
        let updates = raw.updates();
        let started = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call")
            .expect("the tool is announced");
        let finished = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call_update")
            .expect("the tool is finished");
        assert_eq!(started["toolCallId"], finished["toolCallId"]);
        assert_eq!(finished["status"], status);
    }
}

#[test]
fn the_agents_own_cancellation_ends_the_turn_in_either_spelling() {
    for ending in [
        AgentEvent::new(CANCELLED_EVENT, None),
        AgentEvent::new(BLOCKED_EVENT, Some(TURN_CANCELLED.to_owned())),
    ] {
        let probe = Probe::new();
        probe.script(move |_| vec![ending.clone()]);
        let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
        let session = raw.open();
        let answer = raw.prompt(&session, "go");
        assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
    }
}

#[test]
fn being_blocked_on_a_refused_permission_does_not_end_the_turn() {
    let probe = Probe::new();
    probe.script(|_| {
        vec![
            AgentEvent::new(BLOCKED_EVENT, Some("a permission was refused".to_owned())),
            AgentEvent::new(COMPLETED_EVENT, None),
        ]
    });
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    assert_eq!(answer["result"]["stopReason"], "end_turn");
    let extension: Vec<_> = raw
        .notifications
        .iter()
        .filter(|message| message["method"] == EVENT_NOTIFICATION)
        .collect();
    assert_eq!(extension[0]["params"]["event"]["kind"], BLOCKED_EVENT);
}

#[test]
fn a_limit_refuses_the_turn_and_its_details_reach_the_client() {
    let probe = Probe::new();
    *probe.identity.lock().unwrap() = Some("provider-7".to_owned());
    probe.script(|_| {
        let mut limit = AgentEvent::new(LIMIT_EVENT, Some("out of quota".to_owned()));
        limit.limit = Some(UsageLimit {
            resets_at: Some(1_800_000_000),
            window: Some("weekly".to_owned()),
        });
        limit.quota = vec![QuotaReport {
            window: "weekly".to_owned(),
            used_permille: Some(1000),
            resets_at: None,
            status: Some("rejected".to_owned()),
        }];
        vec![limit]
    });
    let session = connect(&probe, "go");
    let mut events = Vec::new();
    loop {
        let event = session.next_event(WAIT).expect("the turn ends");
        let ended = matches!(event, Event::Failed(_));
        events.push(event);
        if ended {
            break;
        }
    }
    let extension = events
        .iter()
        .find_map(|event| match event {
            Event::Extension { method, params } if params.get("event").is_some() => {
                assert_eq!(method, EVENT_NOTIFICATION);
                Some(params.clone())
            }
            _ => None,
        })
        .expect("the limit's details are told");
    assert_eq!(extension["event"]["limit"]["window"], "weekly");
    assert_eq!(extension["event"]["quota"][0]["status"], "rejected");
    assert_eq!(extension["providerSessionId"], "provider-7");
    session.close();
}

#[test]
fn a_failed_event_fails_the_prompt_in_its_own_words_and_the_session_goes_on() {
    let probe = Probe::new();
    let mut first = true;
    probe.script(move |text| {
        if first {
            first = false;
            vec![AgentEvent::new(
                FAILED_EVENT,
                Some("the harness exited with 3".to_owned()),
            )]
        } else {
            support::echo(text)
        }
    });
    let session = connect(&probe, "go");
    let events = turn(&session);
    assert!(
        matches!(events.last(), Some(Event::Failed(text)) if text.contains("the harness exited with 3")),
        "{events:?}"
    );
    session.send("again").unwrap();
    assert_eq!(text(&turn(&session)), "echo: again");
    session.close();
}

#[test]
fn the_provider_identity_is_told_once_known_and_resumes_the_session() {
    let probe = Probe::new();
    *probe.identity.lock().unwrap() = Some("provider-7".to_owned());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "hi");
    let told: Vec<_> = raw
        .notifications
        .iter()
        .filter(|message| message["method"] == EVENT_NOTIFICATION)
        .collect();
    assert_eq!(told.len(), 1, "once: {told:?}");
    assert_eq!(told[0]["params"]["sessionId"], json!(session));
    assert_eq!(told[0]["params"]["providerSessionId"], "provider-7");
    raw.call("session/close", &json!({"sessionId": session}));

    let (agent, client) = coqui_acp::duplex();
    facade(&probe, quick())
        .serve(agent)
        .expect("a thread starts");
    let resumed = Client::builder(cwd())
        .resume("provider-7")
        .prompt("back again")
        .connect(client)
        .expect("the provider resumes it");
    let events = turn(&resumed);
    assert_eq!(text(&events), "echo: back again");
    assert_eq!(resumed.id().as_deref(), Some("provider-7"));
    let resumes = probe.resumes.lock().unwrap();
    assert_eq!(resumes[0].session_id, "provider-7");
    assert_eq!(resumes[0].prompt, None, "the prompt goes as a message");
    assert_eq!(resumes[0].working_directory, cwd());
    drop(resumes);
    resumed.close();
}

#[test]
fn a_resume_fails_in_the_providers_own_words() {
    let probe = Probe::new();
    probe.fault(support::Call::Resume, support::Fault::Fail);
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, quick())
        .serve(agent)
        .expect("a thread starts");
    let Err(error) = Client::builder(cwd()).resume("gone").connect(client) else {
        panic!("the provider refused it");
    };
    assert!(
        error.to_string().contains("the provider refused"),
        "{error}"
    );
}

#[test]
fn a_provider_that_resumes_nothing_says_so_and_leaves_the_identity_free() {
    struct Starts;
    impl coqui_acp_provider::AgentProvider for Starts {
        fn start(
            &self,
            _request: coqui_acp_provider::StartRequest,
        ) -> Result<
            coqui_acp_provider::guatiao::library::Object<dyn coqui_acp_provider::AgentSession>,
            coqui_acp_provider::guatiao::library::ProviderError,
        > {
            unreachable!("nothing starts here");
        }
    }
    let facade = AcpFacade::new(Arc::new(Starts)).with_settings(quick());
    let (mut raw, _served) = Raw::serve(&facade);
    raw.initialize();
    for _ in 0..2 {
        let answer = raw.call(
            "session/resume",
            &json!({"sessionId": "any", "cwd": cwd(), "mcpServers": []}),
        );
        assert!(detail(&answer).contains("resumes no session"), "{answer}");
    }
}

#[test]
fn initialize_says_what_this_agent_is_and_answers_a_newer_client_with_version_one() {
    let probe = Probe::new();
    let facade = facade(&probe, quick()).with_agent_info("echo", "1.2.3");
    let (mut raw, _served) = Raw::serve(&facade);
    let answer = raw.call("initialize", &json!({"protocolVersion": 2}));
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], 1, "{answer}");
    assert_eq!(result["agentInfo"]["name"], "echo");
    assert_eq!(result["agentInfo"]["version"], "1.2.3");
    let capabilities = &result["agentCapabilities"];
    assert_eq!(capabilities["sessionCapabilities"]["close"], json!({}));
    assert_eq!(capabilities["sessionCapabilities"]["resume"], json!({}));
    assert_eq!(capabilities["loadSession"], false);
    assert_eq!(capabilities["_meta"][EVENTS_CAPABILITY], true);
}

#[test]
fn a_method_this_agent_lacks_is_not_found() {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, quick())
        .serve(agent)
        .expect("a thread starts");
    let answers = exchange(
        client,
        None,
        Some(WAIT),
        Some(WAIT),
        vec![("session/load".to_owned(), json!({"sessionId": "x"}))],
    )
    .unwrap();
    assert!(
        matches!(&answers[0], Err(RpcFailure { code: -32601, .. })),
        "{answers:?}"
    );
}

fn credential_facade(probe: &Arc<Probe>, failures: u32) -> AcpFacade {
    let mut settings = quick();
    settings.max_auth_failures = failures;
    facade(probe, settings).with_auth(FacadeAuth {
        method: "token".to_owned(),
        name: "A token".to_owned(),
        check: Arc::new(|meta: &Map<String, Value>| meta.get("token") == Some(&json!("right"))),
    })
}

#[test]
fn with_authentication_nothing_opens_until_the_client_proves_itself() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&credential_facade(&probe, 5));
    let methods = raw.initialize();
    assert_eq!(methods["authMethods"][0]["id"], "token");
    let params = json!({"cwd": cwd(), "mcpServers": []});
    let refused = raw.call("session/new", &params);
    assert_eq!(refused["error"]["code"], -32000, "{refused}");
    let wrong = raw.call(
        "authenticate",
        &json!({"methodId": "token", "_meta": {"token": "wrong"}}),
    );
    assert_eq!(wrong["error"]["code"], -32000, "{wrong}");
    assert!(
        !wrong.to_string().contains("wrong\""),
        "a refusal quotes nothing: {wrong}"
    );
    assert!(raw.call("session/new", &params)["error"].is_object());
    let right = raw.call(
        "authenticate",
        &json!({"methodId": "token", "_meta": {"token": "right"}}),
    );
    assert_eq!(right["result"], json!({}), "{right}");
    assert!(raw.call("session/new", &params)["result"]["sessionId"].is_string());
}

#[test]
fn a_real_client_authenticates_with_its_builder() {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    credential_facade(&probe, 5)
        .serve(agent)
        .expect("a thread starts");
    let mut meta = Map::new();
    meta.insert("token".to_owned(), json!("right"));
    let session = Client::builder(cwd())
        .authenticate(Authenticate {
            method: "token".to_owned(),
            meta,
        })
        .prompt("hi")
        .connect(client)
        .unwrap();
    assert_eq!(text(&turn(&session)), "echo: hi");
    session.close();
}

#[test]
fn too_many_failed_credentials_end_the_connection() {
    let probe = Probe::new();
    let (mut raw, served) = Raw::serve(&credential_facade(&probe, 2));
    raw.initialize();
    for _ in 0..2 {
        let wrong = raw.call("authenticate", &json!({"methodId": "token", "_meta": {}}));
        assert!(wrong["error"].is_object());
    }
    joined(served).expect("the connection ended cleanly");
}

#[test]
fn a_session_belongs_to_its_connection() {
    let probe = Probe::new();
    let facade = facade(&probe, quick());
    let (mut first, _a) = Raw::serve(&facade);
    let (mut second, _b) = Raw::serve(&facade);
    let session = first.open();
    second.initialize();
    let answer = second.prompt(&session, "hi");
    assert!(answer["error"].is_object(), "{answer}");
    assert_eq!(probe.starts.lock().unwrap().len(), 0);
    assert_eq!(
        first.prompt(&session, "hi")["result"]["stopReason"],
        "end_turn"
    );
}

#[test]
fn the_real_client_closing_closes_the_provider_without_waiting_out_its_limit() {
    let probe = Probe::new();
    let session = connect(&probe, "hi");
    turn(&session);
    let began = std::time::Instant::now();
    session.close();
    assert!(began.elapsed() < Duration::from_secs(2));
    eventually("the provider closed", || {
        probe.closed.load(Ordering::SeqCst) == 1
    });
}
