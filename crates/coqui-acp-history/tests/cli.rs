//! The command: an export is left unchanged by default, and redaction happens only when asked.

#![cfg(feature = "cli")]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use coqui_acp_history::{Document, Line};
use support::document;

const SECRET: &str = "sk-live-4242424242";

fn command(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_coqui-acp-history"))
        .args(args)
        .output()
        .expect("the command runs")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coqui-acp-history-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// Another producer's spelling: spaces and any key order, which the canonical form would change.
fn foreign() -> String {
    let prompt = format!(
        r#"{{ "params": {{ "prompt": [ {{ "text": "key {SECRET}", "type": "text" }} ] }}, "method": "session/prompt" }}"#
    );
    format!(
        "{{ \"version\": 1, \"format\": \"coqui-acp-history\", \"id\": \"5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77\", \"origin\": {{ \"harness\": \"codex\" }} }}\n{prompt}\n{{ \"params\": {{ \"stopReason\": \"end_turn\" }}, \"method\": \"history/turn_end\" }}\n"
    )
}

#[test]
fn convert_leaves_a_history_byte_for_byte_as_it_was() {
    let dir = scratch("unaltered");
    let input = dir.join("in.jsonl");
    let output = dir.join("out.jsonl");
    std::fs::write(&input, foreign()).unwrap();
    let result = command(&["convert", path(&input), "-o", path(&output)]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&output).unwrap(), foreign().as_bytes());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn convert_refuses_a_history_the_format_refuses_and_writes_nothing() {
    let dir = scratch("refused");
    let input = dir.join("in.jsonl");
    let output = dir.join("out.jsonl");
    std::fs::write(
        &input,
        foreign().replace("history/turn_end", "history/unknown"),
    )
    .unwrap();
    let result = command(&["convert", path(&input), "-o", path(&output)]);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("line 3"), "{stderr}");
    assert!(!stderr.contains(SECRET));
    assert!(!output.exists());
    assert!(!dir.join("out.jsonl.part").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn convert_removes_what_it_is_asked_to_and_reports_without_repeating_it() {
    let dir = scratch("redacted");
    let input = dir.join("in.jsonl");
    let output = dir.join("out.jsonl");
    let values = dir.join("values.txt");
    std::fs::write(&input, foreign()).unwrap();
    std::fs::write(&values, format!("{SECRET}\n")).unwrap();
    let result = command(&[
        "convert",
        path(&input),
        "-o",
        path(&output),
        "--redact-values-file",
        path(&values),
        "--seal",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("redacted 1 removals in 1 lines"),
        "{stderr}"
    );
    assert!(!stderr.contains(SECRET));
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(!text.contains(SECRET));
    let doc: Document = text.parse().unwrap();
    assert_eq!(doc.header.redacted.unwrap().replaced, 1);
    assert!(
        doc.header.digest.is_some(),
        "sealed, so the digest was checked on reading"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn show_and_list_describe_a_history() {
    let dir = scratch("show");
    let mut doc = document(2);
    doc.lines.push(Line::compaction("sum"));
    std::fs::write(dir.join("a.jsonl"), doc.to_string().unwrap()).unwrap();
    std::fs::write(dir.join("b.jsonl"), "not a history\n").unwrap();

    let shown = command(&["show", path(&dir.join("a.jsonl"))]);
    assert!(shown.status.success());
    let out = String::from_utf8_lossy(&shown.stdout);
    assert!(out.contains("2 turns"), "{out}");
    assert!(out.contains("a compaction summary"), "{out}");

    let listed = command(&["list", path(&dir)]);
    let out = String::from_utf8_lossy(&listed.stdout);
    assert!(
        out.contains("5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77  claude"),
        "{out}"
    );
    assert!(out.contains("not a history (syntax"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_limit_looser_than_the_default_is_reported() {
    let dir = scratch("limits");
    std::fs::write(dir.join("a.jsonl"), document(1).to_string().unwrap()).unwrap();
    let shown = command(&[
        "show",
        path(&dir.join("a.jsonl")),
        "--max-lines",
        "99999999",
    ]);
    assert!(
        String::from_utf8_lossy(&shown.stderr).contains("setting bounds.max_lines is 99999999")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn export_and_import_say_they_need_a_codec() {
    for args in [
        ["export", "--harness", "claude", "session"],
        ["import", "--harness", "codex", "file"],
    ] {
        let out = command(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("no codec"));
    }
}
