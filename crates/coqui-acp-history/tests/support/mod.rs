//! Helpers the test binaries share: the shared vectors, and documents to build.

#![allow(dead_code, reason = "each test binary uses some of these")]

use std::path::PathBuf;

use serde_json::Value;

use coqui_acp_history::{Bounds, Document, Header, Line, Origin, Stop, StopReason};

pub const ID: &str = "5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77";

/// A shared vector file, parsed.
pub fn vectors(name: &str) -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "vectors",
        "session_history",
        name,
    ]
    .iter()
    .collect();
    let text = std::fs::read_to_string(&path).expect("the vector file is in the repository");
    serde_json::from_str(&text).expect("a vector file is JSON")
}

/// The text of a vector case: its `text`, or its `lines` each ended by a line feed.
pub fn case_text(case: &Value) -> String {
    if let Some(text) = case.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    let lines = case["lines"].as_array().expect("a case has text or lines");
    lines
        .iter()
        .map(|l| format!("{}\n", l.as_str().expect("a line is text")))
        .collect()
}

/// The bounds a case names; one it does not name is its default.
pub fn case_bounds(case: &Value) -> Bounds {
    let mut bounds = Bounds::default();
    if let Some(limits) = case.get("limits").and_then(Value::as_object) {
        for (name, value) in limits {
            let value = value.as_u64().expect("a limit is a number");
            match name.as_str() {
                "max_line_bytes" => bounds.max_line_bytes = value,
                "max_lines" => bounds.max_lines = value,
                "max_total_bytes" => bounds.max_total_bytes = value,
                "max_depth" => bounds.max_depth = value,
                other => panic!("unknown limit {other}"),
            }
        }
    }
    bounds
}

pub fn header() -> Header {
    Header::new(ID, Origin::new("claude"))
}

/// A header, `turns` turns of a prompt, an update and an end, and nothing else.
pub fn document(turns: usize) -> Document {
    let mut doc = Document::new(header());
    for n in 0..turns {
        doc.lines.push(Line::text_prompt(&format!("question {n}")));
        doc.lines.push(Line::update(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": format!("answer {n}")},
        })));
        doc.lines
            .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    }
    doc
}
