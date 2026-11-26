//! What a turn that ends in an error says: each `codexErrorInfo` variant as a limit of the standard form or as a failure,
//! the facts kept for the turn's `_meta`, and the turn's own tokens and model.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use coqui_acp_api::LimitKind;
use coqui_acp_codex::harness::event::{Event, Stop, TurnEnd};
use serde_json::{Value, json};
use support::parser;

fn events_of(lines: &[Value]) -> Vec<Event> {
    let (mut parser, _) = parser();
    lines.iter().flat_map(|line| parser.line(line)).collect()
}

fn end_of(events: &[Event]) -> &TurnEnd {
    events
        .iter()
        .find_map(|event| match event {
            Event::TurnEnd(end) => Some(end),
            _ => None,
        })
        .expect("a turn end")
}

fn meta_of(events: &[Event]) -> BTreeMap<String, Value> {
    events
        .iter()
        .find_map(|event| match event {
            Event::TurnMeta(meta) => Some(meta.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The recorded cases: the first line is the header that says where they come from.
fn cases() -> (Value, Vec<Value>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codex-0.166.0")
        .join("limits.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap());
    let header = lines.next().unwrap();
    (header, lines.collect())
}

fn kind_name(kind: &LimitKind) -> String {
    serde_json::to_value(kind)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_cases_say_where_they_come_from_and_name_every_variant_of_the_schema() {
    let (header, cases) = cases();
    assert!(header["header"].as_str().unwrap().contains("not recorded"));
    for tag in ["rust-v0.166.0", "rust-v0.172.0"] {
        assert!(
            header["sources"][tag][0].as_str().unwrap().contains(tag),
            "{header}"
        );
    }
    let named: Vec<&str> = cases
        .iter()
        .filter_map(|case| case["case"].as_str())
        .collect();
    // CodexErrorInfo's variants at rust-v0.172.0, the newest schema read.
    for variant in [
        "contextWindowExceeded",
        "sessionBudgetExceeded",
        "usageLimitExceeded",
        "apiKeySpendLimitExceeded",
        "projectSpendLimitExceeded",
        "organizationSpendLimitExceeded",
        "rateLimitExceeded",
        "flexUnavailable",
        "serverOverloaded",
        "modelNotFound",
        "policyCapacityUnavailable",
        "inferenceHookDenied",
        "cyberPolicy",
        "bioPolicy",
        "misalignmentPolicyViolation",
        "tooManyDenials",
        "internalServerError",
        "unauthorized",
        "badRequest",
        "threadRollbackFailed",
        "sandboxError",
        "activeTurnNotSteerable",
        "other",
    ] {
        assert!(named.contains(&variant), "no case for {variant}");
    }
    for variant in [
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
        "responseTooManyFailedAttempts",
    ] {
        assert!(
            named.iter().any(|name| name.starts_with(variant)),
            "no case for {variant}"
        );
    }
}

#[test]
fn each_variant_ends_the_turn_as_the_limit_it_is_or_as_a_failure_and_names_itself_in_the_meta() {
    let (_, cases) = cases();
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let events = events_of(case["notifications"].as_array().unwrap());
        let meta = meta_of(&events);
        for (key, value) in case["tags"].as_object().unwrap() {
            assert_eq!(meta.get(key), Some(value), "{name}: {key} in {meta:?}");
        }
        match (end_of(&events), case["limit"].as_object()) {
            (TurnEnd::Limited(hit), Some(want)) => {
                assert_eq!(
                    kind_name(hit.kind.as_ref().unwrap()),
                    want["kind"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(hit.continuable, want["continuable"].as_bool(), "{name}");
                assert_eq!(
                    hit.resets_at,
                    want.get("resets_at").and_then(Value::as_i64),
                    "{name}"
                );
                assert_eq!(
                    hit.bucket.as_deref(),
                    want.get("bucket").and_then(Value::as_str),
                    "{name}"
                );
                assert_eq!(hit.text.as_deref(), Some("Codex said so."), "{name}");
            }
            (TurnEnd::Failed(why), None) => assert_eq!(why, "Codex said so.", "{name}"),
            (end, want) => panic!("{name}: ended {end:?}, wanted {want:?}"),
        }
    }
}

#[test]
fn an_error_that_codex_will_retry_is_no_limit_and_a_turn_with_no_error_says_nothing_of_one() {
    let events = events_of(&[
        json!({"method": "error", "params": {"willRetry": true, "turnId": "u1", "error": {"message": "again", "codexErrorInfo": "serverOverloaded"}}}),
        json!({"method": "turn/completed", "params": {"turn": {"id": "u1", "status": "completed"}}}),
    ]);
    assert_eq!(end_of(&events), &TurnEnd::stopped(Stop::EndTurn));
    assert!(!meta_of(&events).contains_key("codex:error_info"));
}

#[test]
fn a_reached_type_of_an_earlier_turn_names_no_later_limit() {
    let (mut parser, _) = parser();
    parser.line(
        &json!({"method": "account/rateLimits/updated", "params": {"rateLimits": {
        "limitId": "codex", "rateLimitReachedType": "workspace_owner_credits_depleted"}}}),
    );
    parser.prompting();
    let events: Vec<Event> = [
        json!({"method": "error", "params": {"willRetry": false, "error": {"message": "m", "codexErrorInfo": "usageLimitExceeded"}}}),
        json!({"method": "turn/completed", "params": {"turn": {"id": "u2", "status": "failed"}}}),
    ]
    .iter()
    .flat_map(|line| parser.line(line))
    .collect();
    let TurnEnd::Limited(hit) = end_of(&events) else {
        panic!("{:?}", end_of(&events))
    };
    assert_eq!(hit.kind, Some(LimitKind::UsageQuota));
}

fn usage_line(turn: &str, total: [u64; 4], last: [u64; 4]) -> Value {
    let breakdown = |counts: [u64; 4]| {
        json!({"inputTokens": counts[0], "cachedInputTokens": counts[1], "outputTokens": counts[2],
        "reasoningOutputTokens": counts[3], "totalTokens": counts[0] + counts[2]})
    };
    json!({"method": "thread/tokenUsage/updated", "params": {"threadId": "t1", "turnId": turn,
        "tokenUsage": {"total": breakdown(total), "last": breakdown(last), "modelContextWindow": 272_000}}})
}

#[test]
fn a_turns_tokens_are_the_threads_total_less_what_it_was_when_the_turn_began() {
    let (mut parser, _) = parser();
    let turn_of = |events: Vec<Event>| {
        events
            .into_iter()
            .find_map(|event| match event {
                Event::Usage(usage) => usage.turn,
                _ => None,
            })
            .expect("a usage with the turn's tokens")
    };
    // A resumed thread that already holds 1000 input (400 cached), 200 output (50 reasoning): the first request of the turn
    // is told with that total, and the turn starts from the total less its own request.
    parser.prompting();
    let first = turn_of(parser.line(&usage_line("u1", [1100, 400, 230, 60], [100, 0, 30, 10])));
    assert_eq!(
        (first.input, first.cache_read, first.output, first.reasoning),
        (100, 0, 30, Some(10))
    );
    let second = turn_of(parser.line(&usage_line("u1", [1400, 600, 300, 80], [300, 200, 70, 20])));
    assert_eq!(
        (
            second.input,
            second.cache_read,
            second.output,
            second.reasoning
        ),
        // 400 input in the turn, of which 200 cached.
        (200, 200, 100, Some(30))
    );
    // The next turn counts from where the last one left the total.
    parser.prompting();
    let next = turn_of(parser.line(&usage_line("u2", [1500, 600, 320, 80], [100, 0, 20, 0])));
    assert_eq!(
        (next.input, next.cache_read, next.output, next.reasoning),
        (100, 0, 20, Some(0))
    );
}

#[test]
fn the_turns_end_says_the_model_it_ran_on_and_a_reroute_moves_it() {
    let (mut parser, _) = parser();
    parser.set_model(Some("gpt-6.1-sol"));
    parser.prompting();
    parser.set_model(Some("gpt-6.1-sol"));
    let done = json!({"method": "turn/completed", "params": {"turn": {"id": "u1", "status": "completed"}}});
    let plain = parser.line(&done);
    assert_eq!(
        meta_of(&plain).get("model"),
        Some(&json!("gpt-6.1-sol")),
        "{plain:?}"
    );
    parser.prompting();
    let rerouted = parser.line(
        &json!({"method": "model/rerouted", "params": {"threadId": "t1", "turnId": "u2",
        "fromModel": "gpt-6.1-sol", "toModel": "gpt-6.1-safe", "reason": "highRiskCyberActivity"}}),
    );
    assert!(
        matches!(rerouted.as_slice(), [Event::Notice { .. }]),
        "{rerouted:?}"
    );
    let ended = parser.line(&done);
    let meta = meta_of(&ended);
    assert_eq!(meta.get("model"), Some(&json!("gpt-6.1-safe")));
    assert_eq!(meta.get("codex:rerouted_from"), Some(&json!("gpt-6.1-sol")));
    assert_eq!(
        meta.get("codex:reroute_reason"),
        Some(&json!("highRiskCyberActivity"))
    );
    // A turn that follows is not the rerouted one's.
    parser.prompting();
    parser.set_model(Some("gpt-6.1-sol"));
    let later = meta_of(&parser.line(&done));
    assert_eq!(later.get("model"), Some(&json!("gpt-6.1-sol")));
    assert!(!later.contains_key("codex:rerouted_from"));
}
