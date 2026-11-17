//! The Codex account provider against the stand-in: signing in, renewing, watching and reading plan limits, and that it
//! keeps every rule of the kind. A harness applies an account the provider made.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coqui_acp_api::contract::AccountContract;
use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Value};
use coqui_acp_api::{
    AccountGrant, AccountKeeper, AccountLoss, AccountProvider, AccountRenewal, AccountUsageQuery,
    Credentials, LoginAnswer, LoginForm, LoginPrompter, LoginRequest, OpenUrl, ProcessSpawner,
    RenewRequest, WatchRequest,
};
use coqui_acp_codex::{AccountSettings, CodexAccounts};
use coqui_acp_harness::LocalSpawner;

const CANARY: &str = "sk-canary-7f3a91c04b";

fn profile() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    tests
        .parent()
        .and_then(Path::parent)
        .expect("a test binary is in target/<profile>/deps")
        .to_path_buf()
}

fn fake() -> String {
    profile()
        .join("examples")
        .join(format!("fake_codex{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn library() -> PathBuf {
    profile().join("examples").join(format!(
        "{}codex_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-codex-prov-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> CodexAccounts {
    CodexAccounts::new(AccountSettings {
        executable: fake(),
        renew_poll_ms: 100,
        ..AccountSettings::default()
    })
}

fn spawner(dir: &Path) -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().files(dir).into_object()
}

fn credentials_of(pairs: &[(&str, &str)]) -> Credentials {
    pairs.iter().copied().collect()
}

/// A keeper that remembers what it was told.
#[derive(Clone, Default)]
struct Heard {
    renewed: Arc<Mutex<Vec<AccountRenewal>>>,
    lost: Arc<Mutex<Vec<AccountLoss>>>,
}

impl AccountKeeper for Heard {
    fn renewed(&self, renewal: AccountRenewal) -> Result<(), ProviderError> {
        self.renewed.lock().unwrap().push(renewal);
        Ok(())
    }

    fn lost(&self, loss: AccountLoss) -> Result<(), ProviderError> {
        self.lost.lock().unwrap().push(loss);
        Ok(())
    }
}

fn wait_for(what: impl Fn() -> bool) {
    let waited = std::time::Instant::now();
    while !what() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

const LOGIN_FILE: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"stand-in-access-1","refresh_token":"stand-in-refresh-1"}}"#;

const JWT_FILE: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDB9.","refresh_token":"stand-in-refresh-2","account_id":"acct-A"}}"#;

#[derive(Clone)]
struct Person {
    secret: String,
    shown: Arc<Mutex<Vec<String>>>,
    codes: Arc<Mutex<Vec<Option<String>>>>,
    /// What the person pastes when asked for the address, one for each time, `{valid}` standing for the callback of
    /// the page they were shown; when none is left, that callback.
    addresses: Arc<Mutex<std::collections::VecDeque<String>>>,
    /// The title of each form the sign-in put to the person.
    titles: Arc<Mutex<Vec<String>>>,
    /// A person whose browser comes back to Codex by itself, and who never answers the address form.
    never: bool,
}

impl Person {
    fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            shown: Arc::default(),
            codes: Arc::default(),
            addresses: Arc::default(),
            titles: Arc::default(),
            never: false,
        }
    }

    fn pasting(secret: &str, addresses: &[&str]) -> Self {
        let person = Self::new(secret);
        person
            .addresses
            .lock()
            .unwrap()
            .extend(addresses.iter().map(ToString::to_string));
        person
    }

    /// The callback the browser would end on after the page it was shown.
    fn valid(&self) -> String {
        let shown = self.shown.lock().unwrap();
        let page = shown
            .iter()
            .find(|url| url.contains("redirect_uri"))
            .expect("the page");
        let port = page.split("localhost%3A").nth(1).expect("a port");
        let port: String = port.chars().take_while(char::is_ascii_digit).collect();
        format!(
            "http://127.0.0.1:{port}/auth/callback?code=stand-in-code&scope=openid+profile&state=stand-in-state"
        )
    }
}

impl LoginPrompter for Person {
    fn say(&self, text: &str) -> Result<(), ProviderError> {
        self.shown.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn open_url(&self, open: OpenUrl) -> Result<(), ProviderError> {
        self.shown.lock().unwrap().push(open.url);
        self.codes.lock().unwrap().push(open.user_code);
        Ok(())
    }

    fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        let schema = coqui_acp_harness::provider::to_json(&form.schema);
        self.titles.lock().unwrap().push(form.title.clone());
        let mut secrets = Credentials::new();
        for (name, property) in schema["properties"].as_object().expect("fields") {
            assert_eq!(property["writeOnly"], true, "only secrets are asked");
            if name == "address" {
                if self.never {
                    std::thread::sleep(Duration::from_secs(4));
                    return Err(ProviderError::new(Status::GUATIAO_ERR_GONE, "gone"));
                }
                let pasted = self.addresses.lock().unwrap().pop_front();
                let pasted = pasted.unwrap_or_else(|| "{valid}".to_string());
                let other = self.valid().replace("stand-in-state", "somebody-elses");
                secrets.insert(
                    name.clone(),
                    pasted
                        .replace("{other}", &other)
                        .replace("{valid}", &self.valid()),
                );
                continue;
            }
            secrets.insert(name.clone(), self.secret.clone());
        }
        Ok(LoginAnswer::new(
            coqui_acp_api::guatiao::Value::from(coqui_acp_api::guatiao::Map::new()),
            secrets,
        ))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

fn login(
    method: &str,
    dir: &Path,
    person: &Person,
) -> Result<coqui_acp_api::LoginOutcome, ProviderError> {
    provider().login(
        LoginRequest::new(method, dir.to_string_lossy()).timeout_ms(300_000),
        spawner(dir),
        person.clone().into_object(),
    )
}

#[test]
fn both_chatgpt_sign_ins_relay_the_page_and_the_device_code_and_read_the_login_they_wrote() {
    for (method, wants_code) in [("chatgpt", false), ("chatgpt-device", true)] {
        let dir = scratch(&format!("in-{method}"));
        let person = Person::new(CANARY);
        let outcome =
            login(method, &dir, &person).unwrap_or_else(|e| panic!("{method}: {}", e.message()));
        assert_eq!(outcome.kind, "codex-chatgpt");
        assert!(outcome.checked);
        let file = outcome
            .credentials
            .expose()
            .get("auth_file")
            .expect("the login")
            .to_string();
        assert!(file.contains("stand-in-refresh-1"));
        let shown = person.shown.lock().unwrap().clone();
        assert!(
            shown
                .iter()
                .any(|s| s.starts_with("https://auth.openai.com/")),
            "{method}: {shown:?}"
        );
        let codes = person.codes.lock().unwrap().clone();
        assert_eq!(
            codes,
            [wants_code.then(|| "ABCD-12345".to_string())],
            "{method}"
        );
        if method == "chatgpt" {
            let seen = std::fs::read_to_string(dir.join("fake-callback")).unwrap();
            assert_eq!(seen.trim(), "callback accepted");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_browser_sign_in_is_finished_from_the_pasted_address_and_it_is_in_no_message() {
    let dir = scratch("paste");
    let person = Person::new(CANARY);
    let outcome = login("chatgpt", &dir, &person).expect("signed in");
    assert!(outcome.checked);
    let titles = person.titles.lock().unwrap().clone();
    assert_eq!(titles.len(), 1, "{titles:?}");
    assert!(titles[0].contains("paste the address your browser ends on"));
    assert!(titles[0].contains("http://127.0.0.1:"), "{titles:?}");
    let shown = person.shown.lock().unwrap().join("\n");
    for text in titles.iter().chain([&shown]) {
        assert!(!text.contains("stand-in-code"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_address_that_is_not_the_waiting_callback_is_refused_with_a_reason_and_asked_again() {
    let dir = scratch("refuse");
    let person = Person::pasting(
        CANARY,
        &[
            "https://127.0.0.1:1/auth/callback?code=stand-in-code&state=stand-in-state",
            "http://evil.example:1455/auth/callback?code=stand-in-code&state=stand-in-state",
            "{valid}&state=again",
        ],
    );
    login("chatgpt", &dir, &person).expect_err("three refused in a row");
    let titles = person.titles.lock().unwrap().clone();
    assert_eq!(titles.len(), 3, "{titles:?}");
    assert!(
        titles[1].starts_with("That address was not taken: "),
        "{titles:?}"
    );
    assert!(titles[2].contains("127.0.0.1"), "{titles:?}");
    for title in &titles {
        assert!(
            !title.contains("evil.example") && !title.contains("stand-in-code"),
            "{title}"
        );
    }
    // Nothing that was refused reached the login server.
    assert!(!dir.join("fake-callback").exists());
    let _ = std::fs::remove_dir_all(&dir);

    let dir = scratch("refuse-then");
    let person = Person::pasting(
        CANARY,
        &["http://127.0.0.1:1/auth/callback?code=stand-in-code&state=stand-in-state"],
    );
    let outcome = login("chatgpt", &dir, &person).expect("the second address is the callback");
    assert!(outcome.checked);
    assert_eq!(person.titles.lock().unwrap().len(), 2);
    let seen = std::fs::read_to_string(dir.join("fake-callback")).unwrap();
    assert_eq!(seen.trim(), "callback accepted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_state_of_another_sign_in_is_refused_and_never_requested() {
    let dir = scratch("state");
    let person = Person::pasting(CANARY, &["{other}", "{valid}"]);
    login("chatgpt", &dir, &person).expect("the second address is the callback");
    let titles = person.titles.lock().unwrap().clone();
    assert!(titles[1].contains("state differs"), "{titles:?}");
    let seen = std::fs::read_to_string(dir.join("fake-callback")).unwrap();
    assert_eq!(
        seen.trim(),
        "callback accepted",
        "the other state reached the login server"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_browser_on_this_machine_finishes_the_sign_in_without_the_address() {
    let dir = scratch("local");
    std::fs::write(dir.join("fake-login"), "local").unwrap();
    let mut person = Person::new(CANARY);
    person.never = true;
    let started = std::time::Instant::now();
    let outcome = login("chatgpt", &dir, &person).expect("signed in");
    assert!(outcome.checked);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "it waited for the form"
    );
    assert!(!dir.join("fake-callback").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_key_or_token_is_saved_on_standard_input_and_a_refused_one_names_neither_secret_nor_page() {
    for (method, kind, key, flag) in [
        ("api-key", "codex-api-key", "api_key", "--with-api-key"),
        (
            "access-token",
            "codex-access-token",
            "access_token",
            "--with-access-token",
        ),
    ] {
        let dir = scratch(&format!("in-{method}"));
        let outcome = login(method, &dir, &Person::new(CANARY)).expect("accepted");
        assert_eq!(outcome.kind, kind);
        assert_eq!(outcome.credentials.expose().get(key), Some(CANARY));
        let text = std::fs::read_to_string(dir.join("fake-codex.log")).unwrap();
        assert!(!text.contains(CANARY));
        assert!(text.contains(flag));
        let _ = std::fs::remove_dir_all(&dir);
    }
    let dir = scratch("refused");
    let error = login("access-token", &dir, &Person::new("bad-token-value")).expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(error.message().starts_with("login refused: "));
    assert!(error.message().contains("JWT") && !error.message().contains("bad-token-value"));
    let error = login("no-such-method", &dir, &Person::new(CANARY)).expect_err("unknown");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_command_a_sign_in_started_is_ended_when_the_sign_in_is() {
    let dir = scratch("late");
    // `codex login --with-api-key` takes 2.5 s and then writes the key.
    std::fs::write(dir.join("fake-login"), "sleep").unwrap();
    let error = provider()
        .login(
            LoginRequest::new("api-key", dir.to_string_lossy()).timeout_ms(700),
            spawner(&dir),
            Person::new(CANARY).into_object(),
        )
        .expect_err("it ran out of time");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    std::thread::sleep(Duration::from_millis(3500));
    assert!(
        !dir.join("auth.json").exists(),
        "the command outlived the sign-in and wrote the key"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refusal_that_quotes_part_of_the_key_shows_none_of_it() {
    let dir = scratch("echo");
    std::fs::write(dir.join("fake-login"), "echo").unwrap();
    let key = "sk-live-abcdef0123456789";
    let error = provider()
        .login(
            LoginRequest::new("api-key", dir.to_string_lossy()).timeout_ms(300_000),
            spawner(&dir),
            Person::new(key).into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert_eq!(error.message(), "login refused: it was not accepted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renew_refreshes_the_login_and_a_refused_refresh_is_lost() {
    let provider = provider();
    let dir = scratch("renew");
    // The harness placed the login in the home, as it does when it starts on the account.
    std::fs::write(dir.join("auth.json"), LOGIN_FILE).unwrap();
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy());
    let report = provider
        .renew(RenewRequest::new(grant.clone()), spawner(&dir))
        .expect("renewed");
    let text = report
        .credentials
        .expect("a new login")
        .expose()
        .get("auth_file")
        .expect("the file")
        .to_string();
    assert!(text.contains("stand-in-refresh-2") && !text.contains("stand-in-refresh-1"));
    for scripted in ["refuse-blank", "refuse-delete"] {
        let refusing = scratch(scripted);
        std::fs::write(refusing.join("fake-refresh"), scripted).unwrap();
        std::fs::write(refusing.join("auth.json"), LOGIN_FILE).unwrap();
        let grant = AccountGrant::new("acct", "codex-chatgpt")
            .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
            .state_dir(refusing.to_string_lossy());
        let error = provider
            .renew(RenewRequest::new(grant), spawner(&refusing))
            .expect_err("lost");
        assert_eq!(error.status, Status::GUATIAO_ERR_GONE, "{scripted}");
        assert!(!error.message().contains("stand-in"));
        let _ = std::fs::remove_dir_all(&refusing);
    }
    let key = AccountGrant::new("k", "codex-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let error = provider
        .renew(RenewRequest::new(key), spawner(&dir))
        .expect_err("unsupported");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_tells_a_login_renewed_behind_the_hosts_back_and_one_deleted_is_lost() {
    let provider = provider();
    let dir = scratch("watch");
    // The harness placed the login before the watch began.
    std::fs::write(dir.join("auth.json"), LOGIN_FILE).unwrap();
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy())
        .revision(5);
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    std::thread::sleep(Duration::from_millis(400));
    assert!(heard.renewed.lock().unwrap().is_empty() && heard.lost.lock().unwrap().is_empty());
    let rotated = LOGIN_FILE.replace("stand-in-refresh-1", "stand-in-refresh-2");
    std::fs::write(dir.join("auth.json"), &rotated).unwrap();
    wait_for(|| !heard.renewed.lock().unwrap().is_empty());
    {
        let renewed = heard.renewed.lock().unwrap();
        assert_eq!(renewed.len(), 1);
        assert_eq!(renewed[0].from_revision, 5);
        assert_eq!(
            renewed[0].credentials.expose().get("auth_file"),
            Some(rotated.as_str())
        );
    }
    std::fs::remove_file(dir.join("auth.json")).unwrap();
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_says_when_the_access_token_ends_and_a_login_of_another_account_is_lost() {
    let provider = provider();
    let dir = scratch("identity");
    let before = JWT_FILE.replace("stand-in-refresh-2", "stand-in-refresh-1");
    std::fs::write(dir.join("auth.json"), &before).unwrap();
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", &before)]))
        .state_dir(dir.to_string_lossy());
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    std::fs::write(dir.join("auth.json"), JWT_FILE).unwrap();
    wait_for(|| !heard.renewed.lock().unwrap().is_empty());
    {
        let renewed = heard.renewed.lock().unwrap();
        assert_eq!(renewed.len(), 1);
        assert_eq!(
            renewed[0].expires_at.as_deref(),
            Some("2100-01-01T00:00:00Z")
        );
    }
    // Someone else's login in the home is no renewal of this account.
    std::fs::write(dir.join("auth.json"), JWT_FILE.replace("acct-A", "acct-B")).unwrap();
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_never_puts_the_grants_copy_back_and_tells_a_login_that_is_gone_at_once() {
    let provider = provider();
    let dir = scratch("watch-gone");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy());
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert!(!dir.join("auth.json").exists());
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kind_with_no_login_file_has_nothing_to_watch() {
    let dir = scratch("nowatch");
    let grant = AccountGrant::new("acct", "codex-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let error = provider()
        .watch(
            WatchRequest::new(grant),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .map(|_| ())
        .expect_err("unsupported");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(!error.message().contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kind_it_does_not_keep_is_bad_value_even_when_a_harness_would_take_its_standard() {
    let dir = scratch("foreign");
    let foreign = AccountGrant::new("acct", "someone-elses-kind")
        .provides(["codex"])
        .credentials(credentials_of(&[("token", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let error = provider()
        .renew(RenewRequest::new(foreign.clone()), spawner(&dir))
        .expect_err("not its kind");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let error = provider()
        .usage(
            AccountUsageQuery::new(foreign),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect_err("not its kind");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A loopback server that answers every request with `body`.
fn stand_in(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let addr = listener.local_addr().expect("its address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nx-ratelimit-limit-requests: 500\r\nx-ratelimit-remaining-requests: 499\r\nx-ratelimit-reset-requests: 120ms\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    addr
}

const DOCUMENT: &str = r#"{"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":17,"limit_window_seconds":18000,"reset_after_seconds":3600,"reset_at":4102450000},"secondary_window":{"used_percent":42,"limit_window_seconds":604800,"reset_after_seconds":90000,"reset_at":4102500000}},"additional_rate_limits":[],"credits":{"has_credits":false,"unlimited":false,"balance":null}}"#;

#[test]
fn a_logins_plan_windows_are_its_usage_and_a_keys_are_its_rate_limits() {
    let addr = stand_in(DOCUMENT);
    let provider = CodexAccounts::new(AccountSettings {
        executable: fake(),
        usage_endpoint: Some(format!("http://{addr}/backend-api")),
        ..AccountSettings::default()
    });
    let dir = scratch("usage");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", JWT_FILE)]))
        .state_dir(dir.to_string_lossy());
    let usage = provider
        .usage(
            AccountUsageQuery::new(grant.clone()),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("read");
    assert!(usage.supported);
    assert_eq!(usage.account.as_deref(), Some("acct"));
    let ids: Vec<&str> = usage.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["codex:five_hour", "codex:weekly"]);
    assert_eq!(usage.revision, 1);
    let again = provider
        .usage(
            AccountUsageQuery::new(grant),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("read");
    assert_eq!(again.revision, 1, "nothing changed");
    assert!(!format!("{usage:?}").contains("stand-in-refresh"));

    let key = AccountGrant::new("k", "codex-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let limits = provider
        .usage(
            AccountUsageQuery::new(key),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("answered");
    assert!(limits.supported);
    assert_eq!(limits.buckets[0].id, "requests");
    assert!(!format!("{limits:?}").contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suite, run in a process of its own whose home is the suite's, so that no test sets a variable of this one.
#[test]
fn the_provider_keeps_every_rule_of_the_kind_against_the_stand_in() {
    let dir = scratch("contract");
    let out = Command::new(std::env::current_exe().expect("the test knows itself"))
        .args([
            "--exact",
            "the_suite_inside",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .env("ACP_HARNESS_CHECK_HOME", &dir)
        .env_remove("CODEX_HOME")
        .output()
        .expect("the test runs itself");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "run by the test that sets its environment"]
fn the_suite_inside() {
    let addr = stand_in(DOCUMENT);
    let mut registry = Registry::new("coqui-acp-codex-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    settings.set("renew_poll_ms", 100);
    settings.set(
        "usage_endpoint",
        Value::from(format!("http://{addr}/backend-api").as_str()),
    );
    let provider = registry
        .offer::<dyn AccountProvider>("codex_accounts")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = AccountContract::new("codex").program(true).run(&provider);
    drop(provider);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}
