//! The Claude Code provider applying accounts, against the stand-in: what a start and a models call do to its
//! environment and its configuration directory, and that no credential reaches a command line. Signing in, renewing and
//! watching are the account provider's (`account_provider.rs`).
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

#[test]
fn an_account_made_for_another_harness_starts_when_its_standard_is_accepted_and_is_refused_when_not()
 {
    let provider = provider();
    let dir = scratch("standards");
    let key = AccountGrant::new("acct", "a-gateways-kind")
        .provides(["openai-api", "anthropic-api"])
        .credentials(credentials_of(&[("api_key", CANARY)]))
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"base_url": "https://gateway.example"}),
        ));
    let environment: coqui_acp_api::Environment =
        [("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned())]
            .into_iter()
            .collect();
    let (sender, received) = std::sync::mpsc::channel();
    let channel = provider
        .start(
            StartRequest::builder()
                .account(key)
                .environment(environment)
                .build(),
            spawner(&dir),
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
    let started = log(&dir);
    let seen = names(started.last().expect("it was started"));
    assert!(seen.contains(&"ANTHROPIC_API_KEY".to_string()), "{seen:?}");
    assert!(seen.contains(&"ANTHROPIC_BASE_URL".to_string()), "{seen:?}");
    assert!(
        !std::fs::read_to_string(dir.join("fake-claude.log"))
            .unwrap()
            .contains(CANARY)
    );

    let (sender, _received) = std::sync::mpsc::channel();
    let openai = AccountGrant::new("acct", "a-gateways-kind")
        .provides(["openai-api"])
        .credentials(credentials_of(&[("api_key", CANARY)]));
    let refused = provider.start(
        StartRequest::builder().account(openai).build(),
        spawner(&dir),
        Sink(sender).into_object(),
        Heard::default().into_object(),
    );
    let Err(error) = refused else {
        panic!("an account of a standard Claude Code does not take started");
    };
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let said = error.message();
    assert!(
        said.contains("claude-code")
            && said.contains("anthropic-api")
            && said.contains("openai-api"),
        "{said}"
    );
    assert!(!said.contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_harness_signs_no_one_in_renews_nothing_and_tells_its_keeper_nothing() {
    let provider = provider();
    let catalog = provider.accounts().expect("listed");
    assert!(catalog.logins.is_empty());
    assert!(catalog.kinds.iter().all(|kind| !kind.renews));
    let dir = scratch("elsewhere");
    let error = provider
        .login(
            LoginRequest::new("subscription", dir.to_string_lossy()),
            spawner(&dir),
            NoPrompter.into_object(),
        )
        .expect_err("not the harness's");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let grant = AccountGrant::new("acct", "claude-login")
        .credentials(credentials_of(&[(
            "credentials_file",
            &login_text("a1", "r1"),
        )]))
        .state_dir(dir.to_string_lossy());
    let error = provider
        .renew(RenewRequest::new(grant.clone()), spawner(&dir))
        .expect_err("not the harness's");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    // A connection whose login the agent renews, and a models call after a rotation, tell the keeper nothing: the
    // account provider's watch does.
    let heard = Heard::default();
    run_session(&provider, grant.clone(), &dir, &heard);
    std::fs::write(dir.join(".credentials.json"), login_text("a2", "r2")).unwrap();
    provider
        .models(
            ModelsRequest::new().account(grant),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("listed");
    std::thread::sleep(Duration::from_millis(300));
    assert!(heard.renewed.lock().unwrap().is_empty() && heard.lost.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

fn login_text(access: &str, refresh: &str) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":4102444800000}}}}"#
    )
}

struct NoPrompter;

impl LoginPrompter for NoPrompter {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(&self, _form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        Err(ProviderError::new(
            Status::GUATIAO_ERR_GONE,
            "nobody to ask",
        ))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(true)
    }
}
