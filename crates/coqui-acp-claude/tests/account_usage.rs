//! The plan limits of a Claude account read on their own, against a loopback stand-in for the OAuth API: what is
//! asked, with which credential, what comes back, and what is never sent.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::usage::{Coverage, Refill, Status as Room};
use coqui_acp_api::{AccountGrant, Credentials};
use coqui_acp_claude::account_usage::{ReadOptions, read_limits};
use coqui_acp_claude::accounts::{API_KEY, AUTH_TOKEN, CONSOLE, FILE_KEY, LOGIN, OAUTH_TOKEN};

const TOKEN: &str = "sk-ant-oat01-canary-5d81";
const NOW: i64 = 1_794_800_000;

/// What the stand-in saw of one request.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A server on a loopback port that answers each request with the next of `answers` (status, extra header, body),
/// and the last again.
fn stand_in(answers: Vec<(u16, Option<String>, String)>) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let addr = listener.local_addr().expect("its address");
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            let request = read_request(&mut stream);
            log.lock().expect("log").push(request);
            let (code, extra, body) = &answers[index.min(answers.len() - 1)];
            let extra = extra
                .as_deref()
                .map(|h| format!("{h}\r\n"))
                .unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 {code} X\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (addr, seen)
}

fn read_request(stream: &mut TcpStream) -> Seen {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut lines = text.split("\r\n");
    let line = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(": "))
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    Seen { line, headers }
}

fn document() -> String {
    serde_json::json!({
        "five_hour": {"utilization": 12.5, "resets_at": "2026-11-16T20:00:00+00:00"},
        "seven_day": {"utilization": 34.0, "resets_at": "2026-11-20T10:00:00+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": {"utilization": 5.0, "resets_at": "2026-11-20T10:00:00+00:00"},
        "model_scoped": [{"display_name": "Fable", "utilization": 61.0, "resets_at": "2026-11-21T00:00:00Z"}],
    })
    .to_string()
}

fn login_file(scopes: &str, expires_ms: i64) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","refreshToken":"sk-ant-ort01-canary","expiresAt":{expires_ms},"scopes":{scopes},"subscriptionType":"max"}}}}"#
    )
}

fn login(file: &str) -> AccountGrant {
    AccountGrant::new("owner", LOGIN)
        .credentials(Credentials::from_iter([(FILE_KEY, file)]))
        .state_dir("/some/dir")
}

fn options(addr: SocketAddr) -> ReadOptions {
    ReadOptions::new()
        .endpoint(format!("http://{addr}/"))
        .now(NOW)
}

const LATER: i64 = (NOW + 3600) * 1000;
const PROFILE: &str = r#"["user:inference","user:profile"]"#;

#[test]
fn a_subscription_login_reads_its_windows_with_the_oauth_token() {
    let (addr, seen) = stand_in(vec![(200, None, document())]);
    let report = read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect("read");
    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].line, "GET /api/oauth/usage HTTP/1.1");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(seen[0].header("anthropic-beta"), Some("oauth-2025-04-20"));

    assert!(report.supported);
    assert_eq!(report.account.as_deref(), Some("owner"));
    let ids: Vec<&str> = report.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "five_hour",
            "seven_day",
            "seven_day_sonnet",
            "seven_day_model_fable"
        ]
    );
    assert_eq!(report.buckets[0].used, Some(125));
    assert_eq!(report.buckets[0].limit, Some(1000));
    assert_eq!(report.buckets[0].status, Room::Ok);
    assert_eq!(
        report.buckets[0].refill,
        Refill::FixedWindow {
            length: Some(core::time::Duration::from_secs(18_000)),
            resets_at: Some(1_794_859_200)
        }
    );
    assert_eq!(
        report.buckets[3].models,
        Coverage::Only {
            models: vec!["Fable".to_string()]
        }
    );
    assert!(!format!("{report:?}").contains("canary"));
}

#[test]
fn a_long_lived_token_is_tried_and_a_refusal_for_scope_is_not_offered() {
    let (addr, seen) = stand_in(vec![(403, None, "{}".to_string())]);
    let grant =
        AccountGrant::new("t", OAUTH_TOKEN).credentials(Credentials::from_iter([("token", TOKEN)]));
    let report = read_limits(&grant, &options(addr)).expect("an answer");
    assert!(!report.supported);
    assert!(report.unsupported_because.is_some());
    let seen = seen.lock().expect("log").clone();
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

#[test]
fn a_login_that_expired_or_lacks_the_profile_scope_sends_nothing() {
    let (addr, seen) = stand_in(vec![(200, None, document())]);
    let expired = read_limits(
        &login(&login_file(PROFILE, (NOW - 1) * 1000)),
        &options(addr),
    )
    .expect_err("expired");
    assert_eq!(expired.status, Status::GUATIAO_ERR_GONE);
    assert!(!expired.message().to_string().contains("canary"));

    let narrow = read_limits(
        &login(&login_file(r#"["user:inference"]"#, LATER)),
        &options(addr),
    )
    .expect("an answer");
    assert!(!narrow.supported);
    assert!(seen.lock().expect("log").is_empty());
}

#[test]
fn a_refused_token_is_gone_and_says_nothing_of_it() {
    let (addr, _) = stand_in(vec![(401, None, format!(r#"{{"echo":"{TOKEN}"}}"#))]);
    let error = read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect_err("401");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    assert!(!error.message().to_string().contains("canary"));

    let (addr, _) = stand_in(vec![(500, None, "{}".to_string())]);
    let error = read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect_err("500");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);
    assert!(error.message().to_string().contains("500"));
}

#[test]
fn a_server_that_is_not_there_or_answers_nonsense_is_an_error() {
    let gone = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        listener.local_addr().expect("its address")
    };
    let error = read_limits(&login(&login_file(PROFILE, LATER)), &options(gone)).expect_err("none");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);

    let (addr, _) = stand_in(vec![(200, None, "not json".to_string())]);
    let error = read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect_err("junk");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);
}

#[test]
fn a_redirect_is_not_followed_so_the_token_goes_nowhere_else() {
    let (elsewhere, other) = stand_in(vec![(200, None, document())]);
    let (addr, _) = stand_in(vec![(
        302,
        Some(format!("Location: http://{elsewhere}/api/oauth/usage")),
        String::new(),
    )]);
    let error =
        read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect_err("redirect");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);
    assert!(other.lock().expect("log").is_empty());
}

#[test]
fn an_account_with_no_plan_limits_is_not_offered_and_nothing_is_sent() {
    let (addr, seen) = stand_in(vec![(200, None, document())]);
    let grants = [AccountGrant::new("c", CONSOLE)
        .credentials(Credentials::from_iter([(
            FILE_KEY,
            login_file(PROFILE, LATER),
        )]))
        .state_dir("/some/dir")];
    for grant in grants {
        let report = read_limits(&grant, &options(addr)).expect("an answer");
        assert!(!report.supported, "{}", grant.kind);
        assert!(report.unsupported_because.is_some());
        assert!(report.buckets.is_empty());
    }
    assert!(seen.lock().expect("log").is_empty());
}

#[test]
fn a_grant_the_provider_refuses_is_refused_and_a_document_with_no_window_is_not_offered() {
    let (addr, seen) = stand_in(vec![(200, None, r#"{"five_hour":null}"#.to_string())]);
    let wrong = AccountGrant::new("w", "no-such-kind");
    assert!(read_limits(&wrong, &options(addr)).is_err());
    let report = read_limits(&login(&login_file(PROFILE, LATER)), &options(addr)).expect("answer");
    assert!(!report.supported);
    assert_eq!(seen.lock().expect("log").len(), 1);
}

fn key(kind: &str, secret: &str, base: SocketAddr) -> AccountGrant {
    let name = if kind == API_KEY {
        "api_key"
    } else {
        "auth_token"
    };
    AccountGrant::new("k", kind)
        .credentials(Credentials::from_iter([(name, secret)]))
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"base_url": format!("http://{base}")}),
        ))
}

const RATE: &str = "anthropic-ratelimit-requests-limit: 50
anthropic-ratelimit-requests-remaining: 7
anthropic-ratelimit-requests-reset: 2026-11-16T20:00:00Z
anthropic-ratelimit-tokens-limit: 40000
anthropic-ratelimit-tokens-remaining: 0";

#[test]
fn an_api_key_reports_the_rate_limit_headers_of_the_model_list() {
    let (addr, seen) = stand_in(vec![
        (200, Some(RATE.to_string()), r#"{"data":[]}"#.to_string()),
        (404, None, r#"{"error":"no such route"}"#.to_string()),
    ]);
    let report = read_limits(
        &key(API_KEY, "sk-ant-api03-canary", addr),
        &ReadOptions::new().now(NOW),
    )
    .expect("an answer");
    assert!(report.supported);
    assert_eq!(report.account.as_deref(), Some("k"));
    let ids: Vec<&str> = report.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["requests", "tokens"]);
    assert_eq!(
        (report.buckets[0].used, report.buckets[0].limit),
        (Some(43), Some(50))
    );
    assert_eq!(report.buckets[1].status, Room::Exhausted);
    let seen = seen.lock().expect("log");
    assert!(seen[0].line.starts_with("GET /v1/models"));
    assert_eq!(seen[0].header("x-api-key"), Some("sk-ant-api03-canary"));
    assert!(seen[0].header("authorization").is_none());
    assert!(seen[0].header("anthropic-version").is_some());
}

#[test]
fn a_gateway_token_also_reports_the_gateways_budget() {
    let info =
        serde_json::json!({"info": {"spend": 2.5, "max_budget": 5.0, "rpm_limit": 30}}).to_string();
    let (addr, seen) = stand_in(vec![
        (200, None, r#"{"data":[]}"#.to_string()),
        (200, None, info),
    ]);
    let report = read_limits(
        &key(AUTH_TOKEN, "gw-canary", addr),
        &ReadOptions::new().now(NOW),
    )
    .expect("an answer");
    let ids: Vec<&str> = report.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["budget", "rpm"]);
    assert_eq!(report.buckets[0].limit, Some(5_000_000));
    let seen = seen.lock().expect("log");
    assert_eq!(seen[0].header("authorization"), Some("Bearer gw-canary"));
    assert!(seen[1].line.starts_with("GET /key/info"));
    assert_eq!(seen[1].header("authorization"), Some("Bearer gw-canary"));
}

#[test]
fn a_key_that_reports_nothing_is_not_offered_and_a_refused_key_is_an_error() {
    let (addr, _) = stand_in(vec![
        (200, None, r#"{"data":[]}"#.to_string()),
        (404, None, "{}".to_string()),
    ]);
    let report =
        read_limits(&key(API_KEY, "k", addr), &ReadOptions::new().now(NOW)).expect("an answer");
    assert!(!report.supported);
    assert!(report.unsupported_because.is_some());

    let (addr, _) = stand_in(vec![(401, None, "{}".to_string())]);
    let error = read_limits(
        &key(API_KEY, "sk-refused-canary", addr),
        &ReadOptions::new().now(NOW),
    )
    .expect_err("refused");
    assert!(error.message().contains("refused") && !error.message().contains("canary"));
}
