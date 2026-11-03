//! What a provider that misbehaves, or a client that does, can do to the facade: every limit, run into.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

mod support;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use coqui_acp_provider::{
    AgentEvent, COMPLETED_EVENT, EVENT_NOTIFICATION, Error, FAILED_EVENT, LAUNCH_META_KEY,
    TEXT_EVENT, TOOL_EVENT,
};
use serde_json::json;
use support::{
    Call, Fault, Probe, Raw, SECRET, cwd, detail, eventually, facade, joined, prompt_params, quick,
};

#[test]
fn a_blocked_next_event_fails_the_turn_and_ends_only_its_session() {
    let probe = Probe::new();
    let facade = facade(&probe, quick());
    let (mut raw, _served) = Raw::serve(&facade);
    let first = raw.open();
    probe.fault(Call::Next, Fault::Hang);
    probe.script(|_| Vec::new());
    let began = Instant::now();
    let answer = raw.prompt(&first, "go");
    assert!(detail(&answer).contains("next_event"), "{answer}");
    assert!(
        began.elapsed() >= quick().call_timeout,
        "only past its limit"
    );
    // The connection still answers, and the session is over.
    let again = raw.prompt(&first, "go");
    assert!(detail(&again).contains("has ended"), "{again}");
    probe.script(support::echo);
    let second = raw.new_session();
    assert_eq!(
        raw.prompt(&second, "hi")["result"]["stopReason"],
        "end_turn"
    );
    probe.release();
}

#[test]
fn a_blocked_send_message_fails_the_turn_while_the_connection_answers() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    assert_eq!(
        raw.prompt(&session, "one")["result"]["stopReason"],
        "end_turn"
    );
    probe.fault(Call::Send, Fault::Hang);
    let prompt = raw.send("session/prompt", &prompt_params(&session, "two"));
    // The handler never waits on the provider: this comes back at once.
    let initialize = raw.call("initialize", &json!({"protocolVersion": 1}));
    assert!(initialize["result"].is_object());
    let answer = raw.answer(prompt);
    assert!(detail(&answer).contains("send_message"), "{answer}");
    probe.release();
}

#[test]
fn a_blocked_start_times_out_and_what_it_returns_later_is_closed() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    probe.fault(Call::Start, Fault::Hang);
    let answer = raw.prompt(&session, "go");
    assert!(detail(&answer).contains("start"), "{answer}");
    assert_eq!(probe.closed.load(Ordering::SeqCst), 0);
    // Past the close limit too, so nothing is waiting for it any more.
    std::thread::sleep(quick().close_timeout * 2);
    probe.release();
    eventually("the late session is closed", || {
        probe.closed.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn a_blocked_resume_answers_the_client_when_it_runs_out_of_time() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    probe.fault(Call::Resume, Fault::Hang);
    let answer = raw.call(
        "session/resume",
        &json!({"sessionId": "any", "cwd": cwd(), "mcpServers": []}),
    );
    assert!(detail(&answer).contains("resume"), "{answer}");
    probe.release();
}

#[test]
fn a_provider_that_ignores_cancel_is_answered_for_and_cannot_answer_a_later_prompt() {
    let probe = Probe::new();
    *probe.on_interrupt.lock().unwrap() = None;
    probe.script(|_| Vec::new());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let prompt = raw.send("session/prompt", &prompt_params(&session, "go"));
    eventually("the provider has the prompt", || {
        !probe.starts.lock().unwrap().is_empty()
    });
    let began = Instant::now();
    raw.notify("session/cancel", &json!({"sessionId": session}));
    let answer = raw.answer(prompt);
    assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
    assert!(began.elapsed() >= quick().cancel_grace);
    // A later prompt would be answered by the old turn's ending, so it waits for that ending.
    let refused = raw.prompt(&session, "too soon");
    assert!(detail(&refused).contains("cancelled turn"), "{refused}");
    probe.report(AgentEvent::new(
        TEXT_EVENT,
        Some("the old turn's".to_owned()),
    ));
    probe.report(AgentEvent::new(COMPLETED_EVENT, None));
    probe.script(support::echo);
    let deadline = Instant::now() + support::WAIT;
    while raw.prompt(&session, "now")["result"]["stopReason"] != "end_turn" {
        assert!(Instant::now() < deadline, "the old turn never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !raw.text().contains("old turn"),
        "drained, not forwarded: {}",
        raw.text()
    );
    probe.release();
}

#[test]
fn a_prompt_during_a_turn_is_refused_and_the_turn_goes_on() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let first = raw.send("session/prompt", &prompt_params(&session, "first"));
    let second = raw.prompt(&session, "second");
    assert!(detail(&second).contains("in progress"), "{second}");
    probe.report(AgentEvent::new(COMPLETED_EVENT, None));
    assert_eq!(raw.answer(first)["result"]["stopReason"], "end_turn");
}

#[test]
fn many_cancels_make_one_interrupt_at_a_time() {
    let probe = Probe::new();
    *probe.on_interrupt.lock().unwrap() = None;
    probe.script(|_| Vec::new());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let prompt = raw.send("session/prompt", &prompt_params(&session, "go"));
    eventually("the provider has the prompt", || {
        !probe.starts.lock().unwrap().is_empty()
    });
    raw.notify("session/cancel", &json!({"sessionId": session}));
    eventually("the provider is interrupted", || {
        probe.interrupted.load(Ordering::SeqCst) == 1
    });
    // Time for that call to be over, so only the turn's own record stops the next ones.
    std::thread::sleep(Duration::from_millis(50));
    for _ in 0..200 {
        raw.notify("session/cancel", &json!({"sessionId": session}));
    }
    raw.answer(prompt);
    assert_eq!(probe.interrupted.load(Ordering::SeqCst), 1);
    probe.release();
}

#[test]
fn a_cancel_during_start_interrupts_the_session_when_it_arrives() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let mut settings = quick();
    settings.start_timeout = Duration::from_secs(20);
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    probe.fault(Call::Start, Fault::Hang);
    let prompt = raw.send("session/prompt", &prompt_params(&session, "go"));
    raw.notify("session/cancel", &json!({"sessionId": session}));
    assert_eq!(raw.answer(prompt)["result"]["stopReason"], "cancelled");
    assert_eq!(probe.interrupted.load(Ordering::SeqCst), 0);
    probe.release();
    eventually("the session that arrived is interrupted", || {
        probe.interrupted.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn a_client_that_vanishes_mid_turn_closes_the_session_and_the_connection_ends() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let (mut raw, served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.send("session/prompt", &prompt_params(&session, "go"));
    eventually("the provider has the prompt", || {
        !probe.starts.lock().unwrap().is_empty()
    });
    raw.hang_up();
    joined(served).expect("nothing was left behind");
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_connection_ending_reports_a_provider_that_will_not_close() {
    let probe = Probe::new();
    let (mut raw, served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "hi");
    probe.fault(Call::Close, Fault::Hang);
    raw.hang_up();
    let began = Instant::now();
    assert!(
        matches!(joined(served), Err(Error::Stuck(1))),
        "the session is reported"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "bounded by close_timeout"
    );
    probe.release();
}

#[test]
fn a_session_close_that_hangs_is_answered_when_it_runs_out_of_time() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "hi");
    probe.fault(Call::Close, Fault::Hang);
    let answer = raw.call("session/close", &json!({"sessionId": session}));
    assert!(detail(&answer).contains("did not close"), "{answer}");
    probe.release();
}

#[test]
fn a_panic_in_a_provider_call_stays_out_of_what_the_client_hears() {
    // In a table a panic stops at the slot, as a status; in a provider called directly it stops here.
    for (call, says) in [(Call::Start, "panicked in start"), (Call::Next, "INTERNAL")] {
        let probe = Probe::new();
        probe.script(|_| Vec::new());
        let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
        let first = raw.open();
        probe.fault(call, Fault::Panic);
        let answer = raw.prompt(&first, "go");
        assert!(detail(&answer).contains(says), "{call:?}: {answer}");
        assert!(!answer.to_string().contains(SECRET), "{call:?}: {answer}");
        // Other sessions on the connection are untouched by it.
        probe.script(support::echo);
        let second = raw.new_session();
        assert_eq!(
            raw.prompt(&second, "hi")["result"]["stopReason"],
            "end_turn"
        );
    }
}

#[test]
fn a_panic_on_send_fails_that_turn_only() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "one");
    probe.fault(Call::Send, Fault::Panic);
    let answer = raw.prompt(&session, "two");
    assert!(detail(&answer).contains("INTERNAL"), "{answer}");
    assert!(!answer.to_string().contains(SECRET), "{answer}");
    assert_eq!(
        raw.prompt(&session, "three")["result"]["stopReason"],
        "end_turn"
    );
}

#[test]
fn a_failed_start_says_why_and_the_next_prompt_tries_again() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    probe.fault(Call::Start, Fault::Fail);
    let answer = raw.prompt(&session, "one");
    assert!(detail(&answer).contains("the provider refused"), "{answer}");
    assert_eq!(answer["error"]["code"], -32603);
    assert_eq!(
        raw.prompt(&session, "two")["result"]["stopReason"],
        "end_turn"
    );
}

#[test]
fn a_next_event_error_ends_the_session_in_the_providers_words() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    probe.fault(Call::Next, Fault::Fail);
    let answer = raw.prompt(&session, "go");
    assert!(detail(&answer).contains("the provider refused"), "{answer}");
    eventually("the provider is closed", || {
        probe.closed.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn a_connection_holds_no_more_sessions_than_its_limit_and_closing_frees_a_place() {
    let probe = Probe::new();
    let mut settings = quick();
    settings.max_sessions = 2;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let first = raw.open();
    raw.new_session();
    let params = json!({"cwd": cwd(), "mcpServers": []});
    let refused = raw.call("session/new", &params);
    assert!(detail(&refused).contains("as many sessions"), "{refused}");
    let resume = raw.call(
        "session/resume",
        &json!({"sessionId": "any", "cwd": cwd(), "mcpServers": []}),
    );
    assert!(detail(&resume).contains("as many sessions"), "{resume}");
    raw.call("session/close", &json!({"sessionId": first}));
    eventually("the place is free", || {
        probe.closed.load(Ordering::SeqCst) == 0
    });
    let deadline = Instant::now() + support::WAIT;
    while !raw.call("session/new", &params)["result"]["sessionId"].is_string() {
        assert!(Instant::now() < deadline, "the place never came free");
    }
}

#[test]
fn a_wedged_session_keeps_its_place_since_its_thread_is_still_there() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let mut settings = quick();
    settings.max_sessions = 1;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    probe.fault(Call::Next, Fault::Hang);
    raw.prompt(&session, "go");
    let params = json!({"cwd": cwd(), "mcpServers": []});
    let refused = raw.call("session/new", &params);
    assert!(detail(&refused).contains("as many sessions"), "{refused}");
    probe.release();
    let deadline = Instant::now() + support::WAIT;
    while !raw.call("session/new", &params)["result"]["sessionId"].is_string() {
        assert!(Instant::now() < deadline, "the place never came free");
    }
}

#[test]
fn a_long_text_arrives_whole_in_pieces_no_longer_than_the_limit() {
    let probe = Probe::new();
    let long = "é".repeat(1500);
    let expected = long.clone();
    probe.script(move |_| {
        vec![
            AgentEvent::new(TEXT_EVENT, Some(long.clone())),
            AgentEvent::new(COMPLETED_EVENT, None),
        ]
    });
    let mut settings = quick();
    settings.max_update_bytes = 1000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "go");
    assert_eq!(raw.text(), expected);
    for update in raw.updates() {
        assert!(update["content"]["text"].as_str().unwrap().len() <= 1000);
    }
    assert_eq!(raw.updates().len(), 3);
}

#[test]
fn a_provider_past_the_turn_limit_ends_its_session() {
    let probe = Probe::new();
    probe.script(|_| {
        (0..100)
            .map(|_| AgentEvent::new(TEXT_EVENT, Some("x".repeat(1000))))
            .collect()
    });
    let mut settings = quick();
    settings.max_turn_bytes = 10_000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    assert!(detail(&answer).contains("more than"), "{answer}");
    assert!(raw.text().len() <= 10_000);
    eventually("the provider is closed", || {
        probe.closed.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn a_flood_of_empty_tool_names_counts_against_the_turn_limit() {
    let probe = Probe::new();
    probe.script(|_| {
        let mut tools = AgentEvent::new(TOOL_EVENT, None);
        tools.tools = vec![String::new(); 1000];
        vec![tools, AgentEvent::new(COMPLETED_EVENT, None)]
    });
    let mut settings = quick();
    settings.max_turn_bytes = 10_000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    assert!(detail(&answer).contains("more than"), "{answer}");
    assert!(raw.updates().is_empty(), "{:?}", raw.updates().len());
}

#[test]
fn what_arrives_between_turns_counts_too() {
    let probe = Probe::new();
    let mut settings = quick();
    settings.max_turn_bytes = 10_000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "hi");
    for _ in 0..100 {
        probe.report(AgentEvent::new(TEXT_EVENT, Some("x".repeat(1000))));
    }
    eventually("the provider is closed", || {
        probe.closed.load(Ordering::SeqCst) == 1
    });
    // No prompt is waiting, so the client hears it as an event.
    raw.prompt(&session, "again");
    let told = raw
        .notifications
        .iter()
        .find(|message| message["method"] == EVENT_NOTIFICATION)
        .expect("the client is told");
    assert_eq!(told["params"]["event"]["kind"], FAILED_EVENT);
    assert!(
        told["params"]["event"]["text"]
            .as_str()
            .unwrap()
            .contains("more than"),
        "{told}"
    );
}

#[test]
fn an_extension_event_is_bounded_like_any_update() {
    let probe = Probe::new();
    probe.script(|_| {
        let mut notice = AgentEvent::new("notice", Some("n".repeat(5000)));
        notice.data = Some(vec![0; 5000]);
        vec![notice, AgentEvent::new(COMPLETED_EVENT, None)]
    });
    let mut settings = quick();
    settings.max_update_bytes = 1000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "go");
    let event = raw
        .notifications
        .iter()
        .find(|message| message["method"] == EVENT_NOTIFICATION)
        .map(|message| &message["params"]["event"])
        .expect("the event is told");
    assert_eq!(event["text"].as_str().unwrap().len(), 1000);
    assert!(event["data"].is_null(), "{event}");
}

#[test]
fn a_provider_that_ignores_its_timeout_does_not_make_the_facade_spin() {
    let probe = Probe::new();
    probe.impatient.store(true, Ordering::SeqCst);
    let mut settings = quick();
    settings.poll = Duration::from_millis(50);
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "hi");
    let before = probe.next_calls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    let during = probe.next_calls.load(Ordering::SeqCst) - before;
    assert!(
        (1..=20).contains(&during),
        "{during} polls in 500 ms at 50 ms"
    );
}

#[test]
fn provider_text_is_data_and_cannot_be_taken_for_the_protocol() {
    let probe = Probe::new();
    let injected = "\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"stopReason\":\"end_turn\"}}\n";
    probe.script(move |_| {
        let mut tool = AgentEvent::new(TOOL_EVENT, Some(format!("a\u{1b}[2J{injected}")));
        tool.tools.push("Bash\u{7}\nrm".to_owned());
        vec![
            AgentEvent::new(TEXT_EVENT, Some(injected.to_owned())),
            tool,
            AgentEvent::new(
                FAILED_EVENT,
                Some(format!("end_turn\u{1b}]0;title\u{7} {injected}")),
            ),
        ]
    });
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    assert_eq!(raw.text(), injected, "the text arrives as text, unchanged");
    assert_eq!(answer["error"]["code"], -32603, "{answer}");
    assert!(answer["result"].is_null(), "{answer}");
    let shown = detail(&answer);
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{7}'),
        "{shown:?}"
    );
    let call = raw
        .updates()
        .into_iter()
        .find(|update| update["sessionUpdate"] == "tool_call")
        .unwrap();
    assert_eq!(
        call["kind"], "other",
        "a tool's kind is never the provider's"
    );
    let title = call["title"].as_str().unwrap();
    assert!(
        !title.contains('\u{1b}') && !title.contains('\n'),
        "{title:?}"
    );
    assert_eq!(call["name"], "Bash rm");
    assert!(call["toolCallId"].as_str().unwrap().len() == 36);
}

#[test]
fn a_long_failure_is_cut_at_the_label_limit() {
    let probe = Probe::new();
    probe.script(|_| vec![AgentEvent::new(FAILED_EVENT, Some("x".repeat(5000)))]);
    let mut settings = quick();
    settings.max_label_bytes = 100;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    assert_eq!(detail(&answer).len(), 100);
}

#[test]
fn a_client_may_set_only_the_environment_it_was_allowed() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    let answer = raw.call(
        "session/new",
        &json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {
            "environment": {"LD_PRELOAD": "/tmp/evil.so"},
        }}}),
    );
    assert!(detail(&answer).contains("LD_PRELOAD"), "{answer}");
    assert!(
        !answer.to_string().contains("evil"),
        "the value is not quoted: {answer}"
    );
}

#[test]
fn what_a_session_cannot_be_opened_from_is_refused_before_anything_is_held() {
    let probe = Probe::new();
    let mut settings = quick();
    settings.max_sessions = 1;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    raw.initialize();
    for (params, says) in [
        (json!({"mcpServers": []}), "cwd"),
        (
            json!({"cwd": "relative/path", "mcpServers": []}),
            "absolute",
        ),
        (
            json!({"cwd": cwd(), "mcpServers": [{"name": "x", "command": "evil", "args": [], "env": []}]}),
            "MCP",
        ),
        (
            json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {"modle": "sk-typo"}}}),
            "launch",
        ),
        (
            json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {"model": ["sk-wrong-type"]}}}),
            "launch",
        ),
    ] {
        let answer = raw.call("session/new", &params);
        assert_eq!(answer["error"]["code"], -32602, "{answer}");
        assert!(detail(&answer).contains(says), "{answer}");
        assert!(
            !answer.to_string().contains("sk-"),
            "the input is not quoted: {answer}"
        );
    }
    assert!(
        raw.call("session/new", &json!({"cwd": cwd(), "mcpServers": []}))["result"]["sessionId"]
            .is_string()
    );
}

#[test]
fn a_prompt_it_cannot_carry_is_refused_and_starts_nothing() {
    let probe = Probe::new();
    let mut settings = quick();
    settings.max_prompt_bytes = 10;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let image = raw.call(
        "session/prompt",
        &json!({"sessionId": session, "prompt": [{"type": "image", "data": "x", "mimeType": "image/png"}]}),
    );
    assert_eq!(image["error"]["code"], -32602, "{image}");
    let long = raw.prompt(&session, "longer than ten bytes");
    assert!(detail(&long).contains("longer"), "{long}");
    let unknown = raw.prompt("no-such-session", "hi");
    assert!(unknown["error"].is_object(), "{unknown}");
    assert!(probe.starts.lock().unwrap().is_empty());
    let linked = raw.call(
        "session/prompt",
        &json!({"sessionId": session, "prompt": [{"type": "resource_link", "uri": "file:///a", "name": "a"}]}),
    );
    assert_eq!(linked["result"]["stopReason"], "end_turn", "{linked}");
    assert_eq!(probe.starts.lock().unwrap()[0].prompt, "file:///a");
}

#[test]
fn a_resume_names_an_identity_of_bounded_length_and_only_once() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    let long = raw.call(
        "session/resume",
        &json!({"sessionId": "x".repeat(5000), "cwd": cwd(), "mcpServers": []}),
    );
    assert!(detail(&long).contains("too long"), "{long}");
    assert!(probe.resumes.lock().unwrap().is_empty());
    let params = json!({"sessionId": "same", "cwd": cwd(), "mcpServers": []});
    assert_eq!(raw.call("session/resume", &params)["result"], json!({}));
    let twice = raw.call("session/resume", &params);
    assert!(detail(&twice).contains("already open"), "{twice}");
}
