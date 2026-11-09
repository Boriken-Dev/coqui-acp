//! The bounds a hostile history meets: values, nesting, the memory of a whole document, and what a
//! reader or writer hands over before it has verified the body.

mod support;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use coqui_acp_history::{
    Body, Bounds, DEFAULT_DOCUMENT_MAX_TOTAL_BYTES, Document, HistoryError, Line, PARSER_DEPTH,
    Reader, ReaderSettings, Writer, WriterSettings,
};
use support::{ID, document, header};

fn header_text() -> String {
    format!(
        r#"{{"format":"coqui-acp-history","version":1,"id":"{ID}","origin":{{"harness":"claude"}}}}"#
    )
}

fn settings(change: impl FnOnce(&mut Bounds)) -> ReaderSettings {
    let mut settings = ReaderSettings::default();
    change(&mut settings.bounds);
    settings
}

fn read_all(text: &str, settings: ReaderSettings) -> Result<usize, HistoryError> {
    let mut reader = Reader::builder(text.as_bytes()).settings(settings).open()?;
    let mut n = 0;
    while reader.next_entry()?.is_some() {
        n += 1;
    }
    Ok(n)
}

/// A `session/update` line whose `rawOutput` is `payload`.
fn update_with(payload: &str) -> String {
    format!(
        r#"{{"method":"session/update","params":{{"update":{{"rawOutput":{payload},"sessionUpdate":"x"}}}}}}"#
    )
}

fn file(lines: &[String]) -> String {
    let mut text = header_text();
    text.push('\n');
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

fn zeros(count: usize) -> String {
    format!("[{}]", vec!["0"; count].join(","))
}

#[test]
fn a_line_with_more_values_than_the_bound_is_refused_while_it_is_parsed() {
    // The line itself holds 11 values (the envelope, params, update, rawOutput and their keys), so
    // 499,989 zeros fill the default of 500,000 and one more is refused.
    let ok = file(&[update_with(&zeros(499_989))]);
    assert_eq!(read_all(&ok, ReaderSettings::default()).unwrap(), 1);
    let hostile = file(&[update_with(&zeros(499_990))]);
    let error = read_all(&hostile, ReaderSettings::default()).unwrap_err();
    assert_eq!(error.code(), "too_many_values");
    assert_eq!(error.line(), Some(2));
    assert!(error.to_string().contains("500000"), "{error}");
    let looser = settings(|b| b.max_values = 600_000);
    assert_eq!(read_all(&hostile, looser).unwrap(), 1);
    let tight = settings(|b| b.max_values = 10);
    assert_eq!(
        read_all(&file(&[update_with(&zeros(20))]), tight)
            .unwrap_err()
            .code(),
        "too_many_values"
    );
}

#[test]
fn keys_count_as_values() {
    let object = format!(
        "{{{}}}",
        (0..100)
            .map(|n| format!(r#""k{n}":0"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let text = file(&[update_with(&object)]);
    let enough = settings(|b| b.max_values = 400);
    assert_eq!(read_all(&text, enough).unwrap(), 1);
    // The line's own values (the envelope, params, update, rawOutput and their keys) come to 10,
    // the object adds one and 200 for its keys and zeros.
    let short = settings(|b| b.max_values = 200);
    assert_eq!(
        read_all(&text, short).unwrap_err().code(),
        "too_many_values"
    );
}

#[test]
fn the_writer_holds_a_line_to_the_value_bound_too() {
    let mut bounds = Bounds::default();
    bounds.max_values = 50;
    let mut writer = Writer::builder(Vec::new())
        .settings({
            let mut s = WriterSettings::default();
            s.bounds = bounds;
            s
        })
        .start(&header())
        .unwrap();
    let wide = Line::update(json!({"sessionUpdate": "x", "rawOutput": vec![0; 100]}));
    assert_eq!(writer.write(&wide).unwrap_err().code(), "too_many_values");
}

fn nested_meta(levels: usize) -> String {
    // `levels` objects in `_meta`, which is itself a level below the line.
    let mut meta = String::from("1");
    for _ in 0..levels {
        meta = format!(r#"{{"a":{meta}}}"#);
    }
    format!(r#"{{"method":"session/cancel","_meta":{meta}}}"#)
}

fn with_turn(line: String) -> String {
    file(&[
        r#"{"method":"session/prompt","params":{"prompt":[{"text":"x","type":"text"}]}}"#.into(),
        line,
    ])
}

#[test]
fn the_parser_reads_exactly_127_levels_whatever_max_depth_says() {
    let high = settings(|b| b.max_depth = 1000);
    // The line is level 1 and `_meta` the first of the nested objects: 126 of them make 127.
    assert_eq!(
        read_all(&with_turn(nested_meta(126)), high.clone()).unwrap(),
        2
    );
    let error = read_all(&with_turn(nested_meta(127)), high).unwrap_err();
    assert_eq!(error.code(), "too_deep");
    assert_eq!(error.line(), Some(3));
    match error {
        HistoryError::TooDeep {
            limit, by_parser, ..
        } => {
            assert_eq!(limit, PARSER_DEPTH);
            assert!(by_parser, "the setting was higher than the parser reads");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        HistoryError::TooDeep {
            line: 3,
            limit: 127,
            by_parser: true
        }
        .to_string()
        .contains("parser's own limit")
    );
}

#[test]
fn a_depth_the_setting_forbids_is_the_settings_not_the_parsers() {
    let mid = settings(|b| b.max_depth = 100);
    assert_eq!(
        read_all(&with_turn(nested_meta(99)), mid.clone()).unwrap(),
        2
    );
    let error = read_all(&with_turn(nested_meta(100)), mid).unwrap_err();
    match error {
        HistoryError::TooDeep {
            limit, by_parser, ..
        } => {
            assert_eq!(limit, 100);
            assert!(!by_parser);
        }
        other => panic!("{other:?}"),
    }
    assert!(!read_all(&with_turn(nested_meta(70)), ReaderSettings::default()).is_ok());
}

#[test]
fn the_writer_and_the_parser_agree_on_the_deepest_line() {
    let mut bounds = Bounds::default();
    bounds.max_depth = 1000;
    let mut deepest: Value = json!(1);
    for _ in 0..126 {
        deepest = json!({ "a": deepest });
    }
    let mut writer = Writer::builder(Vec::new())
        .bounds(bounds.clone())
        .start(&header())
        .unwrap();
    writer
        .write(&Line::text_prompt("x"))
        .expect("a prompt opens the turn");
    writer
        .write(&Line::new(Body::Cancel).meta(deepest.clone()))
        .expect("127 levels are written");
    let (bytes, _) = writer.finish().unwrap();
    let mut read = ReaderSettings::default();
    read.bounds = bounds.clone();
    assert_eq!(
        read_all(core::str::from_utf8(&bytes).unwrap(), read).unwrap(),
        2
    );

    let mut writer = Writer::builder(Vec::new())
        .bounds(bounds)
        .start(&header())
        .unwrap();
    writer.write(&Line::text_prompt("x")).unwrap();
    let error = writer
        .write(&Line::new(Body::Cancel).meta(json!({ "a": deepest })))
        .unwrap_err();
    assert_eq!(error.code(), "too_deep");
}

#[test]
fn a_document_held_in_memory_is_held_to_a_smaller_total_than_a_stream() {
    let big_line = update_with(&format!(r#""{}""#, "x".repeat(1 << 20)));
    let lines = vec![big_line; 17];
    let text = file(&lines);
    assert!(text.len() as u64 > DEFAULT_DOCUMENT_MAX_TOTAL_BYTES);
    let error = text.parse::<Document>().unwrap_err();
    assert_eq!(error.code(), "too_large");
    assert!(Document::from_slice(text.as_bytes()).is_err());
    // A stream is not held to it, and a caller can read a document under bounds of its own.
    assert_eq!(read_all(&text, ReaderSettings::default()).unwrap(), 17);
    let reader = Reader::builder(text.as_bytes()).open().unwrap();
    assert_eq!(Document::from_reader(reader).unwrap().lines.len(), 17);
}

fn digest(body: &str) -> String {
    format!(
        "sha256:{}",
        data_encoding::HEXLOWER.encode(&Sha256::digest(body.as_bytes()))
    )
}

fn tampered() -> String {
    let body = "{\"method\":\"session/prompt\",\"params\":{\"prompt\":[{\"text\":\"a\",\"type\":\"text\"}]}}\n\
                {\"method\":\"history/turn_end\",\"params\":{\"stopReason\":\"end_turn\"}}\n";
    let head = header_text().replace(
        r#""origin""#,
        &format!(r#""digest":"{}","origin""#, digest("something else\n")),
    );
    format!("{head}\n{body}")
}

#[test]
fn a_reader_hands_over_lines_before_the_digest_and_a_check_does_not() {
    let text = tampered();
    let mut reader = Reader::builder(text.as_bytes()).open().unwrap();
    assert!(
        reader.next_entry().unwrap().is_some(),
        "lines come before the end, unverified"
    );
    assert!(reader.next_entry().unwrap().is_some());
    assert_eq!(reader.next_entry().unwrap_err().code(), "digest_mismatch");

    let error = Reader::builder(text.as_bytes())
        .open()
        .unwrap()
        .check()
        .unwrap_err();
    assert_eq!(error.code(), "digest_mismatch");
    assert_eq!(
        text.parse::<Document>().unwrap_err().code(),
        "digest_mismatch"
    );
    let mut sink = Vec::new();
    let error = Reader::builder(text.as_bytes())
        .open()
        .unwrap()
        .copy_to(&mut sink)
        .unwrap_err();
    assert_eq!(error.code(), "digest_mismatch");

    let intact = tampered().replace(&digest("something else\n"), &{
        let body = tampered();
        let body = body.split_once('\n').unwrap().1.to_owned();
        digest(&body)
    });
    let summary = Reader::builder(intact.as_bytes())
        .open()
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(summary.lines, 3);
    assert_eq!(summary.turns, 1);
}

#[test]
fn a_writer_whose_header_names_another_body_says_so_when_it_finishes() {
    let mut named = header();
    named.digest = Some(digest("not the body\n"));
    let mut writer = Writer::builder(Vec::new()).start(&named).unwrap();
    writer.write(&Line::text_prompt("a")).unwrap();
    assert_eq!(writer.finish().unwrap_err().code(), "digest_mismatch");

    let mut right = Writer::builder(Vec::new()).start(&header()).unwrap();
    right.write(&Line::text_prompt("a")).unwrap();
    let body = right.body_digest();
    let mut named = header();
    named.digest = Some(body);
    let mut writer = Writer::builder(Vec::new()).start(&named).unwrap();
    writer.write(&Line::text_prompt("a")).unwrap();
    let (bytes, _) = writer.finish().expect("the digest names the body");
    Document::from_slice(&bytes).expect("and a reader agrees");
}

#[test]
fn a_document_checks_its_lines_whatever_digest_its_header_holds() {
    let mut doc = document(2);
    doc.header.digest = Some(digest("stale\n"));
    assert_eq!(doc.check().unwrap().turns, 2);
    let text = doc.to_string().unwrap();
    let read: Document = text.parse().unwrap();
    doc.seal().unwrap();
    assert_eq!(
        read.header.digest, doc.header.digest,
        "writing a document makes its digest the written body's"
    );
}
