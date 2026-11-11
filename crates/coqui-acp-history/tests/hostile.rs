//! Histories built to hurt a reader or a writer: time, nesting, spelling and what a header may hold.

mod support;

use core::time::Duration;
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use coqui_acp_history::{Document, Header, Line, Origin, Reader, Writer};
use support::{ID, document};

fn digest_of(body: &str) -> String {
    format!(
        "sha256:{}",
        data_encoding::HEXLOWER.encode(&Sha256::digest(body.as_bytes()))
    )
}

fn header_line(extra: &str) -> String {
    format!(
        r#"{{"format":"coqui-acp-history","version":1,"id":"{ID}","origin":{{"harness":"claude"}}{extra}}}"#
    )
}

/// A history whose header is `extra` (more header members, after a comma) and whose body is `lines`.
fn history(extra: &str, lines: &[&str]) -> String {
    let mut text = header_line(extra);
    text.push('\n');
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

fn refusal(text: &str) -> coqui_acp_history::HistoryError {
    text.parse::<Document>()
        .expect_err("the history should be refused")
}

/// A turn of `n` permission requests, answered in the order that costs a list the most.
fn permission_turn(n: usize) -> String {
    let mut text = header_line("");
    text.push('\n');
    text.push_str(
        r#"{"method":"session/prompt","params":{"prompt":[{"text":"go","type":"text"}]}}"#,
    );
    text.push('\n');
    for n in 0..n {
        text.push_str(&format!(
            r#"{{"method":"session/request_permission","params":{{"options":[{{"optionId":"a"}}],"toolCall":{{"toolCallId":"call-{n}"}}}}}}"#
        ));
        text.push('\n');
    }
    for n in (0..n).rev() {
        text.push_str(&format!(
            r#"{{"method":"history/permission_answer","params":{{"outcome":{{"optionId":"a","outcome":"selected"}},"toolCallId":"call-{n}"}}}}"#
        ));
        text.push('\n');
    }
    text.push_str(r#"{"method":"history/turn_end","params":{"stopReason":"end_turn"}}"#);
    text.push('\n');
    text
}

/// The shortest of three readings of `n` requests; the shortest is the one a busy machine disturbed least.
fn reading_time(n: usize) -> Duration {
    let text = permission_turn(n);
    (0..3)
        .map(|_| {
            let started = Instant::now();
            let mut reader = Reader::builder(text.as_bytes()).open().unwrap();
            while reader.next_entry().unwrap().is_some() {}
            assert_eq!(reader.summary().lines, 2 * n as u64 + 3);
            started.elapsed()
        })
        .min()
        .unwrap()
}

#[test]
fn a_turn_of_a_hundred_thousand_permission_requests_is_matched_in_linear_time() {
    // Ten times the requests cost about ten times as much when matching is linear and a hundred times as much when it is
    // quadratic; a bound in seconds would measure the machine. The slack of four is for a machine busy with other work.
    const N: usize = 100_000;
    let small = reading_time(N / 10);
    let big = reading_time(N);
    assert!(
        big < small * 40,
        "{N} requests took {big:?} and a tenth of them {small:?}: matching them is not linear"
    );
}

#[test]
fn a_document_read_from_another_producers_spelling_keeps_a_digest_it_can_vouch_for() {
    let body = concat!(
        r#"{ "params": { "prompt": [ { "type": "text", "text": "hi" } ] }, "method": "session/prompt" }"#,
        "\n",
        r#"{ "params": { "stopReason": "end_turn" }, "method": "history/turn_end" }"#,
        "\n",
    );
    let text = format!(
        "{}\n{body}",
        header_line(&format!(r#","digest":"{}""#, digest_of(body)))
    );
    let doc: Document = text.parse().unwrap();
    assert!(doc.header.digest.is_some());
    let written = doc.to_string().unwrap();
    assert_ne!(
        written, text,
        "the document is written in canonical spelling"
    );
    let again: Document = written
        .parse()
        .expect("what a document writes, a reader reads");
    assert_eq!(again.lines, doc.lines);
    assert!(again.header.digest.is_some());
}

fn nested(levels: usize) -> Value {
    let mut value = json!(1);
    for _ in 0..levels {
        value = json!({ "a": value });
    }
    value
}

#[test]
fn a_header_nested_past_the_depth_bound_is_not_written() {
    let mut header = Header::new(ID, Origin::new("claude"));
    header.meta = Some(nested(70));
    let doc = Document::new(header);
    match doc.to_string() {
        Err(error) => assert_eq!(error.code(), "too_deep"),
        Ok(text) => panic!(
            "written, and a reader says {:?}",
            text.parse::<Document>().map(|_| ()).map_err(|e| e.code())
        ),
    }
    let mut sink = Vec::new();
    let mut bounds = coqui_acp_history::Bounds::default();
    bounds.max_depth = 80;
    Writer::builder(&mut sink)
        .bounds(bounds)
        .start(&doc.header)
        .expect("under a looser bound the header is written");
}

#[test]
fn header_strings_with_control_characters_are_refused() {
    for field in [
        r#","model":"a\u0000b""#,
        r#","mode":"a\u001bb""#,
        r#","effort":"a\nb""#,
        r#","cwd":"/w\u0000""#,
        r#","cwd":"/w\u009b""#,
        r#","cwd":"/w\u007f""#,
    ] {
        let error = refusal(&history(field, &[]));
        assert_eq!(error.code(), "shape", "{field}");
    }
    for origin in [
        r#"{"harness":"claude","version":"1\u001b[2K"}"#,
        r#"{"harness":"claude","sessionId":"../../evil\u0000"}"#,
    ] {
        let text = format!(
            r#"{{"format":"coqui-acp-history","version":1,"id":"{ID}","origin":{origin}}}"#
        );
        assert_eq!(refusal(&format!("{text}\n")).code(), "shape", "{origin}");
    }
}

#[test]
fn a_working_directory_is_absolute_in_either_path_style() {
    for good in [
        "/home/a",
        "/",
        r"C:\Work\a",
        "C:/Work/a",
        r"\\server\share\dir",
        r"\\?\C:\dir",
    ] {
        let json = serde_json::to_string(good).unwrap();
        history(&format!(r#","cwd":{json}"#), &[])
            .parse::<Document>()
            .unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        "relative/dir",
        "..",
        r"..\..\Windows",
        "C:dir",
        "~/a",
        "./a",
        "file:///a",
    ] {
        let json = serde_json::to_string(bad).unwrap();
        let error = refusal(&history(&format!(r#","cwd":{json}"#), &[]));
        assert_eq!(error.code(), "shape", "{bad}");
        assert_eq!(error.pointer(), Some("/cwd"), "{bad}");
    }
}

/// A small deterministic generator (xorshift), so a failure reproduces.
fn doubles(count: usize) -> Vec<f64> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut found = Vec::new();
    while found.len() < count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = f64::from_bits(state);
        if value.is_finite() {
            found.push(value);
        }
    }
    found
}

#[test]
fn every_double_survives_a_read_and_a_write_exactly() {
    let values = doubles(20_000);
    let mut doc = document(0);
    for chunk in values.chunks(500) {
        doc.lines.push(Line::update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t",
            "rawOutput": chunk,
        })));
    }
    let text = doc.to_string().unwrap();
    let read: Document = text.parse().unwrap();
    let mut got = Vec::new();
    for line in &read.lines {
        if let coqui_acp_history::Body::Update { update } = &line.body {
            for number in update["rawOutput"].as_array().unwrap() {
                got.push(number.as_f64().unwrap());
            }
        }
    }
    let wrong = values
        .iter()
        .zip(&got)
        .filter(|(want, got)| want.to_bits() != got.to_bits())
        .count();
    assert_eq!(wrong, 0, "of {} doubles", values.len());
    assert_eq!(read.to_string().unwrap(), text);
}
