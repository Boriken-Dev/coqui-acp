//! Pi's RPC lines as events: the recorded ones, and the shapes the stand-in for the model cannot make Pi write.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_pi::harness::event::{
    Content, Event, FileChange, Severity, Stop, ToolContent, ToolKind, ToolStatus, TurnEnd,
};
use coqui_acp_pi::parse::text_of;
use coqui_acp_pi::{models_of, user_input};
use serde_json::{Value, json};
use support::{Scratch, parsed, parser, recorded, work};

fn ends_of(events: &[Event]) -> Vec<&TurnEnd> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::TurnEnd(end) => Some(end),
            _ => None,
        })
        .collect()
}

/// The last `Event::Tool` for each tool id, in the order the ids first appeared.
fn tools_of(events: &[Event]) -> Vec<coqui_acp_pi::harness::event::ToolCall> {
    let mut ids: Vec<String> = Vec::new();
    let mut last = std::collections::BTreeMap::new();
    for event in events {
        if let Event::Tool(call) = event {
            if !ids.contains(&call.id) {
                ids.push(call.id.clone());
            }
            let entry = last
                .entry(call.id.clone())
                .or_insert_with(|| coqui_acp_pi::harness::event::ToolCall::new(call.id.clone()));
            if call.name.is_some() {
                entry.name.clone_from(&call.name);
            }
            if call.kind.is_some() {
                entry.kind = call.kind;
            }
            if call.status.is_some() {
                entry.status = call.status;
            }
            if call.title.is_some() {
                entry.title.clone_from(&call.title);
            }
            if call.locations.is_some() {
                entry.locations.clone_from(&call.locations);
            }
            if call.content.is_some() {
                entry.content.clone_from(&call.content);
            }
        }
    }
    ids.into_iter()
        .map(|id| last.remove(&id).unwrap())
        .collect()
}

// Recorded: Pi 1.5.2 on Windows against a loopback stand-in for the model.

#[test]
fn a_recorded_turn_streams_text_ends_its_message_and_reports_what_it_used() {
    let (events, _) = parsed("prompt_text");
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                Event::MessageChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        "Hello from the stand-in."
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::MessageEnd { .. }))
            .count(),
        1
    );
    let usage = events
        .iter()
        .find_map(|event| match event {
            Event::Usage(usage) => Some(usage),
            _ => None,
        })
        .expect("usage");
    assert_eq!((usage.input, usage.output), (25, 6));
    assert_eq!(usage.context_used, Some(31));
    assert!(usage.cost_usd.is_some_and(|cost| cost > 0.0));
    assert_eq!(ends_of(&events), [&TurnEnd::stopped(Stop::EndTurn)]);
}

#[test]
fn reasoning_is_a_thought_of_its_own_ended_though_pi_ends_it_late() {
    let (events, _) = parsed("prompt_thinking");
    let thought: String = events
        .iter()
        .filter_map(|event| match event {
            Event::ThoughtChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!thought.is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::ThoughtEnd { .. }))
            .count(),
        1
    );
    let thought_id = events.iter().find_map(|event| match event {
        Event::ThoughtChunk { message, .. } => Some(message.clone()),
        _ => None,
    });
    let text_id = events.iter().find_map(|event| match event {
        Event::MessageChunk { message, .. } => Some(message.clone()),
        _ => None,
    });
    assert_ne!(
        thought_id, text_id,
        "a thought and the answer are two messages"
    );
}

#[test]
fn a_command_is_a_tool_with_a_terminal_holding_its_output_and_exit_code() {
    let (events, _) = parsed("tool_bash");
    let first = tools_of(&events).remove(0);
    assert_eq!(first.kind, Some(ToolKind::Execute));
    // The recorded command exits with code 3, which Pi reports as an error.
    assert_eq!(first.status, Some(ToolStatus::Failed));
    assert_eq!(
        first.content,
        Some(vec![ToolContent::Terminal(first.id.clone())])
    );
    let terminals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Terminal(terminal) if terminal.id == first.id => Some(terminal),
            _ => None,
        })
        .collect();
    assert!(terminals[0].command.is_some());
    let output: String = terminals
        .iter()
        .filter_map(|terminal| terminal.output.as_deref())
        .collect();
    assert!(output.contains("hi from bash"), "{output:?}");
    assert_eq!(terminals.last().unwrap().exit, Some(Some(3)));
}

#[test]
fn an_edit_shows_its_replacements_with_a_git_patch_and_the_line_it_changed() {
    let (events, _) = parsed("tool_edit");
    let calls = tools_of(&events);
    assert_eq!(calls.len(), 2, "the model reused an id: {calls:?}");
    assert_ne!(calls[0].id, calls[1].id);
    let edit = &calls[0];
    assert_eq!(edit.kind, Some(ToolKind::Edit));
    assert_eq!(edit.status, Some(ToolStatus::Completed));
    let Some(ToolContent::Diff(diff)) = edit.content.as_ref().and_then(|content| content.first())
    else {
        panic!("no diff in {edit:?}");
    };
    assert_eq!(diff.change, FileChange::Modify);
    assert_eq!(diff.old_text.as_deref(), Some("alpha"));
    assert_eq!(diff.new_text.as_deref(), Some("ALPHA"));
    assert_eq!(diff.path, work().join("notes.txt").to_string_lossy());
    let patch = diff.patch.as_deref().expect("a patch");
    assert!(patch.starts_with("diff --git "), "{patch}");
    assert!(patch.contains("@@ -1,2 +1,2 @@"), "{patch}");
    assert_eq!(edit.locations.as_ref().unwrap()[0].line, Some(1));
    // The second edit found nothing to replace.
    assert_eq!(calls[1].status, Some(ToolStatus::Failed));
    let Some(ToolContent::Text(why)) = calls[1]
        .content
        .as_ref()
        .and_then(|content| content.first())
    else {
        panic!("no reason in {:?}", calls[1]);
    };
    assert!(why.starts_with("Could not find the exact text"), "{why}");
}

#[test]
fn a_write_is_a_new_file_unless_one_is_there_to_read_first() {
    let (events, _) = parsed("tool_write");
    let write = &tools_of(&events)[0];
    let Some(ToolContent::Diff(diff)) = write.content.as_ref().and_then(|content| content.first())
    else {
        panic!("no diff in {write:?}");
    };
    assert_eq!(diff.change, FileChange::Add);
    assert_eq!(diff.old_text, None);
    assert!(
        diff.new_text
            .as_deref()
            .is_some_and(|text| text.contains("line two"))
    );
    assert_eq!(write.status, Some(ToolStatus::Completed));

    // Pi gives no old content, so the parser reads the file before the write, inside the directories of the session.
    let scratch = Scratch::new("write");
    std::fs::write(scratch.0.join("out.txt"), "old\n").unwrap();
    let (parser, _) = parser();
    let mut parser = parser.with_roots(&scratch.0, Vec::new());
    let start = json!({"type": "tool_execution_start", "toolCallId": "w", "toolName": "write",
                       "args": {"path": "out.txt", "content": "new\n"}});
    let events = parser.line(&start);
    let call = tools_of(&events).remove(0);
    let Some(ToolContent::Diff(diff)) = call.content.as_ref().and_then(|content| content.first())
    else {
        panic!("no diff in {call:?}");
    };
    assert_eq!(
        (diff.change, diff.old_text.as_deref()),
        (FileChange::Modify, Some("old\n"))
    );

    // A file outside them is never read, however it is named.
    let elsewhere = Scratch::new("elsewhere");
    std::fs::write(elsewhere.0.join("hidden.txt"), "TOPSECRETCONTENT").unwrap();
    let start = json!({"type": "tool_execution_start", "toolCallId": "x", "toolName": "write",
                       "args": {"path": elsewhere.0.join("secret.txt"), "content": "new\n"}});
    let events = parser.line(&start);
    assert!(format!("{events:?}").contains("Add"), "{events:?}");
    assert!(
        !format!("{events:?}").contains("TOPSECRETCONTENT"),
        "{events:?}"
    );
}

#[test]
fn a_read_names_its_file_and_an_image_in_a_result_is_only_counted() {
    let (events, _) = parsed("tool_read");
    let read = &tools_of(&events)[0];
    assert_eq!(read.kind, Some(ToolKind::Read));
    assert!(read.title.as_deref().unwrap().starts_with("Read "));
    assert_eq!(
        read.locations.as_ref().unwrap()[0].path,
        work().join("notes.txt").to_string_lossy()
    );
    let (events, _) = parsed("tool_read_image");
    let shown = format!("{:?}", tools_of(&events)[0].content);
    assert!(shown.contains("1 image not shown"), "{shown}");
}

#[test]
fn calls_made_together_are_two_tools_that_end_as_they_finish() {
    let (events, _) = parsed("tool_two");
    let calls = tools_of(&events);
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].id, calls[1].id);
    // One read, and one of a tool Pi lacks, which ends as a failure in Pi's words.
    let mut statuses: Vec<_> = calls
        .iter()
        .map(|call| call.status.unwrap().as_str())
        .collect();
    statuses.sort_unstable();
    assert_eq!(statuses, ["completed", "failed"]);
    let failed = calls
        .iter()
        .find(|call| call.status == Some(ToolStatus::Failed))
        .unwrap();
    assert_eq!(
        failed.content,
        Some(vec![ToolContent::Text("Tool ls not found".into())])
    );
}

#[test]
fn an_image_in_a_prompt_is_sent_as_one_and_a_text_resource_as_a_context_block() {
    let (message, images) = user_input(&[
        Content::Text("What is this?".into()),
        Content::Image {
            mime_type: "image/png".into(),
            data: "AAAA".into(),
        },
    ])
    .unwrap();
    assert_eq!(message, "What is this?");
    assert_eq!(
        images,
        vec![json!({"type": "image", "data": "AAAA", "mimeType": "image/png"})]
    );
    let sent = recorded("prompt_image");
    let echoed = sent
        .iter()
        .find(|line| line["type"] == "message_end" && line["message"]["role"] == "user")
        .expect("the user message");
    assert_eq!(echoed["message"]["content"][1]["type"], "image");
}

#[test]
fn a_stopped_run_is_cancelled_and_its_unfinished_command_with_it() {
    let (events, _) = parsed("abort_turn");
    assert_eq!(ends_of(&events)[0], &TurnEnd::stopped(Stop::Cancelled));
    // A command killed by the abort: cancelled when the person asked to stop, else failed.
    let lines = recorded("abort_tool");
    for (interrupting, status) in [(true, ToolStatus::Cancelled), (false, ToolStatus::Failed)] {
        let (mut parser, _) = parser();
        parser.prompting();
        if interrupting {
            assert!(parser.interrupting());
        }
        let events: Vec<Event> = lines.iter().flat_map(|line| parser.line(line)).collect();
        assert_eq!(tools_of(&events)[0].status, Some(status));
        let terminal = events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::Terminal(terminal) => Some(terminal),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            terminal.exit,
            Some(None),
            "no exit code of a killed command"
        );
        assert_eq!(ends_of(&events).len(), 1);
    }
}

#[test]
fn a_provider_error_is_a_failed_turn_a_limit_when_it_says_so_and_a_retry_is_told() {
    let (events, _) = parsed("errors");
    let ends = ends_of(&events);
    assert_eq!(ends.len(), 4, "{ends:?}");
    let TurnEnd::Limited(hit) = ends[0] else {
        panic!("a 429 ended {:?}", ends[0]);
    };
    assert_eq!(hit.text.as_deref(), Some("Error 429"));
    let TurnEnd::Failed(why) = ends[1] else {
        panic!("a 500 ended {:?}", ends[1]);
    };
    assert!(why.starts_with("500: "), "{why}");
    let TurnEnd::Failed(why) = ends[2] else {
        panic!("a cut stream ended {:?}", ends[2]);
    };
    assert_eq!(why, "Stream ended without finish_reason");
    // A 429 retried and answered the second time is no failure.
    assert_eq!(ends[3], &TurnEnd::stopped(Stop::EndTurn));
    assert!(events.iter().any(|event| matches!(event,
        Event::Notice { severity: Severity::Info, title } if title.starts_with("Pi is retrying (attempt 1 of 3)"))));
}

#[test]
fn a_cancelled_retry_ends_the_turn_cancelled_when_the_person_stopped_it_and_failed_when_not() {
    let lines = recorded("retry_abort");
    for (interrupting, cancelled) in [(true, true), (false, false)] {
        let (mut parser, _) = parser();
        parser.prompting();
        if interrupting {
            parser.interrupting();
        }
        let events: Vec<Event> = lines.iter().flat_map(|line| parser.line(line)).collect();
        let ends = ends_of(&events);
        assert_eq!(ends.len(), 1);
        assert_eq!(
            matches!(
                ends[0],
                TurnEnd::Stopped {
                    stop: Stop::Cancelled,
                    ..
                }
            ),
            cancelled,
            "{:?}",
            ends[0]
        );
    }
}

#[test]
fn a_compaction_is_told_with_its_summary() {
    let (events, _) = parsed("compaction");
    let summary = events.iter().find_map(|event| match event {
        Event::Compaction { summary, .. } => summary.clone(),
        _ => None,
    });
    assert!(summary.is_some_and(|text| text.contains("Stand-in summary")));
}

#[test]
fn what_an_extension_shows_is_a_notice_and_what_it_asks_is_cancelled() {
    let (events, written) = parsed("ui_methods");
    assert!(events.iter().any(|event| matches!(event,
        Event::Notice { severity: Severity::Warning, title } if title == "hello notify")));
    let written = written.lock().unwrap();
    let cancelled = written
        .iter()
        .filter(|line| line["type"] == "extension_ui_response" && line["cancelled"] == true)
        .count();
    assert_eq!(
        cancelled, 4,
        "select, confirm, input and editor: {written:?}"
    );
}

#[test]
fn a_prompt_pi_refuses_is_an_answer_that_says_why() {
    let (mut parser, _) = parser();
    for line in recorded("no_credentials") {
        parser.line(&line);
    }
    let answers = parser.answers();
    let refused = answers
        .iter()
        .find_map(|(_, answer)| answer.as_ref().err())
        .expect("a refusal");
    assert!(refused.starts_with("No API key found"), "{refused}");
}

#[test]
fn the_models_and_thinking_levels_pi_lists_are_read() {
    let lines = recorded("startup_state");
    let models = lines
        .iter()
        .find(|line| line["command"] == "get_available_models")
        .map(|line| models_of(&line["data"]))
        .unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["stub/stub-fast", "stub/stub-think"]
    );
    assert!(!models[0].reasoning && !models[0].images);
    assert!(models[1].reasoning && models[1].images);
    assert_eq!(models[1].context_window, Some(32000));
    let state = lines
        .iter()
        .find(|line| line["command"] == "get_state")
        .unwrap();
    assert_eq!(state["data"]["thinkingLevel"], "medium");
    // `set_model` of a model Pi lacks is a refusal in its words.
    let (mut parser, _) = parser();
    for line in recorded("models_thinking") {
        parser.line(&line);
    }
    let refusals: Vec<_> = parser
        .answers()
        .into_iter()
        .filter_map(|(_, answer)| answer.err())
        .collect();
    assert_eq!(refusals, ["Model not found: stub/no-such-model"]);
}

#[test]
fn a_session_pi_reopens_is_replayed_as_the_conversation_it_was() {
    let lines = recorded("session");
    let messages: Vec<Value> = lines
        .iter()
        .filter(|line| line["command"] == "get_messages")
        .filter_map(|line| line["data"]["messages"].as_array())
        .max_by_key(|messages| messages.len())
        .cloned()
        .unwrap();
    assert!(!messages.is_empty());
    let (mut parser, _) = parser();
    let events = parser.history(&messages);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::UserChunk { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::UserEnd { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::MessageChunk { .. }))
    );
    assert!(ends_of(&events).is_empty(), "history ends no turn");
    // A command the person ran through `bash`, and a call with its result, replay as finished tools.
    let history = parser.history(&[
        json!({"role": "bashExecution", "command": "echo hi", "output": "hi\n", "exitCode": 0}),
        json!({"role": "assistant", "content": [
            {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": "a.txt"}}]}),
        json!({"role": "toolResult", "toolCallId": "c1", "toolName": "read",
               "content": [{"type": "text", "text": "A"}], "isError": false}),
    ]);
    let calls = tools_of(&history);
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| call.status == Some(ToolStatus::Completed)),
        "{calls:?}"
    );
}

#[test]
fn result_text_is_the_text_parts_joined() {
    assert_eq!(
        text_of(
            &json!([{"type": "text", "text": "a"}, {"type": "image"}, {"type": "text", "text": "b"}])
        ),
        "a\nb"
    );
    assert_eq!(text_of(&json!("whole")), "whole");
}

#[test]
fn a_command_of_an_extension_is_a_turn_that_ends_at_once() {
    let (mut parser, _) = parser();
    parser.prompting();
    let events = parser.handled();
    assert_eq!(ends_of(&events), [&TurnEnd::stopped(Stop::EndTurn)]);
    // A run that began is not ended by it.
    parser.prompting();
    parser.line(&json!({"type": "agent_start"}));
    assert!(parser.handled().is_empty());
}
