//! A Pi session against the stand-in: what it starts with, a turn, the permission gate, stopping, reopening, and the models.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::Path;

use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::{
    Content, Event, FileChange, Outcome, Severity, Stop, ToolContent, ToolStatus, TurnEnd,
};
use coqui_acp_pi::{Error, GATE_SOURCE, PiLaunch, PiSession, PiSettings, PlainSpawn, SessionStart};
use support::{Events, Scratch, launching, logged};

struct World {
    scratch: Scratch,
    cwd: std::path::PathBuf,
    home: std::path::PathBuf,
}

fn world(name: &str) -> World {
    let scratch = Scratch::new(name);
    let cwd = scratch.dir("work");
    let home = scratch.dir("home");
    World { scratch, cwd, home }
}

impl World {
    fn launch(&self) -> PiLaunch {
        launching(&self.cwd, &self.home)
    }

    fn start(&self, launch: &PiLaunch) -> (PiSession, Events) {
        let (sink, events) = Events::new();
        let session =
            PiSession::start(launch, &PlainSpawn, PiSettings::new(), sink).expect("it starts");
        (session, events)
    }
}

fn say(session: &PiSession, text: &str) {
    session
        .prompt(&[Content::Text(text.into())])
        .expect("taken");
}

fn end_of(events: &Events) -> &TurnEnd {
    events
        .seen
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::TurnEnd(end) => Some(end),
            _ => None,
        })
        .expect("a turn end")
}

#[test]
fn a_session_opens_lists_what_pi_offers_runs_a_turn_and_titles_it() {
    let world = world("turn");
    let (session, mut events) = world.start(&world.launch());
    assert_eq!(session.session().len(), 36);
    assert_eq!(
        session
            .models()
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["stub/stub-fast", "stub/stub-think"]
    );
    let (model, level, levels) = session.choice();
    assert_eq!(model.as_deref(), Some("stub/stub-think"));
    assert_eq!(level.as_deref(), Some("medium"));
    assert_eq!(levels, ["off", "minimal", "low", "medium", "high"]);
    say(&session, "hello");
    events.turn();
    assert_eq!(events.said(), "echo: hello");
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::EndTurn));
    events.drain();
    assert!(
        events
            .seen
            .iter()
            .any(|event| matches!(event, Event::Title(title) if title == "hello"))
    );
    assert!(
        events
            .seen
            .iter()
            .any(|event| matches!(event, Event::Commands(commands) if commands[0].name == "fake"))
    );
    assert!(
        events
            .seen
            .iter()
            .any(|event| matches!(event, Event::Usage(usage) if usage.context_size == Some(32000)))
    );
}

#[test]
fn the_gate_is_a_private_file_that_goes_with_the_session_and_nothing_is_written_to_pis_configuration()
 {
    let world = world("gate");
    let mut launch = world.launch();
    launch.gate_dir = Some(world.scratch.dir("gates"));
    let (session, _events) = world.start(&launch);
    let started = &logged(&world.home)[0];
    let args = started["args"].as_array().unwrap();
    let at = args
        .iter()
        .position(|arg| arg == "-e")
        .expect("the gate is loaded with -e");
    let extension = Path::new(args[at + 1].as_str().unwrap());
    assert!(extension.starts_with(world.scratch.0.join("gates")));
    assert_eq!(started["gate"], GATE_SOURCE);
    assert_eq!(&args[..2], ["--mode", "rpc"]);
    assert_eq!(
        Path::new(started["cwd"].as_str().unwrap()).file_name(),
        world.cwd.file_name()
    );
    assert!(extension.exists());
    drop(session);
    assert!(!extension.exists(), "the gate went with the session");
    assert_eq!(
        std::fs::read_dir(world.scratch.0.join("gates"))
            .unwrap()
            .count(),
        0
    );
    // Pi's agent directory holds what the stand-in kept, and no extension.
    let kept: Vec<_> = std::fs::read_dir(&world.home)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    assert!(
        kept.iter()
            .all(|name| name == "fake-pi.log" || name == "sessions"),
        "{kept:?}"
    );
}

#[test]
fn a_command_in_manual_mode_is_asked_and_runs_only_when_allowed() {
    for (answer, said, ran) in [
        (Outcome::Selected("allow".into()), "echo: allowed", true),
        (Outcome::Selected("reject".into()), "echo: refused", false),
    ] {
        let world = world("ask");
        let (session, mut events) = world.start(&world.launch());
        say(&session, "ask");
        events.until(|event| matches!(event, Event::Permission(_)));
        let Some(Event::Permission(request)) = events.seen.pop() else {
            panic!("no question");
        };
        assert_eq!(request.title, "Run echo checked > checked.txt");
        request.responder.answer(answer).unwrap();
        events.turn();
        assert_eq!(events.said(), said);
        assert_eq!(world.cwd.join("checked.txt").exists(), ran);
    }
}

#[test]
fn an_answer_of_cancelled_ends_the_turn_cancelled_and_the_call_does_not_run() {
    let world = world("cancel-answer");
    let (session, mut events) = world.start(&world.launch());
    say(&session, "ask");
    events.until(|event| matches!(event, Event::Permission(_)));
    let Some(Event::Permission(request)) = events.seen.pop() else {
        panic!("no question");
    };
    request.responder.answer(Outcome::Cancelled).unwrap();
    events.turn();
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::Cancelled));
    assert!(!world.cwd.join("checked.txt").exists());
}

#[test]
fn stopping_a_run_with_a_question_open_cancels_the_question_first_and_the_turn_ends_cancelled() {
    let world = world("stop-asking");
    let (session, mut events) = world.start(&world.launch());
    say(&session, "ask");
    events.until(|event| matches!(event, Event::Permission(_)));
    let Some(Event::Permission(request)) = events.seen.pop() else {
        panic!("no question");
    };
    session.interrupt().unwrap();
    events.turn();
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::Cancelled));
    // The client answers cancelled afterwards, as ACP has it do.
    request.responder.answer(Outcome::Cancelled).unwrap();
    assert!(!world.cwd.join("checked.txt").exists());
    // And the session goes on.
    say(&session, "again");
    events.turn();
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::EndTurn));
}

#[test]
fn a_waiting_turn_ends_cancelled_when_stopped_and_stopping_when_idle_does_nothing() {
    let world = world("wait");
    let (session, mut events) = world.start(&world.launch());
    session.interrupt().unwrap();
    say(&session, "wait");
    session.interrupt().unwrap();
    events.turn();
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::Cancelled));
}

#[test]
fn the_mode_decides_which_calls_are_asked_which_run_and_which_are_refused_as_a_plan() {
    let world = world("modes");
    let (session, mut events) = world.start(&world.launch());
    session.set_mode(Mode::Bypass);
    say(&session, "ask");
    events.turn();
    assert_eq!(events.said(), "echo: allowed");
    assert!(world.cwd.join("checked.txt").exists());
    assert!(
        !events
            .seen
            .iter()
            .any(|event| matches!(event, Event::Permission(_)))
    );
    std::fs::remove_file(world.cwd.join("checked.txt")).unwrap();
    session.set_mode(Mode::Plan);
    events.seen.clear();
    say(&session, "ask");
    events.turn();
    assert_eq!(events.said(), "echo: refused");
    assert!(!world.cwd.join("checked.txt").exists());
    assert!(events.seen.iter().any(
        |event| matches!(event, Event::Tool(call) if call.status == Some(ToolStatus::Failed))
    ));
}

#[test]
fn a_write_and_an_edit_show_their_diffs_and_a_read_its_text() {
    let world = world("tools");
    std::fs::write(world.cwd.join("notes.txt"), "alpha beta\ngamma\n").unwrap();
    let mut launch = world.launch();
    launch.mode = Some(Mode::AcceptEdits);
    let (session, mut events) = world.start(&launch);
    say(&session, "[[edit:notes.txt]] change it");
    events.turn();
    assert_eq!(
        std::fs::read_to_string(world.cwd.join("notes.txt")).unwrap(),
        "ALPHA beta\ngamma\n"
    );
    let diff = events.seen.iter().find_map(|event| match event {
        Event::Tool(call) if call.status == Some(ToolStatus::Completed) => call.content.clone(),
        _ => None,
    });
    let Some(ToolContent::Diff(diff)) = diff.and_then(|content| content.into_iter().next()) else {
        panic!("no diff");
    };
    assert!(
        diff.patch
            .is_some_and(|patch| patch.contains("+ALPHA beta"))
    );
    events.seen.clear();
    say(&session, "[[write:out/new.txt]] write");
    events.turn();
    let diff = events
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::Tool(call) => call.content.as_ref(),
            _ => None,
        })
        .flatten()
        .find_map(|content| match content {
            ToolContent::Diff(diff) => Some(diff.clone()),
            _ => None,
        })
        .expect("a diff");
    assert_eq!(diff.change, FileChange::Add);
    assert!(world.cwd.join("out").join("new.txt").exists());
    events.seen.clear();
    // Writing it again replaces a file the parser read first.
    say(&session, "[[write:out/new.txt]] again");
    events.turn();
    let old = events.seen.iter().find_map(|event| match event {
        Event::Tool(call) => call
            .content
            .as_ref()?
            .iter()
            .find_map(|content| match content {
                ToolContent::Diff(diff) => diff.old_text.clone(),
                _ => None,
            }),
        _ => None,
    });
    assert_eq!(old.as_deref(), Some("written by stand-in\nline two\n"));
}

#[test]
fn a_session_reopens_by_its_id_resumed_or_with_its_history_replayed() {
    let world = world("reopen");
    let (first, mut events) = world.start(&world.launch());
    say(&first, "first");
    events.turn();
    let id = first.session().to_string();
    drop(first);

    let mut launch = world.launch();
    launch.session = SessionStart::Resume(id.clone());
    let (resumed, mut events) = world.start(&launch);
    assert_eq!(resumed.session(), id);
    events.drain();
    assert!(
        !events
            .seen
            .iter()
            .any(|event| matches!(event, Event::UserChunk { .. })),
        "a resume replays nothing"
    );
    say(&resumed, "again");
    events.turn();
    assert_eq!(events.said(), "echo: again");
    drop(resumed);

    launch.session = SessionStart::Load(id.clone());
    let (loaded, mut events) = world.start(&launch);
    events.drain();
    let user: Vec<_> = events
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::UserChunk {
                content: Content::Text(text),
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(user, ["first", "again"]);
    assert!(events.said().contains("echo: first"), "{}", events.said());
    drop(loaded);

    // A session Pi has none of is refused in its words, and a bad id before Pi is asked.
    launch.session = SessionStart::Resume("00000000-0000-0000-0000-000000000000".into());
    let (sink, _events) = Events::new();
    let refused = PiSession::start(&launch, &PlainSpawn, PiSettings::new(), sink).unwrap_err();
    assert!(
        refused.to_string().contains("No session found"),
        "{refused}"
    );
    launch.session = SessionStart::Resume("../x".into());
    let (sink, _events) = Events::new();
    assert!(matches!(
        PiSession::start(&launch, &PlainSpawn, PiSettings::new(), sink),
        Err(Error::Refused(_))
    ));
}

#[test]
fn the_model_and_the_thinking_level_are_chosen_from_what_pi_offers() {
    let world = world("choose");
    let (session, _events) = world.start(&world.launch());
    session.set_effort("high").unwrap();
    assert_eq!(session.choice().1.as_deref(), Some("high"));
    assert!(
        matches!(session.set_effort("xhigh"), Err(Error::Refused(_))),
        "Pi would clamp it silently"
    );
    session.set_model("stub/stub-fast").unwrap();
    let (model, level, levels) = session.choice();
    assert_eq!(model.as_deref(), Some("stub/stub-fast"));
    assert_eq!(level.as_deref(), Some("off"));
    assert_eq!(levels, ["off"]);
    assert!(matches!(
        session.set_model("stub/none"),
        Err(Error::Refused(_))
    ));
    assert!(matches!(session.set_effort("high"), Err(Error::Refused(_))));
    let mut launch = world.launch();
    launch.model = Some("stub/stub-fast".into());
    launch.effort = Some("off".into());
    let (started, _events) = world.start(&launch);
    assert_eq!(started.choice().0.as_deref(), Some("stub/stub-fast"));
    launch.model = Some("--evil".into());
    let (sink, _events) = Events::new();
    assert!(matches!(
        PiSession::start(&launch, &PlainSpawn, PiSettings::new(), sink),
        Err(Error::Refused(_))
    ));
}

#[test]
fn models_are_listed_with_no_turn_no_gate_and_no_session_kept() {
    let world = world("models");
    let models = PiSession::read_models(&world.launch(), &PlainSpawn, PiSettings::new()).unwrap();
    assert_eq!(models.len(), 2);
    let started = &logged(&world.home)[0];
    assert!(
        started["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--no-session")
    );
    assert!(started["gate"].is_null(), "no gate for a listing");
    assert!(!world.home.join("sessions").exists());
}

#[test]
fn an_untested_version_is_a_warning_and_a_tested_one_is_not() {
    let world = world("version");
    for (found, warned) in [("1.4.0", true), ("1.5.2", false)] {
        let mut settings = PiSettings::new();
        settings.known_version = Some(found.into());
        let (sink, mut events) = Events::new();
        let _session = PiSession::start(&world.launch(), &PlainSpawn, settings, sink).unwrap();
        events.drain();
        let warning = events.seen.iter().find_map(|event| match event {
            Event::Notice {
                severity: Severity::Warning,
                title,
            } if title.contains("Pi") && title.contains(found) => Some(title.clone()),
            _ => None,
        });
        assert_eq!(warning.is_some(), warned, "{found}: {:?}", events.seen);
    }
}

#[test]
fn the_system_prompt_is_a_file_and_an_image_goes_with_the_prompt() {
    let world = world("parts");
    let log = world.scratch.0.join("prompts.log");
    let mut launch = world.launch();
    launch.system_prompt = Some("Answer briefly.".into());
    launch
        .env
        .insert("COQUI_ACP_FAKE_PI_LOG".into(), log.clone().into());
    let (session, mut events) = world.start(&launch);
    session
        .prompt(&[
            Content::Text("what is this".into()),
            Content::Image {
                mime_type: "image/png".into(),
                data: "AAAA".into(),
            },
        ])
        .unwrap();
    events.turn();
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let start = &lines[0];
    assert_eq!(start["system_prompt"], "Answer briefly.");
    let args = start["args"].as_array().unwrap();
    assert!(args.iter().any(|arg| arg == "--append-system-prompt"));
    assert!(
        !args.iter().any(|arg| arg
            .as_str()
            .is_some_and(|arg| arg.contains("Answer briefly"))),
        "the text is in no argument"
    );
    let prompt = lines
        .iter()
        .find(|line| line.get("prompt").is_some())
        .unwrap();
    assert_eq!(
        (prompt["prompt"].as_str(), prompt["images"].as_u64()),
        (Some("what is this"), Some(1))
    );
}

#[test]
fn a_command_of_an_extension_is_a_turn_that_ends_with_no_run() {
    let world = world("command");
    let (session, mut events) = world.start(&world.launch());
    say(&session, "/fake now");
    events.turn();
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::EndTurn));
    assert_eq!(events.said(), "");
}

#[test]
fn a_provider_error_ends_the_turn_failed_or_limited() {
    let world = world("errors");
    let (session, mut events) = world.start(&world.launch());
    say(&session, "fail");
    events.turn();
    assert!(matches!(end_of(&events), TurnEnd::Failed(why) if why.starts_with("500: ")));
    say(&session, "limit");
    events.turn();
    assert!(
        matches!(end_of(&events), TurnEnd::Limited(_)),
        "{:?}",
        end_of(&events)
    );
}

#[test]
fn a_program_that_is_not_there_is_a_spawn_error() {
    let world = world("missing");
    let mut launch = world.launch();
    launch.executable = world.cwd.join("no-such-pi").into();
    let (sink, _events) = Events::new();
    let error = PiSession::start(&launch, &PlainSpawn, PiSettings::new(), sink).unwrap_err();
    assert!(matches!(error, Error::Spawn(_)), "{error}");
}
