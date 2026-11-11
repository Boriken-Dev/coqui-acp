//! Every tool call in the recordings reaches the client with its kind, its input as the model gave it and its output as
//! the model was given it: a history written from these events is only as faithful as they are.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_claude::harness::event::{Event, ToolCall, ToolStatus};
use serde_json::Value;
use support::{fixtures, parsed};

/// The tools whose calls the parser tells as a plan, not as a call.
fn told_as_plan(name: &str) -> bool {
    name == "TodoWrite" || (name.starts_with("Task") && name != "Task")
}

fn tools_of(events: &[Event]) -> Vec<&ToolCall> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Tool(call) => Some(call),
            _ => None,
        })
        .collect()
}

/// The `tool_use` and `tool_result` blocks of the lines of a recording.
fn blocks(lines: &[Value]) -> (Vec<&Value>, Vec<&Value>) {
    let mut uses = Vec::new();
    let mut results = Vec::new();
    for line in lines {
        let content = line.pointer("/message/content").and_then(Value::as_array);
        for block in content.into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => uses.push(block),
                Some("tool_result") => results.push(block),
                _ => {}
            }
        }
    }
    (uses, results)
}

fn recordings() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".stdout").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_recorded_tool_call_has_its_kind_input_and_output() {
    let mut calls = 0;
    let mut results = 0;
    for name in recordings() {
        let lines = support::recorded(&name);
        let (events, _) = parsed(&name);
        let told = tools_of(&events);
        let (uses, outcomes) = blocks(&lines);
        let mut names = std::collections::BTreeMap::new();
        for block in uses {
            let id = block["id"].as_str().unwrap();
            let tool = block["name"].as_str().unwrap();
            names.insert(id.to_string(), tool.to_string());
            if told_as_plan(tool) {
                continue;
            }
            // A line that is told twice (streamed, then whole) makes the call twice: the first of each is the call.
            let call = told
                .iter()
                .find(|call| call.id == id && call.kind.is_some())
                .unwrap_or_else(|| panic!("{name}: {tool} {id} was never told as a call"));
            assert_eq!(
                call.input.as_ref(),
                block.get("input"),
                "{name}: the input of {tool} {id}"
            );
            calls += 1;
        }
        for block in outcomes {
            let id = block["tool_use_id"].as_str().unwrap();
            if names.get(id).is_some_and(|tool| told_as_plan(tool)) {
                continue;
            }
            let call = told
                .iter()
                .find(|call| {
                    call.id == id
                        && matches!(
                            call.status,
                            Some(ToolStatus::Completed | ToolStatus::Failed)
                        )
                })
                .unwrap_or_else(|| panic!("{name}: the result of {id} was never told"));
            assert_eq!(
                call.output.as_ref(),
                block.get("content"),
                "{name}: the output of {id}"
            );
            results += 1;
        }
    }
    assert!(
        calls >= 6 && results >= 6,
        "{calls} calls, {results} results"
    );
}

#[test]
fn a_replayed_transcript_keeps_its_tool_calls_whole() {
    let text = std::fs::read_to_string(fixtures().join("turn.transcript.jsonl")).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut parser = coqui_acp_claude::Parser::new(std::sync::Arc::new(|_: &Value| Ok(())), 4096);
    let events: Vec<Event> = lines.iter().flat_map(|line| parser.history(line)).collect();
    let told = tools_of(&events);
    let (uses, outcomes) = blocks(&lines);
    let mut checked = 0;
    for block in uses {
        if told_as_plan(block["name"].as_str().unwrap()) {
            continue;
        }
        let id = block["id"].as_str().unwrap();
        let call = told
            .iter()
            .find(|call| call.id == id && call.kind.is_some())
            .expect("a call");
        assert_eq!(call.input.as_ref(), block.get("input"), "{id}");
        checked += 1;
    }
    for block in outcomes {
        let id = block["tool_use_id"].as_str().unwrap();
        if let Some(call) = told.iter().find(|call| {
            call.id == id
                && matches!(
                    call.status,
                    Some(ToolStatus::Completed | ToolStatus::Failed)
                )
        }) {
            assert_eq!(call.output.as_ref(), block.get("content"), "{id}");
            checked += 1;
        }
    }
    assert!(checked >= 8, "{checked} checked");
}
