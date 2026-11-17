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
    ModelsRequest, ProcessSpawner, StartRequest,
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

#[test]
fn a_connection_and_a_models_call_tell_their_keeper_nothing() {
    let provider = provider();
    let dir = scratch("silent");
    let grant = AccountGrant::new("acct", "codex-chatgpt")
        .credentials(credentials_of(&[("auth_file", LOGIN_FILE)]))
        .state_dir(dir.to_string_lossy());
    // A connection whose login the agent renews, and a models call after a rotation, tell the keeper nothing: the
    // account provider's watch does.
    let heard = Heard::default();
    run_session(&provider, grant.clone(), &dir, &heard);
    for refresh in ["refresh-8", "refresh-9"] {
        std::fs::write(
            dir.join("auth.json"),
            LOGIN_FILE.replace("refresh-1", refresh),
        )
        .unwrap();
        provider
            .models(
                ModelsRequest::new().account(grant.clone()),
                spawner(&dir),
                heard.clone().into_object(),
            )
            .expect("listed");
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(heard.renewed.lock().unwrap().is_empty() && heard.lost.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
