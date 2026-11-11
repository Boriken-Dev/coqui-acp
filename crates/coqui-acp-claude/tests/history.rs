//! Claude Code's transcript as a standard history, and back: the recorded sessions round-trip with
//! nothing lost, a compaction keeps its summary, and an import keeps the rules for hostile histories.
#![cfg(feature = "history")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_claude::history::{ImportOptions, export, export_file, import, import_file};
use coqui_acp_history::{
    Body, Document, Header, Line, Origin, Stop, StopReason, THINKING, TOOL_AS_NOTE,
};
use serde_json::{Value, json};
use support::{Scratch, fixtures};

const ID: &str = "0b2e9a4c-7a35-4d2f-9f0e-0f4f3f1f2a11";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}

fn values(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// What the model is given by a transcript: each message of the person or the assistant, as
/// `role` and content blocks, in order. What Claude Code keeps beside it (ids, times, the
/// permission it recorded) is not part of it.
fn conversation(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for value in values(text) {
        let kind = value["type"].as_str().unwrap_or_default();
        if !matches!(kind, "user" | "assistant") || value["isSidechain"] == json!(true) {
            continue;
        }
        let content = &value["message"]["content"];
        let blocks = match content {
            Value::String(text) => vec![json!({"type": "text", "text": text})],
            Value::Array(blocks) => blocks.clone(),
            _ => Vec::new(),
        };
        for block in blocks {
            out.push((kind.to_owned(), block));
        }
    }
    out
}

/// The lines of a history without what only shows a tool's result (its diff or text), which is made from what
/// Claude Code keeps beside the conversation and is not part of it.
fn shown_alike(doc: &Document) -> Vec<Line> {
    doc.lines
        .iter()
        .cloned()
        .map(|mut line| {
            if let Body::Update { update } = &mut line.body
                && update["sessionUpdate"] == "tool_call_update"
            {
                update.as_object_mut().unwrap().remove("content");
            }
            line
        })
        .collect()
}

fn methods(doc: &Document) -> Vec<&'static str> {
    doc.lines.iter().map(|line| line.body.method()).collect()
}

fn updates(doc: &Document, kind: &str) -> Vec<Value> {
    doc.lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Update { update } if update["sessionUpdate"] == kind => Some(update.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_recorded_transcript_exports_as_a_history_with_its_tools_and_turns() {
    let doc = export(&fixture("turn.transcript.jsonl"), ID).unwrap();
    assert_eq!(doc.header.id, ID);
    assert_eq!(doc.header.origin.harness, "claude");
    assert_eq!(doc.header.origin.version.as_deref(), Some("2.1.318"));
    assert_eq!(
        doc.header.origin.session_id.as_deref(),
        Some("6f2c1b0e-0000-4000-8000-00000000c0de")
    );
    assert_eq!(doc.header.cwd.as_deref(), Some("C:\\coqui-fx\\w"));
    assert!(doc.header.digest.is_some());
    doc.check().unwrap();
    let methods = methods(&doc);
    assert_eq!(
        methods.iter().filter(|m| **m == "session/prompt").count(),
        2
    );
    assert_eq!(
        methods.iter().filter(|m| **m == "history/turn_end").count(),
        2
    );
    assert_eq!(methods.last(), Some(&"history/turn_end"));

    // Every tool call of the recording, with the kind, name and input it has natively.
    let calls = updates(&doc, "tool_call");
    let names: Vec<&str> = calls
        .iter()
        .map(|call| call["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["TodoWrite", "Read", "Edit", "Bash", "Write", "Grep"]
    );
    let by_name = |name: &str| calls.iter().find(|call| call["name"] == name).unwrap();
    assert_eq!(by_name("Edit")["kind"], "edit");
    assert_eq!(
        by_name("Edit")["rawInput"],
        json!({"replace_all": false, "file_path": "C:/coqui-fx/w/notes.txt", "old_string": "two", "new_string": "three"})
    );
    assert_eq!(by_name("Bash")["kind"], "execute");
    assert_eq!(by_name("Grep")["kind"], "search");
    assert_eq!(
        by_name("Read")["locations"],
        json!([{"path": "C:/coqui-fx/w/notes.txt"}])
    );

    // Their results: the output as the model was given it, and how each ended.
    let results = updates(&doc, "tool_call_update");
    assert_eq!(results.len(), 6);
    let read = results
        .iter()
        .find(|r| r["toolCallId"] == "toolu_standin_002_0")
        .unwrap();
    assert_eq!(read["status"], "completed");
    assert_eq!(read["rawOutput"], "1\tone\n2\ttwo\n3\t");
    let refused = results
        .iter()
        .find(|r| r["toolCallId"] == "toolu_standin_004_0")
        .unwrap();
    assert_eq!(refused["status"], "failed");
    assert_eq!(refused["rawOutput"], "The person refused this.");
    let edit = results
        .iter()
        .find(|r| r["toolCallId"] == "toolu_standin_003_0")
        .unwrap();
    assert_eq!(edit["content"][0]["type"], "diff");

    // The thinking keeps its signature where only Claude Code reads it.
    let thought = doc
        .lines
        .iter()
        .find(|line| matches!(&line.body, Body::Update { update } if update["sessionUpdate"] == "agent_thought_chunk"))
        .unwrap();
    assert_eq!(
        thought.meta.as_ref().unwrap()["dev.boriken.history/native"]["signature"],
        "c3RhbmQtaW4="
    );
    // The first prompt carries its three blocks.
    let Body::Prompt { prompt } = &doc.lines[0].body else {
        panic!("the first line is a prompt")
    };
    let kinds: Vec<&str> = prompt
        .iter()
        .map(|block| block["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["text", "image", "text"]);
}

#[test]
fn a_recorded_session_goes_from_claude_to_claude_with_nothing_lost() {
    let original = fixture("turn.transcript.jsonl");
    let doc = export(&original, ID).unwrap();
    let imported = import(&doc, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
    assert!(imported.report.is_empty(), "{:?}", imported.report);
    assert!(!imported.closed_turn && imported.closed_calls == 0);
    // Every message the model is given, block for block.
    assert_eq!(conversation(&imported.text), conversation(&original));
    // And a history made of it is the history it came from.
    let again = export(&imported.text, ID).unwrap();
    assert_eq!(shown_alike(&again), shown_alike(&doc));
    // Claude Code can follow the chain: one root, every other line the child of the one before.
    let lines = values(&imported.text);
    assert_eq!(lines[0]["parentUuid"], Value::Null);
    for pair in lines.windows(2) {
        assert_eq!(pair[1]["parentUuid"], pair[0]["uuid"]);
    }
    assert!(
        lines
            .iter()
            .all(|line| line["sessionId"] == ID && line["cwd"] == "C:\\coqui-fx\\w")
    );
    assert!(
        lines
            .iter()
            .all(|line| matches!(line["type"].as_str(), Some("user" | "assistant")))
    );
}

#[test]
fn a_compaction_keeps_its_summary_and_seeds_the_session() {
    let original = fixture("compact.transcript.jsonl");
    let doc = export(&original, ID).unwrap();
    let summary = doc
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::Compaction { summary, .. } => summary.clone(),
            _ => None,
        })
        .expect("the compaction has its summary");
    assert!(summary.starts_with("This session is being continued"));
    let imported = import(&doc, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
    let lines = values(&imported.text);
    assert_eq!(lines[0]["subtype"], "compact_boundary");
    assert_eq!(lines[0]["compactMetadata"]["trigger"], "manual");
    assert_eq!(lines[1]["isCompactSummary"], true);
    assert_eq!(lines[1]["message"]["content"], summary.as_str());
    assert_eq!(imported.seeded_from.as_ref().unwrap().line, 4);
    // What came before the boundary is the summary now: nothing of it is written.
    assert!(!imported.text.contains("First answer."));
    // The summary goes to its own slot and nowhere else.
    assert_eq!(imported.text.matches(&summary[..40]).count(), 1);
    // Seeding without it is every line, and writes no summary.
    let options = ImportOptions::default().use_summary(false);
    let whole = import(&doc, "C:\\coqui-fx\\w", ID, &options).unwrap();
    assert!(whole.text.contains("First answer."));
    assert!(!whole.text.contains("isCompactSummary"));
    assert_eq!(whole.report.count("summary_missing"), 0);
}

/// A history of one turn: a prompt, a tool call with `tool`'s name and input, its result.
fn history(origin: &str, name: &str, input: Value, output: Value) -> Document {
    let mut doc = Document::new(Header::new(ID, Origin::new(origin)));
    doc.lines.push(Line::text_prompt("Run it."));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}),
    ));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "tool_call", "toolCallId": "call:1/../x", "name": name,
        "title": name, "kind": "other", "status": "pending", "rawInput": input}),
    ));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "call:1/../x",
        "status": "completed", "rawOutput": output}),
    ));
    doc.lines.push(Line::update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Done."}})));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    doc
}

#[test]
fn another_harnesss_tools_become_claudes_or_a_note_and_its_thinking_is_dropped() {
    let shell = history(
        "codex",
        "shell_command",
        json!({"command": "echo hi", "workdir": "/w", "timeout_ms": 5000}),
        json!("hi\n"),
    );
    let imported = import(&shell, "/w", ID, &ImportOptions::default()).unwrap();
    let lines = values(&imported.text);
    let call = lines
        .iter()
        .find(|l| l["message"]["content"][0]["type"] == "tool_use")
        .unwrap();
    assert_eq!(call["message"]["content"][0]["name"], "Bash");
    assert_eq!(
        call["message"]["content"][0]["input"],
        json!({"command": "echo hi", "timeout": 5000})
    );
    // A call id Claude Code would not take is made one it does.
    assert_eq!(call["message"]["content"][0]["id"], "call_1____x");
    let result = lines
        .iter()
        .find(|l| l["message"]["content"][0]["type"] == "tool_result")
        .unwrap();
    assert_eq!(
        result["message"]["content"][0]["tool_use_id"],
        "call_1____x"
    );
    assert_eq!(result["message"]["content"][0]["content"], "hi\n");
    assert_eq!(imported.report.count(THINKING), 1);
    assert_eq!(imported.report.count(TOOL_AS_NOTE), 0);

    let patch = history(
        "codex",
        "apply_patch",
        json!({"input": "*** Begin Patch\n*** End Patch\n"}),
        json!("done"),
    );
    let imported = import(&patch, "/w", ID, &ImportOptions::default()).unwrap();
    assert_eq!(imported.report.count(TOOL_AS_NOTE), 1);
    assert!(imported.text.contains("[Tool call: apply_patch"));
    assert!(!imported.text.contains("tool_use"));
}

#[test]
fn an_import_ends_what_the_history_left_open_and_grants_nothing() {
    let mut doc = Document::new(Header::new(ID, Origin::new("claude")));
    doc.lines.push(Line::text_prompt("Do it."));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "name": "Bash",
        "title": "ls", "kind": "execute", "status": "in_progress", "rawInput": {"command": "ls"}}),
    ));
    doc.lines.push(Line::new(Body::PermissionRequest {
        tool_call: json!({"toolCallId": "t1"}),
        options: vec![
            json!({"optionId": "allow_always", "name": "Always", "kind": "allow_always"}),
        ],
    }));
    doc.lines.push(Line::new(Body::PermissionAnswer {
        tool_call_id: "t1".into(),
        outcome: json!({"outcome": "selected", "optionId": "allow_always"}),
    }));
    let imported = import(&doc, "/w", ID, &ImportOptions::default()).unwrap();
    assert!(imported.closed_turn);
    assert_eq!(imported.closed_calls, 1);
    let lines = values(&imported.text);
    // The call has a result that says it did not run, and the turn ends interrupted.
    let result = lines
        .iter()
        .find(|l| l["message"]["content"][0]["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["message"]["content"][0]["is_error"], true);
    assert_eq!(
        lines.last().unwrap()["message"]["content"],
        "[Request interrupted by user]"
    );
    // No grant, no rule, no snapshot.
    for forbidden in [
        "allow_always",
        "permission",
        "file-history-snapshot",
        "updatedPermissions",
    ] {
        assert!(!imported.text.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn a_session_is_written_where_resume_finds_it_and_never_over_one() {
    let doc = export(&fixture("turn.transcript.jsonl"), ID).unwrap();
    let scratch = Scratch::new("history-files");
    let config = scratch.0.join("config");
    let cwd = scratch.0.join("work");
    let imported = import_file(&config, &cwd, ID, &doc, &ImportOptions::default()).unwrap();
    // A name the history holds is no path: the file is where the host's id and directory say.
    let path = coqui_acp_claude::transcript(&config, &cwd, ID).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), imported.text);
    let again = import_file(&config, &cwd, ID, &doc, &ImportOptions::default());
    assert!(again.is_err(), "an import never overwrites");
    // And it reads back as the history it was written from.
    let back = export_file(&config, &cwd, ID, 1 << 20).unwrap();
    assert_eq!(shown_alike(&back), shown_alike(&doc));
    assert!(
        export_file(&config, &cwd, ID, 100).is_err(),
        "a transcript past the bound is refused"
    );
    assert!(import(&doc, "/w", "../../x", &ImportOptions::default()).is_err());
}
