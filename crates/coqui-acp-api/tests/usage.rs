//! The usage model's one spelling: `vectors/usage.json`, read and written by Rust through both of its
//! wires (serde's JSON, the ACP extension's, and Guatiao's value, the kind's), which must agree.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

use coqui_acp_api::guatiao::schema::read::SchemaRef;
use coqui_acp_api::guatiao::schema::validate::validate_map;
use coqui_acp_api::guatiao::{FromValue, Schema, ToValue, Value};
use coqui_acp_api::{
    Bucket, Coverage, LIMIT_META_KEY, Refill, Source, StartRequest, Status, USAGE_CAPABILITY,
    USAGE_METHOD, USAGE_NOTIFICATION, USAGE_VERSION, Unit, UsageQuery, UsageReport, UsageSupport,
};
use guatiao_serde::{Serializable, from_serde};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

fn vectors() -> Json {
    serde_json::from_str(include_str!("../../../vectors/usage.json")).expect("the vectors are JSON")
}

fn cases(group: &str) -> Vec<Json> {
    vectors()[group]
        .as_array()
        .expect("a list of cases")
        .clone()
}

/// Reads `json` through Guatiao's value, as the kind carries it.
fn read_value<T: FromValue>(json: &Json) -> Result<T, String> {
    let value: Value = from_serde(json.clone()).map_err(|e| e.to_string())?;
    T::from_value(&value).map_err(|e| e.to_string())
}

/// Writes `model` through Guatiao's value as JSON.
fn write_value<T: ToValue>(model: &T) -> Json {
    let value = model.to_value().expect("a model is a value");
    serde_json::to_value(Serializable::from(&value)).expect("a value is JSON")
}

fn read_json<T: DeserializeOwned>(json: &Json) -> Result<T, String> {
    serde_json::from_value(json.clone()).map_err(|e| e.to_string())
}

fn write_json<T: Serialize>(model: &T) -> Json {
    serde_json::to_value(model).expect("a model is JSON")
}

#[test]
fn every_canonical_report_reads_and_writes_the_same_through_both_wires() {
    for case in cases("roundtrip") {
        let name = case["name"].as_str().expect("named");
        let report = &case["report"];
        let by_json: UsageReport = read_json(report).unwrap_or_else(|e| panic!("{name}: {e}"));
        let by_value: UsageReport = read_value(report).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            by_json, by_value,
            "{name}: the wires read different reports"
        );
        assert_eq!(&write_json(&by_json), report, "{name}: serde writes");
        assert_eq!(&write_value(&by_value), report, "{name}: Guatiao writes");
    }
}

#[test]
fn what_a_newer_writer_named_reads_as_other_and_never_fails() {
    for case in cases("lenient") {
        let name = case["name"].as_str().expect("named");
        let by_json: UsageReport =
            read_json(&case["read"]).unwrap_or_else(|e| panic!("{name}: {e}"));
        let by_value: UsageReport =
            read_value(&case["read"]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            by_json, by_value,
            "{name}: the wires read different reports"
        );
        assert_eq!(
            write_json(&by_json),
            case["reads_as"],
            "{name}: serde writes"
        );
        assert_eq!(
            write_value(&by_value),
            case["reads_as"],
            "{name}: Guatiao writes"
        );
        let unknown = case["unknown"].as_object().expect("named what is unknown");
        for report in [&by_json, &by_value] {
            if let Some(bucket) = report.buckets.first() {
                assert_eq!(
                    matches!(bucket.unit, Unit::Other(_)),
                    unknown.contains_key("unit"),
                    "{name}: unit"
                );
                assert_eq!(
                    matches!(bucket.status, Status::Other(_)),
                    unknown.contains_key("status"),
                    "{name}: status"
                );
                assert_eq!(
                    matches!(bucket.source, Source::Other(_)),
                    unknown.contains_key("source"),
                    "{name}: source"
                );
                assert_eq!(
                    bucket.refill == Refill::Other,
                    unknown.contains_key("refill"),
                    "{name}: refill"
                );
                assert_eq!(
                    bucket.models == Coverage::Other,
                    unknown.contains_key("models"),
                    "{name}: coverage"
                );
            }
        }
    }
}

#[test]
fn a_name_equal_to_a_known_one_is_that_variant() {
    assert_eq!(Unit::from(String::from("tokens")), Unit::Tokens);
    assert_eq!(Status::from(String::from("ok")), Status::Ok);
    assert_eq!(Source::from(String::from("inferred")), Source::Inferred);
    assert_eq!(Unit::Other("credits".into()).as_str(), "credits");
}

#[test]
fn a_request_with_a_key_nobody_declared_is_refused() {
    for case in cases("refused") {
        let name = case["name"].as_str().expect("named");
        let refused = match case["type"].as_str().expect("typed") {
            "UsageQuery" => read_value::<UsageQuery>(&case["json"]).is_err(),
            "StartRequest" => read_value::<StartRequest>(&case["json"]).is_err(),
            other => panic!("{name}: no type {other}"),
        };
        assert!(refused, "{name}: a key nobody declared was taken");
    }
    let query = read_value::<UsageQuery>(&json!({"account": "pool/alice"})).expect("declared");
    assert_eq!(query.account.as_deref(), Some("pool/alice"));
}

fn bucket_of(figures: &Json) -> Bucket {
    let mut bucket = Bucket::new("b");
    bucket.used = figures["used"].as_u64();
    bucket.limit = figures["limit"].as_u64();
    bucket.remaining = figures["remaining"].as_u64();
    bucket
}

#[test]
fn the_fraction_is_used_over_limit_else_what_is_left() {
    for case in cases("fraction") {
        let name = case["name"].as_str().expect("named");
        let expected = case["permille"]
            .as_u64()
            .map(|p| u16::try_from(p).expect("a permille"));
        assert_eq!(
            UsageReport::fraction_permille(&bucket_of(&case["bucket"])),
            expected,
            "{name}"
        );
    }
    let mut huge = Bucket::new("b");
    huge.used = Some(u64::MAX);
    huge.limit = Some(u64::MAX);
    assert_eq!(UsageReport::fraction_permille(&huge), Some(1000));
    huge.used = Some(u64::MAX / 2);
    assert_eq!(UsageReport::fraction_permille(&huge), Some(499));
}

#[test]
fn a_window_says_how_long_until_it_ends() {
    for case in cases("resets_in") {
        let name = case["name"].as_str().expect("named");
        let mut bucket = Bucket::new("b");
        bucket.refill = read_json(&case["refill"]).unwrap_or_else(|e| panic!("{name}: {e}"));
        let now = case["now"].as_i64().expect("a time");
        assert_eq!(
            bucket.resets_in(now).map(|left| left.as_secs()),
            case["seconds"].as_u64(),
            "{name}"
        );
    }
}

#[test]
fn an_unsupported_report_and_an_empty_one_are_different_answers() {
    let unsupported = UsageReport::unsupported("nothing here");
    assert!(!unsupported.supported && unsupported.buckets.is_empty());
    assert_eq!(
        unsupported.unsupported_because.as_deref(),
        Some("nothing here")
    );
    let empty = UsageReport::new();
    assert!(empty.supported && empty.unsupported_because.is_none());
    assert_ne!(unsupported, empty);
    assert!(!UsageSupport::none().reports());
    assert!(UsageSupport::new(vec![Source::Reported], false).reports());
}

/// The names a client and an agent match as text, in every language.
#[test]
fn the_extension_names_are_pinned() {
    assert_eq!(USAGE_CAPABILITY, "dev.boriken.coqui.acp/usage");
    assert_eq!(USAGE_METHOD, "_coqui_acp/usage");
    assert_eq!(USAGE_NOTIFICATION, "_coqui_acp/usage_update");
    assert_eq!(LIMIT_META_KEY, "dev.boriken.coqui.acp/limit");
    assert_eq!(USAGE_VERSION, 1);
}

#[test]
fn every_variant_validates_against_its_own_schema() {
    // A value the model writes is one its schema accepts, for the tagged enums the codec reads leniently too.
    for case in cases("roundtrip") {
        let report: UsageReport = read_json(&case["report"]).expect("read");
        let schema = UsageReport::schema().expect("described");
        let value = report.to_value().expect("a value");
        validate_map(SchemaRef::new(&schema).expect("a schema"), &value)
            .unwrap_or_else(|e| panic!("{}: {e:?}", case["name"]));
    }
}
