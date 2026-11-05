//! A Codex session against a stand-in process that replays recorded app-server traffic.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_codex::harness::event::{Content, Event, Outcome, Stop, TurnEnd};
use coqui_acp_codex::{CodexLaunch, CodexSession, CodexSettings, PlainSpawn, ThreadStart};
use serde_json::Value;
use support::{Scratch, replaying};

/// The thread of the `resume` recording.
const RESUMED: &str = "01a1a7cb-a273-75b0-8d9a-b9e012cce172";

fn start(
    launch: &CodexLaunch,
) -> (
    Result<CodexSession, coqui_acp_codex::Error>,
    mpsc::Receiver<Event>,
) {
    let (sender, events) = mpsc::channel();
    let session = CodexSession::start(launch, &PlainSpawn, CodexSettings::new(), move |event| {
        sender.send(event).is_ok()
    });
    (session, events)
}

/// Takes events until the turn ends, answering each permission request with the next of `answers`.
fn until_end(
    events: &mpsc::Receiver<Event>,
    answers: &mut dyn Iterator<Item = &str>,
) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        match events
            .recv_timeout(Duration::from_secs(20))
            .expect("the turn did not end")
        {
            Event::Permission(request) => {
                let answer = answers.next().expect("an answer for each question");
                request
                    .responder
                    .answer(Outcome::Selected(answer.into()))
                    .unwrap();
            }
            event => {
                let ended = matches!(event, Event::TurnEnd(_));
                seen.push(event);
                if ended {
                    return seen;
                }
            }
        }
    }
}

fn inputs(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Carried over from AI6: a thread runs, the app server ends, and a resume reaches the same thread.
#[test]
fn process_session_restarts_and_resumes_with_the_same_provider_thread() {
    let scratch = Scratch::new("restart");
    let mut first = replaying(Some("turn"), &scratch.0);
    first.developer_instructions = Some("context before the prompt".into());
    let written = scratch.0.join("first.jsonl");
    first
        .env
        .insert("COQUI_ACP_CODEX_INPUTS".into(), written.clone().into());
    let (session, events) = start(&first);
    let session = session.unwrap();
    assert_eq!(session.thread(), "01a1a7cb-a273-75b0-8d9a-b9e012cce172");
    session
        .prompt(&[Content::Text("Change two to three.".into())])
        .unwrap();
    let seen = until_end(
        &events,
        &mut ["accept", "decline", "accept_for_session", "accept"].into_iter(),
    );
    assert!(matches!(
        seen.last(),
        Some(Event::TurnEnd(TurnEnd::Stopped(Stop::EndTurn)))
    ));
    session.close();
    let sent = inputs(&written);
    let thread_start = sent
        .iter()
        .find(|input| input["method"] == "thread/start")
        .unwrap();
    assert_eq!(
        thread_start["params"]["developerInstructions"],
        "context before the prompt"
    );

    let mut resumed = replaying(Some("resume"), &scratch.0);
    resumed.thread = ThreadStart::Resume(RESUMED.into());
    resumed.model = Some("gpt-5".into());
    resumed.effort = Some("high".into());
    let written = scratch.0.join("resumed.jsonl");
    resumed
        .env
        .insert("COQUI_ACP_CODEX_INPUTS".into(), written.clone().into());
    let (session, events) = start(&resumed);
    let session = session.unwrap();
    assert_eq!(session.thread(), RESUMED);
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    let seen = until_end(&events, &mut std::iter::empty());
    assert!(
        seen.iter().any(
            |event| matches!(event, Event::MessageChunk { text, .. } if text.contains("kayak"))
        )
    );
    session.close();
    // Carried over from AI6: resume reattaches and submits a turn to the same thread.
    let sent = inputs(&written);
    let methods: Vec<_> = sent
        .iter()
        .filter_map(|input| input["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "model/list",
            "thread/resume",
            "turn/start"
        ]
    );
    let resume = &sent[3]["params"];
    assert_eq!(
        (
            resume["threadId"].as_str(),
            resume["excludeTurns"].as_bool(),
            resume["model"].as_str()
        ),
        (Some(RESUMED), Some(true), Some("gpt-5"))
    );
    assert_eq!(resume["cwd"], scratch.0.to_string_lossy().as_ref());
    let turn = &sent[4]["params"];
    assert_eq!(
        (
            turn["threadId"].as_str(),
            turn["effort"].as_str(),
            turn["input"][0]["text"].as_str()
        ),
        (Some(RESUMED), Some("high"), Some("Which word?"))
    );
}

/// Carried over from AI6: a resume that reaches another thread closes the app server and fails.
#[test]
fn resume_rejects_a_different_thread_and_closes_the_provider_process() {
    let scratch = Scratch::new("different");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.thread = ThreadStart::Resume("codex-thread-42".into());
    let (session, _) = start(&launch);
    let error = session.unwrap_err().to_string();
    assert!(error.contains("not the one requested"), "{error}");
}

/// Carried over from AI6: an app server that exits fails with its status and what it wrote last.
#[test]
fn an_app_server_that_exits_fails_the_session_with_its_status_and_last_output() {
    let scratch = Scratch::new("exit");
    let mut launch = replaying(None, &scratch.0);
    launch.env.insert("COQUI_ACP_CODEX_EXIT".into(), "2".into());
    let (session, _) = start(&launch);
    let error = session.unwrap_err().to_string();
    assert!(error.contains("exited with code 2"), "{error}");
    assert!(error.contains("failed to load configuration"), "{error}");
}

/// Carried over from AI6: the end of the output is one failure, never an idle wait.
#[test]
fn terminal_eof_is_a_provider_failure_not_an_idle_timeout() {
    let scratch = Scratch::new("eof");
    let mut launch = replaying(Some("errors"), &scratch.0);
    launch.env.insert("COQUI_ACP_CODEX_END".into(), "1".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Hit the limit.".into())])
        .unwrap();
    assert!(matches!(
        until_end(&events, &mut std::iter::empty()).last(),
        Some(Event::TurnEnd(TurnEnd::Limited(_)))
    ));
    session.prompt(&[Content::Text("Fail.".into())]).unwrap();
    assert!(matches!(
        until_end(&events, &mut std::iter::empty()).last(),
        Some(Event::TurnEnd(TurnEnd::Failed(_)))
    ));
    let exited = events
        .recv_timeout(Duration::from_secs(20))
        .expect("the end of the output is told");
    assert!(
        matches!(&exited, Event::Exited(reason) if reason.contains("exited with code 0")),
        "{exited:?}"
    );
    assert!(events.recv_timeout(Duration::from_secs(2)).is_err(), "once");
}

#[test]
fn an_interrupt_names_the_running_turn_and_the_turn_ends_cancelled() {
    let scratch = Scratch::new("interrupt");
    let launch = replaying(Some("interrupt"), &scratch.0);
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Take your time.".into())])
        .unwrap();
    session.interrupt().unwrap();
    assert!(matches!(
        until_end(&events, &mut std::iter::empty()).last(),
        Some(Event::TurnEnd(TurnEnd::Stopped(Stop::Cancelled)))
    ));
    session.prompt(&[Content::Text("Go on.".into())]).unwrap();
    assert!(matches!(
        until_end(&events, &mut std::iter::empty()).last(),
        Some(Event::TurnEnd(TurnEnd::Stopped(Stop::EndTurn)))
    ));
    session.interrupt().unwrap();
}

#[test]
fn a_load_replays_the_threads_turns_before_it_returns() {
    let scratch = Scratch::new("load");
    let mut launch = replaying(None, &scratch.0);
    launch.thread = ThreadStart::Load(RESUMED.into());
    let resumed = std::fs::read_to_string(support::fixtures().join("resume.stdout")).unwrap();
    let answer: Value = resumed
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line.pointer("/result/thread/turns").is_some())
        .unwrap();
    // A stand-in that answers `thread/resume` with the recorded thread.
    let recording = scratch.0.join("load");
    std::fs::write(recording.with_extension("stdout"), format!("{answer}\n")).unwrap();
    let steps = serde_json::json!([{"after": 0, "input": {"id": 3, "method": "thread/resume", "params": {}}}]);
    std::fs::write(recording.with_extension("steps.json"), steps.to_string()).unwrap();
    launch
        .env
        .insert("COQUI_ACP_CODEX_REPLAY".into(), recording.into());
    let (sender, events) = mpsc::channel();
    let session = CodexSession::start(&launch, &PlainSpawn, CodexSettings::new(), move |event| {
        sender.send(event).is_ok()
    })
    .unwrap();
    let replayed: Vec<_> = events.try_iter().collect();
    assert!(
        replayed
            .iter()
            .any(|event| matches!(event, Event::UserChunk { .. })),
        "{replayed:?}"
    );
    assert!(
        replayed
            .iter()
            .any(|event| matches!(event, Event::MessageEnd { .. }))
    );
    session.close();
}
