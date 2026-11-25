//! The Claude Code account provider against the stand-in: signing in, renewing, watching and reading plan limits, and
//! that it keeps every rule of the kind. A harness applies an account the provider made.
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
    AccountGrant, AccountKeeper, AccountLoss, AccountModelsQuery, AccountProvider, AccountRenewal,
    AccountUsageQuery, Credentials, HARNESS_LOGIN, LoginAnswer, LoginForm, LoginPrompter,
    LoginRequest, OpenUrl, ProcessSpawner, RenewRequest, WatchRequest,
};
use coqui_acp_claude::{AccountSettings, ClaudeAccounts};
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
        .join(format!("fake_claude{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn library() -> PathBuf {
    profile().join("examples").join(format!(
        "{}claude_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-claude-prov-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> ClaudeAccounts {
    ClaudeAccounts::new(AccountSettings {
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

fn log(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("fake-claude.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a log line"))
        .collect()
}

fn names(entry: &serde_json::Value) -> Vec<String> {
    entry["env_names"]
        .as_array()
        .expect("names")
        .iter()
        .map(|n| n.as_str().expect("a name").to_string())
        .collect()
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

fn login_text(access: &str, refresh: &str, expires_ms: u64) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires_ms},"scopes":["user:inference","user:profile"]}}}}"#
    )
}

fn login_grant(dir: &Path, text: &str) -> AccountGrant {
    AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", text)]))
        .state_dir(dir.to_string_lossy())
}

/// A person at a prompter: answers every secret with `secret` and records what it was shown.
#[derive(Clone)]
struct Person {
    secret: String,
    shown: Arc<Mutex<Vec<String>>>,
}

impl Person {
    fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            shown: Arc::default(),
        }
    }
}

impl LoginPrompter for Person {
    fn say(&self, text: &str) -> Result<(), ProviderError> {
        self.shown.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn open_url(&self, open: OpenUrl) -> Result<(), ProviderError> {
        self.shown.lock().unwrap().push(open.url);
        Ok(())
    }

    fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        let schema = coqui_acp_harness::provider::to_json(&form.schema);
        let mut secrets = Credentials::new();
        for (name, property) in schema["properties"].as_object().expect("fields") {
            assert_eq!(property["writeOnly"], true, "only secrets are asked");
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

fn near_expiry() -> String {
    let soon = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 60_000;
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"stand-in-access-1","refreshToken":"stand-in-refresh-1","expiresAt":{soon}}}}}"#
    )
}

#[test]
fn renew_refreshes_a_login_in_its_last_minutes_and_leaves_one_that_is_not_due() {
    let provider = provider();
    let dir = scratch("renew");
    // The harness placed the login in the directory, as it does when it starts on the account.
    let text = near_expiry();
    std::fs::write(dir.join(".credentials.json"), &text).unwrap();
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", &text)]))
        .state_dir(dir.to_string_lossy())
        .revision(2);
    let report = provider
        .renew(RenewRequest::new(grant.clone()), spawner(&dir))
        .expect("renewed");
    let credentials = report.credentials.expect("a new login");
    let text = credentials
        .expose()
        .get("credentials_file")
        .expect("the file")
        .to_string();
    assert!(text.contains("stand-in-access-2") && !text.contains("stand-in-access-1"));
    assert!(report.expires_at.is_some() && report.not_renewed_because.is_none());

    let far =
        r#"{"claudeAiOauth":{"accessToken":"x","refreshToken":"y","expiresAt":4102444800000}}"#;
    let other = scratch("renew-far");
    std::fs::write(other.join(".credentials.json"), far).unwrap();
    let grant = AccountGrant::new("far", "claude-login")
        .credentials(credentials_of(&[("credentials_file", far)]))
        .state_dir(other.to_string_lossy());
    let report = provider
        .renew(RenewRequest::new(grant), spawner(&other))
        .expect("answered");
    assert!(report.credentials.is_none());
    assert!(
        report
            .not_renewed_because
            .is_some_and(|why| why.contains("five minutes"))
    );
    assert!(
        log(&other).is_empty(),
        "nothing was run for a login that is not due"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn a_refused_refresh_is_a_lost_login_and_never_a_renewal() {
    for scripted in ["refuse-blank", "refuse-delete"] {
        let provider = provider();
        let dir = scratch(scripted);
        std::fs::write(dir.join("fake-refresh"), scripted).unwrap();
        let text = near_expiry();
        std::fs::write(dir.join(".credentials.json"), &text).unwrap();
        let grant = AccountGrant::new("acct", "claude-login")
            .credentials(credentials_of(&[("credentials_file", &text)]))
            .state_dir(dir.to_string_lossy());
        let error = provider
            .renew(RenewRequest::new(grant), spawner(&dir))
            .expect_err("lost");
        assert_eq!(error.status, Status::GUATIAO_ERR_GONE, "{scripted}");
        assert!(!error.message().is_empty() && !error.message().contains("stand-in"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_kind_with_no_renewal_says_so() {
    let dir = scratch("norenew");
    let grant = AccountGrant::new("acct", "anthropic-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]));
    let error = provider()
        .renew(RenewRequest::new(grant), spawner(&dir))
        .expect_err("unsupported");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(!error.message().contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

fn login(
    provider: &ClaudeAccounts,
    method: &str,
    dir: &Path,
    person: &Person,
) -> Result<coqui_acp_api::LoginOutcome, ProviderError> {
    provider.login(
        LoginRequest::new(method, dir.to_string_lossy()).timeout_ms(300_000),
        spawner(dir),
        person.clone().into_object(),
    )
}

#[test]
fn a_subscription_sign_in_relays_the_page_and_the_code_and_reads_the_login_it_wrote() {
    let dir = scratch("signin");
    let person = Person::new(CANARY);
    let outcome = login(&provider(), "subscription", &dir, &person).expect("signed in");
    assert_eq!(outcome.kind, "claude-login");
    assert!(outcome.checked && outcome.expires_at.is_some());
    let file = outcome
        .credentials
        .expose()
        .get("credentials_file")
        .expect("the login")
        .to_string();
    assert!(file.contains("stand-in-access-1"));
    let shown = person.shown.lock().unwrap().clone();
    assert!(
        shown
            .iter()
            .any(|s| s.starts_with("https://claude.com/cai/oauth/authorize?")),
        "{shown:?}"
    );
    let commands = std::fs::read_to_string(dir.join("fake-claude.log")).unwrap();
    assert!(
        !commands.contains(CANARY),
        "the code is in no command line or environment"
    );
    for entry in log(&dir) {
        let seen = names(&entry);
        for variable in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            assert!(!seen.contains(&variable.to_string()));
        }
    }
    // A code the sign-in refuses is a refused login that names no code and no page.
    let refusing = scratch("signin-refused");
    std::fs::write(refusing.join("fake-login"), "refuse").unwrap();
    let error = login(&provider(), "console", &refusing, &person).expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(error.message().starts_with("login refused: "));
    assert!(!error.message().contains(CANARY) && !error.message().contains("://"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&refusing);
}

#[test]
fn a_key_or_token_is_checked_with_auth_status_and_never_put_on_a_command_line() {
    for (method, kind, key) in [
        ("api-key", "anthropic-api-key", "api_key"),
        ("auth-token", "anthropic-auth-token", "auth_token"),
    ] {
        let dir = scratch(method);
        let outcome = login(&provider(), method, &dir, &Person::new(CANARY)).expect("accepted");
        assert_eq!(outcome.kind, kind);
        assert!(outcome.checked);
        assert_eq!(outcome.credentials.expose().get(key), Some(CANARY));
        let commands = std::fs::read_to_string(dir.join("fake-claude.log")).unwrap();
        assert!(!commands.contains(CANARY));
        assert!(commands.contains("\"auth\"") && commands.contains("\"status\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_method_it_cannot_run_is_unsupported_with_the_reason_and_an_unknown_one_is_bad_value() {
    let dir = scratch("methods");
    let error = login(&provider(), "long-lived-token", &dir, &Person::new(CANARY))
        .expect_err("unsupported");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(error.message().contains("terminal"));
    let error =
        login(&provider(), "no-such-method", &dir, &Person::new(CANARY)).expect_err("unknown");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renew_answers_a_login_newer_than_the_grant_as_the_renewal_it_is() {
    let provider = provider();
    let dir = scratch("newer");
    let held =
        r#"{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":4102444800000}}"#;
    let newer =
        r#"{"claudeAiOauth":{"accessToken":"a2","refreshToken":"r2","expiresAt":4102444900000}}"#;
    // The harness renewed the login after the host stored its copy: the directory holds the newer one.
    std::fs::write(dir.join(".credentials.json"), newer).unwrap();
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", held)]))
        .state_dir(dir.to_string_lossy());
    let report = provider
        .renew(RenewRequest::new(grant), spawner(&dir))
        .expect("answered");
    let credentials = report.credentials.expect("the newer login is the renewal");
    assert_eq!(credentials.expose().get("credentials_file"), Some(newer));
    assert!(report.not_renewed_because.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_command_a_sign_in_started_is_ended_when_the_sign_in_is() {
    let dir = scratch("late");
    // `claude auth status` takes 2.5 s and leaves a mark when it ends.
    std::fs::write(dir.join("fake-status-sleep"), "").unwrap();
    let error = provider()
        .login(
            LoginRequest::new("api-key", dir.to_string_lossy()).timeout_ms(700),
            spawner(&dir),
            Person::new(CANARY).into_object(),
        )
        .expect_err("it ran out of time");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    assert!(error.message().contains("timed out"));
    std::thread::sleep(Duration::from_millis(3500));
    assert!(
        !dir.join("status-finished").exists(),
        "the command outlived the sign-in"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_tells_a_login_renewed_behind_the_hosts_back_and_one_blanked_is_lost() {
    let provider = provider();
    let dir = scratch("watch");
    let text = login_text("a1", "r1", 4_102_444_800_000);
    // The harness placed the login before the watch began.
    std::fs::write(dir.join(".credentials.json"), &text).unwrap();
    let grant = login_grant(&dir, &text).revision(3);
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
    let rotated = login_text("a2", "r2", 4_102_444_900_000);
    std::fs::write(dir.join(".credentials.json"), &rotated).unwrap();
    wait_for(|| !heard.renewed.lock().unwrap().is_empty());
    {
        let renewed = heard.renewed.lock().unwrap();
        assert_eq!(renewed.len(), 1);
        assert_eq!(renewed[0].from_revision, 3);
        assert_eq!(
            renewed[0].credentials.expose().get("credentials_file"),
            Some(rotated.as_str())
        );
        assert_eq!(
            renewed[0].expires_at.as_deref(),
            Some("2100-01-01T00:01:40Z")
        );
    }
    // A refused refresh blanks both tokens: lost, never renewed.
    std::fs::write(
        dir.join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":1}}"#,
    )
    .unwrap();
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_never_puts_the_grants_copy_back_and_tells_a_login_that_is_gone_at_once() {
    let provider = provider();
    let dir = scratch("watch-gone");
    let text = login_text("a1", "r1", 4_102_444_800_000);
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(login_grant(&dir, &text)),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert!(!dir.join(".credentials.json").exists());
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_kind_with_no_login_file_has_nothing_to_watch() {
    let dir = scratch("nowatch");
    let grant = AccountGrant::new("acct", "anthropic-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]));
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
        .provides(["claude-code"])
        .credentials(credentials_of(&[("token", CANARY)]));
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
    stand_in_seen(body).0
}

/// [`stand_in`], and the requests it was sent, as text.
fn stand_in_seen(body: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let addr = listener.local_addr().expect("its address");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::clone(&seen);
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
            heard
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&bytes).into_owned());
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nanthropic-ratelimit-requests-limit: 50\r\nanthropic-ratelimit-requests-remaining: 7\r\nanthropic-ratelimit-requests-reset: 2026-11-16T20:00:00Z\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (addr, seen)
}

const DOCUMENT: &str = r#"{"five_hour":{"utilization":12.5,"resets_at":"2026-11-16T20:00:00+00:00"},"seven_day":{"utilization":34.0,"resets_at":"2026-11-20T10:00:00+00:00"}}"#;

#[test]
fn a_logins_plan_windows_are_its_usage_and_a_console_login_has_none() {
    let addr = stand_in(DOCUMENT);
    let provider = ClaudeAccounts::new(AccountSettings {
        executable: fake(),
        usage_endpoint: Some(format!("http://{addr}")),
        ..AccountSettings::default()
    });
    let dir = scratch("usage");
    let text = login_text("a1", "r1", 4_102_444_800_000);
    let usage = provider
        .usage(
            AccountUsageQuery::new(login_grant(&dir, &text)),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("read");
    assert!(usage.supported);
    assert_eq!(usage.account.as_deref(), Some("acct"));
    let ids: Vec<&str> = usage.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["five_hour", "seven_day"]);
    assert_eq!(usage.revision, 1);
    let again = provider
        .usage(
            AccountUsageQuery::new(login_grant(&dir, &text)),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("read");
    assert_eq!(again.revision, 1, "nothing changed");
    assert!(!format!("{usage:?}").contains("a1"));

    let console = AccountGrant::new("c", "claude-console")
        .credentials(credentials_of(&[("credentials_file", &text)]))
        .state_dir(dir.to_string_lossy());
    let none = provider
        .usage(
            AccountUsageQuery::new(console),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("answered");
    assert!(!none.supported && none.unsupported_because.is_some() && none.buckets.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suite, run in a process of its own whose home is the suite's, so that no test sets a variable of this one.
#[test]
fn the_provider_keeps_every_rule_of_the_kind_against_the_stand_in() {
    let dir = scratch("contract");
    // The login Claude Code keeps in this home, which the local account's plan usage is read with.
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(
        dir.join(".claude").join(".credentials.json"),
        login_text("contract-access", "contract-refresh", 4_102_444_800_000),
    )
    .unwrap();
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
        .env_remove("CLAUDE_CONFIG_DIR")
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
    let mut registry = Registry::new("coqui-acp-claude-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    settings.set("renew_poll_ms", 100);
    settings.set(
        "usage_endpoint",
        Value::from(format!("http://{addr}").as_str()),
    );
    let provider = registry
        .offer::<dyn AccountProvider>("claude_accounts")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = AccountContract::new("claude").program(true).run(&provider);
    drop(provider);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}

/// What the local account's models are for a provider running `executable` with `environment` and nothing else of the
/// host's.
fn listing_of(
    executable: String,
    environment: coqui_acp_api::Environment,
) -> coqui_acp_api::ModelListing {
    ClaudeAccounts::new(AccountSettings {
        executable,
        ..AccountSettings::default()
    })
    .models(
        AccountModelsQuery::new(AccountGrant::new("local", HARNESS_LOGIN)).environment(environment),
        LocalSpawner::new().into_object(),
        Heard::default().into_object(),
    )
    .expect("answered")
}

/// The recordings of Claude Code 2.1.318.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("2.1.318")
}

/// The replaying stand-in, built beside the tests.
fn replay_program() -> PathBuf {
    profile()
        .join("examples")
        .join(format!("replay_claude{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn it_offers_the_login_of_its_program_as_the_automatic_local_account() {
    let offer = provider().offers().expect("offered");
    assert!(offer.local && offer.reports_usage(HARNESS_LOGIN));
    assert!(
        offer.kind(HARNESS_LOGIN).is_none(),
        "it cannot sign in to it"
    );
}

/// What the local account's usage is for a provider on `endpoint`, with Claude Code's configuration in `config`.
fn local_usage(
    endpoint: Option<String>,
    config: &Path,
) -> Result<coqui_acp_api::AccountUsage, ProviderError> {
    ClaudeAccounts::new(AccountSettings {
        executable: fake(),
        usage_endpoint: endpoint,
        ..AccountSettings::default()
    })
    .usage(
        AccountUsageQuery::new(AccountGrant::new("local", HARNESS_LOGIN)).environment(
            [("CLAUDE_CONFIG_DIR", config.to_string_lossy().into_owned())]
                .into_iter()
                .collect(),
        ),
        LocalSpawner::new().into_object(),
        Heard::default().into_object(),
    )
}

#[test]
fn the_local_account_reads_plan_usage_with_the_login_in_claude_codes_configuration_directory() {
    let (addr, seen) = stand_in_seen(DOCUMENT);
    let config = scratch("local-usage");
    std::fs::write(
        config.join(".credentials.json"),
        login_text("local-access-9", "local-refresh-9", 4_102_444_800_000),
    )
    .unwrap();
    let usage = local_usage(Some(format!("http://{addr}")), &config).expect("read");
    assert!(usage.supported, "{usage:?}");
    assert_eq!(usage.account.as_deref(), Some("local"));
    let ids: Vec<&str> = usage.buckets.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["five_hour", "seven_day"]);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].starts_with("GET /api/oauth/usage "));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer local-access-9"),
        "the figures are read with the login Claude Code keeps"
    );
    assert!(!format!("{usage:?}").contains("local-access-9"));
    assert!(log(&config).is_empty(), "no program was needed");
    // The login is read, never written.
    assert_eq!(
        std::fs::read_to_string(config.join(".credentials.json")).unwrap(),
        login_text("local-access-9", "local-refresh-9", 4_102_444_800_000)
    );
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn the_local_account_renews_an_expired_login_through_claude_code_and_reads_again() {
    let (addr, seen) = stand_in_seen(DOCUMENT);
    let config = scratch("local-usage-renew");
    std::fs::write(
        config.join(".credentials.json"),
        login_text("stand-in-access-1", "stand-in-refresh-1", 1_000),
    )
    .unwrap();
    let usage = local_usage(Some(format!("http://{addr}")), &config).expect("read");
    assert!(usage.supported, "{usage:?}");
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "the expired token was never sent");
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer stand-in-access-2"),
        "the renewed token was read from the same directory"
    );
    assert!(
        log(&config)
            .iter()
            .any(|entry| entry["args"] == serde_json::json!(["auth", "status"]))
    );
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn the_local_account_with_no_login_file_has_no_usage_and_says_why() {
    let config = scratch("local-usage-none");
    let usage = local_usage(None, &config).expect("answered");
    assert!(!usage.supported && usage.buckets.is_empty());
    assert!(usage.unsupported_because.is_some());
    assert_eq!(usage.account.as_deref(), Some("local"));
    // A login that cannot be renewed is the person's next step, not a figure.
    std::fs::write(
        config.join(".credentials.json"),
        login_text("a", "r", 1_000),
    )
    .unwrap();
    std::fs::write(config.join("fake-refresh"), "refuse-blank").unwrap();
    let (addr, seen) = stand_in_seen(DOCUMENT);
    let error = local_usage(Some(format!("http://{addr}")), &config).expect_err("expired");
    assert_eq!(error.status, Status::GUATIAO_ERR_GONE);
    assert!(seen.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn models_of_an_account_it_does_not_keep_are_refused_as_a_bad_value() {
    let ask = |grant: AccountGrant| {
        provider().models(
            AccountModelsQuery::new(grant),
            LocalSpawner::new().into_object(),
            Heard::default().into_object(),
        )
    };
    // A login with no credential or directory is not a grant the kind takes.
    let error = ask(AccountGrant::new("a", "claude-login")).expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let error = ask(AccountGrant::new("a", "no-such-kind")).expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
}

/// What the recorded `initialize` of Claude Code 2.1.318 gives a host that asks for the models before any session: no
/// turn is made, since the stand-in replays only the recording's control answers.
#[test]
fn the_local_account_lists_the_recorded_models_with_versions_and_efforts_and_no_turn() {
    let environment: coqui_acp_api::Environment = [(
        "COQUI_ACP_CLAUDE_REPLAY",
        fixtures()
            .join("cli_session")
            .to_string_lossy()
            .into_owned(),
    )]
    .into_iter()
    .collect();
    let listing = listing_of(replay_program().to_string_lossy().into_owned(), environment);
    assert!(listing.known, "{listing:?}");
    let seen: Vec<_> = listing
        .models
        .iter()
        .map(|model| {
            (
                model.id.as_str(),
                model.name.as_deref().unwrap(),
                model.resolved.as_deref().unwrap(),
                model.version.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "default",
                "Default (Opus 5.5)",
                "claude-opus-5-5",
                "Opus 5.5"
            ),
            ("opus", "Opus 5.5", "claude-opus-5-5", "Opus 5.5"),
            ("fable", "Fable 5.1", "claude-fable-5-1", "Fable 5.1"),
            ("sonnet", "Sonnet 5.5", "claude-sonnet-5-5", "Sonnet 5.5"),
            ("haiku", "Haiku 5.5", "claude-haiku-5-5", "Haiku 5.5"),
        ]
    );
    assert_eq!(
        listing
            .models
            .iter()
            .filter(|model| model.is_default)
            .count(),
        1,
        "only the default entry"
    );
    for model in &listing.models {
        assert_eq!(model.efforts, ["low", "medium", "high", "xhigh", "max"]);
    }
}

#[test]
fn the_local_account_models_of_a_program_that_is_not_installed_say_so_by_kind() {
    let dir = scratch("models-missing");
    let missing = dir.join("no-such-claude");
    let listing = listing_of(
        missing.to_string_lossy().into_owned(),
        coqui_acp_api::Environment::new(),
    );
    assert!(!listing.known, "{listing:?}");
    assert_eq!(
        listing.unknown_kind,
        Some(coqui_acp_api::UnknownKind::NotInstalled)
    );
    assert!(listing.unknown_because.is_some() && listing.models.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_local_account_models_of_a_program_that_cannot_be_run_are_a_failure_with_its_text() {
    // A directory is found, and cannot be started.
    let dir = scratch("models-failed");
    let listing = listing_of(
        dir.to_string_lossy().into_owned(),
        coqui_acp_api::Environment::new(),
    );
    assert!(!listing.known, "{listing:?}");
    assert_eq!(
        listing.unknown_kind,
        Some(coqui_acp_api::UnknownKind::Failed)
    );
    assert!(
        listing
            .unknown_because
            .as_deref()
            .is_some_and(|why| why.contains("could not be")),
        "{listing:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_local_account_models_of_a_claude_code_with_no_login_say_nobody_is_signed_in() {
    let dir = scratch("models-signed-out");
    let environment = |signed_out: bool| -> coqui_acp_api::Environment {
        let mut pairs = vec![("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned())];
        if signed_out {
            pairs.push(("COQUI_ACP_FAKE_CLAUDE_SIGNED_OUT", "1".to_owned()));
        }
        pairs.into_iter().collect()
    };
    let out = listing_of(fake(), environment(true));
    assert!(out.known && out.signed_out, "{out:?}");
    assert!(!out.models.is_empty() && out.unknown_kind.is_none());
    let signed_in = listing_of(fake(), environment(false));
    assert!(signed_in.known && !signed_in.signed_out, "{signed_in:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_local_account_of_a_claude_code_that_lists_no_model_says_why_and_never_waits_for_a_session() {
    let dir = scratch("models-none");
    let environment = |signed_out: bool| -> coqui_acp_api::Environment {
        let mut pairs = vec![
            ("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()),
            ("COQUI_ACP_FAKE_CLAUDE_NO_MODELS", "1".to_owned()),
        ];
        if signed_out {
            pairs.push(("COQUI_ACP_FAKE_CLAUDE_SIGNED_OUT", "1".to_owned()));
        }
        pairs.into_iter().collect()
    };
    let out = listing_of(fake(), environment(true));
    assert!(
        !out.known && out.signed_out && out.models.is_empty(),
        "{out:?}"
    );
    assert!(out.unknown_because.is_some(), "{out:?}");
    let other = listing_of(fake(), environment(false));
    assert!(!other.known && !other.signed_out, "{other:?}");
    assert_eq!(other.unknown_kind, Some(coqui_acp_api::UnknownKind::Failed));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ClaudeAccounts::signed_in` for Claude Code's configuration in `config`, with every variable Claude Code signs in by
/// emptied (an empty value is no login) and `extra` set over that.
fn signed_in(config: &Path, extra: &[(&str, &str)]) -> coqui_acp_api::SignedInCheck {
    let mut environment = coqui_acp_api::Environment::new();
    for name in coqui_acp_claude::signed_in::SIGN_IN_VARIABLES {
        environment.insert(*name, "");
    }
    environment.insert("CLAUDE_CONFIG_DIR", config.to_string_lossy().into_owned());
    for (name, value) in extra {
        environment.insert(*name, *value);
    }
    provider()
        .signed_in(coqui_acp_api::SignedInQuery::new().environment(environment))
        .expect("it answers")
}

#[test]
fn a_credentials_file_with_a_login_in_it_is_signed_in_and_shows_nothing_of_it() {
    let config = scratch("signed-in-file");
    let text = login_text("si-access-4", "si-refresh-4", 4_102_444_800_000);
    std::fs::write(config.join(".credentials.json"), &text).unwrap();
    let answer = signed_in(&config, &[]);
    assert_eq!(answer, coqui_acp_api::SignedInCheck::signed_in());
    let shown = format!("{answer:?}");
    assert!(!shown.contains("si-access-4") && !shown.contains("si-refresh-4"));
    assert_eq!(
        std::fs::read_to_string(config.join(".credentials.json")).unwrap(),
        text,
        "the login is read, never written"
    );
    assert!(log(&config).is_empty(), "no program was started");
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn no_credentials_file_or_an_empty_one_is_signed_out_with_what_was_looked_at() {
    let config = scratch("signed-in-none");
    let missing = signed_in(&config, &[]);
    assert_eq!(missing.state, coqui_acp_api::SignedInState::SignedOut);
    let reason = missing.reason.expect("a reason");
    assert!(reason.contains("credentials file"), "{reason}");
    assert!(reason.contains("ANTHROPIC_API_KEY"), "{reason}");
    assert!(
        !reason.contains(&*config.to_string_lossy()),
        "a path is a value of the environment: {reason}"
    );
    for text in ["", "  \n", "{}"] {
        std::fs::write(config.join(".credentials.json"), text).unwrap();
        let empty = signed_in(&config, &[]);
        assert_eq!(
            empty.state,
            coqui_acp_api::SignedInState::SignedOut,
            "{text:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn a_sign_in_variable_that_is_set_counts_by_its_name_and_an_empty_one_does_not() {
    let config = scratch("signed-in-variable");
    for name in coqui_acp_claude::signed_in::SIGN_IN_VARIABLES {
        let answer = signed_in(&config, &[(name, CANARY)]);
        assert_eq!(
            answer,
            coqui_acp_api::SignedInCheck::by_variable(*name),
            "{name}"
        );
        assert!(!format!("{answer:?}").contains(CANARY), "{name}");
    }
    let empty = signed_in(&config, &[("ANTHROPIC_API_KEY", "")]);
    assert_eq!(empty.state, coqui_acp_api::SignedInState::SignedOut);
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn a_credentials_file_that_cannot_be_read_is_unknown_not_signed_out() {
    let config = scratch("signed-in-unreadable");
    // A folder where the file should be.
    std::fs::create_dir_all(config.join(".credentials.json")).unwrap();
    let answer = signed_in(&config, &[]);
    assert_eq!(answer.state, coqui_acp_api::SignedInState::Unknown);
    assert!(answer.reason.is_some_and(|reason| !reason.is_empty()));
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn the_keychain_is_asked_by_the_name_claude_code_keeps_its_item_under_and_never_read() {
    use coqui_acp_claude::signed_in::{check, keychain_service};
    use coqui_acp_harness::keyring::Presence;
    use std::cell::RefCell;
    let config = scratch("signed-in-keychain");
    let mut environment = coqui_acp_api::Environment::new();
    for name in coqui_acp_claude::signed_in::SIGN_IN_VARIABLES {
        environment.insert(*name, "");
    }
    environment.insert("CLAUDE_CONFIG_DIR", config.to_string_lossy().into_owned());
    environment.insert("USER", "kc-user");
    let asked: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
    let ask = |answer: Presence| {
        asked.borrow_mut().clear();
        let lookup = |service: &str, user: &str, _wait: Duration| {
            asked.borrow_mut().push((service.into(), user.into()));
            answer.clone()
        };
        check(&environment, Duration::from_secs(1), Some(&lookup))
    };
    let found = ask(Presence::Present);
    assert_eq!(found, coqui_acp_api::SignedInCheck::signed_in());
    let service = keychain_service(Some(&config.to_string_lossy()));
    assert_eq!(*asked.borrow(), [(service.clone(), "kc-user".to_owned())]);
    assert!(service.starts_with("Claude Code-credentials-") && service.len() == 23 + 1 + 8);
    assert_eq!(keychain_service(None), "Claude Code-credentials");
    // Not found there is unknown: the item's name was not measured on a Mac. A Keychain that failed is unknown too.
    for answer in [Presence::Absent, Presence::Unknown("slow".into())] {
        let said = ask(answer);
        assert_eq!(said.state, coqui_acp_api::SignedInState::Unknown);
        assert!(
            !said.reason.unwrap().contains(&*config.to_string_lossy()),
            "no value of the environment in the reason"
        );
    }
    // A credentials file that holds a login is enough: the Keychain is not asked.
    std::fs::write(config.join(".credentials.json"), "{\"a\":1}").unwrap();
    let from_file = ask(Presence::Absent);
    assert_eq!(from_file, coqui_acp_api::SignedInCheck::signed_in());
    assert!(asked.borrow().is_empty());
    let _ = std::fs::remove_dir_all(&config);
}
