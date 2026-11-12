//! A session recorded from the messages a client exchanges with any ACP agent: the history it builds
//! reads back, orders its turns, and holds v1 and v2 updates as the agent sent them.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use coqui_acp_history::conversation::{Conversation, Item};
use coqui_acp_history::record::{Direction, SessionRecorder};
use coqui_acp_history::{Body, Document, Origin, Stop, StopReason};
use serde_json::{Value, json};

const ID: &str = "0b2e9a4c-7a35-4d2f-9f0e-0f4f3f1f2a11";
const SESSION: &str = "agent-session-7";

fn recorder() -> SessionRecorder {
    SessionRecorder::new(ID, Origin::new("stand-in"))
}

fn sent(recorder: &mut SessionRecorder, message: Value) {
    recorder.observe(Direction::Sent, &message);
}

fn received(recorder: &mut SessionRecorder, message: Value) {
    recorder.observe(Direction::Received, &message);
}

fn update(recorder: &mut SessionRecorder, update: Value) {
    received(
        recorder,
        json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": SESSION, "update": update}}),
    );
}

fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

/// The opening of a session: initialize, then session/new.
fn open(recorder: &mut SessionRecorder, version: u64) {
    sent(
        recorder,
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": version}}),
    );
    received(
        recorder,
        json!({"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": version,
            "agentInfo": {"name": "stand-in", "version": "1.2.3"}}}),
    );
    sent(
        recorder,
        json!({"jsonrpc": "2.0", "id": 1, "method": "session/new",
            "params": {"cwd": "/work/project", "mcpServers": []}}),
    );
    received(
        recorder,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": SESSION,
            "modes": {"currentModeId": "ask", "availableModes": []}}}),
    );
}

fn prompt(recorder: &mut SessionRecorder, id: u64, text: &str) {
    sent(
        recorder,
        json!({"jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {"sessionId": SESSION, "prompt": [{"type": "text", "text": text}]}}),
    );
}

fn stop(recorder: &mut SessionRecorder, id: u64, reason: &str) {
    received(
        recorder,
        json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": reason}}),
    );
}

fn bodies(doc: &Document) -> Vec<&'static str> {
    doc.lines
        .iter()
        .map(|line| match &line.body {
            Body::Prompt { .. } => "prompt",
            Body::Update { .. } => "update",
            Body::Cancel => "cancel",
            Body::PermissionRequest { .. } => "request",
            Body::PermissionAnswer { .. } => "answer",
            Body::TurnEnd { .. } => "end",
            Body::Compaction { .. } => "compaction",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_v1_session_is_recorded_whole_and_round_trips_through_export() {
    let mut rec = recorder();
    open(&mut rec, 1);
    prompt(&mut rec, 2, "fix the test");
    update(&mut rec, chunk("agent_thought_chunk", "look first"));
    update(&mut rec, chunk("agent_message_chunk", "Running "));
    update(&mut rec, chunk("agent_message_chunk", "it."));
    update(
        &mut rec,
        json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "cargo test",
            "kind": "execute", "status": "pending", "rawInput": {"command": "cargo test"}}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "rawOutput": "1 failed"}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "plan", "entries": [
            {"content": "fix it", "priority": "high", "status": "pending"}]}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "current_mode_update", "currentModeId": "code"}),
    );
    stop(&mut rec, 2, "end_turn");

    let doc = rec.finish().expect("a recording is a history");
    assert_eq!(doc.header.id, ID);
    assert_eq!(doc.header.origin.harness, "stand-in");
    assert_eq!(doc.header.origin.version.as_deref(), Some("1.2.3"));
    assert_eq!(doc.header.origin.session_id.as_deref(), Some(SESSION));
    assert_eq!(doc.header.cwd.as_deref(), Some("/work/project"));
    assert_eq!(doc.header.acp_version, Some(1));
    assert_eq!(doc.header.mode.as_deref(), Some("code"));
    assert_eq!(
        bodies(&doc),
        [
            "prompt", "update", "update", "update", "update", "update", "update", "update", "end"
        ]
    );
    assert_eq!(
        doc.lines.last().map(|line| &line.body),
        Some(&Body::TurnEnd {
            stop: Stop::Acp(StopReason::EndTurn),
            error: None
        })
    );

    // Export and read back: the same history, and a conversation a writer can use.
    let text = doc.to_string().expect("it writes");
    let again: Document = text.parse().expect("what was written reads");
    assert_eq!(again.lines, doc.lines);
    let conversation = Conversation::of(&again, true);
    let kinds: Vec<&str> = conversation
        .items
        .iter()
        .map(|item| match item {
            Item::Prompt { .. } => "prompt",
            Item::Message { .. } => "message",
            Item::Thought { .. } => "thought",
            Item::Call { .. } => "call",
            Item::Result { .. } => "result",
            Item::Plan { .. } => "plan",
            Item::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "prompt", "thought", "message", "call", "result", "plan", "end"
        ]
    );
    let call = conversation.call("t1").expect("the call");
    assert_eq!(call.raw_input, Some(json!({"command": "cargo test"})));
    assert_eq!(call.raw_output, Some(json!("1 failed")));
}

#[test]
fn a_mode_the_client_set_is_recorded_when_the_agent_said_nothing() {
    let mut rec = recorder();
    open(&mut rec, 1);
    sent(
        &mut rec,
        json!({"jsonrpc": "2.0", "id": 5, "method": "session/set_mode",
            "params": {"sessionId": SESSION, "modeId": "plan"}}),
    );
    received(&mut rec, json!({"jsonrpc": "2.0", "id": 5, "result": {}}));
    let doc = rec.finish().unwrap();
    assert_eq!(doc.header.mode.as_deref(), Some("plan"));
    let Body::Update { update } = &doc.lines[0].body else {
        panic!("the mode change is an update: {:?}", doc.lines);
    };
    assert_eq!(update["sessionUpdate"], "current_mode_update");
    assert_eq!(update["currentModeId"], "plan");
}

#[test]
fn a_mode_the_agent_announced_is_not_recorded_twice() {
    let mut rec = recorder();
    open(&mut rec, 1);
    sent(
        &mut rec,
        json!({"jsonrpc": "2.0", "id": 5, "method": "session/set_mode",
            "params": {"sessionId": SESSION, "modeId": "plan"}}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "current_mode_update", "currentModeId": "plan"}),
    );
    received(&mut rec, json!({"jsonrpc": "2.0", "id": 5, "result": {}}));
    let doc = rec.finish().unwrap();
    assert_eq!(doc.lines.len(), 1);
}

#[test]
fn a_cancel_and_a_permission_are_recorded_in_their_turn() {
    let mut rec = recorder();
    open(&mut rec, 1);
    prompt(&mut rec, 2, "delete it");
    received(
        &mut rec,
        json!({"jsonrpc": "2.0", "id": 90, "method": "session/request_permission",
            "params": {"sessionId": SESSION, "toolCall": {"toolCallId": "t9"},
                "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]}}),
    );
    sent(
        &mut rec,
        json!({"jsonrpc": "2.0", "id": 90,
            "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}),
    );
    sent(
        &mut rec,
        json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": SESSION}}),
    );
    stop(&mut rec, 2, "cancelled");
    let doc = rec.finish().unwrap();
    assert_eq!(
        bodies(&doc),
        ["prompt", "request", "answer", "cancel", "end"]
    );
    doc.check().expect("the order rules hold");
    assert_eq!(
        doc.lines[2].body,
        Body::PermissionAnswer {
            tool_call_id: "t9".into(),
            outcome: json!({"outcome": "selected", "optionId": "allow"}),
        }
    );
}

#[test]
fn a_failed_prompt_ends_its_turn_with_the_error_and_a_turn_cut_short_stays_open() {
    let mut rec = recorder();
    open(&mut rec, 1);
    prompt(&mut rec, 2, "one");
    received(
        &mut rec,
        json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32000, "message": "rate limited"}}),
    );
    prompt(&mut rec, 3, "two");
    let doc = rec.finish().unwrap();
    assert_eq!(bodies(&doc), ["prompt", "end", "prompt"]);
    assert_eq!(
        doc.lines[1].body,
        Body::TurnEnd {
            stop: Stop::Error,
            error: Some("rate limited".into())
        }
    );
    assert!(doc.check().unwrap().open_turn);
}

#[test]
fn another_sessions_updates_and_unknown_messages_are_not_recorded() {
    let mut rec = recorder();
    open(&mut rec, 1);
    prompt(&mut rec, 2, "hi");
    received(
        &mut rec,
        json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "someone-else", "update": chunk("agent_message_chunk", "no")}}),
    );
    received(
        &mut rec,
        json!({"jsonrpc": "2.0", "method": "x/unknown", "params": {}}),
    );
    received(&mut rec, json!("not a message"));
    update(&mut rec, json!({"no": "sessionUpdate"}));
    stop(&mut rec, 2, "end_turn");
    let doc = rec.finish().unwrap();
    assert_eq!(bodies(&doc), ["prompt", "end"]);
}

#[test]
fn a_v2_session_is_recorded_as_the_agent_sent_it() {
    let mut rec = recorder();
    open(&mut rec, 2);
    prompt(&mut rec, 2, "list files");
    update(
        &mut rec,
        json!({"sessionUpdate": "agent_thought", "messageId": "m0",
            "content": [{"type": "text", "text": "plan it"}]}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "agent_message", "messageId": "m1",
            "content": [{"type": "text", "text": "I will list "}]}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "agent_message", "messageId": "m1",
            "content": [{"type": "text", "text": "I will list them."}]}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "name": "Bash",
            "title": "ls", "kind": "execute", "status": "in_progress",
            "rawInput": {"command": "ls"}}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
            "rawOutput": "a b"}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "plan_update", "plan": {"type": "items", "planId": "p1",
            "entries": [{"content": "list", "priority": "low", "status": "completed"}]}}),
    );
    stop(&mut rec, 2, "end_turn");
    let doc = rec.finish().unwrap();
    assert_eq!(doc.header.acp_version, Some(2));
    let text = doc.to_string().unwrap();
    let again: Document = text.parse().unwrap();
    assert_eq!(again.lines, doc.lines);
    let conversation = Conversation::of(&again, true);
    let messages: Vec<&str> = conversation
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Message { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages,
        ["I will list them."],
        "a v2 message replaces its earlier content"
    );
    assert!(
        conversation
            .items
            .iter()
            .any(|item| matches!(item, Item::Thought { text, .. } if text == "plan it"))
    );
    let call = conversation
        .call("t1")
        .expect("a v2 update announces the call");
    assert!(call.announced);
    assert_eq!(call.name.as_deref(), Some("Bash"));
    assert_eq!(call.raw_output, Some(json!("a b")));
    assert!(
        conversation
            .items
            .iter()
            .any(|item| matches!(item, Item::Plan { entries, .. } if entries.len() == 1))
    );
}

#[test]
fn a_v2_compaction_becomes_a_boundary_with_its_summary() {
    let mut rec = recorder();
    open(&mut rec, 2);
    prompt(&mut rec, 2, "go on");
    stop(&mut rec, 2, "end_turn");
    update(
        &mut rec,
        json!({"sessionUpdate": "compaction_summary_chunk", "compactionId": "c1",
            "content": {"type": "text", "text": "We fixed "}}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "compaction_summary_chunk", "compactionId": "c1",
            "content": {"type": "text", "text": "the test."}}),
    );
    update(
        &mut rec,
        json!({"sessionUpdate": "compaction_update", "compactionId": "c1", "status": "completed"}),
    );
    let doc = rec.finish().unwrap();
    let boundary = doc
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::Compaction { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .expect("a boundary");
    assert_eq!(boundary.as_deref(), Some("We fixed the test."));
    assert_eq!(doc.seed().summary, Some("We fixed the test."));
}

#[test]
fn a_time_given_is_kept_and_a_bad_one_refused() {
    let mut rec = recorder();
    open(&mut rec, 1);
    rec.observe_at(
        Direction::Sent,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt",
            "params": {"sessionId": SESSION, "prompt": [{"type": "text", "text": "x"}]}}),
        Some("2026-11-12T10:00:00Z"),
    );
    rec.observe_at(
        Direction::Received,
        &json!({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}}),
        Some("not a time"),
    );
    let doc = rec.finish().unwrap();
    assert_eq!(doc.lines[0].at.as_deref(), Some("2026-11-12T10:00:00Z"));
    assert_eq!(doc.lines[1].at, None);
}

#[test]
fn an_update_too_large_for_the_bounds_is_skipped_and_counted() {
    let mut rec = recorder().max_line_bytes(256);
    open(&mut rec, 1);
    prompt(&mut rec, 2, "hi");
    update(&mut rec, chunk("agent_message_chunk", &"x".repeat(1000)));
    update(&mut rec, chunk("agent_message_chunk", "ok"));
    stop(&mut rec, 2, "end_turn");
    assert_eq!(rec.skipped(), 1);
    let doc = rec.finish().unwrap();
    assert_eq!(bodies(&doc), ["prompt", "update", "end"]);
}

#[test]
fn a_recording_stops_at_its_line_limit_and_says_so() {
    let mut rec = recorder().max_lines(4);
    open(&mut rec, 1);
    prompt(&mut rec, 2, "hi");
    for n in 0..10 {
        update(&mut rec, chunk("agent_message_chunk", &n.to_string()));
    }
    stop(&mut rec, 2, "end_turn");
    assert!(rec.truncated());
    let doc = rec.finish().unwrap();
    assert!(
        doc.lines.len() <= 3,
        "the header counts as a line: {}",
        doc.lines.len()
    );
    doc.check().expect("what was kept is a valid history");
}
