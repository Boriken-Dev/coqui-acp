//! The contract other programs are built against: the kinds' hashes and description, the strings consumers
//! match on, and the events' JSON.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;

use coqui_acp_provider::guatiao::{FromValue, ToValue};
use coqui_acp_provider::{
    AgentEvent, AgentMessage, AgentProviderVtable, AgentSessionVtable, BLOCKED_EVENT,
    CANCELLED_EVENT, COMPLETED_EVENT, DENIED_EVENT, EVENT_NOTIFICATION, EVENTS_CAPABILITY,
    FAILED_EVENT, LAUNCH_META_KEY, LIMIT_EVENT, QuotaReport, ResumeRequest, StartRequest,
    TEXT_EVENT, TOOL_EVENT, TURN_CANCELLED, TokenUsage, UsageLimit,
};
use serde_json::{Value, json};

fn file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// Every kind of the crate, in declaration order, as one document.
fn rendered() -> String {
    let document = format!(
        "{{\"kinds\":[{},{}]}}",
        AgentProviderVtable::description(),
        AgentSessionVtable::description()
    );
    let value: Value = serde_json::from_str(&document).expect("a kind's description is JSON");
    let mut text = serde_json::to_string_pretty(&value).expect("written");
    text.push('\n');
    text
}

/// Regenerate with `COQUI_ACP_PROVIDER_WRITE_KINDS=1 cargo test -p coqui-acp-provider --test kinds`.
#[test]
fn the_committed_description_is_what_this_build_describes() {
    let path = file("include/coqui_acp_provider.kinds.json");
    let fresh = rendered();
    if std::env::var_os("COQUI_ACP_PROVIDER_WRITE_KINDS").is_some() {
        std::fs::write(&path, &fresh).expect("the description is written");
    }
    let committed = std::fs::read_to_string(&path).expect("the kind description is committed");
    assert!(
        committed == fresh,
        "regenerate include/coqui_acp_provider.kinds.json"
    );
}

/// Changing a slot, a model's name or a kind's name breaks every built provider. This names the numbers, so
/// doing that is a decision.
#[test]
fn the_kinds_hashes_are_pinned() {
    assert_eq!(AgentProviderVtable::FLOOR_HASH, PROVIDER_HASH);
    assert_eq!(AgentSessionVtable::FLOOR_HASH, SESSION_HASH);
}

#[test]
fn the_api_header_says_those_hashes() {
    let header = std::fs::read_to_string(file("AGENTS.md")).expect("the header ships");
    for hash in [PROVIDER_HASH, SESSION_HASH] {
        assert!(header.contains(&hash.to_string()), "AGENTS.md lacks {hash}");
    }
}

const PROVIDER_HASH: u32 = 1182182788;
const SESSION_HASH: u32 = 1414216972;

#[test]
fn the_python_kinds_carry_those_hashes() {
    let module = std::fs::read_to_string(file("python/coqui_acp_provider_kinds.py"))
        .expect("the Python kinds are committed");
    for (kind, hash) in [
        ("agent_provider", PROVIDER_HASH),
        ("agent_session", SESSION_HASH),
    ] {
        assert!(
            module.contains(&format!("kind=\"{kind}\", floor_hash={hash}")),
            "regenerate python/coqui_acp_provider_kinds.py for {kind}"
        );
    }
}

#[test]
fn the_strings_consumers_match_on_stay_what_they_are() {
    assert_eq!(TEXT_EVENT, "text");
    assert_eq!(TOOL_EVENT, "tool");
    assert_eq!(BLOCKED_EVENT, "blocked");
    assert_eq!(COMPLETED_EVENT, "completed");
    assert_eq!(FAILED_EVENT, "failed");
    assert_eq!(CANCELLED_EVENT, "cancelled");
    assert_eq!(LIMIT_EVENT, "limit");
    assert_eq!(DENIED_EVENT, "denied");
    assert_eq!(TURN_CANCELLED, "the agent cancelled the turn");
    assert_eq!(LAUNCH_META_KEY, "dev.boriken.coqui.acp/launch");
    assert_eq!(EVENTS_CAPABILITY, "dev.boriken.coqui.acp/events");
    assert_eq!(EVENT_NOTIFICATION, "_coqui_acp/event");
}

fn full_event() -> AgentEvent {
    AgentEvent {
        kind: "limit".to_owned(),
        text: Some("stopped".to_owned()),
        data: Some(vec![1, 2]),
        limit: Some(UsageLimit {
            resets_at: Some(5),
            window: Some("weekly".to_owned()),
        }),
        usage: Some(TokenUsage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cost_micro_usd: 5,
            cumulative: true,
        }),
        tools: vec!["WebFetch".to_owned()],
        quota: vec![QuotaReport {
            window: "weekly".to_owned(),
            used_permille: Some(10),
            resets_at: None,
            status: Some("allowed".to_owned()),
        }],
    }
}

#[test]
fn an_events_json_shape_is_exact() {
    let bare = AgentEvent::new("text", Some("hi".to_owned()));
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"kind":"text","text":"hi","data":null,"limit":null,"usage":null,"tools":[]}"#,
        "no quota key when there is none"
    );
    assert_eq!(
        serde_json::to_value(full_event()).unwrap(),
        json!({
            "kind": "limit",
            "text": "stopped",
            "data": [1, 2],
            "limit": {"resets_at": 5, "window": "weekly"},
            "usage": {
                "input": 1, "output": 2, "cache_read": 3, "cache_write": 4,
                "cost_micro_usd": 5, "cumulative": true,
            },
            "tools": ["WebFetch"],
            "quota": [{
                "window": "weekly", "used_permille": 10, "resets_at": null, "status": "allowed",
            }],
        })
    );
    let read: AgentEvent = serde_json::from_str(
        r#"{"kind":"text","text":"hi","data":null,"limit":null,"usage":null,"tools":[]}"#,
    )
    .unwrap();
    assert_eq!(read, bare, "an absent quota reads as none");
    assert!(
        serde_json::from_str::<AgentEvent>(r#"{"kind":"text"}"#).is_err(),
        "tools is required in JSON"
    );
    assert_eq!(
        serde_json::to_string(&AgentMessage {
            text: "a".to_owned()
        })
        .unwrap(),
        r#"{"text":"a"}"#
    );
}

#[test]
fn an_event_crosses_as_a_value_and_back() {
    let event = full_event();
    assert_eq!(
        AgentEvent::from_value(&event.to_value().unwrap()).unwrap(),
        event
    );
    let usage = TokenUsage::default();
    assert_eq!(
        TokenUsage::from_value(&usage.to_value().unwrap()).unwrap(),
        usage
    );
}

#[test]
fn a_provider_in_another_language_may_leave_out_what_it_has_no_use_for() {
    let mut map = coqui_acp_provider::guatiao::Map::new();
    map.set("kind", "text");
    let event = AgentEvent::from_value(&coqui_acp_provider::guatiao::Value::from(map)).unwrap();
    assert_eq!(event, AgentEvent::new("text", None));
}

#[test]
fn no_debug_shows_an_environment_value() {
    let mut start = StartRequest::new("/work", "hi");
    start
        .environment
        .insert("TOKEN".to_owned(), "sk-secret".to_owned());
    let mut resume = ResumeRequest::new("id", "/work");
    resume
        .environment
        .insert("TOKEN".to_owned(), "sk-secret".to_owned());
    let mut settings = coqui_acp_provider::Settings::default();
    settings
        .environment
        .insert("TOKEN".to_owned(), "sk-secret".to_owned());
    for shown in [
        format!("{start:?}"),
        format!("{resume:?}"),
        format!("{settings:?}"),
    ] {
        assert!(shown.contains("TOKEN"), "{shown}");
        assert!(!shown.contains("sk-secret"), "{shown}");
    }
}

#[test]
fn requests_cross_as_values_and_need_only_what_they_must_have() {
    let mut start = StartRequest::new("/work", "hi");
    start.context = vec!["c".to_owned()];
    start.model = Some("m".to_owned());
    start.environment.insert("A".to_owned(), "b".to_owned());
    assert_eq!(
        StartRequest::from_value(&start.to_value().unwrap()).unwrap(),
        start
    );

    let mut resume = ResumeRequest::new("id", "/work");
    resume.prompt = Some("hi".to_owned());
    assert_eq!(
        ResumeRequest::from_value(&resume.to_value().unwrap()).unwrap(),
        resume
    );

    let mut map = coqui_acp_provider::guatiao::Map::new();
    map.set("working_directory", "/work");
    map.set("prompt", "hi");
    let bare = StartRequest::from_value(&coqui_acp_provider::guatiao::Value::from(map)).unwrap();
    assert_eq!(bare, StartRequest::new("/work", "hi"));
}
