//! Redaction: only when asked, and it says what it removed without repeating it.

mod support;

use serde_json::json;

use coqui_acp_history::{Document, Line, RedactionSettings, Redactor, RuleKind, Stop, StopReason};
use support::document;

const SECRET: &str = "sk-live-4242424242";

fn with_secret() -> Document {
    let mut doc = document(1);
    doc.header.cwd = Some(format!("/home/{SECRET}/work"));
    doc.lines.insert(1, Line::update(json!({
        "sessionUpdate": "tool_call", "toolCallId": SECRET, "kind": "execute",
        "rawInput": {"command": format!("curl -H 'Authorization: {SECRET}' https://x"), "env": [SECRET, "plain"]},
    })));
    doc.lines.insert(
        0,
        Line::text_prompt(&format!("my key is {SECRET} and again {SECRET}")),
    );
    doc.lines
        .insert(1, Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    doc
}

fn values(values: &[&str]) -> RedactionSettings {
    let mut settings = RedactionSettings::default();
    settings.values = values.iter().map(|v| (*v).to_owned()).collect();
    settings
}

#[test]
fn nothing_is_redacted_unless_a_redactor_is_asked_to() {
    let doc = with_secret();
    let text = doc.to_string().unwrap();
    assert_eq!(
        text.matches(SECRET).count(),
        6,
        "reading and writing alone leave every value in"
    );
    let again: Document = text.parse().unwrap();
    assert_eq!(again.to_string().unwrap(), text);
    assert!(again.header.redacted.is_none());
}

#[test]
fn a_value_is_removed_everywhere_but_from_identifiers_and_reported() {
    let mut doc = with_secret();
    let mut redactor = Redactor::try_new(&values(&[SECRET])).unwrap();
    let report = redactor.document(&mut doc).unwrap();
    let text = doc.to_string().unwrap();

    // The tool call id pairs lines and is kept: that one occurrence is left.
    assert_eq!(text.matches(SECRET).count(), 1);
    assert!(text.contains(r#""toolCallId":"sk-live-4242424242""#));
    assert!(text.contains("my key is [redacted] and again [redacted]"));
    assert!(text.contains("/home/[redacted]/work"));

    assert_eq!(report.replaced, 5);
    assert_eq!(report.rules.len(), 1);
    assert_eq!(report.rules[0].kind, RuleKind::Value);
    assert_eq!(report.rules[0].index, 0);
    assert_eq!(report.rules[0].replaced, 5);
    assert_eq!(report.lines, 3, "the header and two lines changed");
    assert_eq!(doc.header.redacted.unwrap().replaced, 5);
    assert_eq!(doc.header.redacted.unwrap().rules, 1);
    assert!(doc.header.digest.is_none());
}

#[test]
fn the_report_names_rules_by_position_and_never_by_text() {
    let mut doc = with_secret();
    let mut settings = values(&[SECRET]);
    settings.patterns = vec!["sk-[a-z]+-\\d+".into()];
    let mut redactor = Redactor::try_new(&settings).unwrap();
    let report = redactor.document(&mut doc).unwrap();
    for rendering in [
        format!("{report:?}"),
        serde_json::to_string(&report).unwrap(),
        format!("{redactor:?}"),
    ] {
        assert!(!rendering.contains(SECRET), "{rendering}");
        assert!(!rendering.contains("sk-["), "{rendering}");
    }
    assert_eq!(report.rules.len(), 2);
}

#[test]
fn a_pattern_removes_what_it_matches_and_the_replacement_is_not_expanded() {
    let mut doc = with_secret();
    let mut settings = RedactionSettings::default();
    settings.patterns = vec!["sk-(?<kind>[a-z]+)-\\d+".into()];
    settings.replacement = "<$kind>".into();
    let mut redactor = Redactor::try_new(&settings).unwrap();
    redactor.document(&mut doc).unwrap();
    let text = doc.to_string().unwrap();
    assert!(text.contains("my key is <$kind> and again <$kind>"));
}

#[test]
fn a_redacted_document_can_be_sealed_and_read_back() {
    let mut doc = with_secret();
    Redactor::try_new(&values(&[SECRET]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    doc.seal().unwrap();
    let read: Document = doc.to_string().unwrap().parse().unwrap();
    assert_eq!(read, doc);
    assert_eq!(read.header.redacted.unwrap().replaced, 5);
}

#[test]
fn a_redactor_with_nothing_to_remove_removes_nothing_and_says_so() {
    let mut doc = with_secret();
    let before = doc.lines.clone();
    let report = Redactor::try_new(&RedactionSettings::default())
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert_eq!(report.replaced, 0);
    assert_eq!(doc.lines, before);
    assert!(
        doc.header.redacted.is_none(),
        "nothing changed, so nothing is marked"
    );
}

#[test]
fn an_empty_value_removes_nothing() {
    let mut doc = with_secret();
    let report = Redactor::try_new(&values(&[""]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert_eq!(report.replaced, 0);
    assert!(report.rules.is_empty());
}

#[test]
fn a_pattern_that_is_not_usable_is_refused_by_position() {
    let mut settings = RedactionSettings::default();
    settings.patterns = vec!["fine".into(), "(unclosed".into()];
    let error = Redactor::try_new(&settings).unwrap_err();
    assert_eq!(error.code(), "pattern");
    assert!(error.to_string().contains('1'));

    settings.patterns = vec!["a*".into()];
    assert_eq!(
        Redactor::try_new(&settings).unwrap_err().code(),
        "pattern",
        "it would match everywhere"
    );

    settings.patterns = vec!["(a{1000}){1000}".into()];
    assert_eq!(
        Redactor::try_new(&settings).unwrap_err().code(),
        "pattern",
        "it is too large"
    );
}

#[test]
fn a_removal_the_format_would_refuse_changes_nothing() {
    let mut doc = document(1);
    doc.lines.insert(
        1,
        Line::new(coqui_acp_history::Body::PermissionRequest {
            tool_call: json!({"toolCallId": "t"}),
            options: vec![json!({"optionId": "o"})],
        }),
    );
    doc.lines.insert(
        2,
        Line::new(coqui_acp_history::Body::PermissionAnswer {
            tool_call_id: "t".into(),
            outcome: json!({"outcome": "selected", "optionId": "o"}),
        }),
    );
    let before = doc.clone();
    let mut redactor = Redactor::try_new(&values(&["selected"])).unwrap();
    let error = redactor.document(&mut doc).unwrap_err();
    assert_eq!(error.code(), "shape");
    assert_eq!(doc, before, "the document is as it was");
    assert_eq!(redactor.report().replaced, 0);
}

#[test]
fn the_text_of_a_summary_and_an_error_is_redacted() {
    let mut doc = document(1);
    doc.lines
        .push(Line::compaction(format!("summary with {SECRET}")));
    doc.lines.push(Line::text_prompt("go"));
    doc.lines.push(Line::new(coqui_acp_history::Body::TurnEnd {
        stop: Stop::Error,
        error: Some(format!("failed with {SECRET}")),
    }));
    Redactor::try_new(&values(&[SECRET]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert!(!doc.to_string().unwrap().contains(SECRET));
}

fn text_of(doc: &Document) -> String {
    doc.to_string().unwrap()
}

fn one_prompt(text: &str) -> Document {
    let mut doc = document(0);
    doc.lines.push(Line::text_prompt(text));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    doc
}

#[test]
fn a_value_inside_another_value_leaves_none_of_the_longer_one() {
    for order in [["sk-", "sk-SECRET123"], ["sk-SECRET123", "sk-"]] {
        let mut doc = one_prompt("token sk-SECRET123 end");
        let mut redactor = Redactor::try_new(&values(&order)).unwrap();
        let report = redactor.document(&mut doc).unwrap();
        let text = text_of(&doc);
        assert!(!text.contains("SECRET123"), "{order:?}: {text}");
        assert!(text.contains("token [redacted] end"), "{order:?}: {text}");
        assert_eq!(report.replaced, 1, "one span was replaced");
        assert!(
            report.rules.iter().all(|rule| rule.replaced == 1),
            "each rule found its match: {report:?}"
        );
    }
}

#[test]
fn overlapping_occurrences_of_one_value_are_removed_whole() {
    let mut doc = one_prompt("x ababab y");
    Redactor::try_new(&values(&["abab"]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert!(text_of(&doc).contains("x [redacted] y"));
}

#[test]
fn a_value_cannot_split_a_pattern_match() {
    let mut doc = one_prompt("xabcdx");
    let mut settings = values(&["bc"]);
    settings.patterns = vec!["abcd".into()];
    Redactor::try_new(&settings)
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert!(text_of(&doc).contains("x[redacted]x"), "{}", text_of(&doc));
}

#[test]
fn an_identifier_key_is_kept_only_where_the_format_pairs_lines_by_it() {
    let mut doc = document(1);
    doc.lines.insert(
        1,
        Line::update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "execute",
            "rawInput": {"toolCallId": SECRET, "optionId": SECRET, "sessionUpdate": SECRET},
            "_meta": {"toolCallId": SECRET},
        })),
    );
    doc.lines.insert(
        2,
        Line::new(coqui_acp_history::Body::PermissionRequest {
            tool_call: json!({"toolCallId": "t1", "rawInput": {"toolCallId": SECRET}}),
            options: vec![
                json!({"optionId": "allow", "name": "Allow", "_meta": {"optionId": SECRET}}),
            ],
        }),
    );
    doc.lines.insert(
        3,
        Line::new(coqui_acp_history::Body::PermissionAnswer {
            tool_call_id: "t1".into(),
            outcome: json!({"outcome": "selected", "optionId": "allow", "_meta": {"optionId": SECRET}}),
        }),
    );
    Redactor::try_new(&values(&[SECRET]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    let text = text_of(&doc);
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains(r#""toolCallId":"t1""#));
    assert!(text.contains(r#""optionId":"allow""#));
    let read: Document = text.parse().expect("the pairing survives");
    assert_eq!(read.lines, doc.lines);
}

#[test]
fn a_redaction_that_found_nothing_leaves_the_header_and_its_digest() {
    let mut doc = one_prompt("nothing secret here");
    doc.seal().unwrap();
    let before = doc.clone();
    let report = Redactor::try_new(&values(&[SECRET]))
        .unwrap()
        .document(&mut doc)
        .unwrap();
    assert_eq!(report.replaced, 0);
    assert_eq!(doc, before);
    assert!(doc.header.redacted.is_none());
    assert!(doc.header.digest.is_some());
}
