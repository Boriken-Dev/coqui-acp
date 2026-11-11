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
fn export_and_import_say_which_command_does_it() {
    for args in [
        ["export", "--harness", "claude", "session"],
        ["import", "--harness", "codex", "file"],
    ] {
        let out = command(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("history export"));
    }
}

/// Characters a terminal acts on: none of them may reach standard output as they are.
const ACTED_ON: &[char] = &[
    '\u{1b}', '\u{07}', '\r', '\u{85}', '\u{9b}', '\u{7f}', '\u{2028}', '\u{2029}', '\u{202e}',
    '\u{2066}', '\u{200f}', '\u{61c}',
];

fn assert_inert(shown: &str) {
    for c in ACTED_ON {
        assert!(
            !shown.contains(*c),
            "{:?} reached the terminal: {shown:?}",
            c
        );
    }
    assert_eq!(
        shown
            .chars()
            .filter(|c| c.is_control() && *c != '\n')
            .count(),
        0,
        "{shown:?}"
    );
}

#[test]
fn show_prints_what_a_history_says_in_a_form_a_terminal_cannot_act_on() {
    let dir = scratch("escaped-show");
    let hostile = "\u{1b}]52;c;QUJD\u{7}\u{1b}[2K\rfake\u{85}\u{9b}31m\u{7f}a\u{2028}b\u{2029}c\u{202e}d\u{2066}e\u{200f}\u{61c}";
    let mut doc = document(0);
    doc.lines.push(Line::text_prompt(hostile));
    doc.lines
        .push(Line::turn_end(coqui_acp_history::Stop::Error));
    doc.header.model = Some("m\u{202e}odel\u{2028}x".into());
    doc.header.cwd = Some("/w\u{2029}d\u{200f}".into());
    doc.header.origin.version = Some("1\u{2066}.0".into());
    doc.header.origin.session_id = Some("native\u{61c}id".into());
    std::fs::write(dir.join("a.jsonl"), doc.to_string().unwrap()).unwrap();

    let shown = command(&["show", path(&dir.join("a.jsonl"))]);
    assert!(shown.status.success());
    let out = String::from_utf8(shown.stdout).unwrap();
    assert_inert(&out);
    assert!(out.contains(r"\u{1b}]52;c;QUJD\u{7}"), "{out}");
    assert!(out.contains(r"m\u{202e}odel"), "{out}");

    let listed = command(&["list", path(&dir)]);
    let out = String::from_utf8(listed.stdout).unwrap();
    assert_inert(&out);
    assert_eq!(out.lines().count(), 1, "one row for one file: {out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn list_escapes_a_file_name_too() {
    let dir = scratch("escaped-name");
    let name = "a\u{1b}[2Kb\nforged.jsonl";
    std::fs::write(dir.join(name), document(1).to_string().unwrap()).unwrap();
    let listed = command(&["list", path(&dir)]);
    let out = String::from_utf8(listed.stdout).unwrap();
    assert_inert(&out);
    assert_eq!(out.lines().count(), 1, "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_header_with_a_newline_in_it_is_one_row_of_a_list() {
    let dir = scratch("forged-row");
    let mut text = document(1).to_string().unwrap();
    text = text.replacen(
        r#""origin""#,
        r#""cwd":"/w\nffffffff-ffff-ffff-ffff-ffffffffffff  claude  -  /x  forged.jsonl","origin""#,
        1,
    );
    std::fs::write(dir.join("a.jsonl"), text).unwrap();
    let listed = command(&["list", path(&dir)]);
    let out = String::from_utf8(listed.stdout).unwrap();
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(!out.contains("ffffffff-ffff"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn convert_with_a_redaction_that_finds_nothing_leaves_the_file_as_it_was() {
    let dir = scratch("nothing-found");
    let input = dir.join("in.jsonl");
    let output = dir.join("out.jsonl");
    let values = dir.join("values.txt");
    std::fs::write(&input, foreign()).unwrap();
    std::fs::write(&values, "not-in-the-file\n").unwrap();
    let result = command(&[
        "convert",
        path(&input),
        "-o",
        path(&output),
        "--redact-values-file",
        path(&values),
    ]);
    assert!(result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), foreign().as_bytes());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn convert_seal_alone_says_it_rewrites_the_file_in_canonical_spelling() {
    let help = command(&["convert", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    assert!(
        !help.contains("unaltered unless redaction is asked"),
        "{help}"
    );
    assert!(help.contains("canonical"), "{help}");
}

#[test]
fn convert_writes_through_no_name_it_can_guess_and_leaves_nothing_behind() {
    let dir = scratch("temporary");
    let input = dir.join("in.jsonl");
    let output = dir.join("out.jsonl");
    std::fs::write(&input, foreign()).unwrap();

    // What an attacker who can write in the directory would plant.
    let planted = dir.join("out.jsonl.part");
    std::fs::write(&planted, "planted").unwrap();
    #[cfg(unix)]
    {
        std::fs::remove_file(&planted).unwrap();
        let target = dir.join("victim");
        std::fs::write(&target, "victim").unwrap();
        std::os::unix::fs::symlink(&target, &planted).unwrap();
    }

    let result = command(&["convert", path(&input), "-o", path(&output)]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&output).unwrap(), foreign().as_bytes());
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_to_string(dir.join("victim")).unwrap(),
        "victim"
    );
    #[cfg(not(unix))]
    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "planted");
    std::fs::remove_file(&planted).unwrap();
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // A refused history leaves none either.
    std::fs::write(
        &input,
        foreign().replace("history/turn_end", "history/unknown"),
    )
    .unwrap();
    let refused = command(&[
        "convert",
        path(&input),
        "-o",
        path(&dir.join("never.jsonl")),
    ]);
    assert!(!refused.status.success());
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".part") || name == "never.jsonl")
        .collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
