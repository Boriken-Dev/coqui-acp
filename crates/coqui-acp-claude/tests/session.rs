//! A Claude Code session against a stand-in process that replays recorded transcripts.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_claude::harness::event::{Content, Event, Outcome, Stop, TurnEnd};
use coqui_acp_claude::{ClaudeSession, ClaudeSettings, PlainSpawn, SessionStart, Spawn};
use support::{Scratch, replaying};

fn start(
    launch: &coqui_acp_claude::ClaudeLaunch,
) -> (
    Result<ClaudeSession, coqui_acp_claude::Error>,
    mpsc::Receiver<Event>,
) {
    let (sender, events) = mpsc::channel();
    let session = ClaudeSession::start(launch, &PlainSpawn, ClaudeSettings::new(), move |event| {
        sender.send(event).is_ok()
    });
    (session, events)
}

fn until_end(events: &mpsc::Receiver<Event>) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(20))
            .expect("the turn did not end");
        let ended = matches!(event, Event::TurnEnd(_));
        seen.push(event);
        if ended {
            return seen;
        }
    }
}

fn recorded_args(path: &std::path::Path) -> Vec<String> {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A session runs, ends, and a resume reaches the same session with its own arguments.
#[test]
fn process_session_restarts_and_resumes_with_the_same_provider_id() {
    let scratch = Scratch::new("resume");
    let args = scratch.0.join("args.json");
    let mut first = replaying(Some("resume"), &scratch.0);
    first.session = SessionStart::New("6f2c1b0e-0000-4000-8000-00000000a5e5".into());
    first.append_system_prompt = Some("context before the prompt".into());
    first
        .env
        .insert("COQUI_ACP_CLAUDE_ARGS".into(), args.clone().into());
    let (session, events) = start(&first);
    let session = session.unwrap();
    assert_eq!(
        session.id().as_deref(),
        Some("6f2c1b0e-0000-4000-8000-00000000a5e5")
    );
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    let seen = until_end(&events);
    assert!(
        seen.iter().any(
            |event| matches!(event, Event::MessageChunk { text, .. } if text.contains("kayak"))
        )
    );
    session.close();
    assert!(
        recorded_args(&args)
            .iter()
            .any(|arg| arg == "--append-system-prompt=context before the prompt")
    );
    assert!(
        recorded_args(&args)
            .iter()
            .any(|arg| arg == "--session-id=6f2c1b0e-0000-4000-8000-00000000a5e5")
    );

    let mut resumed = replaying(Some("resume"), &scratch.0);
    resumed.session = SessionStart::Resume("6f2c1b0e-0000-4000-8000-00000000a5e5".into());
    resumed.model = Some("sonnet".into());
    resumed.effort = Some("high".into());
    resumed
        .env
        .insert("COQUI_ACP_CLAUDE_ARGS".into(), args.clone().into());
    resumed.env.insert("TOKEN".into(), "fresh".into());
    let (session, events) = start(&resumed);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    assert!(matches!(
        until_end(&events).last(),
        Some(Event::TurnEnd(TurnEnd::Stopped {
            stop: Stop::EndTurn,
            ..
        }))
    ));
    session.close();
    let args = recorded_args(&args);
    for expected in [
        "--resume=6f2c1b0e-0000-4000-8000-00000000a5e5",
        "--model=sonnet",
        "--effort=high",
        "--input-format=stream-json",
    ] {
        assert!(
            args.iter().any(|arg| arg == expected),
            "{expected} in {args:?}"
        );
    }
}

/// A session that reports another id than the one resumed is ended, and nothing of it is told.
#[test]
fn resume_rejects_a_different_session_reported_by_claude() {
    let scratch = Scratch::new("different");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.session = SessionStart::Resume("claude-session-42".into());
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_KEEP_SESSION".into(), "1".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    let mut told = Vec::new();
    let exited = loop {
        match events
            .recv_timeout(Duration::from_secs(20))
            .expect("the session did not end")
        {
            Event::Exited(reason) => break reason,
            other => told.push(other),
        }
    };
    assert!(exited.contains("not the one requested"), "{exited}");
    assert!(
        !told
            .iter()
            .any(|event| matches!(event, Event::MessageChunk { .. } | Event::TurnEnd(_))),
        "{told:?}"
    );
}

/// Claude Code with stream-json input writes nothing until it has read a line, so starting
/// writes first and returns without waiting for output of its own.
#[test]
fn resume_sends_its_message_at_once_to_a_harness_silent_until_it_reads_input() {
    let scratch = Scratch::new("silent");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.session = SessionStart::Resume("6f2c1b0e-0000-4000-8000-00000000a5e5".into());
    let started = std::time::Instant::now();
    let (session, events) = start(&launch);
    let session = session.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "starting waited for output"
    );
    session
        .prompt(&[Content::Text("are you there?".into())])
        .unwrap();
    until_end(&events);
}

/// A harness that exits early fails with its exit status and what it wrote last.
#[test]
fn a_harness_that_exits_early_fails_the_session_with_its_status_and_last_output() {
    let scratch = Scratch::new("exit");
    let mut launch = replaying(None, &scratch.0);
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_EXIT".into(), "3".into());
    let (session, _events) = start(&launch);
    let error = session.unwrap_err().to_string();
    assert!(error.contains("exited with code 3"), "{error}");
    assert!(error.contains("the stored login has expired"), "{error}");
}

/// The end of the harness's output is one failure, never a wait for ever.
#[test]
fn terminal_eof_emits_one_failed_event_instead_of_polling_forever() {
    let scratch = Scratch::new("eof");
    let mut launch = replaying(Some("tasks"), &scratch.0);
    launch.env.insert("COQUI_ACP_CLAUDE_END".into(), "1".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Plan the change.".into())])
        .unwrap();
    until_end(&events);
    let exited = events
        .recv_timeout(Duration::from_secs(20))
        .expect("the end of the output is told");
    assert!(
        matches!(&exited, Event::Exited(reason) if reason.contains("exited with code 0")),
        "{exited:?}"
    );
    assert!(events.recv_timeout(Duration::from_secs(2)).is_err(), "once");
    drop(session);
}

#[test]
fn a_permission_answered_through_the_responder_reaches_claude_code_and_the_turn_goes_on() {
    let scratch = Scratch::new("permission");
    let mut launch = replaying(Some("turn"), &scratch.0);
    launch.session = SessionStart::New("6f2c1b0e-0000-4000-8000-00000000c0de".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Change two to three.".into())])
        .unwrap();
    let mut answers = ["allow", "reject", "allow_always"].into_iter();
    loop {
        match events
            .recv_timeout(Duration::from_secs(20))
            .expect("the turn did not end")
        {
            Event::Permission(request) => request
                .responder
                .answer(Outcome::Selected(answers.next().unwrap().into()))
                .unwrap(),
            Event::TurnEnd(end) => {
                assert_eq!(end.stop(), Some(Stop::EndTurn));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(answers.next(), None, "three questions were asked");
    session.set_model("claude-sonnet-5-5").unwrap();
    session.set_effort(Some("medium")).unwrap();
    session.prompt(&[Content::Text("And now?".into())]).unwrap();
    assert!(
        until_end(&events).iter().any(
            |event| matches!(event, Event::MessageChunk { text, .. } if text.contains("Second"))
        )
    );
    assert_eq!(session.version().as_deref(), Some("2.1.318"));
    assert_eq!(session.models().expect("listed").len(), 5);
}

/// An interrupt is a control request, and the turn it stops ends as cancelled.
#[test]
fn interrupt_writes_a_control_request_and_the_turn_ends_cancelled() {
    let scratch = Scratch::new("interrupt");
    let mut launch = replaying(Some("interrupt"), &scratch.0);
    launch.session = SessionStart::New("6f2c1b0e-0000-4000-8000-00000000057a".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    session
        .prompt(&[Content::Text("Take your time.".into())])
        .unwrap();
    session.interrupt().unwrap();
    assert!(matches!(
        until_end(&events).last(),
        Some(Event::TurnEnd(TurnEnd::Stopped {
            stop: Stop::Cancelled,
            ..
        }))
    ));
    session.prompt(&[Content::Text("Go on.".into())]).unwrap();
    assert!(matches!(
        until_end(&events).last(),
        Some(Event::TurnEnd(TurnEnd::Stopped {
            stop: Stop::EndTurn,
            ..
        }))
    ));
}

#[test]
fn mcp_secrets_never_reach_the_command_line_and_their_file_is_removed_on_close() {
    let scratch = Scratch::new("mcp-secrets");
    let args = scratch.0.join("args.json");
    let copy = scratch.0.join("mcp.json");
    let mut launch = replaying(None, &scratch.0);
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_ARGS".into(), args.clone().into());
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_COPY".into(), copy.clone().into());
    launch.mcp_servers = vec![
        coqui_acp_claude::harness::McpServer::new(
            "docs",
            coqui_acp_claude::harness::McpTransport::Http {
                url: "https://docs.example.invalid/mcp".into(),
                headers: vec![("Authorization".into(), "Bearer s3cret-header".into())],
            },
        ),
        coqui_acp_claude::harness::McpServer::new(
            "files",
            coqui_acp_claude::harness::McpTransport::Stdio {
                command: "/bin/files".into(),
                args: Vec::new(),
                env: vec![("TOKEN".into(), "s3cret-env".into())],
            },
        ),
    ];
    let (session, _events) = start(&launch);
    let session = session.unwrap();
    let written = recorded_args(&args);
    assert!(
        !written.iter().any(|arg| arg.contains("s3cret")),
        "{written:?}"
    );
    let config = written
        .iter()
        .find_map(|arg| arg.strip_prefix("--mcp-config="))
        .expect("an MCP configuration");
    let copied: serde_json::Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
    assert_eq!(
        copied["mcpServers"]["docs"]["headers"]["Authorization"],
        "Bearer s3cret-header"
    );
    assert!(!format!("{:?}", launch.launch()).contains("s3cret"));
    session.close();
    drop(session);
    assert!(
        !std::path::Path::new(config).exists(),
        "the configuration file outlived the session"
    );
}

#[test]
fn an_overlong_diagnostic_line_does_not_end_the_session() {
    let scratch = Scratch::new("stderr");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.session = SessionStart::Resume("6f2c1b0e-0000-4000-8000-00000000a5e5".into());
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_STDERR".into(), "200000".into());
    let mut settings = ClaudeSettings::new();
    settings.max_line = 65_536;
    let (sender, events) = mpsc::channel();
    let session = ClaudeSession::start(&launch, &PlainSpawn, settings, move |event| {
        sender.send(event).is_ok()
    })
    .unwrap();
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    let seen = until_end(&events);
    assert!(
        matches!(
            seen.last(),
            Some(Event::TurnEnd(TurnEnd::Stopped {
                stop: Stop::EndTurn,
                ..
            }))
        ),
        "{seen:?}"
    );
}

/// The real Claude Code lists its models with no login and no turn: `initialize` is answered before any message.
#[test]
#[ignore = "needs Claude Code installed; set COQUI_ACP_TEST_LIVE_HARNESSES=1 and run with --ignored"]
fn live_claude_code_lists_models_with_no_login_and_no_turn() {
    assert!(
        std::env::var_os("COQUI_ACP_TEST_LIVE_HARNESSES").is_some(),
        "set COQUI_ACP_TEST_LIVE_HARNESSES=1"
    );
    let scratch = Scratch::new("live");
    let mut launch = coqui_acp_claude::ClaudeLaunch::new(&scratch.0);
    launch.clear_env = true;
    for name in ["PATH", "SYSTEMROOT", "PATHEXT", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            launch.env.insert(name.into(), value);
        }
    }
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        launch.env.insert(name.into(), scratch.0.clone().into());
    }
    launch
        .env
        .insert("CLAUDE_CONFIG_DIR".into(), scratch.0.join("config").into());
    launch
        .env
        .insert("ANTHROPIC_API_KEY".into(), "dummy-not-a-key".into());
    launch
        .env
        .insert("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:9".into());
    let (session, _events) = start(&launch);
    let session = session.unwrap();
    assert!(!session.models().expect("listed").is_empty());
    assert!(session.version().is_some());
    session.close();
}

/// What is told of a session that came back as another keeps that other session's id, for whoever has to find it.
#[test]
fn a_different_session_is_named_to_the_sink_before_the_end() {
    let scratch = Scratch::new("different");
    let mut launch = replaying(Some("resume"), &scratch.0);
    launch.session = SessionStart::Resume("6f2c1b0e-0000-4000-8000-00000000bbbb".into());
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_KEEP_SESSION".into(), "1".into());
    let (session, events) = start(&launch);
    let session = session.unwrap();
    let _ = session.prompt(&[Content::Text("Which word?".into())]);
    let mut told = Vec::new();
    while let Ok(event) = events.recv_timeout(Duration::from_secs(20)) {
        let over = matches!(event, Event::Exited(_));
        told.push(event);
        if over {
            break;
        }
    }
    assert!(
        told.iter().any(|event| matches!(
            event,
            Event::SessionId(id) if id == "6f2c1b0e-0000-4000-8000-00000000a5e5"
        )),
        "{told:?}"
    );
}

/// Something it started holds its output open: its exit is still noticed, and what it left behind is ended.
#[test]
fn an_exit_with_its_output_held_open_ends_the_session() {
    let scratch = Scratch::new("held");
    let mut launch = replaying(None, &scratch.0);
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_EXIT".into(), "3".into());
    launch
        .env
        .insert("COQUI_ACP_CLAUDE_LEAVE_CHILD".into(), "1".into());
    let mut settings = ClaudeSettings::new();
    settings.exit_poll = Duration::from_millis(10);
    settings.exit_wait = Duration::from_millis(200);
    settings.initialize_timeout = Duration::from_secs(20);
    let (sender, events) = mpsc::channel();
    let failed = ClaudeSession::start(&launch, &PlainSpawn, settings, move |event| {
        sender.send(event).is_ok()
    });
    let started = std::time::Instant::now();
    let error = failed.unwrap_err().to_string();
    assert!(error.contains("code 3"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    drop(events);
}

#[test]
fn a_line_the_host_refuses_ends_claude_code() {
    let scratch = Scratch::new("hook");
    let mut settings = ClaudeSettings::new();
    settings.on_line = Some(std::sync::Arc::new(|line| line["type"] != "assistant"));
    let (sender, events) = mpsc::channel();
    let session = ClaudeSession::start(
        &replaying(Some("resume"), &scratch.0),
        &PlainSpawn,
        settings,
        move |event| sender.send(event).is_ok(),
    )
    .unwrap();
    session
        .prompt(&[Content::Text("Which word?".into())])
        .unwrap();
    let mut told = Vec::new();
    while let Ok(event) = events.recv_timeout(Duration::from_secs(20)) {
        let over = matches!(event, Event::Exited(_));
        told.push(event);
        if over {
            break;
        }
    }
    assert!(
        matches!(told.last(), Some(Event::Exited(why)) if why.contains("refused")),
        "{told:?}"
    );
    assert!(
        !told.iter().any(|event| matches!(event, Event::TurnEnd(_))),
        "{told:?}"
    );
}

#[test]
fn claude_code_can_choose_the_session_id() {
    let scratch = Scratch::new("chosen");
    let mut launch = replaying(Some("cli_session"), &scratch.0);
    launch.session = SessionStart::Chosen;
    let recorded = support::recorded("cli_session")
        .iter()
        .find_map(|line| line.get("session_id")?.as_str().map(str::to_string))
        .unwrap();
    let (session, events) = start(&launch);
    let session = session.unwrap();
    assert_eq!(session.id(), None);
    session
        .prompt(&[Content::Text("Who named this session?".into())])
        .unwrap();
    let told = until_end(&events);
    assert!(
        told.iter()
            .any(|event| matches!(event, Event::SessionId(id) if *id == recorded)),
        "{told:?}"
    );
    assert_eq!(session.id(), Some(recorded));
}

/// A close waits for the process to have gone, as the caller may remove its directory next.
#[test]
fn a_close_waits_for_a_process_that_takes_its_time_to_end() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Slow(
        std::sync::Arc<AtomicBool>,
        Box<dyn coqui_acp_claude::harness::Process>,
    );
    impl coqui_acp_claude::harness::Process for Slow {
        fn kill(&self) -> std::io::Result<()> {
            let _ = self.1.kill();
            let gone = std::sync::Arc::clone(&self.0);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                gone.store(true, Ordering::SeqCst);
            });
            Ok(())
        }
        fn try_wait(&self) -> std::io::Result<Option<coqui_acp_claude::harness::Exit>> {
            if self.0.load(Ordering::SeqCst) {
                Err(std::io::Error::other("its status is not told"))
            } else {
                Ok(None)
            }
        }
    }
    struct PlainGone;
    impl coqui_acp_claude::harness::Process for PlainGone {
        fn kill(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn try_wait(&self) -> std::io::Result<Option<coqui_acp_claude::harness::Exit>> {
            Ok(None)
        }
    }
    struct OverPipes(std::sync::Arc<AtomicBool>);
    impl coqui_acp_claude::Spawn for OverPipes {
        fn spawn(
            &self,
            launch: &coqui_acp_claude::Launch,
        ) -> std::io::Result<coqui_acp_claude::Spawned> {
            let mut spawned = PlainSpawn.spawn(launch)?;
            let process = core::mem::replace(&mut spawned.process, Box::new(PlainGone));
            spawned.process = Box::new(Slow(std::sync::Arc::clone(&self.0), process));
            Ok(spawned)
        }
    }
    let scratch = Scratch::new("slow");
    let gone = std::sync::Arc::new(AtomicBool::new(false));
    let mut settings = ClaudeSettings::new();
    settings.close_wait = Duration::from_secs(10);
    let session = ClaudeSession::start(
        &replaying(None, &scratch.0),
        &OverPipes(std::sync::Arc::clone(&gone)),
        settings,
        |_| true,
    )
    .unwrap();
    session.close();
    assert!(gone.load(Ordering::SeqCst), "the close did not wait");
}
