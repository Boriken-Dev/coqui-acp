//! Writing, reading and writing again; the writer refusing what the reader would; bounds.

mod support;

use std::io::{self, Read};

use serde_json::json;

use coqui_acp_history::{
    Body, Bounds, Document, HistoryError, Line, OrderRule, Reader, Stop, StopReason, Trigger,
    Writer, WriterSettings,
};
use support::{document, header};

fn end() -> Line {
    Line::turn_end(Stop::Acp(StopReason::EndTurn))
}

fn rich() -> Document {
    let mut doc = document(0);
    doc.header.model = Some("claude-opus".into());
    doc.header.cwd = Some("/w".into());
    doc.header.created_at = Some("2026-11-09T12:00:00Z".into());
    doc.header.acp_version = Some(1);
    doc.header.origin.version = Some("2.1.318".into());
    doc.header.origin.session_id = Some("native-1".into());
    doc.lines
        .push(Line::text_prompt("read a.txt").at("2026-11-09T12:00:01Z"));
    doc.lines.push(Line::update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "read", "status": "pending",
        "rawInput": {"file_path": "/w/a.txt", "n": 1.5, "big": 18446744073709551615u64},
        "_meta": {"dev.boriken.history/native": {"id": "toolu_1"}},
    })));
    doc.lines.push(Line::new(Body::PermissionRequest {
        tool_call: json!({"toolCallId": "t1"}),
        options: vec![json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"})],
    }));
    doc.lines.push(Line::new(Body::PermissionAnswer {
        tool_call_id: "t1".into(),
        outcome: json!({"outcome": "selected", "optionId": "allow"}),
    }));
    doc.lines.push(Line::new(Body::Cancel));
    doc.lines.push(Line::new(Body::TurnEnd {
        stop: Stop::Acp(StopReason::Cancelled),
        error: None,
    }));
    doc.lines.push(Line::new(Body::Compaction {
        summary: Some("Summary \u{65e5}\u{672c}".into()),
        trigger: Some(Trigger::Auto),
        pre_tokens: Some(1234),
    }));
    doc.lines
        .push(Line::text_prompt("next").meta(json!({"dev.boriken.history/native": {"u": 1}})));
    doc.lines.push(Line::new(Body::TurnEnd {
        stop: Stop::Error,
        error: Some("the harness exited".into()),
    }));
    doc
}

#[test]
fn a_document_written_read_and_written_again_is_identical() {
    let doc = rich();
    let first = doc.to_string().unwrap();
    let read: Document = first.parse().unwrap();
    assert_eq!(read, doc);
    assert_eq!(read.to_string().unwrap(), first);
    assert_eq!(read.to_vec().unwrap(), first.as_bytes());
}

#[test]
fn the_file_is_lines_of_json_ended_by_line_feeds_alone() {
    let text = rich().to_string().unwrap();
    assert!(text.ends_with('\n'));
    assert!(!text.contains('\r'));
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("each line is JSON");
    }
    assert!(text.starts_with(r#"{"format":"coqui-acp-history","version":1,"id":"#));
}

#[test]
fn the_canonical_spelling_does_not_depend_on_the_order_a_value_was_built_in() {
    let one = json!({"sessionUpdate": "x", "b": 1, "a": {"y": 1, "x": 2}});
    let mut doc = document(0);
    doc.lines.push(Line::update(one));
    let text = doc.to_string().unwrap();
    assert!(
        text.contains(r#"{"a":{"x":2,"y":1},"b":1,"sessionUpdate":"x"}"#),
        "{text}"
    );
}

#[test]
fn a_sealed_document_is_verified_when_it_is_read() {
    let mut doc = rich();
    doc.seal().unwrap();
    assert!(doc.header.digest.as_deref().unwrap().starts_with("sha256:"));
    let text = doc.to_string().unwrap();
    assert_eq!(text.parse::<Document>().unwrap(), doc);

    let tampered = text.replace("read a.txt", "read b.txt");
    assert_ne!(tampered, text);
    let error = tampered.parse::<Document>().unwrap_err();
    assert_eq!(error.code(), "digest_mismatch");

    let mut reader = Reader::builder(tampered.as_bytes())
        .verify_digest(false)
        .open()
        .unwrap();
    while reader.next_entry().unwrap().is_some() {}
}

#[test]
fn a_streamed_body_digest_matches_the_one_a_document_seals_with() {
    let mut doc = rich();
    let mut writer = Writer::builder(Vec::new()).start(&doc.header).unwrap();
    for line in &doc.lines {
        writer.write(line).unwrap();
    }
    let streamed = writer.body_digest();
    doc.seal().unwrap();
    assert_eq!(doc.header.digest.as_deref(), Some(streamed.as_str()));
}

#[test]
fn the_writer_refuses_each_line_the_reader_would_and_writes_nothing_for_it() {
    let cases: Vec<(Vec<Line>, OrderRule)> = vec![
        (
            vec![Line::text_prompt("a"), Line::text_prompt("b")],
            OrderRule::PromptInsideTurn,
        ),
        (vec![end()], OrderRule::OutsideTurn),
        (vec![Line::new(Body::Cancel)], OrderRule::OutsideTurn),
        (
            vec![
                Line::text_prompt("a"),
                Line::new(Body::PermissionAnswer {
                    tool_call_id: "t".into(),
                    outcome: json!({"outcome": "cancelled"}),
                }),
            ],
            OrderRule::AnswerWithoutRequest,
        ),
    ];
    for (lines, rule) in cases {
        let mut writer = Writer::builder(Vec::new()).start(&header()).unwrap();
        let (last, rest) = lines.split_last().unwrap();
        for line in rest {
            writer.write(line).unwrap();
        }
        let before = writer.summary().lines;
        match writer.write(last) {
            Err(HistoryError::Order { rule: got, line }) => {
                assert_eq!(got, rule);
                assert_eq!(line, before + 1);
            }
            other => panic!("expected an order error, got {other:?}"),
        }
        assert_eq!(
            writer.summary().lines,
            before,
            "a refused line is not counted"
        );
        let (bytes, _) = writer.finish().unwrap();
        // What was written still reads, and the refused line is not in it.
        let doc = Document::from_slice(&bytes).unwrap();
        assert_eq!(doc.lines.len(), lines.len() - 1);
    }
}

#[test]
fn the_writer_goes_on_after_a_refusal() {
    let mut writer = Writer::builder(Vec::new()).start(&header()).unwrap();
    assert!(writer.write(&end()).is_err());
    writer.write(&Line::text_prompt("a")).unwrap();
    writer.write(&end()).unwrap();
    assert_eq!(writer.summary().turns, 1);
}

#[test]
fn the_writer_refuses_a_line_with_a_shape_the_reader_refuses() {
    let mut writer = Writer::builder(Vec::new()).start(&header()).unwrap();
    let error = writer
        .write(&Line::prompt(vec![json!({"text": "no type"})]))
        .unwrap_err();
    assert_eq!(error.code(), "shape");
    assert_eq!(error.pointer(), Some("/params/prompt/0/type"));
    let error = writer
        .write(&Line::update(json!({"content": 1})))
        .unwrap_err();
    assert_eq!(error.code(), "shape");
    let error = writer
        .write(&Line::text_prompt("x").at("noon"))
        .unwrap_err();
    assert_eq!(error.pointer(), Some("/at"));
}

#[test]
fn the_writer_refuses_a_header_the_reader_would_refuse() {
    let mut bad = header();
    bad.id = "not-a-uuid".into();
    let error = Writer::builder(Vec::new())
        .start(&bad)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.code(), "shape");
    assert_eq!(error.pointer(), Some("/id"));
}

#[test]
fn the_writer_holds_a_history_to_its_bounds_and_never_cuts_a_line() {
    let bounds = |f: fn(&mut Bounds)| {
        let mut bounds = Bounds::default();
        f(&mut bounds);
        let mut settings = WriterSettings::default();
        settings.bounds = bounds;
        settings
    };
    let long = Line::text_prompt(&"x".repeat(500));
    let mut writer = Writer::builder(Vec::new())
        .settings(bounds(|b| b.max_line_bytes = 300))
        .start(&header())
        .unwrap();
    assert_eq!(writer.write(&long).unwrap_err().code(), "too_long");
    assert_eq!(writer.summary().lines, 1);

    let mut writer = Writer::builder(Vec::new())
        .settings(bounds(|b| b.max_lines = 3))
        .start(&header())
        .unwrap();
    writer.write(&Line::text_prompt("a")).unwrap();
    writer.write(&end()).unwrap();
    assert_eq!(
        writer.write(&Line::text_prompt("b")).unwrap_err().code(),
        "too_many_lines"
    );

    let mut writer = Writer::builder(Vec::new())
        .settings(bounds(|b| b.max_depth = 3))
        .start(&header())
        .unwrap();
    let error = writer
        .write(&Line::update(json!({"sessionUpdate": "x", "a": {"b": {}}})))
        .unwrap_err();
    assert_eq!(error.code(), "too_deep");

    let mut writer = Writer::builder(Vec::new())
        .settings(bounds(|b| b.max_total_bytes = 300))
        .start(&header())
        .unwrap();
    assert_eq!(writer.write(&long).unwrap_err().code(), "too_large");
}

#[test]
fn a_document_nested_beyond_the_parser_is_refused_not_overflowed() {
    let mut nested = json!(1);
    for _ in 0..300 {
        nested = json!([nested]);
    }
    let mut doc = document(0);
    doc.lines
        .push(Line::update(json!({"sessionUpdate": "x", "deep": nested})));
    let error = doc.to_string().unwrap_err();
    assert!(matches!(error.code(), "shape" | "too_deep"), "{error}");
}

#[test]
fn a_reader_stops_after_its_first_error() {
    let text = "{\"format\":\"coqui-acp-history\",\"version\":1,\"id\":\"5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77\",\"origin\":{\"harness\":\"x\"}}\n{bad\n{\"method\":\"session/cancel\"}\n";
    let mut reader = Reader::builder(text.as_bytes()).open().unwrap();
    assert_eq!(reader.next_entry().unwrap_err().code(), "syntax");
    assert!(reader.next_entry().unwrap().is_none());
}

#[test]
fn a_source_that_never_ends_a_line_is_refused_without_being_held() {
    let settings = {
        let mut bounds = Bounds::default();
        bounds.max_line_bytes = 1000;
        bounds
    };
    let error = Reader::builder(io::repeat(b'a'))
        .bounds(settings)
        .open()
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.code(), "too_long");
    assert_eq!(error.line(), Some(1));
}

#[test]
fn bytes_that_are_not_utf8_are_refused_at_their_line() {
    let mut bytes = document(0).to_vec().unwrap();
    bytes.extend_from_slice(b"{\"method\":\"session/cancel\",\"x\":\"\xff\"}\n");
    let error = Document::from_slice(&bytes).unwrap_err();
    assert_eq!(error.code(), "not_utf8");
    assert_eq!(error.line(), Some(2));
}

#[test]
fn a_failing_source_is_an_io_error() {
    struct Fails;
    impl Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("disk"))
        }
    }
    assert_eq!(
        Reader::builder(Fails)
            .open()
            .map(|_| ())
            .unwrap_err()
            .code(),
        "io"
    );
}

#[test]
fn an_error_says_where_and_never_quotes_the_document() {
    let secret = "hunter2-the-secret";
    let text = format!(
        "{{\"format\":\"coqui-acp-history\",\"version\":1,\"id\":\"5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77\",\"origin\":{{\"harness\":\"x\"}}}}\n\
         {{\"method\":\"session/prompt\",\"params\":{{\"prompt\":[{{\"type\":\"text\"}}],\"{secret}\":\"{secret}\"}}}}\n"
    );
    let error = text.parse::<Document>().unwrap_err();
    assert_eq!(error.code(), "unknown_field");
    for rendering in [error.to_string(), format!("{error:?}")] {
        assert!(!rendering.contains(secret), "{rendering}");
    }
    let malformed = format!("{{\"format\":\"coqui-acp-history\",\"version\":1 {secret}");
    let error = malformed.parse::<Document>().unwrap_err();
    assert!(!format!("{error} {error:?}").contains(secret));
}
