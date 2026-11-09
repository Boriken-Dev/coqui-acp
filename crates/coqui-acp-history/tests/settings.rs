//! Settings as guatiao values: defaults, refusals, and what is reported when one is not default.

use guatiao::Schema;
use guatiao_serde::Presentation;
use guatiao_serde::text::json;

use coqui_acp_history::{
    Bounds, DEFAULT_MAX_DEPTH, DEFAULT_MAX_LINE_BYTES, DEFAULT_MAX_LINES, DEFAULT_MAX_TOTAL_BYTES,
    DEFAULT_REPLACEMENT, Document, Reader, ReaderSettings, RedactionSettings, WriterSettings,
    non_defaults,
};

fn read<T: guatiao::FromWalk>(text: &str) -> Result<T, guatiao_serde::Error> {
    json::from_str_as::<T>(text, Presentation::new())
}

#[test]
fn an_empty_document_reads_to_the_defaults() {
    assert_eq!(
        read::<ReaderSettings>("{}").unwrap(),
        ReaderSettings::default()
    );
    assert_eq!(
        read::<WriterSettings>("{}").unwrap(),
        WriterSettings::default()
    );
    assert_eq!(read::<Bounds>("{}").unwrap(), Bounds::default());
    assert_eq!(
        read::<RedactionSettings>("{}").unwrap(),
        RedactionSettings::default()
    );
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let bounds = Bounds::default();
    assert_eq!(bounds.max_line_bytes, DEFAULT_MAX_LINE_BYTES);
    assert_eq!(bounds.max_line_bytes, 16 * 1024 * 1024);
    assert_eq!(bounds.max_lines, DEFAULT_MAX_LINES);
    assert_eq!(bounds.max_lines, 5_000_000);
    assert_eq!(bounds.max_total_bytes, DEFAULT_MAX_TOTAL_BYTES);
    assert_eq!(bounds.max_total_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(bounds.max_depth, DEFAULT_MAX_DEPTH);
    assert!(ReaderSettings::default().verify_digest);
    assert_eq!(
        RedactionSettings::default().replacement,
        DEFAULT_REPLACEMENT
    );
}

#[test]
fn a_setting_given_is_read_and_a_misspelled_one_is_refused() {
    let settings =
        read::<ReaderSettings>(r#"{"bounds": {"max_lines": 10}, "verify_digest": false}"#).unwrap();
    assert_eq!(settings.bounds.max_lines, 10);
    assert_eq!(settings.bounds.max_depth, DEFAULT_MAX_DEPTH);
    assert!(!settings.verify_digest);
    assert!(read::<ReaderSettings>(r#"{"bounds": {"max_line": 10}}"#).is_err());
    assert!(read::<ReaderSettings>(r#"{"verify": true}"#).is_err());
    assert!(read::<RedactionSettings>(r#"{"pattern": ["x"]}"#).is_err());
}

#[test]
fn the_schema_describes_every_setting_with_its_default() {
    let schema = ReaderSettings::schema().unwrap();
    let text = json::to_string_of(&schema, Presentation::new()).unwrap();
    for needle in [
        "verify_digest",
        "bounds",
        "\"default\":true",
        "Whether a digest in the header is checked",
    ] {
        assert!(text.contains(needle), "{needle} is missing from {text}");
    }
    let text = json::to_string_of(&Bounds::schema().unwrap(), Presentation::new()).unwrap();
    assert!(
        text.contains("16777216") && text.contains("max_depth"),
        "{text}"
    );
}

#[test]
fn the_defaults_report_nothing_and_each_changed_key_reports_itself() {
    assert!(non_defaults(&ReaderSettings::default()).is_empty());
    assert!(non_defaults(&WriterSettings::default()).is_empty());
    assert!(non_defaults(&RedactionSettings::default()).is_empty());

    let mut settings = ReaderSettings::default();
    settings.bounds.max_line_bytes = 1 << 30;
    settings.verify_digest = false;
    let found = non_defaults(&settings);
    let names: Vec<&str> = found.iter().map(|n| n.setting.as_str()).collect();
    assert_eq!(names, ["bounds.max_line_bytes", "verify_digest"]);
    assert_eq!(found[0].value, (1_u64 << 30).to_string());
    assert_eq!(found[0].default, DEFAULT_MAX_LINE_BYTES.to_string());
    assert_eq!(found[1].value, "false");
}

#[test]
fn redaction_values_are_named_as_non_default_and_never_shown() {
    let mut settings = RedactionSettings::default();
    settings.values = vec!["hunter2".into()];
    settings.patterns = vec!["x+".into()];
    let found = non_defaults(&settings);
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|n| !n.value.contains("hunter2")));
    assert_eq!(
        found.iter().find(|n| n.setting == "values").unwrap().value,
        "[not shown]"
    );
}

#[test]
fn settings_filled_by_a_file_reach_the_reader() {
    let settings = read::<ReaderSettings>(r#"{"bounds": {"max_lines": 2}}"#).unwrap();
    let text = coqui_acp_history::Header::new(
        "5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77",
        coqui_acp_history::Origin::new("x"),
    );
    let mut doc = Document::new(text);
    doc.lines.push(coqui_acp_history::Line::text_prompt("a"));
    doc.lines.push(coqui_acp_history::Line::turn_end(
        coqui_acp_history::Stop::Error,
    ));
    let bytes = doc.to_vec().unwrap();
    let mut reader = Reader::builder(bytes.as_slice())
        .settings(settings)
        .open()
        .unwrap();
    reader.next_entry().unwrap();
    assert_eq!(reader.next_entry().unwrap_err().code(), "too_many_lines");
}

#[test]
fn each_builder_setter_reaches_the_reader() {
    let doc = {
        let mut doc = Document::new(coqui_acp_history::Header::new(
            "5f0f6d6e-3f3c-4f1a-8a52-1c1e6f0b9a77",
            coqui_acp_history::Origin::new("x"),
        ));
        doc.lines.push(coqui_acp_history::Line::text_prompt("a"));
        doc
    };
    let bytes = doc.to_vec().unwrap();
    let mut tight = Bounds::default();
    tight.max_line_bytes = 20;
    assert_eq!(
        Reader::builder(bytes.as_slice())
            .bounds(tight)
            .open()
            .map(|_| ())
            .unwrap_err()
            .code(),
        "too_long"
    );
    let mut sealed = doc.clone();
    sealed.header.digest = Some(format!("sha256:{}", "0".repeat(64)));
    let bytes = sealed.to_vec().unwrap();
    let mut strict = Reader::builder(bytes.as_slice()).open().unwrap();
    strict.next_entry().unwrap();
    assert_eq!(strict.next_entry().unwrap_err().code(), "digest_mismatch");
    let mut lax = Reader::builder(bytes.as_slice())
        .verify_digest(false)
        .open()
        .unwrap();
    lax.next_entry().unwrap();
    assert!(lax.next_entry().unwrap().is_none());
}
