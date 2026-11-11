//! The Codex provider applying accounts, against the stand-in: what a start, a models call, a renewal and a sign-in do
//! to its environment and its home directory, and that no credential reaches a command line.
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
use coqui_acp_codex::{CodexProvider, ProviderSettings};
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-codex-acct-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> CodexProvider {
    CodexProvider::new(ProviderSettings {
        executable: fake(),
        renew_poll_ms: 100,
        ..ProviderSettings::default()
    })
}

fn spawner(dir: &Path) -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().files(dir).into_object()
}

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

fn credentials_of(pairs: &[(&str, &str)]) -> Credentials {
    pairs.iter().copied().collect()
}

fn log(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("fake-codex.log"))
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

/// Opens a connection on `grant` and one thread, then closes it.
fn run_session(provider: &CodexProvider, grant: AccountGrant, dir: &Path, heard: &Heard) {
    let (sender, received) = std::sync::mpsc::channel();
    let channel = provider
        .start(
            StartRequest::builder().account(grant).build(),
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

const LOGIN_FILE: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"stand-in-access-1","refresh_token":"stand-in-refresh-1"}}"#;

#[test]
fn a_chatgpt_login_is_written_into_the_home_with_the_store_set_to_a_file() {
    let provider = provider();
    let dir = scratch("chatgpt");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy())
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"chatgpt_base_url": "https://gateway.example/backend"}),
        ));
    run_session(&provider, grant, &dir, &Heard::default());
    assert_eq!(
        std::fs::read_to_string(dir.join("auth.json")).unwrap(),
        LOGIN_FILE
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        "cli_auth_credentials_store = \"file\"\n"
    );
    let started = log(&dir);
    let entry = started.last().expect("it was started");
    assert_eq!(entry["home"].as_str(), Some(dir.to_string_lossy().as_ref()));
    assert!(
        entry["args"].to_string().contains("chatgpt_base_url"),
        "the endpoint reaches Codex as a setting"
    );
    for variable in ["CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_ACCESS_TOKEN"] {
        assert!(!names(entry).contains(&variable.to_string()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_key_is_saved_by_codex_login_on_standard_input_and_not_saved_again() {
    let provider = provider();
    let dir = scratch("apikey");
    let grant = AccountGrant::new("acct", "codex-api-key")
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .state_dir(dir.to_string_lossy());
    // The host's own environment names a key: it does not reach Codex.
    let heard = Heard::default();
    run_session(&provider, grant.clone(), &dir, &heard);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).unwrap()).unwrap();
    assert_eq!(saved["OPENAI_API_KEY"], CANARY);
    let first = log(&dir);
    assert!(
        first
            .iter()
            .any(|e| e["args"] == serde_json::json!(["login", "--with-api-key"]))
    );
    run_session(&provider, grant, &dir, &heard);
    let logins = log(&dir)
        .iter()
        .filter(|e| e["args"] == serde_json::json!(["login", "--with-api-key"]))
        .count();
    assert_eq!(logins, 1, "a key already saved is not saved again");
    let text = std::fs::read_to_string(dir.join("fake-codex.log")).unwrap();
    assert!(
        !text.contains(CANARY),
        "the key is in no command line or environment"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_login_the_connection_saw_renewed_is_reported_and_one_it_saw_deleted_is_lost() {
    let provider = provider();
    let dir = scratch("watch");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy())
        .revision(5);
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
    let rotated = LOGIN_FILE.replace("stand-in-refresh-1", "stand-in-refresh-2");
    std::fs::write(dir.join("auth.json"), &rotated).unwrap();
    let waited = std::time::Instant::now();
    while heard.renewed.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
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

#[test]
fn renew_refreshes_the_login_and_a_refused_refresh_is_lost() {
    let provider = provider();
    let dir = scratch("renew");
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
fn a_models_call_hears_a_rotation() {
    let provider = provider();
    let dir = scratch("models");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
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
    std::fs::write(
        dir.join("auth.json"),
        LOGIN_FILE.replace("refresh-1", "refresh-9"),
    )
    .unwrap();
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
fn a_grant_the_kind_refuses_is_bad_value_and_a_home_without_files_is_unsupported() {
    let provider = provider();
    let dir = scratch("refuse");
    let start = |grant: AccountGrant, spawner: Object<dyn ProcessSpawner>| {
        provider
            .start(
                StartRequest::builder().account(grant).build(),
                spawner,
                Sink(std::sync::mpsc::channel().0).into_object(),
                Heard::default().into_object(),
            )
            .map(|_| ())
            .expect_err("refused")
    };
    let login = AccountGrant::new("a", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", "x")]))
        .state_dir(dir.to_string_lossy());
    for refused in [
        login
            .clone()
            .credentials(credentials_of(&[("api_key", "x")])),
        login.clone().settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"colour": "red"}),
        )),
        login.clone().settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"chatgpt_base_url": "ftp://x"}),
        )),
    ] {
        let error = start(refused, spawner(&dir));
        assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    }
    let error = start(login, LocalSpawner::new().into_object());
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let _ = std::fs::remove_dir_all(&dir);
}

#[derive(Clone)]
struct Person {
    secret: String,
    shown: Arc<Mutex<Vec<String>>>,
    codes: Arc<Mutex<Vec<Option<String>>>>,
}

impl Person {
    fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            shown: Arc::default(),
            codes: Arc::default(),
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
        self.codes.lock().unwrap().push(open.user_code);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
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

/// Starts a session on `grant`, with `environment`, and answers what the stand-in saw.
fn started_with(
    provider: &CodexProvider,
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
fn no_endpoint_or_refresh_lever_of_the_host_reaches_codex_beside_an_account() {
    let dir = scratch("hostile");
    let hostile = [
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        "CODEX_APP_SERVER_LOGIN_ISSUER",
        "CODEX_REVOKE_TOKEN_URL_OVERRIDE",
        "CODEX_SQLITE_HOME",
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
    ];
    let environment: coqui_acp_api::Environment = hostile
        .iter()
        .map(|name| (*name, "host-value".to_string()))
        .collect();
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy());
    let entry = started_with(&provider(), grant, &dir, environment);
    let seen = names(&entry);
    for name in hostile {
        assert!(!seen.contains(&name.to_string()), "{name} reached Codex");
    }
    assert!(seen.contains(&"CODEX_HOME".to_string()));
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
fn a_token_is_saved_once_for_a_home_however_often_the_account_is_used() {
    let provider = provider();
    let dir = scratch("once");
    let grant = AccountGrant::new("acct", "codex-access-token")
        .credentials(credentials_of(&[("access_token", CANARY)]))
        .state_dir(dir.to_string_lossy());
    for _ in 0..3 {
        provider
            .models(
                ModelsRequest::new().account(grant.clone()),
                spawner(&dir),
                Heard::default().into_object(),
            )
            .expect("listed");
    }
    let saves = log(&dir)
        .iter()
        .filter(|e| e["args"] == serde_json::json!(["login", "--with-access-token"]))
        .count();
    assert_eq!(saves, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

const JWT_FILE: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDB9.","refresh_token":"stand-in-refresh-2","account_id":"acct-A"}}"#;

#[test]
fn a_renewal_says_when_the_access_token_ends_and_one_for_another_account_is_refused() {
    let provider = provider();
    let dir = scratch("identity");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[(
            "auth_file",
            &JWT_FILE.replace("stand-in-refresh-2", "stand-in-refresh-1"),
        )]))
        .state_dir(dir.to_string_lossy());
    let heard = Heard::default();
    provider
        .models(
            ModelsRequest::new().account(grant.clone()),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
    std::fs::write(dir.join("auth.json"), JWT_FILE).unwrap();
    provider
        .models(
            ModelsRequest::new().account(grant.clone()),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
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
    provider
        .models(
            ModelsRequest::new().account(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
    assert_eq!(heard.renewed.lock().unwrap().len(), 1);
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[test]
fn a_credential_variable_of_another_case_does_not_survive_on_windows() {
    let dir = scratch("case");
    let environment: coqui_acp_api::Environment = [("openai_api_key", "host-key".to_string())]
        .into_iter()
        .collect();
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy());
    let entry = started_with(&provider(), grant, &dir, environment);
    assert!(
        !names(&entry)
            .iter()
            .any(|n| n.eq_ignore_ascii_case("OPENAI_API_KEY")),
        "{entry}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
