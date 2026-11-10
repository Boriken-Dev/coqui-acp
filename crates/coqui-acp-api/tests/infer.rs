//! `infer_limit`: a failed turn's text read as a limit, from the recorded texts of agents that report
//! no usage, and from hostile input.

use core::time::Duration;

use coqui_acp_api::{DEFAULT_LIMIT_PHRASES, Refill, Source, Status, UsageReport, infer_limit};

const AGY: &str = include_str!("usage_text/agy_429_failed_turn.txt");
const GATEWAY: &str = include_str!("usage_text/litellm_429_gateway.json");
const PROVIDER: &str = include_str!("usage_text/litellm_429_provider.json");

fn defaults(text: &str) -> Option<coqui_acp_api::LimitHit> {
    infer_limit(text, DEFAULT_LIMIT_PHRASES)
}

fn line_starting<'a>(text: &'a str, start: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(start))
        .expect("the line is in the recorded text")
}

#[test]
fn the_recorded_agy_429_is_a_limit_named_by_its_earliest_phrase() {
    let google = AGY.split("\n2. ").next().expect("the first recorded text");
    let hit = defaults(google).expect("a limit");
    assert_eq!(hit.source, Source::Inferred);
    assert_eq!(hit.bucket.as_deref(), Some("inferred"));
    assert_eq!(hit.text.as_deref(), Some("Resource has been exhausted"));
    assert_eq!(
        (hit.resets_at, hit.retry_after, hit.observed_at),
        (None, None, 0)
    );
}

#[test]
fn the_capacity_text_of_agy_names_the_quota_it_resets_without_a_time() {
    let line = line_starting(AGY, "RESOURCE_EXHAUSTED (code 429)");
    let hit = defaults(line).expect("a limit");
    assert_eq!(hit.text.as_deref(), Some("RESOURCE_EXHAUSTED"));
    assert_eq!(hit.retry_after, None, "`after ...` states no length");
}

#[test]
fn the_503_of_agy_is_capacity_and_not_a_limit() {
    let line = line_starting(AGY, "UNAVAILABLE (code 503)");
    let text = line.split("   (").next().expect("the text before the note");
    assert_eq!(
        text,
        "UNAVAILABLE (code 503): No capacity available for model ..."
    );
    assert_eq!(defaults(text), None);
    assert_eq!(
        defaults("503 No capacity available for model gemini-3-pro"),
        None
    );
}

#[test]
fn both_litellm_throttles_are_limits() {
    let gateway = defaults(GATEWAY).expect("a limit");
    assert_eq!(gateway.text.as_deref(), Some("rate limit"));
    assert_eq!(
        gateway.resets_at, None,
        "the recorded time is a placeholder"
    );
    let provider = defaults(PROVIDER).expect("a limit");
    assert_eq!(provider.text.as_deref(), Some("rate limit"));
    let bare =
        defaults(&PROVIDER.replace("Rate limit reached ...", "overloaded")).expect("a limit");
    assert_eq!(bare.text.as_deref(), Some("throttling_error"));
}

#[test]
fn each_default_phrase_matches_in_any_case_and_names_itself() {
    for phrase in DEFAULT_LIMIT_PHRASES {
        for text in [
            format!("failed: {phrase}."),
            format!("failed: {}.", phrase.to_uppercase()),
            format!("failed: {}.", phrase.to_lowercase()),
        ] {
            let hit = defaults(&text).unwrap_or_else(|| panic!("{phrase} in {text}"));
            assert_eq!(hit.text.as_deref(), Some(*phrase), "{text}");
        }
    }
}

#[test]
fn an_unrelated_failure_is_no_limit() {
    for text in [
        "",
        "connection reset by peer",
        "Error 500: internal",
        "invalid api key",
        "No capacity available",
    ] {
        assert_eq!(defaults(text), None, "{text}");
    }
}

#[test]
fn no_phrases_infer_nothing_and_a_host_list_replaces_the_defaults() {
    assert_eq!(infer_limit("RESOURCE_EXHAUSTED quota", &[]), None);
    assert_eq!(infer_limit("RESOURCE_EXHAUSTED quota", &[""]), None);
    assert_eq!(
        infer_limit("RESOURCE_EXHAUSTED quota", &["out of tokens"]),
        None
    );
    let hit = infer_limit("You are Out Of Tokens", &["out of tokens"]).expect("a limit");
    assert_eq!(hit.text.as_deref(), Some("out of tokens"));
}

#[test]
fn the_earliest_phrase_wins() {
    let hit = infer_limit("a quota, then a rate limit", &["rate limit", "quota"]).expect("a limit");
    assert_eq!(hit.text.as_deref(), Some("quota"));
}

#[test]
fn evidence_is_the_phrase_cut_short_never_the_text() {
    let long = "é".repeat(100);
    let text = format!("x {long} y");
    let hit = infer_limit(&text, &[&long]).expect("a limit");
    let evidence = hit.text.expect("evidence");
    assert!(evidence.len() <= 120 && long.starts_with(&evidence));
    let hit = defaults("a very long preamble. quota. a very long epilogue").expect("a limit");
    assert_eq!(hit.text.as_deref(), Some("quota"));
}

#[test]
fn a_stated_length_is_the_retry_after() {
    let cases: [(&str, Option<u64>); 11] = [
        ("quota. Retry after 30 seconds", Some(30)),
        ("quota. Your quota will reset after 2h30m15s.", Some(9015)),
        ("quota. Try again in 1 hour and 5 minutes", Some(3900)),
        ("quota. resets in 3 days", Some(259_200)),
        ("rate limit\nretry-after: 17", Some(17)),
        ("rate limit. retry after 17", None),
        ("quota. retry after 1.5 hours", None),
        ("quota. retry after 5m 1h", None),
        ("quota. retry after 10 fortnights", None),
        ("quota. retry after 99999999999 seconds", None),
        ("quota will reset after ...", None),
    ];
    for (text, seconds) in cases {
        let hit = defaults(text).expect("a limit");
        assert_eq!(hit.retry_after, seconds.map(Duration::from_secs), "{text}");
        assert_eq!(hit.resets_at, None, "{text}");
    }
}

#[test]
fn a_stated_utc_time_is_the_reset() {
    let cases = [
        (
            "rate limit. Limit reset at: 2026-11-10T12:00:00Z",
            Some(1_794_312_000),
        ),
        (
            "rate limit. resets at 1970-01-02 00:00:01.250+00:00",
            Some(86_401),
        ),
        (
            "rate limit. reset_at=\"2024-02-29T23:59:59Z\"",
            Some(1_709_251_199),
        ),
        ("rate limit. reset at: 2026-11-10T12:00:00", None),
        ("rate limit. reset at: 2026-11-10T12:00:00+05:00", None),
        ("rate limit. reset at: 2026-02-30T12:00:00Z", None),
        ("rate limit. reset at: 2026-13-10T12:00:00Z", None),
        ("rate limit. reset at: 2026-11-10T24:00:00Z", None),
        ("rate limit. reset at: <time>", None),
        ("rate limit. reset at: 2026-11-10T12:0", None),
    ];
    for (text, at) in cases {
        let hit = defaults(text).expect("a limit");
        assert_eq!(hit.resets_at, at, "{text}");
    }
}

#[test]
fn hostile_text_neither_panics_nor_scans_without_bound() {
    let near = "RESOURCE_EXHAUSTE ".repeat(200_000);
    assert_eq!(defaults(&near), None);
    let marker = "retry after 9 ".repeat(100_000);
    assert_eq!(
        defaults(&format!("{marker} quota")),
        None,
        "beyond the scanned prefix"
    );
    assert!(defaults(&format!("quota {marker}")).is_some());
    let digits = format!("quota retry after {}", "9".repeat(1_000_000));
    assert_eq!(defaults(&digits).expect("a limit").retry_after, None);
    let wide = "€".repeat(100_000);
    assert_eq!(defaults(&wide), None);
    let cut = format!("{}quota", "é".repeat(32 * 1024 - 2));
    let _ = defaults(&cut);
    let _ = defaults("reset at: 2026-11-10T12:00:00");
    let _ = defaults("quota retry in 9 ");
    let _ = defaults("quota retry-after:");
    let _ = infer_limit("quota", &["q".repeat(300).as_str(), "quota"]);
}

#[test]
fn a_phrase_over_256_bytes_is_skipped() {
    let long = "q".repeat(300);
    assert_eq!(infer_limit(&long, &[&long]), None);
}

#[test]
fn the_host_report_of_an_agent_that_reports_nothing_is_one_exhausted_inferred_bucket() {
    let mut hit = defaults("Error 429. Limit reset at: 2026-11-10T12:00:00Z").expect("a limit");
    hit.observed_at = 1_794_300_000;
    let report = UsageReport::inferred(hit.clone());
    assert!(report.supported);
    assert_eq!(report.sources, [Source::Inferred]);
    assert_eq!(report.observed_at, 1_794_300_000);
    assert_eq!(report.limit.as_ref(), Some(&hit));
    let [bucket] = report.buckets.as_slice() else {
        panic!("one bucket");
    };
    assert_eq!(bucket.id, "inferred");
    assert_eq!(bucket.status, Status::Exhausted);
    assert_eq!(bucket.source, Source::Inferred);
    assert_eq!(bucket.evidence.as_deref(), Some("Error 429"));
    assert_eq!(
        (bucket.used, bucket.limit, bucket.remaining),
        (None, None, None)
    );
    assert_eq!(
        bucket.refill,
        Refill::FixedWindow {
            length: None,
            resets_at: Some(1_794_312_000)
        }
    );
    assert_eq!(
        bucket.resets_in(1_794_300_000),
        Some(Duration::from_secs(12_000))
    );

    let unknown = UsageReport::inferred(defaults("quota").expect("a limit"));
    assert_eq!(unknown.buckets[0].refill, Refill::Unknown);
}
