//! Codex's rollout as a standard history, and back: the recorded sessions round-trip with nothing
//! lost, a compaction keeps its summary, the import keeps the rules for hostile histories, and a
//! session moves between Claude Code and Codex with the loss report the tool table says.
#![cfg(feature = "history")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_codex::history::{ImportOptions, export, export_file, import, import_file};
use coqui_acp_history::{
    Body, Document, Header, Line, Origin, Stop, StopReason, THINKING, TOOL_AS_NOTE,
};
use serde_json::{Value, json};
use support::Scratch;

const ID: &str = "0b2e9a4c-7a35-4d2f-9f0e-0f4f3f1f2a11";

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codex-0.166.0")
        .join(name);
    std::fs::read_to_string(path).unwrap()
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

/// What the model is given by a rollout of what the person and the agent did: the messages, the
/// reasoning and the calls with their outputs, in order, without what Codex adds by itself (its
/// instructions, the environment context, the tool list) and without ids and times.
fn conversation(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in values(text) {
        if line["type"] != "response_item" {
            continue;
        }
        let item = &line["payload"];
        match item["type"].as_str().unwrap_or_default() {
            "message" if item["role"] == "assistant" => out.push(json!({"assistant": item["content"]})),
            "message" if item["role"] == "user" => {
                let text = item["content"][0]["text"].as_str().unwrap_or_default();
                if !text.starts_with('<') && !text.starts_with("# AGENTS") {
                    out.push(json!({"user": item["content"]}));
                }
            }
            "reasoning" => out.push(json!({"reasoning": item["summary"], "sealed": item["encrypted_content"]})),
            "function_call" => out.push(json!({"call": item["name"], "id": item["call_id"], "arguments": item["arguments"]})),
            "custom_tool_call" => out.push(json!({"custom": item["name"], "id": item["call_id"], "input": item["input"]})),
            "function_call_output" | "custom_tool_call_output" => {
                out.push(json!({"output": item["call_id"], "value": item["output"]}));
            }
            _ => {}
        }
    }
    out
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

fn count(doc: &Document, method: &str) -> usize {
    doc.lines
        .iter()
        .filter(|line| line.body.method() == method)
        .count()
}

#[test]
fn a_recorded_rollout_exports_as_a_history_without_codexs_own_context() {
    let doc = export(&fixture("turn.rollout.jsonl"), ID).unwrap();
    assert_eq!(doc.header.id, ID);
    assert_eq!(doc.header.origin.harness, "codex");
    assert_eq!(doc.header.origin.version.as_deref(), Some("0.166.0"));
    assert_eq!(doc.header.cwd.as_deref(), Some("C:\\coqui-fx\\w"));
    assert_eq!(doc.header.model.as_deref(), Some("gpt-6-luna"));
    doc.check().unwrap();
    assert_eq!(count(&doc, "session/prompt"), 2);
    assert_eq!(count(&doc, "history/turn_end"), 2);
    // The instructions and the environment Codex added are no prompt of the person's.
    let prompts: Vec<String> = doc
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Prompt { prompt } => Some(prompt[0]["text"].as_str().unwrap().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["Change two to three in notes.txt.", "And now?"]);
    // Five calls of the script tool, each with its input as Codex has it and its output.
    let calls = updates(&doc, "tool_call");
    assert_eq!(calls.len(), 5);
    assert!(
        calls
            .iter()
            .all(|call| call["name"] == "exec" && call["kind"] == "execute")
    );
    assert!(
        calls[3]["rawInput"]["input"]
            .as_str()
            .unwrap()
            .contains("tools.apply_patch")
    );
    let results = updates(&doc, "tool_call_update");
    assert_eq!(results.len(), 5);
    assert_eq!(
        results[0]["status"], "failed",
        "a script that failed says so"
    );
    assert_eq!(results[1]["status"], "completed");
    assert!(results[1]["rawOutput"].is_array());
    let reasoning = updates(&doc, "agent_thought_chunk");
    assert_eq!(reasoning[0]["content"]["text"], "Plan first, then edit.");
}

#[test]
fn a_recorded_session_goes_from_codex_to_codex_with_nothing_lost() {
    for name in ["turn.rollout.jsonl", "classic.rollout.jsonl"] {
        let original = fixture(name);
        let doc = export(&original, ID).unwrap();
        let imported = import(&doc, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
        assert!(imported.report.is_empty(), "{name}: {:?}", imported.report);
        assert_eq!(
            conversation(&imported.text),
            conversation(&original),
            "{name}"
        );
        let again = export(&imported.text, ID).unwrap();
        assert_eq!(again.lines.len(), doc.lines.len(), "{name}");
        for (at, (a, b)) in again.lines.iter().zip(&doc.lines).enumerate() {
            assert_eq!(a, b, "{name}: line {at}");
        }
        // The thread is named by the host's id, and the file is where Codex looks.
        let lines = values(&imported.text);
        assert_eq!(lines[0]["type"], "session_meta");
        assert_eq!(lines[0]["payload"]["id"], ID);
        assert!(imported.path.ends_with(&format!("-{ID}.jsonl")));
    }
}

#[test]
fn a_classic_tool_call_keeps_its_kind_and_its_paths() {
    let doc = export(&fixture("classic.rollout.jsonl"), ID).unwrap();
    let calls = updates(&doc, "tool_call");
    assert_eq!(calls[0]["name"], "shell_command");
    assert_eq!(calls[0]["kind"], "execute");
    assert_eq!(
        calls[0]["rawInput"],
        json!({"command": "echo checked", "workdir": "C:/coqui-fx/w"})
    );
    assert_eq!(calls[1]["name"], "apply_patch");
    assert_eq!(calls[1]["kind"], "edit");
    assert_eq!(calls[1]["locations"], json!([{"path": "notes.txt"}]));
}

#[test]
fn a_compaction_keeps_its_summary_and_seeds_the_session() {
    let original = fixture("compact.rollout.jsonl");
    let doc = export(&original, ID).unwrap();
    let boundary = doc
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::Compaction { summary, .. } => summary.clone(),
            _ => None,
        })
        .expect("the compaction has its summary");
    assert_eq!(boundary, "Summary of the work so far.");
    let imported = import(&doc, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
    let lines = values(&imported.text);
    assert_eq!(lines[1]["type"], "compacted");
    assert!(
        lines[1]["payload"]["message"]
            .as_str()
            .unwrap()
            .ends_with("Summary of the work so far.")
    );
    // What came before the boundary is the summary now.
    assert!(!imported.text.contains("Say something."));
    assert!(imported.text.contains("And after?"));
    assert!(imported.seeded_from.as_ref().unwrap().line > 2);
    let options = ImportOptions::default().use_summary(false);
    let whole = import(&doc, "C:\\coqui-fx\\w", ID, &options).unwrap();
    assert!(whole.text.contains("Say something."));
    assert!(!whole.text.contains("compacted"));
}

fn one_turn(origin: &str) -> Document {
    let mut doc = Document::new(Header::new(ID, Origin::new(origin)));
    doc.lines.push(Line::text_prompt("Do it."));
    doc.lines.push(Line::update(
        json!({"sessionUpdate": "tool_call", "toolCallId": "../t1", "name": "Bash",
        "title": "ls", "kind": "execute", "status": "in_progress", "rawInput": {"command": "ls"}}),
    ));
    doc.lines.push(Line::new(Body::PermissionRequest {
        tool_call: json!({"toolCallId": "../t1"}),
        options: vec![
            json!({"optionId": "allow_always", "name": "Always", "kind": "allow_always"}),
        ],
    }));
    doc.lines.push(Line::new(Body::PermissionAnswer {
        tool_call_id: "../t1".into(),
        outcome: json!({"outcome": "selected", "optionId": "allow_always"}),
    }));
    doc
}

#[test]
fn an_import_ends_what_the_history_left_open_and_grants_nothing() {
    let imported = import(&one_turn("claude"), "/w", ID, &ImportOptions::default()).unwrap();
    assert!(imported.closed_turn);
    assert_eq!(imported.closed_calls, 1);
    let lines = values(&imported.text);
    let call = lines
        .iter()
        .find(|l| l["payload"]["type"] == "function_call")
        .unwrap();
    assert_eq!(call["payload"]["name"], "shell_command");
    let id = call["payload"]["call_id"].as_str().unwrap();
    assert_eq!(id, "___t1", "a call id is made one Codex takes");
    let output = lines
        .iter()
        .find(|l| l["payload"]["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["payload"]["call_id"], id);
    assert!(
        output["payload"]["output"]
            .as_str()
            .unwrap()
            .contains("did not finish")
    );
    assert_eq!(lines.last().unwrap()["payload"]["type"], "turn_aborted");
    for forbidden in ["allow_always", "approval", "execpolicy", "permission"] {
        assert!(!imported.text.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn a_thread_is_written_where_resume_finds_it_and_never_over_one() {
    let doc = export(&fixture("turn.rollout.jsonl"), ID).unwrap();
    let scratch = Scratch::new("history-files");
    let home = scratch.0.join("home");
    let cwd = scratch.0.join("work");
    let imported = import_file(&home, &cwd, ID, &doc, &ImportOptions::default()).unwrap();
    let path = coqui_acp_codex::history::rollout(&home, ID).unwrap();
    assert!(path.ends_with(imported.path.replace('/', std::path::MAIN_SEPARATOR_STR)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), imported.text);
    assert!(
        import_file(&home, &cwd, ID, &doc, &ImportOptions::default()).is_err(),
        "an import never overwrites"
    );
    let back = export_file(&home, ID, 1 << 22).unwrap();
    assert_eq!(back.lines, doc.lines);
    assert!(
        export_file(&home, ID, 100).is_err(),
        "a rollout past the bound is refused"
    );
    assert!(coqui_acp_codex::history::rollout(&home, "../../x").is_err());
}

// ------------------------------------------------------------------------------------------
// Between Claude Code and Codex
// ------------------------------------------------------------------------------------------

fn loss(report: &coqui_acp_history::LossReport) -> Vec<(String, u64)> {
    report
        .losses()
        .iter()
        .map(|l| (l.kind.clone(), l.count))
        .collect()
}

#[test]
fn a_claude_session_goes_to_codex_with_what_codex_cannot_take_reported() {
    let transcript = claude_fixture("turn.transcript.jsonl");
    let doc = coqui_acp_claude::history::export(&transcript, ID).unwrap();
    let imported = import(&doc, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
    // The thinking is signed for Claude only; Read, Grep and the todo list have no Codex tool in the
    // table; Bash is Codex's shell and Write and Edit are its file changes.
    assert_eq!(
        loss(&imported.report),
        [(THINKING.to_owned(), 1), (TOOL_AS_NOTE.to_owned(), 3)]
    );
    let lines = values(&imported.text);
    let shell: Vec<&Value> = lines
        .iter()
        .filter(|l| l["payload"]["name"] == "shell_command")
        .collect();
    assert_eq!(shell.len(), 1);
    let arguments: Value =
        serde_json::from_str(shell[0]["payload"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(arguments, json!({"command": "echo checked > checked.txt"}));
    // The edit and the write are patches.
    assert!(
        imported
            .text
            .contains("*** Update File: C:/coqui-fx/w/notes.txt")
    );
    assert!(
        imported
            .text
            .contains("*** Add File: C:/coqui-fx/w/new.txt")
    );
    // The notes say what was called and what came of it.
    assert!(imported.text.contains("[Tool call: Read"));
    assert!(imported.text.contains("[Tool call: Grep"));
    assert!(
        imported
            .text
            .contains("The file C:/coqui-fx/w/notes.txt has been updated successfully.")
    );
    assert!(imported.text.contains("Second turn answered."));
    // Codex's own reading of the result is a history again.
    let back = export(&imported.text, ID).unwrap();
    assert_eq!(count(&back, "session/prompt"), 2);
    assert_eq!(count(&back, "history/turn_end"), 2);
}

#[test]
fn and_back_to_claude_with_the_notes_as_text_and_the_shell_as_bash() {
    let transcript = claude_fixture("turn.transcript.jsonl");
    let first = coqui_acp_claude::history::export(&transcript, ID).unwrap();
    let rollout = import(&first, "C:\\coqui-fx\\w", ID, &ImportOptions::default()).unwrap();
    let doc = export(&rollout.text, ID).unwrap();
    assert_eq!(doc.header.origin.harness, "codex");
    let back = coqui_acp_claude::history::import(
        &doc,
        "C:\\coqui-fx\\w",
        ID,
        &coqui_acp_claude::history::ImportOptions::default(),
    )
    .unwrap();
    // Nothing is left to report: the notes are text now, and the shell has its Bash.
    assert!(back.report.is_empty(), "{:?}", loss(&back.report));
    let lines = values(&back.text);
    let bash: Vec<&Value> = lines
        .iter()
        .filter(|l| l["message"]["content"][0]["name"] == "Bash")
        .collect();
    assert_eq!(bash.len(), 1);
    assert_eq!(
        bash[0]["message"]["content"][0]["input"],
        json!({"command": "echo checked > checked.txt"})
    );
    assert!(back.text.contains("[Tool call: Read"));
    let edit = lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Edit")
        .unwrap();
    assert_eq!(
        edit["message"]["content"][0]["input"],
        json!({"file_path": "C:/coqui-fx/w/notes.txt", "old_string": "two", "new_string": "three"})
    );
    let write = lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Write")
        .unwrap();
    assert_eq!(
        write["message"]["content"][0]["input"]["content"],
        "fresh\n"
    );
    // The two prompts of the person survive both ways.
    let prompts: Vec<&Value> = lines
        .iter()
        .filter(|l| l["type"] == "user" && l["message"]["content"][0]["text"] == "And now?")
        .collect();
    assert_eq!(prompts.len(), 1);
}

#[test]
fn a_codex_session_goes_to_claude_with_what_claude_cannot_take_reported() {
    let doc = export(&fixture("turn.rollout.jsonl"), ID).unwrap();
    let options = coqui_acp_claude::history::ImportOptions::default();
    let imported =
        coqui_acp_claude::history::import(&doc, "C:\\coqui-fx\\w", ID, &options).unwrap();
    // The reasoning has no signature, and the script tool has no Claude Code equivalent.
    assert_eq!(
        loss(&imported.report),
        [(THINKING.to_owned(), 1), (TOOL_AS_NOTE.to_owned(), 5)]
    );
    assert!(imported.text.contains("[Tool call: exec"));
    assert!(imported.text.contains("Second turn answered."));
    let classic = export(&fixture("classic.rollout.jsonl"), ID).unwrap();
    let imported =
        coqui_acp_claude::history::import(&classic, "C:\\coqui-fx\\w", ID, &options).unwrap();
    // The shell becomes Bash and the patch an Edit.
    assert_eq!(loss(&imported.report), [(THINKING.to_owned(), 1)]);
    let lines = values(&imported.text);
    let bash = lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Bash")
        .unwrap();
    assert_eq!(
        bash["message"]["content"][0]["input"]["command"],
        "echo checked"
    );
    let edit = lines
        .iter()
        .find(|l| l["message"]["content"][0]["name"] == "Edit")
        .unwrap();
    assert_eq!(
        edit["message"]["content"][0]["input"]["old_string"],
        "one\ntwo"
    );
    assert_eq!(
        edit["message"]["content"][0]["input"]["new_string"],
        "one\nthree"
    );
    // A stop reason or two for the writer to handle.
    assert!(matches!(
        doc.lines.last().unwrap().body,
        Body::TurnEnd {
            stop: Stop::Acp(StopReason::EndTurn),
            ..
        }
    ));
}
