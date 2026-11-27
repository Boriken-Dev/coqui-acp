//! The cleaner of a carried last turn and the continuation rule, over exported histories.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

use coqui_acp_api::{DEFAULT_LIMIT_PHRASES, LimitHit, LimitKind, Source};
use coqui_acp_history::{Body, Document, Line, Stop, StopReason};
use coqui_acp_pool::{Cleaning, DEFAULT_CONTINUATION, Resume, clean, resume};
use serde_json::{Value, json};

const NOTICE: &str = "example.test/notice";

fn fixture(name: &str) -> Document {
    let path = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect::<std::path::PathBuf>();
    std::fs::read_to_string(path)
        .expect("the fixture is in the repository")
        .parse()
        .expect("a fixture is a history")
}

fn cleaning() -> Cleaning<'static> {
    Cleaning::new(DEFAULT_LIMIT_PHRASES).notice_key(NOTICE)
}

/// The agent's text of each message chunk, in order.
fn texts(document: &Document) -> Vec<String> {
    document
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Update { update } if update["sessionUpdate"] == "agent_message_chunk" => {
                update["content"]["text"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

fn call_ids(document: &Document) -> Vec<String> {
    document
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Update { update } if update["sessionUpdate"] == "tool_call" => {
                update["toolCallId"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_turn_a_limit_ended_loses_its_error_its_limit_text_and_its_open_calls() {
    let before = fixture("limit_error.jsonl");
    let cleaned = clean(&before, &cleaning());
    let after = &cleaned.document;
    after.check().expect("what is left is still a history");
    // The closing error is gone, so the turn is left open for the import to close.
    assert!(!after.lines.iter().any(|line| matches!(
        line.body,
        Body::TurnEnd {
            stop: Stop::Error,
            ..
        }
    )));
    assert!(cleaned.removed.end);
    // The limit's text, split across three chunks, and the "wait" after it go; the agent's own words stay.
    assert_eq!(texts(after), ["first answer", "Let me look at the build."]);
    assert_eq!(cleaned.removed.limit_text, 3);
    // The finished call stays; the call that never produced a result and the permission asked for another go.
    assert_eq!(call_ids(after), ["tc1"]);
    assert!(
        !after
            .lines
            .iter()
            .any(|line| matches!(line.body, Body::PermissionRequest { .. }))
    );
    assert_eq!(cleaned.removed.open_calls, 2);
    // The host's own notice is gone from the whole history; the person's prompts are not touched.
    assert_eq!(cleaned.removed.notices, 1);
    let prompts = after
        .lines
        .iter()
        .filter(|line| matches!(line.body, Body::Prompt { .. }))
        .count();
    assert_eq!(prompts, 2);
    assert!(cleaned.had_work);
    assert!(!cleaned.limit_text_left);
    assert_eq!(after.lines.len(), before.lines.len() - 1 - 1 - 2 - 3);
}

#[test]
fn earlier_turns_and_text_before_the_last_tool_call_are_never_touched() {
    let before = fixture("legit_mention.jsonl");
    let cleaned = clean(&before, &cleaning());
    // The turn ended in a limit as far as the host is concerned: only the text after its last tool call can be the limit's.
    assert_eq!(
        texts(&cleaned.document),
        ["I will explain the quota system you asked about in a moment."]
    );
    assert_eq!(cleaned.removed.limit_text, 1);
    // First turn of the other fixture, which a limit did not end, is kept whole.
    let error = fixture("limit_error.jsonl");
    assert_eq!(
        texts(&clean(&error, &cleaning()).document)[0],
        "first answer"
    );
}

#[test]
fn a_turn_that_ended_well_loses_only_the_hosts_notices() {
    let before = fixture("legit_mention.jsonl");
    let cleaned = clean(&before, &cleaning().ended_by_limit(false));
    assert_eq!(cleaned.document, before);
    assert_eq!(cleaned.removed.limit_text, 0);
    assert!(!cleaned.removed.end && !cleaned.limit_text_left);
    let noticed = clean(
        &fixture("limit_error.jsonl"),
        &cleaning().ended_by_limit(false),
    );
    assert_eq!(noticed.removed.notices, 1);
    assert!(
        noticed.removed.end.eq(&false),
        "an error end stays when no limit ended the turn"
    );
    assert_eq!(
        resume(None, (noticed.had_work, noticed.prompt.as_deref()), false),
        Resume::Nothing
    );
}

#[test]
fn a_limit_on_the_first_request_leaves_no_work_and_the_prompt_is_sent_again() {
    let cleaned = clean(&fixture("limit_first_request.jsonl"), &cleaning());
    cleaned.document.check().expect("a history");
    assert!(cleaned.removed.end && !cleaned.had_work);
    let again = cleaned.resume(Some("keep going"), true);
    assert_eq!(
        again,
        Resume::Repeat(vec![json!({"text": "again please", "type": "text"})])
    );
}

#[test]
fn a_refusal_that_names_a_limit_in_a_thought_is_cleaned_like_an_error() {
    let cleaned = clean(&fixture("refusal.jsonl"), &cleaning());
    assert!(cleaned.removed.end);
    let thoughts = cleaned
        .document
        .lines
        .iter()
        .filter(|line| matches!(&line.body, Body::Update { update } if update["sessionUpdate"] == "agent_thought_chunk"))
        .count();
    assert_eq!(thoughts, 0);
    // Partial work stays, so the continuation is the default line.
    assert!(cleaned.had_work);
    assert_eq!(
        cleaned.resume(None, true),
        Resume::Continue(DEFAULT_CONTINUATION.to_owned())
    );
}

#[test]
fn text_that_names_no_limit_stays_and_is_reported() {
    let mut document = fixture("limit_error.jsonl");
    // The agent's wording matches no phrase: nothing but the error can be recognised.
    for line in &mut document.lines {
        if let Body::Update { update } = &mut line.body
            && update["sessionUpdate"] == "agent_message_chunk"
            && let Some(text) = update["content"]["text"].as_str()
            && text.contains("limit")
        {
            update["content"]["text"] = Value::from("Out of allowance");
        }
        if let Body::TurnEnd { error, .. } = &mut line.body {
            *error = Some("exhausted".to_owned());
        }
    }
    // Pretend the error line was not a recognisable end either: only the phrases decide chunk text.
    let cleaned = clean(&document, &Cleaning::new(&["no such phrase"]));
    assert!(
        cleaned.removed.end,
        "the closing error goes whatever it says"
    );
    assert_eq!(cleaned.removed.limit_text, 0);
    assert!(texts(&cleaned.document).contains(&"Out of allowance".to_owned()));
    // With no error line to drop either, the report says the limit's text was not removed.
    let mut open = document.clone();
    open.lines.pop();
    open.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    let cleaned = clean(&open, &Cleaning::new(&["no such phrase"]));
    assert!(cleaned.limit_text_left);
}

#[test]
fn the_phrases_are_the_hosts_and_an_empty_list_matches_nothing() {
    let cleaned = clean(&fixture("limit_error.jsonl"), &Cleaning::new(&[]));
    assert_eq!(cleaned.removed.limit_text, 0);
    let custom = clean(
        &fixture("limit_error.jsonl"),
        &Cleaning::new(&["usage"]).max_window(8),
    );
    assert_eq!(custom.removed.limit_text, 3);
    // A window too small for a message split across three chunks does not find it.
    let narrow = clean(
        &fixture("limit_error.jsonl"),
        &Cleaning::new(DEFAULT_LIMIT_PHRASES).max_window(1),
    );
    assert_eq!(narrow.removed.limit_text, 0);
}

#[test]
fn a_line_marked_in_its_own_meta_is_a_notice_too() {
    let mut document = fixture("limit_error.jsonl");
    let mut line = Line::update(
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "moved"}}),
    );
    line = line.meta(json!({ NOTICE: true }));
    document.lines.insert(2, line);
    let cleaned = clean(&document, &cleaning());
    assert_eq!(cleaned.removed.notices, 2);
    assert!(!texts(&cleaned.document).contains(&"moved".to_owned()));
}

#[test]
fn the_continuation_is_the_roles_text_else_the_default_line() {
    let cleaned = clean(&fixture("limit_error.jsonl"), &cleaning());
    assert_eq!(
        cleaned.resume(Some("Carry on with the plan."), true),
        Resume::Continue("Carry on with the plan.".to_owned())
    );
    assert_eq!(
        cleaned.resume(Some("  "), true),
        Resume::Continue(DEFAULT_CONTINUATION.to_owned())
    );
    assert_eq!(
        cleaned.resume(None, true),
        Resume::Continue(DEFAULT_CONTINUATION.to_owned())
    );
    assert_eq!(
        DEFAULT_CONTINUATION,
        "The previous agent's usage ran out; continue this session where it stopped."
    );
}

/// A history of one turn whose messages carry ids, as version 2 sends them, and an agent that words its limit in no phrase.
fn identified() -> Document {
    let mut document = fixture("limit_first_request.jsonl");
    document.lines.pop();
    for (id, text) in [
        ("m1", "Working on it."),
        ("m2", "Allowance spent"),
        ("m3", "back at nine"),
    ] {
        document.lines.push(Line::update(json!({
            "sessionUpdate": "agent_message_chunk", "messageId": id,
            "content": {"type": "text", "text": text}
        })));
    }
    document.lines.push(Line::turn_end(Stop::Error));
    document
}

#[test]
fn the_messages_the_harness_names_by_id_go_whatever_they_say() {
    let hit = LimitHit::new(0)
        .kind(LimitKind::UsageQuota)
        .text_ids(vec!["m2".into(), "m3".into()]);
    let cleaned = clean(&identified(), &cleaning().report(&hit));
    assert_eq!(
        texts(&cleaned.document).last().map(String::as_str),
        Some("Working on it.")
    );
    assert_eq!((cleaned.removed.limit_text, cleaned.removed.end), (2, true));
    // The phrases would have found nothing here.
    let by_words = clean(&identified(), &cleaning());
    assert_eq!(by_words.removed.limit_text, 0);
}

#[test]
fn the_messages_the_harness_names_by_position_go_and_the_others_stay() {
    let mut document = fixture("limit_error.jsonl");
    // Positions count the chunks of the last turn: "Let me look..." is 0, the limit's three chunks are 1, 2 and 3.
    let hit = LimitHit::new(0).text_indexes(vec![1, 2, 3]);
    let cleaned = clean(&document, &Cleaning::new(&[]).report(&hit));
    assert_eq!(cleaned.removed.limit_text, 3);
    assert_eq!(
        texts(&cleaned.document),
        [
            "first answer",
            "planner moved from Codex to Claude",
            "Let me look at the build."
        ]
    );
    // A position past the turn names nothing.
    document.lines.truncate(8);
    let none = clean(
        &document,
        &Cleaning::new(&[]).report(&LimitHit::new(0).text_indexes(vec![40])),
    );
    assert_eq!(none.removed.limit_text, 0);
}

#[test]
fn a_report_that_names_no_message_says_there_is_none_and_the_words_are_not_matched() {
    let mut hit = LimitHit::new(0).kind(LimitKind::UsageQuota);
    hit.source = Source::Reported;
    let cleaned = clean(&fixture("limit_error.jsonl"), &cleaning().report(&hit));
    assert_eq!(
        cleaned.removed.limit_text, 0,
        "the harness said there is no limit text to find"
    );
    assert!(!cleaned.limit_text_left);
    // A report that only read an error's words falls back to the phrases.
    hit.source = Source::Inferred;
    let by_words = clean(&fixture("limit_error.jsonl"), &cleaning().report(&hit));
    assert_eq!(by_words.removed.limit_text, 3);
}

fn say(text: &str) -> Line {
    Line::update(json!({
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}
    }))
}

fn tool(id: &str, status: &str) -> Line {
    Line::update(
        json!({"sessionUpdate": "tool_call", "toolCallId": id, "status": status, "title": "run"}),
    )
}

fn ask(text: &str) -> Line {
    Line::prompt(vec![json!({"type": "text", "text": text})])
}

/// A turn a limit ended: some work, a call left open, the limit's words and a failed ending.
fn limit_turn(question: &str, said: &str) -> Vec<Line> {
    vec![
        ask(question),
        say("Working on it."),
        tool(&format!("open-{question}"), "in_progress"),
        say(said),
        Line::turn_end(Stop::Error),
    ]
}

#[test]
fn every_turn_a_limit_ended_is_cleaned_not_only_the_last() {
    let mut lines = limit_turn("first", "You have hit your usage limit. Please wait");
    lines.extend(limit_turn("second", "usage limit reached, try again later"));
    let mut document = history(lines);
    let cleaned = clean(&document, &cleaning());
    assert_eq!(
        texts(&cleaned.document),
        ["Working on it.", "Working on it."]
    );
    assert!(call_ids(&cleaned.document).is_empty());
    assert!(
        !cleaned
            .document
            .lines
            .iter()
            .any(|line| matches!(line.body, Body::TurnEnd { .. }))
    );
    assert_eq!(cleaned.removed.limit_text, 2);
    assert_eq!(cleaned.removed.open_calls, 2);
    assert!(cleaned.removed.end);
    // The earlier turn goes even when the last one ended well.
    document.lines.truncate(5);
    document.lines.extend([
        ask("third"),
        say("done"),
        Line::turn_end(Stop::Acp(StopReason::EndTurn)),
    ]);
    let later = clean(&document, &cleaning().ended_by_limit(false));
    assert_eq!(texts(&later.document), ["Working on it.", "done"]);
}

#[test]
fn an_earlier_turn_the_harness_reported_is_cleaned_by_the_messages_it_names() {
    let hit = LimitHit::new(0).text_indexes(vec![1]);
    let mut lines = limit_turn("first", "Allowance spent");
    let end = lines.pop().expect("an ending");
    lines.push(end.meta(json!({ coqui_acp_api::LIMIT_META_KEY: hit })));
    lines.extend([
        ask("second"),
        say("fine"),
        Line::turn_end(Stop::Acp(StopReason::EndTurn)),
    ]);
    let document = history(lines);
    let cleaned = clean(&document, &Cleaning::new(&[]).ended_by_limit(false));
    assert_eq!(texts(&cleaned.document), ["Working on it.", "fine"]);
    assert!(call_ids(&cleaned.document).is_empty());
}

#[test]
fn a_turn_whose_agent_said_quota_before_its_last_tool_call_is_kept() {
    let mut lines = vec![
        ask("explain"),
        say("The quota system works like this."),
        tool("t1", "pending"),
        Line::update(
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}),
        ),
        say("All done."),
        Line::turn_end(Stop::Error),
    ];
    lines.extend([
        ask("next"),
        say("ok"),
        Line::turn_end(Stop::Acp(StopReason::EndTurn)),
    ]);
    let document = history(lines);
    let cleaned = clean(&document, &cleaning().ended_by_limit(false));
    assert_eq!(cleaned.document, document);
}

#[test]
fn an_earlier_turn_that_failed_for_another_reason_is_kept() {
    let mut lines = vec![
        ask("build"),
        say("Compiling."),
        tool("t1", "in_progress"),
        say("The compiler crashed."),
        Line::turn_end(Stop::Error),
    ];
    lines.extend([
        ask("again"),
        say("ok"),
        Line::turn_end(Stop::Acp(StopReason::EndTurn)),
    ]);
    let document = history(lines);
    let cleaned = clean(&document, &cleaning().ended_by_limit(false));
    assert_eq!(cleaned.document, document);
    assert_eq!(cleaned.removed, coqui_acp_pool::Removed::default());
}

/// A history with these lines under the header of a fixture.
fn history(lines: Vec<Line>) -> Document {
    let mut document = fixture("limit_first_request.jsonl");
    document.lines = lines;
    document
}
