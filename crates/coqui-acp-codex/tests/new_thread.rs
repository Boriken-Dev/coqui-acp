//! A second thread on the same app server, against the contract stand-in (which keeps each thread's turns in its home).
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_codex::harness::event::{Content, Event, TurnEnd};
use coqui_acp_codex::{CodexLaunch, CodexSession, CodexSettings, PlainSpawn};
use serde_json::Value;
use support::Scratch;

fn fake() -> std::path::PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("fake_codex{}", std::env::consts::EXE_SUFFIX))
}

/// What the turn that just ran said, and whether it ended.
fn answer_of(events: &mpsc::Receiver<Event>) -> String {
    let mut said = String::new();
    loop {
        match events
            .recv_timeout(Duration::from_secs(20))
            .expect("the turn did not end")
        {
            Event::MessageChunk { text, .. } => said.push_str(&text),
            Event::TurnEnd(TurnEnd::Stopped { .. }) => return said,
            _ => {}
        }
    }
}

fn turns_of(home: &std::path::Path, thread: &str) -> usize {
    let text =
        std::fs::read_to_string(home.join("fake_threads").join(format!("{thread}.json"))).unwrap();
    let thread: Value = serde_json::from_str(&text).unwrap();
    thread["turns"].as_array().map_or(0, Vec::len)
}

#[test]
fn a_new_thread_on_the_same_app_server_takes_the_turns_that_follow_and_the_parser_follows_it() {
    let scratch = Scratch::new("new-thread");
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut launch = CodexLaunch::new(&scratch.0);
    launch.executable = fake().into();
    launch.env.insert("CODEX_HOME".into(), home.clone().into());
    let (sender, events) = mpsc::channel();
    let session = CodexSession::start(&launch, &PlainSpawn, CodexSettings::new(), move |event| {
        sender.send(event).is_ok()
    })
    .unwrap();
    let first = session.thread();
    session.prompt(&[Content::Text("one".into())]).unwrap();
    assert!(answer_of(&events).contains("echo: one"));

    let second = session.new_thread().unwrap();
    assert_ne!(first, second);
    assert_eq!(session.thread(), second);

    // The turn runs on the new thread, and its notifications, which name that thread, are this session's.
    session.prompt(&[Content::Text("two".into())]).unwrap();
    assert!(answer_of(&events).contains("echo: two"));
    assert_eq!(turns_of(&home, &first), 1);
    assert_eq!(turns_of(&home, &second), 1);

    // An interrupt names the new thread: `wait` runs until interrupted.
    session.prompt(&[Content::Text("wait".into())]).unwrap();
    session.interrupt().unwrap();
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(20))
            .expect("the turn did not end");
        if matches!(event, Event::TurnEnd(_)) {
            break;
        }
    }
    assert_eq!(turns_of(&home, &first), 1);
    session.close();
}
