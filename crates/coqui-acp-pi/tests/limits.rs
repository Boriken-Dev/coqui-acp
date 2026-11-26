//! A provider's limit as the gate extension reports it: read from Pi 1.6.1's recorded output, and the extension's own
//! classification over the shapes of each provider's errors.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use coqui_acp_api::usage::{LimitKind, Source};
use coqui_acp_pi::GATE_SOURCE;
use coqui_acp_pi::harness::event::{Event, Severity, TurnEnd};
use serde_json::Value;
use support::{Scratch, parsed_lines, recorded_by};

const PI: &str = "pi-1.6.1";
const KEY: &str = "coqui-acp-pi-report";

fn end_of(events: &[Event]) -> &TurnEnd {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::TurnEnd(end) => Some(end),
            _ => None,
        })
        .expect("a turn ended")
}

/// The recording with the extension's reports taken out: what Pi alone says.
fn without_reports(lines: Vec<Value>) -> Vec<Value> {
    lines
        .into_iter()
        .filter(|line| line["statusKey"] != KEY)
        .collect()
}

#[test]
fn a_rate_limit_is_read_from_the_provider_with_its_bucket_reset_and_wait() {
    let (events, _) = parsed_lines(recorded_by(PI, "limit_rate"));
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("a 429 ended {:?}", end_of(&events));
    };
    assert_eq!(hit.kind, Some(LimitKind::RateLimit));
    assert_eq!(hit.source, Source::Reported);
    assert_eq!(hit.bucket.as_deref(), Some("requests"));
    assert_eq!(hit.resets_at, Some(1_795_674_844));
    assert_eq!(hit.retry_after, Some(Duration::from_secs(7)));
    assert_eq!(hit.continuable, Some(true));
    assert!(
        hit.text
            .as_deref()
            .is_some_and(|text| text.starts_with("Rate limit reached for stub-fast")),
        "{:?}",
        hit.text
    );
}

#[test]
fn a_spent_balance_is_a_credit_limit_that_names_no_reset() {
    let (events, _) = parsed_lines(recorded_by(PI, "limit_quota"));
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("a 429 ended {:?}", end_of(&events));
    };
    assert_eq!(hit.kind, Some(LimitKind::Credit));
    assert_eq!(hit.source, Source::Reported);
    assert_eq!(
        (hit.bucket.as_deref(), hit.resets_at, hit.retry_after),
        (None, None, None)
    );
}

#[test]
fn a_limit_that_outlasts_pis_retries_is_the_turns_and_the_retry_is_told() {
    let (events, _) = parsed_lines(recorded_by(PI, "limit_retry"));
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("a retried 429 ended {:?}", end_of(&events));
    };
    assert_eq!(hit.kind, Some(LimitKind::RateLimit));
    assert_eq!(hit.source, Source::Reported);
    assert!(events.iter().any(|event| matches!(event,
        Event::Notice { severity: Severity::Info, title } if title.starts_with("Pi is retrying (attempt 1 of 1)"))));
}

/// The control: with the reports gone, Pi's words are all there is, and the same turn is a limit read from them.
#[test]
fn without_the_report_the_words_are_the_fallback() {
    for name in ["limit_rate", "limit_quota"] {
        let (events, _) = parsed_lines(without_reports(recorded_by(PI, name)));
        let TurnEnd::Limited(hit) = end_of(&events) else {
            panic!("{name} ended {:?}", end_of(&events));
        };
        assert_eq!(hit.source, Source::Inferred, "{name}");
        assert_eq!(hit.resets_at, None, "{name}");
    }
}

#[test]
fn a_report_of_an_earlier_message_is_not_the_failures() {
    // The first attempt's report does not stand for a later failure that has none.
    let mut lines = recorded_by(PI, "limit_retry");
    let reports: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line["statusKey"] == KEY)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(reports.len(), 2);
    lines.remove(reports[1]);
    let (events, _) = parsed_lines(lines);
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("ended {:?}", end_of(&events));
    };
    assert_eq!(
        hit.source,
        Source::Inferred,
        "the first attempt's report was used"
    );
}

#[test]
fn a_status_of_another_key_is_no_report() {
    let mut lines = recorded_by(PI, "limit_rate");
    for line in &mut lines {
        if line["statusKey"] == KEY {
            line["statusKey"] = "someone-else".into();
        }
    }
    let (events, _) = parsed_lines(lines);
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("ended {:?}", end_of(&events));
    };
    assert_eq!(hit.source, Source::Inferred);
}

/// What the extension makes of each provider's error: the cases in `tests/fixtures/report_cases.json` run through its
/// own code, a field at a time (`null` is a field it has not).
#[test]
#[ignore = "needs a Node with .ts support: set COQUI_ACP_PI_REAL_NODE to its executable"]
fn the_extension_classifies_each_providers_errors() {
    let node =
        std::env::var("COQUI_ACP_PI_REAL_NODE").expect("COQUI_ACP_PI_REAL_NODE names a Node");
    let scratch = Scratch::new("report-cases");
    let extension = scratch.0.join("gate.ts");
    std::fs::write(&extension, GATE_SOURCE.replace("__NONCE__", "test")).unwrap();
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let cases = here.join("fixtures").join("report_cases.json");
    let out = Command::new(node)
        .arg(here.join("support").join("report_driver.mjs"))
        .arg(&extension)
        .arg(&cases)
        .output()
        .expect("node runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let wanted: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(cases).unwrap()).unwrap();
    assert_eq!(got.len(), wanted.len());
    for (got, wanted) in got.iter().zip(&wanted) {
        let name = wanted["name"].as_str().unwrap();
        if wanted["expect"].is_null() {
            assert!(got.is_null(), "{name}: reported {got}");
            continue;
        }
        assert_eq!(got["key"], KEY, "{name}");
        contains(&got["report"], &wanted["expect"], name, "");
    }
}

/// Every field `expected` names is in `got` with that value; a `null` there is a field `got` has not.
fn contains(got: &Value, expected: &Value, name: &str, path: &str) {
    let Some(fields) = expected.as_object() else {
        assert_eq!(got, expected, "{name}: {path}");
        return;
    };
    for (key, wanted) in fields {
        let here = format!("{path}/{key}");
        let have = got.get(key);
        if wanted.is_null() {
            assert!(
                have.is_none_or(Value::is_null),
                "{name}: {here} is {have:?}"
            );
        } else {
            let have = have.unwrap_or_else(|| panic!("{name}: {here} is missing in {got}"));
            contains(have, wanted, name, &here);
        }
    }
}

/// Pi 1.5.2 does not tell an extension of an OpenAI error response, so the report has the status, the type and the code
/// from Pi's own error text, and no headers: the kind is known, the reset and the wait are not.
#[test]
fn pi_1_5_2_gives_the_kind_without_the_headers() {
    let (events, _) = parsed_lines(recorded_by("pi-1.5.2", "limit_rate"));
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("a 429 ended {:?}", end_of(&events));
    };
    assert_eq!(hit.kind, Some(LimitKind::RateLimit));
    assert_eq!(hit.source, Source::Reported);
    assert_eq!(
        (hit.bucket.as_deref(), hit.resets_at, hit.retry_after),
        (None, None, None)
    );
}
