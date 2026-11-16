//! Pi's session file as a standard history, and back: the recorded session exports with its turns and tools, goes
//! through Pi to Pi and through Claude Code to Pi and back with what each cannot take reported, and an import keeps
//! the rules for hostile histories.
#![cfg(feature = "history")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_history::{
    Body, Document, Header, Line, Origin, Stop, StopReason, THINKING, TOOL_AS_NOTE,
};
use coqui_acp_pi::history::{ImportOptions, export, export_file, find, import, import_file};
use serde_json::{Value, json};
use support::{Scratch, fixtures};

const ID: &str = "0b2e9a4c-7a35-4d2f-9f0e-0f4f3f1f2a11";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}

fn claude_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("coqui-acp-claude")
        .join("tests")
        .join("fixtures")
        .join("2.1.318")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn values(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn updates<'a>(doc: &'a Document, kind: &str) -> Vec<&'a Value> {
    doc.lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Update { update } if update["sessionUpdate"] == kind => Some(update),
            _ => None,
        })
        .collect()
}

fn count(doc: &Document, wanted: fn(&Body) -> bool) -> usize {
    doc.lines.iter().filter(|line| wanted(&line.body)).count()
}

/// What a history says, without what only one harness's file would keep (ids, the native `_meta`).
fn essence(doc: &Document) -> Vec<Value> {
    doc.lines
        .iter()
        .map(|line| match &line.body {
            Body::Prompt { prompt } => json!(["prompt", line.at, prompt]),
            Body::Update { update } => json!([
                update["sessionUpdate"],
                line.at,
                update.get("content"),
                update.get("name"),
                update.get("kind"),
                update.get("rawInput"),
                update.get("rawOutput"),
                update.get("status")
            ]),
            Body::Cancel => json!(["cancel"]),
            Body::TurnEnd { stop, error } => json!(["end", stop.name(), error]),
            Body::Compaction { summary, .. } => json!(["compaction", summary]),
            _ => json!(["other"]),
        })
        .collect()
}

/// `essence` with the times blanked, for a history that had none.
fn untimed(doc: &Document) -> Vec<Value> {
    essence(doc)
        .into_iter()
        .map(|mut line| {
            if line.as_array().is_some_and(|a| a.len() > 2) {
                line[1] = Value::Null;
            }
            line
        })
        .collect()
}

fn loss(report: &coqui_acp_history::LossReport) -> Vec<(String, u64)> {
    report
        .losses()
        .iter()
        .map(|loss| (loss.kind.clone(), loss.count))
        .collect()
}

#[test]
fn a_recorded_session_exports_as_a_history_with_its_tools_and_turns() {
    let doc = export(&fixture("session_file.jsonl"), ID).unwrap();
    let header = &doc.header;
    assert_eq!(header.id, ID);
    assert_eq!(header.origin.harness, "pi");
    assert_eq!(
        header.origin.session_id.as_deref(),
        Some("01a1e36a-4570-708c-9a3d-55cd79367e6a")
    );
    assert_eq!(header.cwd.as_deref(), Some("/work/project"));
    assert_eq!(header.model.as_deref(), Some("stub/stub-think"));
    assert_eq!(header.effort.as_deref(), Some("medium"));
    assert!(header.created_at.is_some());
    doc.check().unwrap();
    assert_eq!(Document::from_slice(&doc.to_vec().unwrap()).unwrap(), doc);

    // Nine prompts of the person, and how each turn ended: the stand-in failing, the person stopping a slow answer.
    assert_eq!(count(&doc, |b| matches!(b, Body::Prompt { .. })), 9);
    let stops: Vec<String> = doc
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::TurnEnd { stop, .. } => Some(stop.name().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        stops,
        [
            "end_turn",
            "end_turn",
            "end_turn",
            "end_turn",
            "end_turn",
            "end_turn",
            "error",
            "cancelled",
            "end_turn"
        ]
    );
    let failed = doc
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::TurnEnd {
                stop: Stop::Error,
                error,
                ..
            } => error.clone(),
            _ => None,
        })
        .unwrap();
    assert!(failed.starts_with("500: "), "{failed}");
    assert_eq!(count(&doc, |b| matches!(b, Body::Cancel)), 1);

    // The person's image is a block of the prompt.
    let image = doc.lines.iter().find_map(|line| match &line.body {
        Body::Prompt { prompt } => prompt.iter().find(|b| b["type"] == "image"),
        _ => None,
    });
    assert_eq!(image.unwrap()["mimeType"], "image/png");

    // Thinking keeps the signature Pi wrote, in the native `_meta`.
    let thought = doc
        .lines
        .iter()
        .find(|line| {
            matches!(&line.body, Body::Update { update } if update["sessionUpdate"] == "agent_thought_chunk")
        })
        .unwrap();
    assert_eq!(
        thought.meta.as_ref().unwrap()["dev.boriken.history/native"]["signature"],
        "reasoning_content"
    );

    // Each tool call is Pi's own: its name and input exactly, and a call id the model reused is made another.
    let calls = updates(&doc, "tool_call");
    let names: Vec<&str> = calls.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["read", "write", "edit", "bash", "bash"]);
    let ids: Vec<&str> = calls
        .iter()
        .map(|c| c["toolCallId"].as_str().unwrap())
        .collect();
    assert_eq!(&ids[..4], ["call_1", "call_1~2", "call_1~3", "call_1~4"]);
    let finished = updates(&doc, "tool_call_update");
    assert_eq!(finished.len(), 5);
    for call in &calls {
        assert!(
            finished
                .iter()
                .any(|f| f["toolCallId"] == call["toolCallId"]),
            "{call}"
        );
    }
    assert_eq!(calls[0]["kind"], "read");
    assert_eq!(calls[0]["rawInput"], json!({"path": "notes.txt"}));
    assert_eq!(calls[0]["locations"][0]["path"], "/work/project/notes.txt");
    assert_eq!(
        calls[2]["rawInput"],
        json!({"path": "notes.txt", "edits": [{"oldText": "alpha", "newText": "ALPHA"}]})
    );
    assert_eq!(calls[2]["kind"], "edit");
    assert_eq!(
        calls[2]["content"][0],
        json!({"type": "diff", "path": "/work/project/notes.txt", "oldText": "alpha", "newText": "ALPHA"})
    );
    assert_eq!(calls[3]["rawInput"], json!({"command": "echo hello"}));
    assert_eq!(
        finished[3]["rawOutput"],
        json!([{"type": "text", "text": "hello\n"}])
    );
    assert_eq!(finished[3]["status"], "completed");

    // The command the person ran with `!` is a call outside any turn.
    let by_hand = calls[4];
    assert_eq!(by_hand["rawInput"], json!({"command": "echo by-hand"}));
    let at = doc
        .lines
        .iter()
        .position(|line| matches!(&line.body, Body::Update { update } if update["rawInput"]["command"] == "echo by-hand"))
        .unwrap();
    assert!(matches!(doc.lines[at - 1].body, Body::TurnEnd { .. }));
}

#[test]
fn a_branched_session_exports_its_active_branch_with_the_summaries_and_notes() {
    let doc = export(&fixture("session_branches.jsonl"), ID).unwrap();
    assert_eq!(
        doc.header.model.as_deref(),
        Some("anthropic/claude-sonnet-4-5")
    );
    assert_eq!(doc.header.effort.as_deref(), Some("high"));
    doc.check().unwrap();
    let text = doc.to_string().unwrap();
    // The branch that was left is not in the history, the summary of it is.
    assert!(!text.contains("Approach A is abandoned"));
    assert!(text.contains("Approach B works."));
    assert!(text.contains("Approach A was tried and dropped."));
    // A compaction is a boundary with its summary and the context it replaced.
    let compaction = doc
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::Compaction {
                summary,
                pre_tokens,
                ..
            } => Some((summary.clone(), *pre_tokens)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        compaction,
        (
            Some("The plan was made and approach B chosen.".to_owned()),
            Some(12345)
        )
    );
    // An extension's message is a note, and what is no part of the conversation (a label) is not carried.
    assert!(text.contains("Remember the style guide."));
    assert!(!text.contains("chosen\""));
    // The turns end where the next prompt begins, and a note or a summary lies between two turns, not inside one.
    let methods: Vec<&str> = doc
        .lines
        .iter()
        .map(|line| match &line.body {
            Body::Prompt { .. } => "prompt",
            Body::TurnEnd { .. } => "end",
            Body::Compaction { .. } => "compaction",
            Body::Update { update } if update["sessionUpdate"] == "user_message_chunk" => "note",
            _ => "update",
        })
        .collect();
    let prompts = methods.iter().filter(|m| **m == "prompt").count();
    let ends = methods.iter().filter(|m| **m == "end").count();
    assert_eq!((prompts, ends), (3, 3));
    let note = methods.iter().position(|m| *m == "note").unwrap();
    assert_eq!(methods[note - 1], "end");
    assert!(doc.lines.len() - 1 - note > 0);
}

#[test]
fn a_recorded_session_goes_from_pi_to_pi_with_nothing_lost() {
    let first = export(&fixture("session_file.jsonl"), ID).unwrap();
    let again = "5b8e2a7d-1c3f-4e6a-9d0b-2f4a6c8e0b13";
    let imported = import(&first, "/work/project", again, &ImportOptions::default()).unwrap();
    assert!(imported.report.is_empty(), "{:?}", loss(&imported.report));
    assert_eq!(imported.closed_calls, 0);
    assert!(!imported.closed_turn);
    // Pi's own shapes: a header of the host's id, and every line chained to the one before it.
    let lines = values(&imported.text);
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[0]["version"], 3);
    assert_eq!(lines[0]["id"], again);
    assert_eq!(lines[0]["cwd"], "/work/project");
    assert_eq!(lines[1]["type"], "model_change");
    assert_eq!(lines[1]["provider"], "stub");
    assert_eq!(lines[1]["modelId"], "stub-think");
    assert_eq!(lines[2]["thinkingLevel"], "medium");
    for pair in lines[1..].windows(2) {
        assert_eq!(pair[1]["parentId"], pair[0]["id"]);
    }
    assert_eq!(lines[1]["parentId"], Value::Null);
    let ids: std::collections::BTreeSet<&str> = lines[1..]
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), lines.len() - 1, "an id was used twice");
    // The same history gives the same file, and the file name is Pi's: the time with `:` and `.` as `-`.
    assert_eq!(
        imported,
        import(&first, "/work/project", again, &ImportOptions::default()).unwrap()
    );
    assert!(
        imported
            .file_name(again)
            .ends_with(&format!("_{again}.jsonl"))
    );
    assert!(!imported.file_name(again).contains(':'));

    let back = export(&imported.text, ID).unwrap();
    assert_eq!(essence(&back), essence(&first));
    assert_eq!(back.header.model, first.header.model);
    assert_eq!(back.header.effort, first.header.effort);
    assert_eq!(back.header.cwd, first.header.cwd);
}

#[test]
fn a_compaction_keeps_its_summary_and_seeds_the_session() {
    let doc = export(&fixture("session_branches.jsonl"), ID).unwrap();
    let imported = import(&doc, "/work/project", ID, &ImportOptions::default()).unwrap();
    assert!(imported.report.is_empty(), "{:?}", loss(&imported.report));
    let lines = values(&imported.text);
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "session",
            "model_change",
            "thinking_level_change",
            "compaction",
            "custom_message",
            "message",
            "message",
            "message",
            "message"
        ]
    );
    // Nothing before the summary is written, and the summary keeps nothing before itself.
    let compaction = &lines[3];
    assert_eq!(
        compaction["summary"],
        "The plan was made and approach B chosen."
    );
    assert_eq!(compaction["firstKeptEntryId"], compaction["id"]);
    assert_eq!(compaction["tokensBefore"], 12345);
    assert!(!imported.text.contains("plan the work"));
    assert_eq!(lines[4]["customType"], "reminder");
    assert_eq!(lines[4]["content"], "Remember the style guide.");
    assert_eq!(
        lines[5]["message"]["content"][0]["text"],
        "now run the tests"
    );
    let call = &lines[6]["message"];
    assert_eq!(call["stopReason"], "toolUse");
    assert_eq!(call["provider"], "anthropic");
    assert_eq!(
        call["content"][0],
        json!({"type": "toolCall", "id": "call_t1", "name": "bash",
            "arguments": {"command": "cargo test", "timeout": 120}})
    );
    assert_eq!(lines[7]["message"]["role"], "toolResult");
    assert_eq!(lines[7]["message"]["toolCallId"], "call_t1");
    assert!(imported.seeded_from.as_ref().unwrap().line > 1);

    // Seeded from the first line instead, nothing of the summary is written and everything else is.
    let whole = import(
        &doc,
        "/work/project",
        ID,
        &ImportOptions::default().use_summary(false),
    )
    .unwrap();
    assert!(!whole.text.contains("approach B chosen"));
    assert!(whole.text.contains("plan the work"));
    assert!(whole.text.contains("Two steps."));
    assert!(
        !values(&whole.text)
            .iter()
            .any(|l| l["type"] == "compaction")
    );
}

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

fn call_of(text: &str) -> Value {
    values(text)
        .into_iter()
        .find_map(|l| {
            l["message"]["content"]
                .as_array()?
                .iter()
                .find(|b| b["type"] == "toolCall")
                .cloned()
        })
        .expect("a tool call")
}

#[test]
fn another_harnesss_tools_become_pis_or_a_note_and_its_thinking_is_dropped() {
    let shell = history(
        "codex",
        "shell_command",
        json!({"command": "echo hi", "workdir": "/w", "timeout_ms": 5000}),
        json!("hi\n"),
    );
    let imported = import(&shell, "/w", ID, &ImportOptions::default()).unwrap();
    let call = call_of(&imported.text);
    assert_eq!(call["name"], "bash");
    // Pi's bash has no directory of its own and takes its timeout in seconds.
    assert_eq!(
        call["arguments"],
        json!({"command": "echo hi", "timeout": 5})
    );
    // A call id Pi's providers would not take is made one they do.
    assert_eq!(call["id"], "call_1____x");
    let result = values(&imported.text)
        .into_iter()
        .find(|l| l["message"]["role"] == "toolResult")
        .unwrap();
    assert_eq!(result["message"]["toolCallId"], "call_1____x");
    assert_eq!(result["message"]["toolName"], "bash");
    assert_eq!(
        result["message"]["content"],
        json!([{"type": "text", "text": "hi\n"}])
    );
    assert_eq!(result["message"]["isError"], false);
    assert_eq!(loss(&imported.report), [(THINKING.to_owned(), 1)]);
    // Written as another model's: Pi never sends it the signature of one it did not make.
    let assistant = values(&imported.text)
        .into_iter()
        .find(|l| l["message"]["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["message"]["provider"], "codex");
    assert_eq!(assistant["message"]["api"], "imported");

    let vector = history(
        "codex",
        "shell",
        json!({"command": ["ls", "-la", "my dir"], "workdir": "/w"}),
        json!("x"),
    );
    let call = call_of(
        &import(&vector, "/w", ID, &ImportOptions::default())
            .unwrap()
            .text,
    );
    assert_eq!(call["arguments"], json!({"command": "ls -la 'my dir'"}));

    let add = history(
        "codex",
        "apply_patch",
        json!({"input": coqui_acp_history::patch::add("/w/new.txt", "fresh\n")}),
        json!("done"),
    );
    let call = call_of(
        &import(&add, "/w", ID, &ImportOptions::default())
            .unwrap()
            .text,
    );
    assert_eq!(call["name"], "write");
    assert_eq!(
        call["arguments"],
        json!({"path": "/w/new.txt", "content": "fresh\n"})
    );

    let patch = history(
        "codex",
        "apply_patch",
        json!({"input": "*** Begin Patch\n*** End Patch\n"}),
        json!("done"),
    );
    let imported = import(&patch, "/w", ID, &ImportOptions::default()).unwrap();
    assert_eq!(imported.report.count(TOOL_AS_NOTE), 1);
    assert!(imported.text.contains("[Tool call: apply_patch"));
    assert!(!imported.text.contains("toolCall"));
    assert!(!imported.text.contains("toolResult"));

    for (name, input, expect_name, expect_input) in [
        (
            "Read",
            json!({"file_path": "/w/a", "offset": 3}),
            "read",
            json!({"path": "/w/a", "offset": 3}),
        ),
        (
            "Edit",
            json!({"file_path": "/w/a", "old_string": "o", "new_string": "n", "replace_all": false}),
            "edit",
            json!({"path": "/w/a", "edits": [{"oldText": "o", "newText": "n"}]}),
        ),
        (
            "Grep",
            json!({"pattern": "p", "-i": true}),
            "grep",
            json!({"pattern": "p", "ignoreCase": true}),
        ),
        (
            "Glob",
            json!({"pattern": "*.rs", "path": "/w"}),
            "find",
            json!({"pattern": "*.rs", "path": "/w"}),
        ),
    ] {
        let doc = history("claude", name, input, json!("out"));
        let call = call_of(
            &import(&doc, "/w", ID, &ImportOptions::default())
                .unwrap()
                .text,
        );
        assert_eq!(call["name"], expect_name, "{name}");
        assert_eq!(call["arguments"], expect_input, "{name}");
    }

    // What Pi has no tool for, or cannot do in one call, is told as text and counted.
    for (name, input) in [
        ("WebFetch", json!({"url": "https://example.com"})),
        (
            "Edit",
            json!({"file_path": "/w/a", "old_string": "o", "new_string": "n", "replace_all": true}),
        ),
    ] {
        let doc = history("claude", name, input, json!("out"));
        let imported = import(&doc, "/w", ID, &ImportOptions::default()).unwrap();
        assert_eq!(imported.report.count(TOOL_AS_NOTE), 1, "{name}");
        assert!(!imported.text.contains("toolCall"), "{name}");
        assert!(
            imported.text.contains(&format!("[Tool call: {name}")),
            "{name}"
        );
    }
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
    // The call has a result that says it did not run, and the turn is told as stopped.
    let result = lines
        .iter()
        .find(|l| l["message"]["role"] == "toolResult")
        .unwrap();
    assert_eq!(result["message"]["isError"], true);
    assert!(
        result["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("did not finish")
    );
    // No grant and no rule: a history's permission answer is history, not a setting of Pi's.
    for forbidden in ["allow_always", "permission", "updatedPermissions"] {
        assert!(!imported.text.contains(forbidden), "{forbidden}");
    }
    // Nothing the history names is opened: the file's own directory and id are the host's.
    let hostile = {
        let mut doc = Document::new(Header::new(ID, Origin::new("pi")));
        doc.header.cwd = Some("/etc".into());
        doc.header.origin.session_id = Some("../../x".into());
        doc.lines.push(Line::text_prompt("hi"));
        doc.lines
            .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
        doc
    };
    let imported = import(&hostile, "/w", ID, &ImportOptions::default()).unwrap();
    let head = &values(&imported.text)[0];
    assert_eq!(head["cwd"], "/w");
    assert_eq!(head["id"], ID);
    assert!(!imported.text.contains("../../x"));
    assert!(!imported.text.contains("/etc"));
    assert!(import(&hostile, "/w", "../../x", &ImportOptions::default()).is_err());
}

#[test]
fn a_failed_or_stopped_turn_is_written_as_pi_writes_it() {
    let mut doc = Document::new(Header::new(ID, Origin::new("claude")));
    doc.lines.push(Line::text_prompt("one"));
    doc.lines.push(Line::new(Body::TurnEnd {
        stop: Stop::Error,
        error: Some("the API said no".into()),
    }));
    doc.lines.push(Line::text_prompt("two"));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "partial"}}),
    ));
    doc.lines.push(Line::new(Body::Cancel));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::Cancelled)));
    let imported = import(&doc, "/w", ID, &ImportOptions::default()).unwrap();
    let lines = values(&imported.text);
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    // The failure is an empty assistant message that Pi's own edit hides from the model, as Pi writes one.
    assert_eq!(
        kinds,
        [
            "session",
            "message",
            "message",
            "context_edit",
            "message",
            "message"
        ]
    );
    assert_eq!(lines[2]["message"]["stopReason"], "error");
    assert_eq!(lines[2]["message"]["errorMessage"], "the API said no");
    assert_eq!(lines[2]["message"]["content"], json!([]));
    assert_eq!(lines[3]["targetId"], lines[2]["id"]);
    assert_eq!(lines[3]["replacement"], Value::Null);
    assert_eq!(lines[5]["message"]["stopReason"], "aborted");
    // And a history of it, read back, has the same two turns.
    let back = export(&imported.text, ID).unwrap();
    // A history with no times is written with the time every entry needs, and reads back with it.
    assert_eq!(untimed(&back), untimed(&doc));
}

#[test]
fn a_session_is_written_where_pi_finds_it_and_never_over_one() {
    let scratch = Scratch::new("history-files");
    let agent = scratch.dir("agent");
    let cwd = if cfg!(windows) {
        std::path::PathBuf::from("C:\\work\\project")
    } else {
        std::path::PathBuf::from("/work/project")
    };
    let directory = coqui_acp_pi::paths::session_directory(&agent, &cwd);
    let name = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        name,
        if cfg!(windows) {
            "--C--work-project--"
        } else {
            "--work-project--"
        }
    );
    assert_eq!(directory.parent().unwrap(), agent.join("sessions"));
    let mut doc = Document::new(Header::new(ID, Origin::new("pi")));
    doc.lines.push(Line::text_prompt("hello"));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}),
    ));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    let imported = import_file(&directory, &cwd, ID, &doc, &ImportOptions::default()).unwrap();
    let path = directory.join(imported.file_name(ID));
    assert!(path.is_file());
    assert_eq!(find(&directory, ID).unwrap(), Some(path.clone()));
    assert_eq!(
        find(&directory, "6a1c0f52-0000-4000-8000-000000000000").unwrap(),
        None
    );
    // Read back by the id in its header, whatever the file is called.
    let back = export_file(&directory, ID, 1 << 20).unwrap();
    assert_eq!(untimed(&back), untimed(&doc));
    // Never over a session that is there, and never a file for an id that is no UUID.
    let before = std::fs::read(&path).unwrap();
    let again = import_file(&directory, &cwd, ID, &doc, &ImportOptions::default());
    assert!(again.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        import_file(
            &directory,
            &cwd,
            "not-a-uuid",
            &doc,
            &ImportOptions::default()
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    // A bound on what is read.
    assert!(export_file(&directory, ID, 100).is_err());
    // A file in another place names the same id: found by its header and not by the name it has.
    std::fs::rename(&path, directory.join("renamed.jsonl")).unwrap();
    assert!(find(&directory, ID).unwrap().is_some());
    // A session of no turns is a header, which exports as a history with nothing in it.
    let empty = Document::new(Header::new(
        "7d3c9b1e-2a4f-4c6d-8e0a-1b3d5f7a9c2e",
        Origin::new("pi"),
    ));
    import_file(
        &directory,
        &cwd,
        "7d3c9b1e-2a4f-4c6d-8e0a-1b3d5f7a9c2e",
        &empty,
        &ImportOptions::default(),
    )
    .unwrap();
    let none = export_file(&directory, "7d3c9b1e-2a4f-4c6d-8e0a-1b3d5f7a9c2e", 1 << 20).unwrap();
    assert!(none.lines.is_empty());
}

#[test]
fn a_file_that_is_no_session_or_is_newer_than_this_reads_is_refused() {
    assert!(export("not json\n", ID).is_err());
    assert!(export("{\"type\":\"message\"}\n", ID).is_err());
    let newer = "{\"type\":\"session\",\"version\":99,\"id\":\"x\",\"timestamp\":\"2026-11-10T09:00:00.000Z\",\"cwd\":\"/w\"}\n";
    let error = export(newer, ID).unwrap_err().to_string();
    assert!(error.contains("version 99"), "{error}");
    // A file of the first version has no ids to chain: its entries are read in the order of the file.
    let old = concat!(
        "{\"type\":\"session\",\"id\":\"x\",\"timestamp\":\"2026-11-10T09:00:00.000Z\",\"cwd\":\"/w\"}\n",
        "{\"type\":\"message\",\"timestamp\":\"2026-11-10T09:00:01.000Z\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        "{\"type\":\"message\",\"timestamp\":\"2026-11-10T09:00:02.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"stopReason\":\"stop\"}}\n",
    );
    let doc = export(old, ID).unwrap();
    assert_eq!(count(&doc, |b| matches!(b, Body::Prompt { .. })), 1);
    assert!(doc.to_string().unwrap().contains("\"hi\""));
}

#[test]
fn a_claude_session_goes_to_pi_and_back_with_what_pi_cannot_take_reported() {
    let transcript = claude_fixture("turn.transcript.jsonl");
    let first = coqui_acp_claude::history::export(&transcript, ID).unwrap();
    let imported = import(&first, "C:/coqui-fx/w", ID, &ImportOptions::default()).unwrap();
    // The signed thinking of Claude Code is not Pi's to send, and TodoWrite is a tool Pi has not.
    assert_eq!(
        imported.report.count(THINKING),
        1,
        "{:?}",
        loss(&imported.report)
    );
    assert_eq!(imported.report.count(TOOL_AS_NOTE), 1);
    let lines = values(&imported.text);
    let calls: Vec<Value> = lines
        .iter()
        .flat_map(|l| {
            l["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "toolCall")
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    let names: Vec<&str> = calls.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["read", "edit", "bash", "write", "grep"]);
    assert_eq!(
        calls[1]["arguments"],
        json!({"path": "C:/coqui-fx/w/notes.txt", "edits": [{"oldText": "two", "newText": "three"}]})
    );
    assert_eq!(
        calls[2]["arguments"]["command"],
        "echo checked > checked.txt"
    );
    assert!(imported.text.contains("[Tool call: TodoWrite"));
    // The refusal of the bash call came back as an error result.
    let refused = lines
        .iter()
        .find(|l| l["message"]["toolName"] == "bash")
        .unwrap();
    assert_eq!(refused["message"]["isError"], true);

    // And through Pi's file and back to Claude Code, the tools are Claude Code's own again.
    let doc = export(&imported.text, ID).unwrap();
    assert_eq!(doc.header.origin.harness, "pi");
    assert_eq!(count(&doc, |b| matches!(b, Body::Prompt { .. })), 2);
    let back = coqui_acp_claude::history::import(
        &doc,
        "C:/coqui-fx/w",
        ID,
        &coqui_acp_claude::history::ImportOptions::default(),
    )
    .unwrap();
    let back_lines = values(&back.text);
    let tools: Vec<&str> = back_lines
        .iter()
        .filter_map(|l| l["message"]["content"][0]["name"].as_str())
        .collect();
    assert_eq!(tools, ["Read", "Edit", "Bash", "Write", "Grep"]);
    let edit = back_lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Edit")
        .unwrap();
    assert_eq!(
        edit["message"]["content"][0]["input"],
        json!({"file_path": "C:/coqui-fx/w/notes.txt", "old_string": "two", "new_string": "three"})
    );
    let bash = back_lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Bash")
        .unwrap();
    assert_eq!(
        bash["message"]["content"][0]["input"],
        json!({"command": "echo checked > checked.txt"})
    );
    assert!(back.text.contains("Second turn answered."));
    assert!(back.text.contains("[Tool call: TodoWrite"));
    // The two prompts of the person survive both ways.
    let prompts = back_lines
        .iter()
        .filter(|l| l["type"] == "user" && l["message"]["content"][0]["text"] == "And now?")
        .count();
    assert_eq!(prompts, 1);
}
