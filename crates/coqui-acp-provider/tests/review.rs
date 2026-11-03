//! What a client or a provider could do to the facade beyond the plain limits: race it, flood it, name what
//! another connection holds, or write what a terminal acts on.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use coqui_acp_provider::{
    AgentEvent, COMPLETED_EVENT, DENIED_EVENT, EVENT_NOTIFICATION, FacadeAuth, TEXT_EVENT,
    TOOL_EVENT,
};
use serde_json::{Map, Value, json};
use support::{
    Call, Fault, Probe, Raw, cwd, detail, eventually, facade, joined, prompt_params, quick,
};

fn resume_params(id: &str) -> Value {
    json!({"sessionId": id, "cwd": cwd(), "mcpServers": []})
}

#[test]
fn a_prompt_while_a_resume_is_pending_is_refused_and_starts_nothing() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    probe.fault(Call::Resume, Fault::Slow);
    let resume = raw.send("session/resume", &resume_params("pending"));
    let prompt = raw.send("session/prompt", &prompt_params("pending", "too soon"));
    let refused = raw.answer(prompt);
    assert!(detail(&refused).contains("resumed"), "{refused}");
    assert_eq!(raw.answer(resume)["result"], json!({}));
    assert!(probe.starts.lock().unwrap().is_empty(), "no second session");
    assert_eq!(
        raw.prompt("pending", "now")["result"]["stopReason"],
        "end_turn"
    );
}

#[test]
fn a_failed_resume_leaves_no_session_running_whatever_came_meanwhile() {
    let probe = Probe::new();
    let (mut raw, served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    probe.fault(Call::Resume, Fault::SlowFail);
    let resume = raw.send("session/resume", &resume_params("gone"));
    let prompt = raw.send("session/prompt", &prompt_params("gone", "meanwhile"));
    assert!(raw.answer(prompt)["error"].is_object());
    assert!(raw.answer(resume)["error"].is_object());
    raw.hang_up();
    joined(served).expect("nothing was left behind");
    assert_eq!(
        probe.starts.lock().unwrap().len(),
        probe.closed.load(Ordering::SeqCst),
        "whatever started was closed"
    );
    assert!(probe.starts.lock().unwrap().is_empty());
}

#[test]
fn a_flood_of_wrong_credentials_gets_no_further_than_the_limit() {
    let probe = Probe::new();
    let checked = Arc::new(AtomicUsize::new(0));
    let counted = checked.clone();
    let mut settings = quick();
    settings.max_auth_failures = 5;
    let facade = facade(&probe, settings).with_auth(FacadeAuth {
        method: "token".to_owned(),
        name: "A token".to_owned(),
        check: Arc::new(move |meta: &Map<String, Value>| {
            counted.fetch_add(1, Ordering::SeqCst);
            meta.get("token") == Some(&json!("right"))
        }),
    });
    let (mut raw, _served) = Raw::serve(&facade);
    for _ in 0..2000 {
        raw.send(
            "authenticate",
            &json!({"methodId": "token", "_meta": {"token": "wrong"}}),
        );
    }
    let right = raw.send(
        "authenticate",
        &json!({"methodId": "token", "_meta": {"token": "right"}}),
    );
    let opened = raw.send("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
    for answer in [raw.answer_or_gone(right), raw.answer_or_gone(opened)]
        .into_iter()
        .flatten()
    {
        assert!(answer["error"].is_object(), "{answer}");
    }
    assert_eq!(checked.load(Ordering::SeqCst), 5, "the limit, then nothing");
}

/// The notifications a turn of `events` sends, against a turn limit of `limit` bytes.
fn sent_for(events: Vec<AgentEvent>, limit: usize) -> (usize, Value) {
    let probe = Probe::new();
    probe.script(move |_| events.clone());
    let mut settings = quick();
    settings.max_turn_bytes = limit;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let answer = raw.prompt(&session, "go");
    (raw.notification_bytes, answer)
}

#[test]
fn what_an_unknown_kind_sends_counts_in_full() {
    let mut events: Vec<_> = (0..5000).map(|_| AgentEvent::new("x", None)).collect();
    events.push(AgentEvent::new(COMPLETED_EVENT, None));
    let (sent, answer) = sent_for(events, 200_000);
    assert!(
        sent <= 200_000,
        "{sent} bytes sent past a 200000 byte limit"
    );
    assert!(detail(&answer).contains("more than"), "{answer}");
}

#[test]
fn what_a_denied_name_sends_counts_in_full() {
    let names = vec!["a"; 10_000].join(", ");
    let events = vec![
        AgentEvent::new(DENIED_EVENT, Some(names)),
        AgentEvent::new(COMPLETED_EVENT, None),
    ];
    let (sent, answer) = sent_for(events, 200_000);
    assert!(
        sent <= 200_000,
        "{sent} bytes sent past a 200000 byte limit"
    );
    assert!(detail(&answer).contains("more than"), "{answer}");
}

#[test]
fn a_held_session_cannot_be_resumed_from_another_connection() {
    let probe = Probe::new();
    *probe.identity.lock().unwrap() = Some("provider-7".to_owned());
    let facade = facade(&probe, quick());
    let (mut first, _a) = Raw::serve(&facade);
    let session = first.open();
    first.prompt(&session, "hi");
    eventually("the identity is told", || {
        first.notifications.iter().any(|message| {
            message["method"] == EVENT_NOTIFICATION
                && message["params"]["providerSessionId"] == "provider-7"
        })
    });
    let (mut second, _b) = Raw::serve(&facade);
    second.initialize();
    let refused = second.call("session/resume", &resume_params("provider-7"));
    assert!(detail(&refused).contains("already open"), "{refused}");
    assert!(probe.resumes.lock().unwrap().is_empty());
    first.call("session/close", &json!({"sessionId": session}));
    let deadline = std::time::Instant::now() + support::WAIT;
    while !second.call("session/resume", &resume_params("provider-7"))["error"].is_null() {
        assert!(std::time::Instant::now() < deadline, "never let go of");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn every_line_fits_the_update_limit_whatever_the_provider_names() {
    let probe = Probe::new();
    probe.script(|_| {
        let mut odd = AgentEvent::new("k".repeat(100_000), Some("\u{1}".repeat(5000)));
        odd.tools = vec!["t".repeat(1000); 1000];
        odd.quota = vec![
            coqui_acp_provider::QuotaReport {
                window: "w".repeat(100_000),
                used_permille: None,
                resets_at: None,
                status: Some("s".repeat(100_000)),
            };
            50
        ];
        let mut tool = AgentEvent::new(TOOL_EVENT, Some("\"".repeat(100_000)));
        tool.tools = vec!["\\".repeat(100_000)];
        vec![odd, tool, AgentEvent::new(COMPLETED_EVENT, None)]
    });
    let mut settings = quick();
    settings.max_update_bytes = 4000;
    settings.max_label_bytes = 1000;
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "go");
    let bound = 4000 + 2 * 1000 + 5 * 1024;
    assert!(
        raw.longest_line <= bound,
        "a line of {} bytes, past {bound}",
        raw.longest_line
    );
}

#[test]
fn text_is_split_by_what_it_takes_as_json() {
    let probe = Probe::new();
    let quotes = "\"".repeat(3000) + &"\u{1}".repeat(3000);
    let expected = quotes.clone();
    probe.script(move |_| {
        vec![
            AgentEvent::new(TEXT_EVENT, Some(quotes.clone())),
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
        let text = &update["content"]["text"];
        let length = text.to_string().len();
        assert!(length <= 1000 + 2, "a chunk of {length} bytes as JSON");
    }
}

#[test]
fn a_cancel_in_the_next_turn_reaches_the_provider_while_the_last_interrupt_runs() {
    let probe = Probe::new();
    probe.script(|_| Vec::new());
    let mut settings = quick();
    settings.call_timeout = Duration::from_secs(20);
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let first = raw.send("session/prompt", &prompt_params(&session, "one"));
    eventually("the provider has the prompt", || {
        !probe.starts.lock().unwrap().is_empty()
    });
    // This interrupt ends its turn, then does not return.
    probe.fault(Call::Interrupt, Fault::Hang);
    raw.notify("session/cancel", &json!({"sessionId": session}));
    assert_eq!(raw.answer(first)["result"]["stopReason"], "cancelled");
    let second = raw.send("session/prompt", &prompt_params(&session, "two"));
    eventually("the provider has the second prompt", || {
        probe.messages.lock().unwrap().len() == 2
    });
    raw.notify("session/cancel", &json!({"sessionId": session}));
    eventually("the second cancel reaches the provider", || {
        probe.interrupted.load(Ordering::SeqCst) == 2
    });
    assert_eq!(raw.answer(second)["result"]["stopReason"], "cancelled");
    probe.release();
}

#[test]
fn names_and_identities_lose_what_a_terminal_or_a_bidi_override_acts_on() {
    let probe = Probe::new();
    *probe.identity.lock().unwrap() = Some("evil\u{202e}di".to_owned());
    probe.script(|_| {
        let mut tool = AgentEvent::new(TOOL_EVENT, Some("Re\u{200b}ad".to_owned()));
        tool.tools.push("Read".to_owned());
        vec![
            tool,
            AgentEvent::new(
                DENIED_EVENT,
                Some("Ba\u{202e}sh\nrm, Web\u{2066}Fetch".to_owned()),
            ),
            AgentEvent::new(COMPLETED_EVENT, None),
        ]
    });
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    let session = raw.open();
    raw.prompt(&session, "go");
    let titles: Vec<String> = raw
        .updates()
        .iter()
        .filter(|update| update["sessionUpdate"] == "tool_call")
        .map(|update| update["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(titles, ["Read", "Bash rm", "WebFetch"]);
    let told = raw.notifications.iter().any(|message| {
        message["params"]
            .get("providerSessionId")
            .is_some_and(|id| !id.is_null())
    });
    assert!(!told, "an identity that is not what it shows is not told");
}

#[test]
fn mcp_servers_that_are_not_a_list_are_refused() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    let answer = raw.call("session/new", &json!({"cwd": cwd(), "mcpServers": "evil"}));
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
}

#[test]
fn a_close_limit_without_end_does_not_panic_the_connection() {
    let probe = Probe::new();
    let mut settings = quick();
    settings.close_timeout = Duration::MAX;
    let (mut raw, served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    raw.prompt(&session, "hi");
    raw.hang_up();
    joined(served).expect("the connection ends cleanly");
}
