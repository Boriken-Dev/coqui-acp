//! Every tool item in the recordings reaches the client as a call with its kind, its input and, once it is done, its
//! output: a history written from these events is only as faithful as they are.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_codex::harness::event::{Event, ToolCall, ToolStatus};
use serde_json::Value;
use support::{fixtures, parsed, recorded};

const TOOL_ITEMS: [&str; 5] = [
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "webSearch",
    "imageView",
];

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

fn calls_of(events: &[Event]) -> Vec<&ToolCall> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Tool(call) => Some(call),
            _ => None,
        })
        .collect()
}

#[test]
fn every_recorded_tool_item_is_a_call_with_its_kind_input_and_output() {
    let mut started = 0;
    let mut done = 0;
    for name in recordings() {
        let (events, _) = parsed(&name);
        let told = calls_of(&events);
        for line in recorded(&name) {
            let Some(item) = line.pointer("/params/item") else {
                continue;
            };
            let kind = item["type"].as_str().unwrap_or_default();
            if !TOOL_ITEMS.contains(&kind) {
                continue;
            }
            let id = item["id"].as_str().unwrap();
            let finished = line["method"] == "item/completed";
            let want = |call: &&&ToolCall| call.id == id && call.kind.is_some();
            let call = told
                .iter()
                .find(want)
                .unwrap_or_else(|| panic!("{name}: {kind} {id} was never told as a call"));
            assert!(call.input.is_some(), "{name}: the input of {kind} {id}");
            if kind == "commandExecution" {
                assert_eq!(
                    call.input.as_ref().and_then(|input| input.get("command")),
                    item.get("command"),
                    "{name}: the command of {id}"
                );
            }
            if finished {
                let end = told
                    .iter()
                    .rev()
                    .find(|call| {
                        call.id == id
                            && matches!(
                                call.status,
                                Some(ToolStatus::Completed | ToolStatus::Failed)
                            )
                            && call.output.is_some()
                    })
                    .unwrap_or_else(|| panic!("{name}: {kind} {id} ended with no output"));
                if kind == "commandExecution" {
                    let output: &Value = end.output.as_ref().unwrap();
                    assert_eq!(output["aggregatedOutput"], item["aggregatedOutput"], "{name}");
                    assert_eq!(output["exitCode"], item["exitCode"], "{name}");
                }
                done += 1;
            } else {
                started += 1;
            }
        }
    }
    assert!(started >= 4 && done >= 4, "{started} started, {done} done");
}
