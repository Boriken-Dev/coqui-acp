//! The permission gate's questions: what each mode answers by itself, what it asks the person, and what an abort does to
//! a question the person has not answered.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::{Arc, Mutex};

use coqui_acp_pi::Parser;
use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::{Event, OptionKind, Outcome, PermissionRequest, ToolKind};
use coqui_acp_pi::mode::Policy;
use coqui_acp_pi::parse::{ALLOW, ALLOW_TOOL, REJECT};
use serde_json::{Value, json};
use support::{recorded, work};

type Written = Arc<Mutex<Vec<Value>>>;

fn parser_in(mode: Mode) -> (Parser, Written, Arc<Mutex<Policy>>) {
    let written: Written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let policy = Arc::new(Mutex::new(Policy::new(mode, false)));
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    )
    .with_policy(Arc::clone(&policy))
    .with_roots(&work(), Vec::new());
    (parser, written, policy)
}

/// A call as Pi reports it before the gate asks, and the gate's question.
fn call(tool: &str, args: &Value, dialog: &str) -> Vec<Value> {
    vec![
        json!({"type": "tool_execution_start", "toolCallId": dialog, "toolName": tool, "args": args}),
        json!({"type": "extension_ui_request", "id": dialog, "method": "select",
               "title": format!("coqui-acp-pi-gate {}", json!({"id": dialog, "tool": tool})),
               "options": ["allow", "deny", "plan"]}),
    ]
}

fn feed(parser: &mut Parser, lines: &[Value]) -> Vec<Event> {
    lines.iter().flat_map(|line| parser.line(line)).collect()
}

fn asked(events: Vec<Event>) -> Vec<PermissionRequest> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::Permission(request) => Some(request),
            _ => None,
        })
        .collect()
}

fn answers(written: &Written) -> Vec<Value> {
    written
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line["type"] == "extension_ui_response")
        .cloned()
        .collect()
}

#[test]
fn a_command_in_manual_mode_is_asked_with_three_answers_and_the_one_chosen_is_sent() {
    let (mut parser, written, _) = parser_in(Mode::Manual);
    parser.prompting();
    let bash = json!({"command": "echo allowed"});
    let mut requests = asked(feed(&mut parser, &call("bash", &bash, "d1")));
    assert_eq!(requests.len(), 1);
    let request = requests.remove(0);
    assert_eq!(request.title, "Run echo allowed");
    assert_eq!(request.tool.kind, Some(ToolKind::Execute));
    assert_eq!(request.tool.id, "d1");
    let kinds: Vec<_> = request.options.iter().map(|option| option.kind).collect();
    assert_eq!(
        kinds,
        [
            OptionKind::AllowOnce,
            OptionKind::AllowAlways,
            OptionKind::RejectOnce
        ]
    );
    let ids: Vec<_> = request
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect();
    assert_eq!(ids, [ALLOW, ALLOW_TOOL, REJECT]);
    request
        .responder
        .answer(Outcome::Selected(ALLOW.into()))
        .unwrap();
    assert_eq!(
        answers(&written),
        [json!({"type": "extension_ui_response", "id": "d1", "value": "allow"})]
    );

    for (outcome, expected) in [
        (Outcome::Selected(REJECT.into()), json!({"value": "deny"})),
        (Outcome::Refused, json!({"value": "deny"})),
        (Outcome::Cancelled, json!({"cancelled": true})),
    ] {
        written.lock().unwrap().clear();
        let mut requests = asked(feed(
            &mut parser,
            &call("write", &json!({"path": "a", "content": "b"}), "d2"),
        ));
        requests.remove(0).responder.answer(outcome).unwrap();
        let sent = answers(&written);
        assert_eq!(sent.len(), 1);
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&sent[0][key], value, "{sent:?}");
        }
    }
}

#[test]
fn a_dropped_question_is_cancelled_so_pi_is_never_left_waiting() {
    let (mut parser, written, _) = parser_in(Mode::Manual);
    parser.prompting();
    drop(asked(feed(
        &mut parser,
        &call("bash", &json!({"command": "x"}), "d1"),
    )));
    assert_eq!(
        answers(&written),
        [json!({"type": "extension_ui_response", "id": "d1", "cancelled": true})]
    );
}

#[test]
fn always_allowing_a_tool_answers_it_without_asking_again_but_not_another_tool() {
    let (mut parser, written, _) = parser_in(Mode::Manual);
    parser.prompting();
    let mut requests = asked(feed(
        &mut parser,
        &call("bash", &json!({"command": "a"}), "d1"),
    ));
    requests
        .remove(0)
        .responder
        .answer(Outcome::Selected(ALLOW_TOOL.into()))
        .unwrap();
    written.lock().unwrap().clear();
    assert!(
        asked(feed(
            &mut parser,
            &call("bash", &json!({"command": "b"}), "d2")
        ))
        .is_empty()
    );
    assert_eq!(answers(&written)[0]["value"], "allow");
    assert_eq!(
        asked(feed(
            &mut parser,
            &call("write", &json!({"path": "a", "content": ""}), "d3")
        ))
        .len(),
        1
    );
}

#[test]
fn each_mode_answers_what_it_does_not_ask() {
    let write = json!({"path": "a", "content": "b"});
    let bash = json!({"command": "ls"});
    let read = json!({"path": "a"});
    // (mode, tool, args, asked, answer sent without asking)
    let table: [(Mode, &str, &Value, bool, &str); 10] = [
        (Mode::Manual, "read", &read, false, "allow"),
        (Mode::Manual, "write", &write, true, ""),
        (Mode::AcceptEdits, "write", &write, false, "allow"),
        (Mode::AcceptEdits, "bash", &bash, true, ""),
        (Mode::Plan, "read", &read, false, "allow"),
        (Mode::Plan, "write", &write, false, "plan"),
        (Mode::Plan, "bash", &bash, false, "plan"),
        (Mode::Bypass, "bash", &bash, false, "allow"),
        (Mode::Bypass, "write", &write, false, "allow"),
        (Mode::Manual, "mcp__x__y", &bash, true, ""),
    ];
    for (mode, tool, args, ask, answer) in table {
        let (mut parser, written, _) = parser_in(mode);
        parser.prompting();
        let requests = asked(feed(&mut parser, &call(tool, args, "d")));
        assert_eq!(requests.len() == 1, ask, "{mode:?} {tool}");
        if !ask {
            assert_eq!(answers(&written)[0]["value"], answer, "{mode:?} {tool}");
        }
        drop(requests);
    }
}

#[test]
fn the_mode_set_between_calls_decides_the_next_one() {
    let (mut parser, written, policy) = parser_in(Mode::Manual);
    parser.prompting();
    let write = json!({"path": "a", "content": "b"});
    drop(asked(feed(&mut parser, &call("write", &write, "d1"))));
    policy.lock().unwrap().set_mode(Mode::Plan);
    written.lock().unwrap().clear();
    assert!(asked(feed(&mut parser, &call("write", &write, "d2"))).is_empty());
    assert_eq!(answers(&written)[0]["value"], "plan");
}

#[test]
fn stopping_a_run_cancels_the_questions_the_person_has_not_answered_and_a_late_answer_adds_nothing()
{
    let (mut parser, written, _) = parser_in(Mode::Manual);
    parser.prompting();
    let mut requests = asked(feed(
        &mut parser,
        &call("bash", &json!({"command": "sleep 30"}), "d1"),
    ));
    let request = requests.remove(0);
    assert!(parser.interrupting());
    assert_eq!(
        answers(&written),
        [json!({"type": "extension_ui_response", "id": "d1", "cancelled": true})]
    );
    // The client answers cancelled after the abort, as ACP has it do: nothing more is written.
    request.responder.answer(Outcome::Cancelled).unwrap();
    assert_eq!(answers(&written).len(), 1);
}

#[test]
fn a_question_the_gate_did_not_ask_is_matched_to_the_oldest_call_of_that_tool() {
    let (mut parser, _, _) = parser_in(Mode::Manual);
    parser.prompting();
    let lines = vec![
        json!({"type": "tool_execution_start", "toolCallId": "first", "toolName": "bash", "args": {"command": "one"}}),
        json!({"type": "tool_execution_start", "toolCallId": "second", "toolName": "bash", "args": {"command": "two"}}),
        // The gate could not name the call.
        json!({"type": "extension_ui_request", "id": "q", "method": "select",
               "title": "coqui-acp-pi-gate {\"id\":null,\"tool\":\"bash\"}", "options": ["allow", "deny", "plan"]}),
    ];
    let requests = asked(feed(&mut parser, &lines));
    assert_eq!(requests[0].tool.id, "first");
    assert_eq!(requests[0].title, "Run one");
}

#[test]
fn the_recorded_gate_questions_are_the_ones_this_parser_answers() {
    // `permission_gate` is Pi asking through an extension before `bash`, `write` and `edit`; its titles are rewritten to
    // the gate this crate ships.
    let lines = recorded("permission_gate");
    let questions: Vec<&Value> = lines
        .iter()
        .filter(|line| line["type"] == "extension_ui_request")
        .collect();
    assert!(questions.len() >= 3, "{questions:?}");
    for question in &questions {
        assert!(
            question["title"]
                .as_str()
                .unwrap()
                .starts_with("coqui-acp-pi-gate ")
        );
        assert_eq!(question["options"], json!(["allow", "deny", "plan"]));
    }
    let (mut parser, written, _) = parser_in(Mode::Manual);
    parser.prompting();
    let mut seen = 0;
    for line in &lines {
        for event in parser.line(line) {
            if let Event::Permission(request) = event {
                seen += 1;
                request
                    .responder
                    .answer(Outcome::Selected(ALLOW.into()))
                    .unwrap();
            }
        }
    }
    assert_eq!(seen, answers(&written).len());
    assert!(seen >= 3);
}
