//! Antigravity's account and quota read from a stored login in a home made for each test, against a loopback stand-in for
//! Google's token endpoint, userinfo and the Cloud Code service. No keyring, no real login, no network and no process.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use coqui_acp_api::usage::{Coverage, Refill, Source, Status, Unit};
use coqui_acp_harness::antigravity::{
    AcpModel, AgyLogin, Cause, ClientSource, KEYRING_SERVICE, KEYRING_USER, Keyring, LoginError,
    LoginReading, LoginSettings, OAuthClient, SignedIn, Step, USER_AGENT,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const EMAIL: &str = "person@example.test";
const STORED_ACCESS: &str = "stored-access-token";
const REFRESH: &str = "stored-refresh-token";
const REFRESHED: &str = "refreshed-access-token";

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap()
        .format(&Rfc3339)
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// A home of the test's own.

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "coqui-acp-agy-login-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(".gemini")).unwrap();
        Self(path)
    }

    /// Writes agy's login file with an access token that expires at `expiry`.
    fn login(&self, access: &str, expiry: i64) -> PathBuf {
        let file = self.0.join(".gemini").join("jetski-standalone-oauth-token");
        std::fs::write(&file, login_json(access, expiry).to_string()).unwrap();
        file
    }

    /// Every file under the home with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, into);
                } else {
                    into.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.0, &mut files);
        files
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn login_json(access: &str, expiry: i64) -> Value {
    json!({
        "token": {"access_token": access, "token_type": "Bearer", "refresh_token": REFRESH, "expiry": rfc3339(expiry)},
        "auth_method": "consumer",
        "id_token_email": EMAIL,
        "project_id": "fixture-project",
        "user_tier": "g1-pro-tier",
        "tier_display_name": "Stored Plan",
        "writer_id": "fixture"
    })
}

/// A client id and a secret as a program's strings hold them, made here so no source file holds one.
fn client_id(number: &str, letter: char) -> String {
    format!(
        "{number}-{}{}",
        letter.to_string().repeat(32),
        ".apps.googleusercontent.com"
    )
}

fn client_secret(letter: char) -> String {
    format!(
        "{}{}",
        concat!("GOC", "SPX-"),
        letter.to_string().repeat(28)
    )
}

/// A stand-in program: other strings run into two client ids and two secrets.
fn program(home: &Home) -> PathBuf {
    let path = home.0.join("agy-program.bin");
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 0, 0];
    bytes.extend(b"(low|medium|high)");
    bytes.extend(client_id("111111111111", 'a').as_bytes());
    bytes.extend(b"https://example.test/");
    bytes.extend(client_secret('X').as_bytes());
    bytes.extend(client_secret('Y').as_bytes());
    bytes.extend([0, 1, 2, 3]);
    bytes.extend(b"updating in-memory auth session");
    bytes.extend(client_id("2222222222222", 'b').as_bytes());
    bytes.extend([0xff; 16]);
    std::fs::write(&path, bytes).unwrap();
    path
}

// ---------------------------------------------------------------------------------------------
// The stand-in for Google.

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    user_agent: String,
    body: Vec<u8>,
}

impl Request {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn form(&self) -> BTreeMap<String, String> {
        url_decode_form(std::str::from_utf8(&self.body).unwrap())
    }
}

type Handler = dyn Fn(&Request) -> (u16, Value) + Send + Sync;

struct Google {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Google {
    fn start(handle: impl Fn(&Request) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handle: Arc<Handler> = Arc::new(handle);
        let (stopping, recording) = (Arc::clone(&stop), Arc::clone(&seen));
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                recording.lock().unwrap().push(request.clone());
                let (status, body) = handle(&request);
                let body = body.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        Self {
            address,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// The usual answers: a token endpoint that takes only the second id with the first secret, userinfo, and the service,
    /// which takes only `access` as the bearer.
    fn usual(access: &'static str, models: Value) -> Self {
        Self::start(move |request| answer(request, access, &models))
    }

    /// The usual answers, and the service's summary of the quota: the groups of models for a client that names itself agy's
    /// command line, and only the group every client knows for any other.
    fn with_summary(access: &'static str, models: Value) -> Self {
        let summary: Value =
            serde_json::from_str(include_str!("fixtures/agy-1.5.15/retrieveusersummary.json"))
                .unwrap();
        Self::start(move |request| {
            if request.path != "/v1internal:retrieveUserQuotaSummary" {
                return answer(request, access, &models);
            }
            let mut summary = summary.clone();
            if !request.user_agent.starts_with("antigravity/cli/") {
                summary["groups"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|group| group["displayName"] == "Claude and GPT models");
            }
            (200, summary)
        })
    }

    fn base(&self) -> String {
        format!("http://{}", self.address)
    }

    fn settings(&self, home: &Home) -> LoginSettings {
        LoginSettings::new()
            .home(&home.0)
            .keyring(Keyring::Off)
            .client(ClientSource::Off)
            .token_url(format!("{}/token", self.base()))
            .userinfo_url(format!("{}/userinfo", self.base()))
            .service(self.base())
            .timeout(Duration::from_secs(120))
    }

    fn seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen()
            .into_iter()
            .map(|request| request.path)
            .collect()
    }
}

impl Drop for Google {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(request: &Request, access: &str, models: &Value) -> (u16, Value) {
    if request.path == "/token" {
        let form = request.form();
        let right = form.get("grant_type").map(String::as_str) == Some("refresh_token")
            && form.get("refresh_token").map(String::as_str) == Some(REFRESH)
            && form.get("client_id") == Some(&client_id("2222222222222", 'b'))
            && form.get("client_secret") == Some(&client_secret('X'));
        return if right {
            (
                200,
                json!({"access_token": REFRESHED, "expires_in": 3599, "token_type": "Bearer"}),
            )
        } else {
            (
                401,
                json!({"error": "invalid_client", "error_description": "Unauthorized"}),
            )
        };
    }
    if request.authorization.as_deref() != Some(&format!("Bearer {access}")) {
        return (
            401,
            json!({"error": {"code": 401, "status": "UNAUTHENTICATED"}}),
        );
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/userinfo") => (
            200,
            json!({"id": "1", "email": EMAIL, "name": "A Person", "picture": "https://example.test/p"}),
        ),
        ("POST", "/v1internal:loadCodeAssist") => (
            200,
            json!({
                "currentTier": {"id": "free-tier", "name": "Free"},
                "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro"},
                "cloudaicompanionProject": "fixture-project"
            }),
        ),
        ("POST", "/v1internal:fetchAvailableModels") => (200, models.clone()),
        _ => (404, json!({})),
    }
}

/// Two models sharing a five-hour window, half used, and one with a week, unused.
fn models() -> Value {
    let now = now();
    json!({
        "models": {
            "gemini-3.8-flash-high": {"displayName": "Gemini 3.8 Flash", "quotaInfo": {"remainingFraction": 0.5, "resetTime": rfc3339(now + 3_600)}},
            "gemini-3.8-flash-low": {"displayName": "Gemini 3.8 Flash Low", "quotaInfo": {"remainingFraction": 0.75, "resetTime": rfc3339(now + 3_600)}},
            "gemini-4-argon-medium": {"displayName": "Gemini 4 Argon", "quotaInfo": {"remainingFraction": 1, "resetTime": rfc3339(now + 604_800)}},
            "no-quota": {"displayName": "No quota"}
        },
        "defaultAgentModelId": "gemini-3.8-flash-high"
    })
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let mut length = 0;
    let mut authorization = None;
    let mut user_agent = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("user-agent") {
            value.trim().clone_into(&mut user_agent);
        }
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        authorization,
        user_agent,
        body,
    })
}

fn url_decode_form(text: &str) -> BTreeMap<String, String> {
    let decode = |part: &str| {
        let bytes = part.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'+' => out.push(b' '),
                b'%' => {
                    out.push(u8::from_str_radix(&part[at + 1..at + 3], 16).unwrap());
                    at += 2;
                }
                other => out.push(other),
            }
            at += 1;
        }
        String::from_utf8(out).unwrap()
    };
    text.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (decode(name), decode(value)))
        .collect()
}

fn signed_in(reading: LoginReading) -> SignedIn {
    match reading {
        LoginReading::SignedIn(signed_in) => signed_in,
        other => panic!("not signed in: {other:?}"),
    }
}

fn acp_models() -> Vec<AcpModel> {
    vec![
        AcpModel::new("gemini-3.8-flash-high", "Gemini 3.8 Flash"),
        AcpModel::new("gemini-3.8-flash-low", "Gemini 3.8 Flash Low"),
        AcpModel::new("gemini-4-argon-medium", "Gemini 4 Argon"),
    ]
}

// ---------------------------------------------------------------------------------------------

#[test]
fn a_valid_token_reads_the_account_plan_and_buckets() {
    let home = Home::new("valid");
    home.login(STORED_ACCESS, now() + 3_600);
    let google = Google::usual(STORED_ACCESS, models());
    let mut reader = AgyLogin::new(google.settings(&home));

    let read = signed_in(reader.read(&acp_models()).unwrap());

    assert_eq!(read.account.email.as_deref(), Some(EMAIL));
    assert_eq!(read.account.name.as_deref(), Some("A Person"));
    assert_eq!(
        read.account.plan.as_deref(),
        Some("Google AI Pro"),
        "the service's paid tier, over the stored name"
    );
    let buckets = read.usage.unwrap();
    let shown: Vec<_> = buckets
        .iter()
        .map(|bucket| {
            (
                bucket.id.as_str(),
                bucket.remaining,
                bucket.label.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("agy:gemini-3.8-flash-high", Some(500), None),
            ("agy:gemini-4-argon-medium", Some(1000), Some("7 d"))
        ]
    );
    for bucket in &buckets {
        assert_eq!(
            (&bucket.source, &bucket.unit, bucket.limit),
            (&Source::Reported, &Unit::Permille, Some(1000))
        );
        assert_eq!(bucket.status, Status::Ok);
    }
    assert!(matches!(
        buckets[0].refill,
        Refill::FixedWindow {
            resets_at: Some(_),
            ..
        }
    ));
    assert_eq!(reader.unmatched(), 0);

    assert_eq!(
        google.paths(),
        [
            "/userinfo",
            "/v1internal:loadCodeAssist",
            "/v1internal:retrieveUserQuotaSummary",
            "/v1internal:fetchAvailableModels"
        ],
        "no refresh for a token that has not expired"
    );
    let seen = google.seen();
    assert!(
        seen.iter()
            .all(|request| request.authorization.as_deref() == Some("Bearer stored-access-token"))
    );
    assert_eq!(seen[1].json()["metadata"]["ideType"], "ANTIGRAVITY");
    assert_eq!(seen[1].json()["cloudaicompanionProject"], "fixture-project");
    assert_eq!(seen[2].json(), json!({"project": "fixture-project"}));
    assert_eq!(seen[3].json(), json!({"project": "fixture-project"}));
}

#[test]
fn the_quota_is_the_groups_of_models_with_their_windows() {
    let home = Home::new("groups");
    home.login(STORED_ACCESS, now() + 3_600);
    let google = Google::with_summary(STORED_ACCESS, models());
    let mut reader = AgyLogin::new(google.settings(&home));

    let read = signed_in(reader.read(&acp_models()).unwrap());
    let buckets = read.usage.unwrap();

    let shown: Vec<_> = buckets
        .iter()
        .map(|bucket| {
            (
                bucket.group.as_deref().unwrap(),
                bucket.id.as_str(),
                bucket.status.clone(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("Gemini Models", "agy:gemini-weekly", Status::Ok),
            ("Gemini Models", "agy:gemini-5h", Status::Ok),
            ("Claude and GPT models", "agy:3p-weekly", Status::Exhausted),
            ("Claude and GPT models", "agy:3p-5h", Status::Disabled),
            ("Gemini 4 Argon", "agy:gemini-argon-wtu-weekly", Status::Ok),
        ]
    );
    // The argon group names a model the agent lists, so only it is covered.
    assert_eq!(
        buckets[4].models,
        Coverage::Only {
            models: vec!["gemini-4-argon-medium".into()]
        }
    );
    assert_eq!(
        google.paths(),
        [
            "/userinfo",
            "/v1internal:loadCodeAssist",
            "/v1internal:retrieveUserQuotaSummary"
        ],
        "the models' own quota is not asked when the summary answers"
    );
}

#[test]
fn the_request_names_agys_command_line_or_the_service_leaves_groups_out() {
    let home = Home::new("agent");
    home.login(STORED_ACCESS, now() + 3_600);
    let google = Google::with_summary(STORED_ACCESS, models());

    let mut reader = AgyLogin::new(google.settings(&home).user_agent("antigravity"));
    let read = signed_in(reader.read(&[]).unwrap());
    let groups: Vec<_> = read
        .usage
        .unwrap()
        .into_iter()
        .filter_map(|bucket| bucket.group)
        .collect();
    assert!(
        groups.iter().all(|group| group == "Claude and GPT models"),
        "{groups:?}"
    );

    let mut reader = AgyLogin::new(google.settings(&home));
    let read = signed_in(reader.read(&[]).unwrap());
    let groups: std::collections::BTreeSet<_> = read
        .usage
        .unwrap()
        .into_iter()
        .filter_map(|bucket| bucket.group)
        .collect();
    assert_eq!(groups.len(), 3, "{groups:?}");
    assert!(
        google
            .seen()
            .iter()
            .any(|request| request.user_agent == USER_AGENT)
    );
}

#[test]
fn an_expired_token_is_refreshed_in_memory_and_the_login_is_left_as_it_was() {
    let home = Home::new("expired");
    let file = home.login(STORED_ACCESS, now() - 60);
    let program = program(&home);
    let before = home.snapshot();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let google = Google::usual(REFRESHED, models());
    let mut reader = AgyLogin::new(
        google
            .settings(&home)
            .client(ClientSource::Program(program)),
    );

    let read = signed_in(reader.read(&acp_models()).unwrap());
    assert_eq!(read.usage.map(|buckets| buckets.len()), Ok(2));
    let tried = google
        .seen()
        .iter()
        .filter(|request| request.path == "/token")
        .count();
    assert_eq!(
        tried, 3,
        "the program's ids each with its secrets, in the order they appear, until one is taken"
    );
    let last = google
        .seen()
        .into_iter()
        .rfind(|request| request.path == "/token")
        .unwrap();
    assert_eq!(last.form()["client_id"], client_id("2222222222222", 'b'));
    assert!(
        google
            .seen()
            .iter()
            .filter(|request| request.path != "/token")
            .all(
                |request| request.authorization.as_deref() == Some("Bearer refreshed-access-token")
            )
    );

    // The refreshed token is kept in memory: a second reading refreshes nothing.
    let calls = google.seen().len();
    signed_in(reader.read(&acp_models()).unwrap())
        .usage
        .unwrap();
    assert_eq!(
        google.seen()[calls..]
            .iter()
            .filter(|request| request.path == "/token")
            .count(),
        0
    );

    assert_eq!(
        home.snapshot(),
        before,
        "nothing under the home was written, added or removed"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn no_login_is_not_signed_in() {
    let home = Home::new("none");
    let google = Google::usual(STORED_ACCESS, models());
    let mut reader = AgyLogin::new(google.settings(&home));
    assert_eq!(reader.read(&[]).unwrap(), LoginReading::NotSignedIn);
    assert!(google.seen().is_empty());

    // A login with no refresh token is no sign-in either, as agy counts it.
    std::fs::write(
        home.0.join(".gemini").join("jetski-standalone-oauth-token"),
        json!({"token": {"access_token": STORED_ACCESS}}).to_string(),
    )
    .unwrap();
    assert_eq!(reader.read(&[]).unwrap(), LoginReading::NotSignedIn);
}

#[test]
fn a_service_error_names_the_step_and_keeps_the_account() {
    let home = Home::new("error");
    home.login(STORED_ACCESS, now() + 3_600);
    let google = Google::start(|request| {
        if request.path == "/v1internal:fetchAvailableModels" {
            (503, json!({"error": {"code": 503}}))
        } else {
            answer(request, STORED_ACCESS, &Value::Null)
        }
    });
    let mut reader = AgyLogin::new(google.settings(&home));

    let read = signed_in(reader.read(&[]).unwrap());
    let error = read.usage.clone().unwrap_err();
    assert_eq!(
        error,
        LoginError::new(Step::FetchAvailableModels, Cause::Status(503))
    );
    assert_eq!(read.account.plan.as_deref(), Some("Google AI Pro"));
    let said = format!("{error} {error:?} {read:?}");
    assert!(said.contains("fetchAvailableModels"), "{said}");
    for secret in [EMAIL, STORED_ACCESS, REFRESH, "A Person"] {
        assert!(!said.contains(secret), "{said}");
    }
}

#[test]
fn with_no_client_the_account_comes_from_the_stored_login_and_usage_waits_for_antigravity() {
    let home = Home::new("noclient");
    home.login(STORED_ACCESS, now() - 60);
    let empty = home.0.join("empty-program.bin");
    std::fs::write(&empty, b"no client in here").unwrap();
    let google = Google::usual(REFRESHED, models());

    for client in [ClientSource::Off, ClientSource::Program(empty)] {
        let mut reader = AgyLogin::new(google.settings(&home).client(client));
        let read = signed_in(reader.read(&[]).unwrap());
        assert_eq!(read.account.email.as_deref(), Some(EMAIL));
        assert_eq!(read.account.plan.as_deref(), Some("Stored Plan"));
        assert_eq!(read.account.name, None);
        assert_eq!(
            read.usage,
            Err(LoginError::new(Step::Refresh, Cause::NoClient))
        );
    }
    assert!(
        google.seen().is_empty(),
        "nothing was asked: {:?}",
        google.paths()
    );
}

#[test]
fn the_keyring_entry_is_read_as_go_keyring_writes_it_and_the_later_login_wins() {
    let home = Home::new("keyring");
    home.login("file-access-token", now() + 600);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let keyring = {
        let asked = Arc::clone(&asked);
        let wide: Vec<u8> = login_json("keyring-access-token", now() + 3_000)
            .to_string()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        Keyring::Given(Arc::new(move |service: &str, user: &str| {
            asked
                .lock()
                .unwrap()
                .push((service.to_owned(), user.to_owned()));
            Ok(Some(wide.clone()))
        }))
    };
    let google = Google::usual("keyring-access-token", models());
    let mut reader = AgyLogin::new(google.settings(&home).keyring(keyring));
    signed_in(reader.read(&[]).unwrap()).usage.unwrap();
    assert_eq!(
        *asked.lock().unwrap(),
        [(KEYRING_SERVICE.to_owned(), KEYRING_USER.to_owned())]
    );
    assert_eq!((KEYRING_SERVICE, KEYRING_USER), ("gemini", "antigravity"));

    // A file whose token expires later was written after the keyring's entry, and is read instead.
    home.login("file-access-token", now() + 3_500);
    let google = Google::usual("file-access-token", models());
    let keyring = Keyring::Given(Arc::new(|_: &str, _: &str| {
        Ok(Some(
            login_json("keyring-access-token", now() + 3_000)
                .to_string()
                .into_bytes(),
        ))
    }));
    let mut reader = AgyLogin::new(google.settings(&home).keyring(keyring));
    signed_in(reader.read(&[]).unwrap()).usage.unwrap();

    // A keyring that fails, with no file, is an error of that step.
    let empty = Home::new("keyring-failed");
    let failing = Keyring::Given(Arc::new(|_: &str, _: &str| {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }));
    let mut reader = AgyLogin::new(google.settings(&empty).keyring(failing));
    assert_eq!(
        reader.read(&[]).unwrap_err(),
        LoginError::new(
            Step::Keyring,
            Cause::Io(std::io::ErrorKind::PermissionDenied)
        )
    );
}

#[test]
fn a_stored_token_the_service_refuses_is_refreshed_once() {
    let home = Home::new("revoked");
    home.login(STORED_ACCESS, now() + 3_600);
    let google = Google::usual(REFRESHED, models());
    let given = OAuthClient::new(client_id("2222222222222", 'b'), client_secret('X'));
    let mut reader = AgyLogin::new(google.settings(&home).client(ClientSource::Given(given)));
    let read = signed_in(reader.read(&[]).unwrap());
    assert_eq!(read.usage.map(|buckets| buckets.len()), Ok(2));
    assert_eq!(
        google
            .paths()
            .iter()
            .filter(|path| *path == "/token")
            .count(),
        1
    );
}
