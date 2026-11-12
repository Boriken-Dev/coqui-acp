//! A history read as a conversation for a codec writing a harness's files, and the recorder a codec
//! reading them builds a history with.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use coqui_acp_history::conversation::{
    Call, Conversation, Item, Recorder, Stamp, native_meta, native_of,
};
use coqui_acp_history::{Body, Document, Header, Line, Origin, Stop, StopReason, Trigger};
use serde_json::{Map, Value, json};

const ID: &str = "0b2e9a4c-7a35-4d2f-9f0e-0f4f3f1f2a11";

fn doc(lines: Vec<Line>) -> Document {
    let mut doc = Document::new(Header::new(ID, Origin::new("claude")));
    doc.lines = lines;
    doc
}

fn chunk(kind: &str, text: &str) -> Line {
    Line::update(json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}}))
}

fn call(id: &str) -> Line {
    Line::update(
        json!({"sessionUpdate": "tool_call", "toolCallId": id, "name": "Bash", "title": "ls",
        "kind": "execute", "status": "pending", "rawInput": {"command": "ls"}}),
    )
}

fn result(id: &str, status: &str) -> Line {
    Line::update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": status, "rawOutput": "out"}),
    )
}

fn end() -> Line {
    Line::turn_end(Stop::Acp(StopReason::EndTurn))
}

#[test]
fn chunks_join_and_a_call_and_its_result_are_one_call_in_two_places() {
    let conversation = Conversation::of(
        &doc(vec![
            Line::text_prompt("Hi"),
            chunk("agent_message_chunk", "Hel"),
            chunk("agent_message_chunk", "lo"),
            chunk("agent_thought_chunk", "hm"),
            call("t1"),
            Line::update(json!({"sessionUpdate": "usage_update", "used": 1, "size": 2})),
            result("t1", "completed"),
            end(),
        ]),
        true,
    );
    let kinds: Vec<&str> = conversation
        .items
        .iter()
        .map(|item| match item {
            Item::Prompt { .. } => "prompt",
            Item::Message { .. } => "message",
            Item::Thought { .. } => "thought",
            Item::Call { .. } => "call",
            Item::Result { .. } => "result",
            Item::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["prompt", "message", "thought", "call", "result", "end"]
    );
    assert!(matches!(&conversation.items[1], Item::Message { text, .. } if text == "Hello"));
    let merged = conversation.call("t1").unwrap();
    assert_eq!(merged.name.as_deref(), Some("Bash"));
    assert_eq!(merged.raw_input, Some(json!({"command": "ls"})));
    assert_eq!(merged.raw_output, Some(json!("out")));
    assert!(merged.is_done() && !merged.is_failed() && merged.announced);
    assert_eq!(merged.output_text(), "out");
    assert_eq!(
        (conversation.closed_calls, conversation.closed_turn),
        (0, false)
    );
}

#[test]
fn what_the_history_left_open_is_closed_as_cancelled_and_permissions_are_not_carried() {
    let conversation = Conversation::of(
        &doc(vec![
            Line::text_prompt("Do it"),
            call("t1"),
            Line::new(Body::PermissionRequest {
                tool_call: json!({"toolCallId": "t1"}),
                options: vec![json!({"optionId": "yes", "name": "Yes", "kind": "allow_always"})],
            }),
            Line::new(Body::PermissionAnswer {
                tool_call_id: "t1".into(),
                outcome: json!({"outcome": "selected", "optionId": "yes"}),
            }),
        ]),
        true,
    );
    assert!(conversation.closed_turn);
    assert_eq!(conversation.closed_calls, 1);
    assert_eq!(
        conversation.call("t1").unwrap().status.as_deref(),
        Some("cancelled")
    );
    assert!(conversation.call("t1").unwrap().is_failed());
    let tail: Vec<&Item> = conversation.items.iter().rev().take(2).collect();
    assert!(matches!(
        tail[0],
        Item::TurnEnd {
            stop: Stop::Acp(StopReason::Cancelled),
            closed_here: true,
            ..
        }
    ));
    assert!(matches!(tail[1], Item::Result { id } if id == "t1"));
    // Nothing of the request or the answer is an item.
    assert!(
        conversation
            .items
            .iter()
            .all(|item| !matches!(item, Item::Note { .. }))
    );
}

#[test]
fn a_result_for_a_call_the_seed_does_not_hold_is_an_orphan() {
    let conversation = Conversation::of(
        &doc(vec![
            result("gone", "completed"),
            chunk("agent_message_chunk", "x"),
            end(),
        ]),
        true,
    );
    let orphan = conversation.call("gone").unwrap();
    assert!(!orphan.announced);
    assert!(
        conversation
            .items
            .iter()
            .any(|item| matches!(item, Item::Result { id } if id == "gone"))
    );
}

#[test]
fn the_seed_starts_at_the_latest_summary_and_can_be_taken_without_it() {
    let mut boundary = Line::compaction("The summary.");
    boundary =
        boundary.meta(native_meta("claude", Map::from_iter([("k".to_owned(), json!(1))])).unwrap());
    let lines = vec![
        Line::text_prompt("old"),
        end(),
        boundary,
        Line::text_prompt("new"),
        chunk("agent_message_chunk", "answer"),
        end(),
    ];
    let document = doc(lines);
    let seeded = Conversation::of(&document, true);
    let summary = seeded.summary.as_ref().unwrap();
    assert_eq!(summary.summary, "The summary.");
    assert_eq!(
        native_of(summary.stamp.meta.as_ref(), "claude").unwrap()["k"],
        1
    );
    assert!(native_of(summary.stamp.meta.as_ref(), "codex").is_none());
    assert_eq!(seeded.seeded_from.as_ref().unwrap().line, 4);
    assert!(matches!(&seeded.items[0], Item::Prompt { blocks, .. } if blocks[0]["text"] == "new"));
    assert!(!seeded.summary_missing);
    let whole = Conversation::of(&document, false);
    assert!(whole.summary.is_none());
    assert_eq!(whole.seeded_from.as_ref().unwrap().line, 2);
    assert!(matches!(&whole.items[0], Item::Prompt { blocks, .. } if blocks[0]["text"] == "old"));
}

#[test]
fn a_boundary_with_no_summary_is_reported_and_does_not_displace_an_earlier_one() {
    let mut bare = Line::new(Body::Compaction {
        summary: None,
        trigger: Some(Trigger::Auto),
        pre_tokens: Some(9),
    });
    bare = bare.at("2026-11-11T10:00:00Z");
    let conversation = Conversation::of(
        &doc(vec![
            Line::compaction("Earlier."),
            Line::text_prompt("kept"),
            end(),
            bare,
        ]),
        true,
    );
    assert_eq!(conversation.summary.as_ref().unwrap().summary, "Earlier.");
    assert!(conversation.summary_missing);
    assert!(
        matches!(&conversation.items[0], Item::Prompt { blocks, .. } if blocks[0]["text"] == "kept")
    );
}

#[test]
fn the_recorder_keeps_the_order_rules() {
    let mut recorder = Recorder::new();
    recorder.cancel(Stamp::default());
    recorder.end_turn(Stop::Acp(StopReason::EndTurn), None, Stamp::default());
    recorder.message(
        "outside a turn",
        Stamp::at(Some("2026-11-11T10:00:00Z".to_owned())),
    );
    recorder.prompt(
        vec![json!({"type": "text", "text": "one"})],
        Stamp::default(),
    );
    recorder.thought("hm", Stamp::default());
    recorder.update(call("t1").body_update(), Stamp::default());
    // A second prompt ends the turn before it.
    recorder.prompt(
        vec![json!({"type": "text", "text": "two"})],
        Stamp::default(),
    );
    recorder.cancel(Stamp::default());
    recorder.end_turn(Stop::Acp(StopReason::Cancelled), None, Stamp::default());
    recorder.note(json!({"type": "text", "text": "echo"}), Stamp::default());
    recorder.prompt(
        vec![json!({"type": "text", "text": "three"})],
        Stamp::default(),
    );
    assert!(recorder.is_open());
    let lines = recorder.finish();
    let methods: Vec<&str> = lines.iter().map(|line| line.body.method()).collect();
    assert_eq!(
        methods,
        [
            "session/update",
            "session/prompt",
            "session/update",
            "session/update",
            "history/turn_end",
            "session/prompt",
            "session/cancel",
            "history/turn_end",
            "session/update",
            "session/prompt",
        ]
    );
    assert_eq!(lines[0].at.as_deref(), Some("2026-11-11T10:00:00Z"));
    let mut document = doc(lines);
    document.check().unwrap();
    document.seal().unwrap();
}

#[test]
fn a_call_makes_the_updates_a_history_holds() {
    let mut made = Call::new("t1");
    made.name = Some("Bash".to_owned());
    made.kind = Some("execute".to_owned());
    made.raw_input = Some(json!({"command": "ls"}));
    let announced = made.announce();
    assert_eq!(announced["sessionUpdate"], "tool_call");
    assert_eq!(announced["status"], "pending");
    assert_eq!(announced["title"], "Tool");
    made.status = Some("failed".to_owned());
    made.raw_output = Some(Value::String("no".to_owned()));
    let finished = made.finish();
    assert_eq!(
        (finished["status"].as_str(), finished["rawOutput"].as_str()),
        (Some("failed"), Some("no"))
    );
}

trait BodyUpdate {
    fn body_update(&self) -> Value;
}

impl BodyUpdate for Line {
    fn body_update(&self) -> Value {
        match &self.body {
            Body::Update { update } => update.clone(),
            _ => Value::Null,
        }
    }
}

#[test]
fn a_time_is_valid_only_when_the_format_reads_it() {
    use coqui_acp_history::conversation::valid_time;
    assert!(valid_time("2026-11-12T10:00:00Z"));
    assert!(valid_time("2026-11-12T10:00:00.250+02:00"));
    assert!(!valid_time("not a time"));
    assert!(!valid_time("2026-13-12T10:00:00Z"));
    assert!(!valid_time(""));
}
