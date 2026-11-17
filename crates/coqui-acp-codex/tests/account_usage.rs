//! The plan limits of a Codex account read on their own, against a loopback stand-in for the ChatGPT backend: what is
//! asked, with which credential and account, what comes back, and what is never sent.
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
use coqui_acp_codex::account_usage::{ReadOptions, read_limits};
use coqui_acp_codex::accounts::{ACCESS_TOKEN, API_KEY, CHATGPT, FILE_KEY};

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

/// An unsigned JWT with `claims`.
fn jwt(claims: &serde_json::Value) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let encode = |bytes: &[u8]| {
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0_u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..=chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            }
        }
        out
    };
    format!(
        "{}.{}.canary",
        encode(br#"{"alg":"none"}"#),
        encode(claims.to_string().as_bytes())
    )
}

fn access(exp: i64) -> String {
    jwt(
        &serde_json::json!({"exp": exp, "https://api.openai.com/auth": {"chatgpt_account_id": "acct-claimed"}}),
    )
}

fn document() -> String {
    serde_json::json!({
        "plan_type": "plus",
        "rate_limit": {"allowed": true, "limit_reached": false,
            "primary_window": {"used_percent": 17, "limit_window_seconds": 18000, "reset_after_seconds": 3600, "reset_at": 1_794_850_000},
            "secondary_window": {"used_percent": 42, "limit_window_seconds": 604_800, "reset_after_seconds": 90_000, "reset_at": 1_795_000_000}},
        "additional_rate_limits": [{"limit_name": "GPT-5.3-Codex-Spark", "metered_feature": "codex_bengalfox",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 3, "limit_window_seconds": 18000, "reset_after_seconds": 100, "reset_at": 1_794_846_000},
                "secondary_window": null}}],
        "credits": {"has_credits": false, "unlimited": false, "balance": null},
    })
    .to_string()
}

fn login_file(access_token: &str) -> String {
    serde_json::json!({
        "auth_mode": "chatgpt", "OPENAI_API_KEY": null,
        "tokens": {"id_token": "i", "access_token": access_token, "refresh_token": "rt-canary", "account_id": "acct-file"},
    })
    .to_string()
}

fn login(file: &str, addr: SocketAddr) -> AccountGrant {
    AccountGrant::new("owner", CHATGPT)
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"chatgpt_base_url": format!("http://{addr}/backend-api/")}),
        ))
        .credentials(Credentials::from_iter([(FILE_KEY, file)]))
        .state_dir("/some/dir")
}

fn options() -> ReadOptions {
    ReadOptions::new().now(NOW)
}

#[test]
fn a_chatgpt_login_reads_its_windows_from_the_accounts_own_endpoint() {
    let (addr, seen) = stand_in(vec![(200, None, document())]);
    let token = access(NOW + 3600);
    let report = read_limits(&login(&login_file(&token), addr), &options()).expect("read");
    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].line, "GET /backend-api/wham/usage HTTP/1.1");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    assert_eq!(seen[0].header("chatgpt-account-id"), Some("acct-file"));

    assert!(report.supported);
    assert_eq!(report.account.as_deref(), Some("owner"));
    let ids: Vec<&str> = report.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "codex:five_hour",
            "codex:weekly",
            "codex_bengalfox:five_hour"
        ]
    );
    assert_eq!(report.buckets[0].used, Some(170));
    assert_eq!(report.buckets[0].status, Room::Ok);
    assert_eq!(
        report.buckets[1].refill,
        Refill::FixedWindow {
            length: Some(core::time::Duration::from_secs(604_800)),
            resets_at: Some(1_795_000_000)
        }
    );
    assert_eq!(
        report.buckets[2].label.as_deref(),
        Some("GPT-5.3-Codex-Spark")
    );
    assert_eq!(report.buckets[2].models, Coverage::Unknown);
    assert!(report.buckets.iter().all(|b| b.observed_at == NOW));
    assert!(!format!("{report:?}").contains("canary"));
}

#[test]
fn an_access_token_names_its_account_in_its_claims_and_a_stand_in_endpoint_overrides_the_setting() {
    let (addr, seen) = stand_in(vec![(200, None, document())]);
    let token = access(NOW + 3600);
    let grant = AccountGrant::new("t", ACCESS_TOKEN)
        .credentials(Credentials::from_iter([("access_token", token.as_str())]))
        .state_dir("/some/dir");
    let report = read_limits(&grant, &options().endpoint(format!("http://{addr}"))).expect("read");
    assert!(report.supported);
    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen[0].line, "GET /api/codex/usage HTTP/1.1");
    assert_eq!(seen[0].header("chatgpt-account-id"), Some("acct-claimed"));
}

#[test]
fn an_expired_token_sends_nothing_and_a_refused_one_is_gone_without_the_token() {
    let (addr, seen) = stand_in(vec![(401, None, "{}".to_string())]);
    let expired = access(NOW - 1);
    let error = read_limits(&login(&login_file(&expired), addr), &options()).expect_err("expired");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    assert!(seen.lock().expect("log").is_empty());

    let live = access(NOW + 3600);
    let error = read_limits(&login(&login_file(&live), addr), &options()).expect_err("401");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    let said = error.message().to_string();
    assert!(!said.contains("canary") && !said.contains(&live), "{said}");

    let (addr, _) = stand_in(vec![(502, None, "{}".to_string())]);
    let error = read_limits(&login(&login_file(&live), addr), &options()).expect_err("502");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);
}

#[test]
fn a_redirect_is_not_followed_and_junk_or_no_server_is_an_error() {
    let (elsewhere, other) = stand_in(vec![(200, None, document())]);
    let (addr, _) = stand_in(vec![(
        302,
        Some(format!(
            "Location: http://{elsewhere}/backend-api/wham/usage"
        )),
        String::new(),
    )]);
    let live = login_file(&access(NOW + 3600));
    let error = read_limits(&login(&live, addr), &options()).expect_err("redirect");
    assert_eq!(error.status, Status::GUATIAO_ERR_INTERNAL);
    assert!(other.lock().expect("log").is_empty());

    let (addr, _) = stand_in(vec![(200, None, "not json".to_string())]);
    assert!(read_limits(&login(&live, addr), &options()).is_err());

    let gone = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        listener.local_addr().expect("its address")
    };
    assert!(read_limits(&login(&live, gone), &options()).is_err());
}

#[test]
fn a_plan_with_no_limits_says_so() {
    let (addr, seen) = stand_in(vec![(200, None, r#"{"plan_type":"free"}"#.to_string())]);
    let live = login_file(&access(NOW + 3600));
    let report = read_limits(&login(&live, addr), &options()).expect("answer");
    assert!(!report.supported);
    assert_eq!(seen.lock().expect("log").len(), 1);

    assert!(read_limits(&AccountGrant::new("w", "no-such-kind"), &options()).is_err());
}

fn key(secret: &str, base: &str) -> AccountGrant {
    AccountGrant::new("k", API_KEY)
        .credentials(Credentials::from_iter([("api_key", secret)]))
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"openai_base_url": base}),
        ))
        .state_dir("/some/dir")
}

const RATE: &str = "x-ratelimit-limit-requests: 500
x-ratelimit-remaining-requests: 499
x-ratelimit-reset-requests: 120ms
x-ratelimit-limit-tokens: 30000
x-ratelimit-remaining-tokens: 0
x-ratelimit-reset-tokens: 6m0s";

#[test]
fn an_api_key_reports_the_rate_limit_headers_and_a_gateways_budget() {
    let info = serde_json::json!({"info": {"spend": 12.0, "max_budget": 10.0, "budget_duration": "1mo", "tpm_limit": 90000}}).to_string();
    let (addr, seen) = stand_in(vec![
        (200, Some(RATE.to_string()), r#"{"data":[]}"#.to_string()),
        (200, None, info),
    ]);
    let report =
        read_limits(&key("sk-canary", &format!("http://{addr}/v1")), &options()).expect("answer");
    assert!(report.supported);
    assert_eq!(report.account.as_deref(), Some("k"));
    let ids: Vec<&str> = report.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["requests", "tokens", "budget", "tpm"]);
    assert_eq!(report.buckets[0].remaining, Some(499));
    assert_eq!(report.buckets[1].status, Room::Exhausted);
    assert_eq!(report.buckets[2].status, Room::Exhausted);
    let seen = seen.lock().expect("log");
    assert!(seen[0].line.starts_with("GET /v1/models"));
    assert_eq!(seen[0].header("authorization"), Some("Bearer sk-canary"));
    assert!(seen[1].line.starts_with("GET /key/info"));
}

#[test]
fn a_key_that_reports_nothing_is_not_offered_and_a_refused_key_is_an_error() {
    let (addr, _) = stand_in(vec![
        (200, None, r#"{"data":[]}"#.to_string()),
        (404, None, "{}".to_string()),
    ]);
    let report =
        read_limits(&key("sk-canary", &format!("http://{addr}/v1")), &options()).expect("answer");
    assert!(!report.supported);

    let (addr, _) = stand_in(vec![(401, None, "{}".to_string())]);
    let error = read_limits(
        &key("sk-refused-canary", &format!("http://{addr}/v1")),
        &options(),
    )
    .expect_err("refused");
    assert!(error.message().contains("refused") && !error.message().contains("canary"));
}
