//! The limits that hold for a whole connection: what it may send in all, and where its sessions may work.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

mod support;

use std::path::PathBuf;

use coqui_acp_provider::{AgentEvent, COMPLETED_EVENT, TEXT_EVENT};
use serde_json::json;
use support::{Probe, Raw, cwd, detail, facade, joined, quick};

#[test]
fn a_client_that_keeps_prompting_is_cut_off_at_the_connection_limit() {
    let probe = Probe::new();
    probe.script(|_| {
        vec![
            AgentEvent::new(TEXT_EVENT, Some("x".repeat(50_000))),
            AgentEvent::new(COMPLETED_EVENT, None),
        ]
    });
    let mut settings = quick();
    settings.max_turn_bytes = 100_000;
    settings.max_connection_bytes = 300_000;
    let (mut raw, served) = Raw::serve(&facade(&probe, settings));
    let session = raw.open();
    let mut answered = 0;
    for _ in 0..20 {
        let id = raw.send(
            "session/prompt",
            &json!({"sessionId": session, "prompt": [{"type": "text", "text": "more"}]}),
        );
        match raw.answer_or_gone(id) {
            Some(answer) if answer["result"]["stopReason"] == "end_turn" => answered += 1,
            _ => break,
        }
    }
    assert!((1..20).contains(&answered), "{answered} turns answered");
    assert!(
        raw.notification_bytes <= 300_000 + 2_000,
        "{} bytes went out past a 300000 byte limit",
        raw.notification_bytes
    );
    joined(served).expect("the connection ends on its own");
}

#[test]
fn a_session_works_only_where_the_host_allows() {
    let probe = Probe::new();
    let root = std::env::temp_dir().join("coqui-acp-provider-root");
    let mut settings = quick();
    settings.working_roots = vec![root.clone()];
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    raw.initialize();
    let open = |raw: &mut Raw, cwd: PathBuf| {
        raw.call(
            "session/new",
            &json!({"cwd": cwd.to_string_lossy(), "mcpServers": []}),
        )
    };
    let inside = open(&mut raw, root.join("project"));
    assert!(inside["result"]["sessionId"].is_string(), "{inside}");
    for outside in [PathBuf::from(cwd()), root.join("..").join("elsewhere")] {
        let refused = open(&mut raw, outside);
        assert!(detail(&refused).contains("outside"), "{refused}");
    }
}
