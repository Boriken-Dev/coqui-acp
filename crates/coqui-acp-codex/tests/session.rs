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
fn resume_thread() -> String {
    support::recorded_thread("resume")
}

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

/// a thread runs, the app server ends, and a resume reaches the same thread.
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
    assert_eq!(session.thread(), support::recorded_thread("turn"));
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
    resumed.thread = ThreadStart::Resume(resume_thread());
    resumed.model = Some("gpt-5".into());
    resumed.effort = Some("high".into());
    let written = scratch.0.join("resumed.jsonl");
    resumed
        .env
        .insert("COQUI_ACP_CODEX_INPUTS".into(), written.clone().into());
    let (session, events) = start(&resumed);
    let session = session.unwrap();
    assert_eq!(session.thread(), resume_thread());
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
    // resume reattaches and submits a turn to the same thread.
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
        (Some(resume_thread().as_str()), Some(true), Some("gpt-5"))
    );
    assert_eq!(resume["cwd"], scratch.0.to_string_lossy().as_ref());
    let turn = &sent[4]["params"];
    assert_eq!(
        (
            turn["threadId"].as_str(),
            turn["effort"].as_str(),
            turn["input"][0]["text"].as_str()
        ),
        (
            Some(resume_thread().as_str()),
            Some("high"),
            Some("Which word?")
        )
    );
}

/// a resume that reaches another thread closes the app server and fails.
#[test]
fn resume_rejects_a_different_thread_and_closes_the_provider_process() {
    let scratch = Scratch::new("different");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.thread = ThreadStart::Resume("codex-thread-42".into());
    let (session, _) = start(&launch);
    let error = session.unwrap_err().to_string();
    assert!(error.contains("not the one requested"), "{error}");
}

/// an app server that exits fails with its status and what it wrote last.
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

/// the end of the output is one failure, never an idle wait.
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
    launch.thread = ThreadStart::Load(resume_thread());
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

/// The real Codex lists its models with no login and no turn, and opens a thread on them.
#[test]
#[ignore = "needs Codex installed; set COQUI_ACP_TEST_LIVE_HARNESSES=1 and run with --ignored"]
fn live_codex_lists_models_with_no_login_and_no_turn() {
    assert!(
        std::env::var_os("COQUI_ACP_TEST_LIVE_HARNESSES").is_some(),
        "set COQUI_ACP_TEST_LIVE_HARNESSES=1"
    );
    let scratch = Scratch::new("live");
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut launch = CodexLaunch::new(&scratch.0);
    launch.clear_env = true;
    for name in ["PATH", "SYSTEMROOT", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            launch.env.insert(name.into(), value);
        }
    }
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        launch.env.insert(name.into(), scratch.0.clone().into());
    }
    launch.env.insert("CODEX_HOME".into(), home.into());
    launch
        .env
        .insert("HTTPS_PROXY".into(), "http://127.0.0.1:9".into());
    let session =
        CodexSession::start(&launch, &PlainSpawn, CodexSettings::new(), |_| true).unwrap();
    assert!(!session.models().is_empty());
    assert!(session.version().is_some());
    assert!(!session.thread().is_empty());
    session.close();
}

/// A key that cannot be written is refused before anything starts.
#[test]
fn a_config_key_that_cannot_be_written_is_refused() {
    let scratch = Scratch::new("key");
    let mut launch = replaying(None, &scratch.0);
    launch.config_overrides = vec![("model=x".into(), Value::Bool(true))];
    let refused = CodexSession::start(&launch, &PlainSpawn, CodexSettings::new(), |_| true);
    assert!(matches!(refused, Err(coqui_acp_codex::Error::Refused(_))));
}

/// The real Codex's commands end with its process tree: with a terminal and without, sandboxed by its own default and
/// not. Nothing here reaches a model.
#[test]
#[ignore = "needs Codex installed; set COQUI_ACP_TEST_LIVE_HARNESSES=1 and run with --ignored"]
fn live_a_killed_app_server_leaves_none_of_its_commands_running() {
    use std::io::{BufRead, BufReader, Write};

    use coqui_acp_codex::{Launch, Spawn};

    assert!(
        std::env::var_os("COQUI_ACP_TEST_LIVE_HARNESSES").is_some(),
        "set COQUI_ACP_TEST_LIVE_HARNESSES=1"
    );
    let loop_writing = |marker: &std::path::Path| -> Vec<String> {
        let path = marker.to_string_lossy();
        if cfg!(windows) {
            vec![
                "powershell.exe".into(),
                "-NoProfile".into(),
                "-Command".into(),
                format!(
                    "while ($true) {{ Add-Content -LiteralPath '{path}' x; Start-Sleep -Milliseconds 100 }}"
                ),
            ]
        } else {
            vec![
                "sh".into(),
                "-c".into(),
                format!("while :; do echo x >> '{path}'; sleep 0.1; done"),
            ]
        }
    };
    for (tty, sandbox) in [
        (true, Value::Null),
        (false, Value::Null),
        (true, serde_json::json!({"type": "dangerFullAccess"})),
        (false, serde_json::json!({"type": "dangerFullAccess"})),
    ] {
        let scratch = Scratch::new("tree");
        let home = scratch.0.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let marker = scratch.0.join("marker.txt");
        let mut launch = Launch::new("codex")
            .arg("app-server")
            .cwd(scratch.0.clone());
        launch.env.insert("CODEX_HOME".into(), home.into());
        let spawned = PlainSpawn.spawn(&launch).unwrap();
        let mut stdin = spawned.stdin;
        let mut send = |message: Value| {
            let mut line = serde_json::to_vec(&message).unwrap();
            line.push(b'\n');
            stdin.write_all(&line).unwrap();
            stdin.flush().unwrap();
        };
        let (lines, read) = mpsc::channel();
        let stdout = spawned.stdout;
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
        send(
            serde_json::json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "test", "version": "0"}, "capabilities": {}}}),
        );
        read.recv_timeout(Duration::from_secs(30)).unwrap();
        send(serde_json::json!({"method": "initialized", "params": {}}));
        let mut exec = serde_json::json!({"id": 2, "method": "command/exec", "params": {
            "command": loop_writing(&marker), "tty": tty, "processId": "tree", "streamStdoutStderr": true,
            "disableTimeout": true, "cwd": scratch.0.to_string_lossy()}});
        if !sandbox.is_null() {
            exec["params"]["sandboxPolicy"] = sandbox.clone();
        }
        send(exec);
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while std::fs::metadata(&marker).map_or(0, |found| found.len()) < 8 {
            assert!(
                std::time::Instant::now() < until,
                "the command never ran: tty {tty}, sandbox {sandbox}: {:?}",
                read.try_iter().collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        spawned.process.kill().unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let then = std::fs::metadata(&marker).unwrap().len();
        std::thread::sleep(Duration::from_secs(2));
        let now = std::fs::metadata(&marker).unwrap().len();
        assert_eq!(then, now, "still running: tty {tty}, sandbox {sandbox}");
        drop(scratch);
    }
}
