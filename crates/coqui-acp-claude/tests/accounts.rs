//! The Claude Code provider applying accounts, against the stand-in: what a start, a models call, a renewal and a
//! sign-in do to its environment and its configuration directory, and that no credential reaches a command line.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    AccountGrant, AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ChannelSink, Credentials,
    LoginAnswer, LoginForm, LoginPrompter, LoginRequest, ModelsRequest, OpenUrl, ProcessSpawner,
    RenewRequest, StartRequest,
};
use coqui_acp_claude::{ClaudeProvider, ProviderSettings};
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-claude-acct-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> ClaudeProvider {
    ClaudeProvider::new(ProviderSettings {
        executable: fake(),
        renew_poll_ms: 100,
        ..ProviderSettings::default()
    })
}

fn spawner(dir: &Path) -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().files(dir).into_object()
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

struct Sink(std::sync::mpsc::Sender<serde_json::Value>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

fn answer_to(
    received: &std::sync::mpsc::Receiver<serde_json::Value>,
    id: u64,
) -> serde_json::Value {
    loop {
        let message = received
            .recv_timeout(Duration::from_secs(30))
            .expect("an answer");
        if message["id"] == id && message.get("method").is_none() {
            return message;
        }
    }
}

/// Opens a connection on `grant` and one session, then closes it; the heard keeper has what it was told.
fn run_session(provider: &ClaudeProvider, grant: AccountGrant, dir: &Path, heard: &Heard) {
    let (sender, received) = std::sync::mpsc::channel();
    let request = StartRequest::builder().account(grant).build();
    let channel = provider
        .start(
            request,
            spawner(dir),
            Sink(sender).into_object(),
            heard.clone().into_object(),
        )
        .expect("it starts");
    let send =
        |message: serde_json::Value| channel.send(message.to_string().as_bytes()).expect("taken");
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}}),
    );
    answer_to(&received, 1);
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "session/new",
        "params": {"cwd": dir.to_string_lossy(), "mcpServers": []}}),
    );
    let opened = answer_to(&received, 2);
    assert!(opened["result"]["sessionId"].is_string(), "{opened}");
    channel.close(0, "");
    drop(channel);
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

fn credentials_of(pairs: &[(&str, &str)]) -> Credentials {
    pairs.iter().copied().collect()
}

#[test]
fn an_environment_kind_sets_its_variable_and_removes_every_other_credential_variable() {
    let provider = provider();
    for (kind, key, variable) in [
        ("claude-oauth-token", "token", "CLAUDE_CODE_OAUTH_TOKEN"),
        ("anthropic-api-key", "api_key", "ANTHROPIC_API_KEY"),
        ("anthropic-auth-token", "auth_token", "ANTHROPIC_AUTH_TOKEN"),
    ] {
        let dir = scratch(kind);
        // The host's own environment names a credential and the configuration directory.
        let environment: coqui_acp_api::Environment = [
            ("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()),
            ("ANTHROPIC_API_KEY", "host-key".to_string()),
            ("ANTHROPIC_AWS_API_KEY", "host-aws".to_string()),
        ]
        .into_iter()
        .collect();
        let grant = AccountGrant::new("acct", kind)
            .credentials(credentials_of(&[(key, CANARY)]))
            .settings(coqui_acp_harness::provider::schema(
                &serde_json::json!({"base_url": "https://gateway.example"}),
            ));
        let (sender, received) = std::sync::mpsc::channel();
        let request = StartRequest::builder()
            .account(grant)
            .environment(environment)
            .build();
        let channel = provider
            .start(
                request,
                spawner(&dir),
                Sink(sender).into_object(),
                Heard::default().into_object(),
            )
            .expect("it starts");
        channel
            .send(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#,
            )
            .expect("taken");
        answer_to(&received, 1);
        channel
            .send(
                serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "session/new",
                    "params": {"cwd": dir.to_string_lossy(), "mcpServers": []}})
                .to_string()
                .as_bytes(),
            )
            .expect("taken");
        answer_to(&received, 2);
        channel.close(0, "");
        drop(channel);
        let started = log(&dir);
        let entry = started.last().expect("it was started");
        let seen = names(entry);
        assert!(
            seen.contains(&variable.to_string()),
            "{kind}: {variable} set"
        );
        assert!(
            seen.contains(&"ANTHROPIC_BASE_URL".to_string()),
            "{kind}: the endpoint"
        );
        for other in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_AWS_API_KEY",
        ] {
            if other != variable {
                assert!(
                    !seen.contains(&other.to_string()),
                    "{kind}: {other} removed"
                );
            }
        }
        assert!(
            !std::fs::read_to_string(dir.join("fake-claude.log"))
                .unwrap()
                .contains(CANARY),
            "{kind}: the command line and environment names hold no credential"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_login_kind_writes_its_file_into_the_directory_and_points_claude_code_at_it() {
    let provider = provider();
    let dir = scratch("login");
    let text = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{CANARY}","refreshToken":"r","expiresAt":4102444800000}}}}"#
    );
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", &text)]))
        .state_dir(dir.to_string_lossy());
    let heard = Heard::default();
    run_session(&provider, grant, &dir, &heard);
    assert_eq!(
        std::fs::read_to_string(dir.join(".credentials.json")).unwrap(),
        text
    );
    let started = log(&dir);
    let entry = started.last().expect("it was started");
    assert_eq!(
        entry["config"].as_str(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert!(
        !std::fs::read_to_string(dir.join("fake-claude.log"))
            .unwrap()
            .contains(CANARY)
    );
    assert!(heard.renewed.lock().unwrap().is_empty() && heard.lost.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_login_the_connection_saw_renewed_is_reported_and_one_it_saw_blanked_is_lost() {
    let provider = provider();
    let dir = scratch("watch");
    let text =
        r#"{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":4102444800000}}"#;
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", text)]))
        .state_dir(dir.to_string_lossy())
        .revision(3);
    let heard = Heard::default();
    let (sender, received) = std::sync::mpsc::channel();
    let channel = provider
        .start(
            StartRequest::builder().account(grant).build(),
            spawner(&dir),
            Sink(sender).into_object(),
            heard.clone().into_object(),
        )
        .expect("it starts");
    channel
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#)
        .expect("taken");
    answer_to(&received, 1);
    let rotated =
        r#"{"claudeAiOauth":{"accessToken":"a2","refreshToken":"r2","expiresAt":4102444900000}}"#;
    std::fs::write(dir.join(".credentials.json"), rotated).unwrap();
    let waited = std::time::Instant::now();
    while heard.renewed.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    {
        let renewed = heard.renewed.lock().unwrap();
        assert_eq!(renewed.len(), 1);
        assert_eq!(renewed[0].from_revision, 3);
        assert_eq!(
            renewed[0].credentials.expose().get("credentials_file"),
            Some(rotated)
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
    let waited = std::time::Instant::now();
    while heard.lost.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    channel.close(0, "");
    drop(channel);
    let _ = std::fs::remove_dir_all(&dir);
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
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", &near_expiry())]))
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
        let grant = AccountGrant::new("acct", "claude-login")
            .credentials(credentials_of(&[("credentials_file", &near_expiry())]))
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

#[test]
fn a_models_call_hears_a_rotation_and_leaves_no_credential_on_a_command_line() {
    let provider = provider();
    let dir = scratch("models");
    let text =
        r#"{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":4102444800000}}"#;
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", text)]))
        .state_dir(dir.to_string_lossy());
    let heard = Heard::default();
    provider
        .models(
            ModelsRequest::new().account(grant.clone()),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
    assert!(heard.renewed.lock().unwrap().is_empty());
    let rotated =
        r#"{"claudeAiOauth":{"accessToken":"a2","refreshToken":"r2","expiresAt":4102444900000}}"#;
    std::fs::write(dir.join(".credentials.json"), rotated).unwrap();
    provider
        .models(
            ModelsRequest::new().account(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_grant_the_kind_refuses_is_bad_value_and_a_directory_without_files_is_unsupported() {
    let provider = provider();
    let dir = scratch("refuse");
    let sink = || Sink(std::sync::mpsc::channel().0).into_object();
    let keeper = || Heard::default().into_object();
    let start = |grant: AccountGrant, spawner: Object<dyn ProcessSpawner>| {
        provider
            .start(
                StartRequest::builder().account(grant).build(),
                spawner,
                sink(),
                keeper(),
            )
            .map(|_| ())
            .expect_err("refused")
    };
    let login = AccountGrant::new("a", "claude-login")
        .credentials(credentials_of(&[("credentials_file", "x")]))
        .state_dir(dir.to_string_lossy());
    // The credential of another kind, one more than the kind takes, a setting it lacks, an address that is not one.
    for refused in [
        login
            .clone()
            .credentials(credentials_of(&[("api_key", "x")])),
        login
            .clone()
            .credentials(credentials_of(&[("credentials_file", "x"), ("extra", "y")])),
        login.clone().settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"colour": "red"}),
        )),
        login.clone().settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"base_url": "ftp://x"}),
        )),
    ] {
        let error = start(refused, spawner(&dir));
        assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
        assert!(!error.message().contains(CANARY));
    }
    let error = start(login, LocalSpawner::new().into_object());
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let _ = std::fs::remove_dir_all(&dir);
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

fn login(
    provider: &ClaudeProvider,
    method: &str,
    dir: &Path,
    person: &Person,
) -> Result<coqui_acp_api::LoginOutcome, ProviderError> {
    provider.login(
        LoginRequest::new(method, dir.to_string_lossy()).timeout_ms(20_000),
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

/// Starts a session on `grant`, with `environment` and the provider given, and answers what the stand-in saw.
fn started_with(
    provider: &ClaudeProvider,
    grant: AccountGrant,
    dir: &Path,
    environment: coqui_acp_api::Environment,
) -> serde_json::Value {
    let (sender, received) = std::sync::mpsc::channel();
    let request = StartRequest::builder()
        .account(grant)
        .environment(environment)
        .build();
    let channel = provider
        .start(
            request,
            spawner(dir),
            Sink(sender).into_object(),
            Heard::default().into_object(),
        )
        .expect("it starts");
    channel
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#)
        .expect("taken");
    answer_to(&received, 1);
    channel
        .send(
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "session/new",
                "params": {"cwd": dir.to_string_lossy(), "mcpServers": []}})
            .to_string()
            .as_bytes(),
        )
        .expect("taken");
    answer_to(&received, 2);
    channel.close(0, "");
    drop(channel);
    log(dir).last().expect("it was started").clone()
}

#[test]
fn no_endpoint_switch_or_cloud_credential_of_the_host_reaches_claude_code_beside_an_account() {
    let dir = scratch("hostile");
    let hostile = [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_VERTEX_BASE_URL",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_BEDROCK",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_ACCESS_KEY_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "ANTHROPIC_AUTH_TOKEN",
    ];
    let mut environment: coqui_acp_api::Environment = hostile
        .iter()
        .map(|name| (*name, "host-value".to_string()))
        .collect();
    environment.insert("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned());
    let grant = AccountGrant::new("acct", "anthropic-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]));
    let entry = started_with(&provider(), grant, &dir, environment);
    let seen = names(&entry);
    for name in hostile {
        assert!(
            !seen.contains(&name.to_string()),
            "{name} reached Claude Code"
        );
    }
    assert!(seen.contains(&"ANTHROPIC_API_KEY".to_string()));
    // The endpoint comes from the grant alone.
    let grant = AccountGrant::new("acct", "anthropic-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"base_url": "https://gateway.example"}),
        ));
    let mut environment: coqui_acp_api::Environment =
        [("ANTHROPIC_BASE_URL", "https://host.example".to_string())]
            .into_iter()
            .collect();
    environment.insert("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned());
    let entry = started_with(&provider(), grant, &dir, environment);
    assert_eq!(
        names(&entry)
            .iter()
            .filter(|n| *n == "ANTHROPIC_BASE_URL")
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_environment_kind_leaves_the_users_own_settings_out_unless_the_entry_says_otherwise() {
    let dir = scratch("sources");
    let environment = || -> coqui_acp_api::Environment {
        [("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned())]
            .into_iter()
            .collect()
    };
    let key = || {
        AccountGrant::new("acct", "anthropic-api-key")
            .credentials(credentials_of(&[("api_key", CANARY)]))
    };
    let entry = started_with(&provider(), key(), &dir, environment());
    assert!(
        entry["args"]
            .to_string()
            .contains("--setting-sources=project,local"),
        "{entry}"
    );
    // The entry's own sources stand.
    let own = ClaudeProvider::new(ProviderSettings {
        executable: fake(),
        setting_sources: Some(vec![coqui_acp_claude::SettingSource::User]),
        ..ProviderSettings::default()
    });
    let entry = started_with(&own, key(), &dir, environment());
    assert!(
        entry["args"]
            .to_string()
            .contains("--setting-sources=user\""),
        "{entry}"
    );
    // A login kind runs in a directory of its own with no user settings, and is left as it is.
    let login_dir = scratch("sources-login");
    let text =
        r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":4102444800000}}"#;
    let login = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[("credentials_file", text)]))
        .state_dir(login_dir.to_string_lossy());
    let entry = started_with(
        &provider(),
        login,
        &login_dir,
        coqui_acp_api::Environment::new(),
    );
    assert!(
        !entry["args"].to_string().contains("--setting-sources"),
        "{entry}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&login_dir);
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

#[cfg(windows)]
#[test]
fn a_credential_variable_of_another_case_does_not_survive_on_windows() {
    let dir = scratch("case");
    let mut environment: coqui_acp_api::Environment =
        [("anthropic_api_key", "host-key".to_string())]
            .into_iter()
            .collect();
    environment.insert("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned());
    let grant = AccountGrant::new("acct", "claude-oauth-token")
        .credentials(credentials_of(&[("token", CANARY)]));
    let entry = started_with(&provider(), grant, &dir, environment);
    assert!(
        !names(&entry)
            .iter()
            .any(|n| n.eq_ignore_ascii_case("ANTHROPIC_API_KEY")),
        "{entry}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
