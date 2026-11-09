//! The shared vectors, which a Dart or Python reader runs too.

mod support;

use serde_json::{Value, json};

use coqui_acp_history::{Document, HistoryError, Reader, ReaderSettings, Stop, Tool, tools};
use support::{case_bounds, case_text, vectors};

fn read_all(
    text: &str,
    settings: ReaderSettings,
) -> Result<(Document, coqui_acp_history::Summary), HistoryError> {
    let mut reader = Reader::builder(text.as_bytes()).settings(settings).open()?;
    let mut lines = Vec::new();
    while let Some(entry) = reader.next_entry()? {
        lines.push(entry.line);
    }
    let summary = reader.summary().clone();
    let mut doc = Document::new(reader.header().clone());
    doc.lines = lines;
    Ok((doc, summary))
}

fn cases(file: &str) -> Vec<Value> {
    vectors(file)["cases"].as_array().expect("cases").clone()
}

#[test]
fn every_valid_vector_reads_to_its_expected_parse() {
    let all = cases("valid.json");
    assert!(all.len() >= 20);
    for case in &all {
        let name = case["name"].as_str().unwrap();
        let text = case_text(case);
        let expect = &case["expect"];
        let (doc, summary) =
            read_all(&text, ReaderSettings::default()).unwrap_or_else(|e| panic!("{name}: {e}"));

        let header = &expect["header"];
        assert_eq!(doc.header.id, header["id"], "{name}");
        assert_eq!(doc.header.origin.harness, header["harness"], "{name}");
        assert_eq!(
            doc.header.origin.version.as_deref(),
            header["harness_version"].as_str(),
            "{name}"
        );
        assert_eq!(
            doc.header.origin.session_id.as_deref(),
            header["harness_session_id"].as_str(),
            "{name}"
        );
        assert_eq!(
            doc.header.model.as_deref(),
            header["model"].as_str(),
            "{name}"
        );
        assert_eq!(doc.header.cwd.as_deref(), header["cwd"].as_str(), "{name}");

        let want = &expect["summary"];
        assert_eq!(
            summary.lines,
            want["lines"].as_u64().unwrap(),
            "{name}: lines"
        );
        assert_eq!(
            summary.turns,
            want["turns"].as_u64().unwrap(),
            "{name}: turns"
        );
        assert_eq!(
            summary.updates,
            want["updates"].as_u64().unwrap(),
            "{name}: updates"
        );
        assert_eq!(
            summary.compactions,
            want["compactions"].as_u64().unwrap(),
            "{name}: compactions"
        );
        assert_eq!(
            summary.open_turn,
            want["open_turn"].as_bool().unwrap(),
            "{name}: open_turn"
        );
        assert_eq!(
            summary.last_stop.map(Stop::name),
            want["last_stop"].as_str(),
            "{name}: last_stop"
        );

        let methods: Vec<&str> = doc.lines.iter().map(|l| l.body.method()).collect();
        let want_methods: Vec<&str> = expect["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(methods, want_methods, "{name}: methods");

        let seed = doc.seed();
        let want = &expect["seed"];
        assert_eq!(
            seed.point.as_ref().map(|p| p.line),
            want["line"].as_u64(),
            "{name}: seed line"
        );
        assert_eq!(
            seed.point.as_ref().map(|p| p.digest.as_str()),
            want["digest"].as_str(),
            "{name}: seed digest"
        );
        assert_eq!(
            seed.summary,
            want["summary"].as_str(),
            "{name}: seed summary"
        );
        assert_eq!(
            seed.first_line,
            want["first_line"].as_u64().unwrap(),
            "{name}: first line"
        );
        assert_eq!(
            seed.mid_turn,
            want["mid_turn"].as_bool().unwrap(),
            "{name}: mid_turn"
        );
        assert_eq!(
            seed.turns(),
            want["turns"].as_u64().unwrap(),
            "{name}: seed turns"
        );
    }
}

#[test]
fn a_canonical_vector_is_written_back_identically() {
    for case in cases("valid.json") {
        if !case["canonical"].as_bool().unwrap() {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let text = case_text(&case);
        let doc: Document = text.parse().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(doc.to_string().unwrap(), text, "{name}");
    }
}

#[test]
fn every_vector_is_copied_unaltered() {
    for case in cases("valid.json") {
        let name = case["name"].as_str().unwrap();
        let text = case_text(&case);
        let reader = Reader::builder(text.as_bytes()).open().unwrap();
        let mut out = Vec::new();
        reader.copy_to(&mut out).unwrap();
        // The only byte a copy drops is one leading byte-order mark.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            text.trim_start_matches('\u{feff}'),
            "{name}"
        );
    }
}

#[test]
fn every_refusal_vector_is_refused_with_its_error() {
    let all = cases("refusals.json");
    assert!(all.len() >= 50);
    for case in &all {
        let name = case["name"].as_str().unwrap();
        let text = case_text(case);
        let error = read_all(&text, ReaderSettings::default())
            .expect_err(&format!("{name}: should be refused"));
        let want = &case["expect"];
        assert_eq!(
            error.code(),
            want["code"].as_str().unwrap(),
            "{name}: {error}"
        );
        assert_eq!(error.line(), want["line"].as_u64(), "{name}: line");
        if want.get("pointer").is_some() {
            assert_eq!(error.pointer(), want["pointer"].as_str(), "{name}: pointer");
        }
        // A refusal is the same through the whole-document reader.
        let again = text.parse::<Document>().expect_err(name);
        assert_eq!(again.code(), error.code(), "{name}");
    }
}

#[test]
fn every_bounds_vector_holds_the_reader_to_its_limits() {
    for case in cases("bounds.json") {
        let name = case["name"].as_str().unwrap();
        let text = case_text(&case);
        let mut settings = ReaderSettings::default();
        settings.bounds = case_bounds(&case);
        let result = read_all(&text, settings);
        let want = &case["expect"];
        if want["ok"].as_bool() == Some(true) {
            result.unwrap_or_else(|e| panic!("{name}: {e}"));
        } else {
            let error = result.expect_err(name);
            assert_eq!(
                error.code(),
                want["code"].as_str().unwrap(),
                "{name}: {error}"
            );
            assert_eq!(error.line(), want["line"].as_u64(), "{name}");
        }
    }
}

#[test]
fn the_tool_table_matches_its_vectors() {
    let table = vectors("tools.json");
    for tool in table["tools"].as_array().unwrap() {
        let tool = Tool::from_name(tool.as_str().unwrap()).expect("a tool of the table");
        let kind = table["kinds"][tool.name()].as_str().unwrap();
        assert_eq!(
            serde_json::to_value(tool.kind()).unwrap(),
            json!(kind),
            "{}",
            tool.name()
        );
    }
    assert_eq!(table["tools"].as_array().unwrap().len(), Tool::ALL.len());
    for case in table["normalise"].as_array().unwrap() {
        let (harness, name) = (
            case["harness"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        let (tool, input) = tools::normalise(harness, name, &case["input"])
            .unwrap_or_else(|| panic!("{harness} {name}"));
        assert_eq!(
            tool.name(),
            case["expect"]["tool"].as_str().unwrap(),
            "{harness} {name}"
        );
        assert_eq!(input, case["expect"]["input"], "{harness} {name}");
    }
    for case in table["denormalise"].as_array().unwrap() {
        let tool = Tool::from_name(case["tool"].as_str().unwrap()).unwrap();
        let harness = case["harness"].as_str().unwrap();
        let (name, input) = tools::denormalise(tool, harness, &case["input"]).unwrap();
        assert_eq!(name, case["expect"]["name"].as_str().unwrap());
        assert_eq!(input, case["expect"]["input"]);
    }
    for case in table["unknown_native"].as_array().unwrap() {
        let (harness, name) = (
            case["harness"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        assert_eq!(Tool::from_native(harness, name), None, "{harness} {name}");
    }
    for case in table["no_native"].as_array().unwrap() {
        let tool = Tool::from_name(case["tool"].as_str().unwrap()).unwrap();
        assert!(tool.native(case["harness"].as_str().unwrap()).is_none());
    }
}

#[test]
fn a_normalised_input_goes_round_to_its_native_and_back() {
    let table = vectors("tools.json");
    for case in table["normalise"].as_array().unwrap() {
        let (harness, name) = (
            case["harness"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        let (tool, normal) = tools::normalise(harness, name, &case["input"]).unwrap();
        let (native_name, native) = tools::denormalise(tool, harness, &normal).unwrap();
        // A harness with two names for a tool comes back to its preferred one.
        assert_eq!(native, case["input"], "{harness} {name}");
        assert_eq!(Tool::from_native(harness, native_name), Some(tool));
    }
}

#[test]
fn two_keys_that_rename_to_one_name_normalise_to_nothing() {
    let table = vectors("tools.json");
    let cases = table["collisions"].as_array().unwrap();
    assert!(cases.len() >= 3);
    for case in cases {
        let (harness, name) = (
            case["harness"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        assert!(
            tools::normalise(harness, name, &case["input"]).is_none(),
            "{harness} {name}"
        );
    }
    for case in table["collisions_denormalise"].as_array().unwrap() {
        let tool = Tool::from_name(case["tool"].as_str().unwrap()).unwrap();
        let harness = case["harness"].as_str().unwrap();
        assert!(tools::denormalise(tool, harness, &case["input"]).is_none());
    }
}
