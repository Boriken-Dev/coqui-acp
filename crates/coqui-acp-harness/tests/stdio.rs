//! The stdio provider, held to the `acp_harness` kind against a stand-in for Grok and Antigravity: every kind and
//! login method of each, and what the stand-in saw of the arguments and the environment.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Value};
use coqui_acp_api::{
    AccountGrant, AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ChannelSink, Credentials,
    Environment, ProcessSpawner, StartRequest,
};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_harness::stdio::{StdioHarness, StdioProvider, StdioSettings};

/// The credential the contract suite grants, which no argument list or environment may carry.
const CANARY: &str = "coqui-acp-contract-canary-6b1d";

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
        .join(format!("fake_stdio_cli{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn library() -> PathBuf {
    profile().join("examples").join(format!(
        "{}stdio_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coqui-acp-stdio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Runs the suite for one kind of one harness in a process of its own, whose environment names the stand-in's home and
/// log, so that no test sets a variable of this one. Answers what the stand-in logged.
fn run_suite(harness: &str, kind: &str, logins: bool) -> String {
    let dir = scratch(&format!("{harness}-{kind}"));
    let log = dir.join("fake.log");
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
        .env("COQUI_ACP_FAKE_STDIO_LOG", &log)
        .env("STDIO_CASE_HARNESS", harness)
        .env("STDIO_CASE_KIND", kind)
        .env("STDIO_CASE_LOGINS", if logins { "1" } else { "0" })
        .env_remove("GROK_HOME")
        .env_remove("PI_CODING_AGENT_DIR")
        .output()
        .expect("the test runs itself");
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{harness} {kind}:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !logged.contains(CANARY),
        "{harness} {kind}: the stand-in's log holds a credential"
    );
    assert!(
        logged.lines().count() > 0,
        "{harness} {kind}: the stand-in was never started"
    );
    logged
}

#[test]
fn grok_keeps_every_rule_of_the_kind_for_each_kind_and_login() {
    for kind in ["harness-login", "grok-login", "xai-api-key", "grok-oidc"] {
        run_suite("grok", kind, true);
    }
}

#[test]
fn antigravity_keeps_every_rule_of_the_kind() {
    run_suite("antigravity", "harness-login", false);
}

#[test]
#[ignore = "run by the tests that set its environment"]
fn the_suite_inside() {
    let harness = std::env::var("STDIO_CASE_HARNESS").expect("a harness");
    let kind = std::env::var("STDIO_CASE_KIND").expect("a kind");
    let logins = std::env::var("STDIO_CASE_LOGINS").as_deref() == Ok("1");
    let mut registry = Registry::new("coqui-acp-harness-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("harness", Value::from(harness.as_str()));
    settings.set("executable", Value::from(fake().as_str()));
    let provider = registry
        .offer::<dyn AcpHarness>("stdio")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = Contract::new(format!("stdio {harness} {kind}"))
        .program_agent(true)
        .imports(false)
        .logins(logins)
        .holds(Vec::new())
        .account(kind)
        .run(&provider);
    drop(provider);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}

struct Keeper;

impl AccountKeeper for Keeper {
    fn renewed(&self, _renewal: AccountRenewal) -> Result<(), ProviderError> {
        Ok(())
    }

    fn lost(&self, _loss: AccountLoss) -> Result<(), ProviderError> {
        Ok(())
    }
}

struct Sink(mpsc::Sender<serde_json::Value>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

fn provider(harness: StdioHarness) -> StdioProvider {
    let mut settings = StdioSettings::new(harness);
    settings.executable = Some(fake());
    StdioProvider::new(settings)
}

/// Starts a connection, initializes it and closes it, so that the stand-in has logged its start.
fn start_and_close(provider: &StdioProvider, request: StartRequest, dir: &Path) {
    let (sender, received) = mpsc::channel();
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(dir).into_object();
    let channel = provider
        .start(
            request,
            spawner,
            Sink(sender).into_object(),
            Keeper.into_object(),
        )
        .expect("it starts");
    channel
        .send(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}})
                .to_string()
                .as_bytes(),
        )
        .expect("taken");
    let answer = received
        .recv_timeout(Duration::from_secs(30))
        .expect("an answer");
    assert_eq!(answer["id"], 1, "{answer}");
    channel.close(0, "");
    drop(channel);
}

fn logged(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("the stand-in logged")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON value a line"))
        .collect()
}

fn names_of(entry: &serde_json::Value) -> Vec<String> {
    entry["env_names"]
        .as_array()
        .expect("names")
        .iter()
        .map(|n| n.as_str().expect("text").to_owned())
        .collect()
}

/// An account of a key kind starts the harness with the host's other credential variables gone and its own set, in the
/// directory the grant names; the key is in no argument.
#[test]
fn a_key_account_clears_the_other_credentials_and_sets_its_own() {
    let dir = scratch("clears");
    let log = dir.join("fake.log");
    let key = "grok-key-value-3f9a";
    // The names the stand-in sees are the process's own plus the spec's: the host's environment is what is passed.
    let environment: Environment = [
        ("GROK_OIDC_ISSUER", "https://issuer.example"),
        ("XAI_API_KEY", "the-hosts-own-key"),
        ("COQUI_ACP_FAKE_STDIO_LOG", log.to_str().expect("text")),
    ]
    .into_iter()
    .collect();
    let grant = AccountGrant::new("a-key", "xai-api-key")
        .credentials(Credentials::from_iter([("api_key", key)]))
        .state_dir(dir.to_str().expect("text"));
    let request = StartRequest::builder()
        .environment(environment)
        .account(grant)
        .build();
    start_and_close(&provider(StdioHarness::Grok), request, &dir);
    let entries = logged(&log);
    let entry = entries.last().expect("a start");
    let names = names_of(entry);
    assert!(!names.contains(&"GROK_OIDC_ISSUER".to_owned()), "{names:?}");
    assert!(names.contains(&"XAI_API_KEY".to_owned()), "{names:?}");
    assert_eq!(
        entry["directories"]["GROK_HOME"],
        serde_json::json!(dir.to_str().expect("text"))
    );
    assert_eq!(entry["args"], serde_json::json!(["agent", "stdio"]));
    assert!(!entry.to_string().contains(key));
    let _ = std::fs::remove_dir_all(&dir);
}

fn names_without(entry: &serde_json::Value, name: &str) -> bool {
    !names_of(entry).iter().any(|n| {
        if cfg!(windows) {
            n.eq_ignore_ascii_case(name)
        } else {
            n == name
        }
    })
}

/// Every endpoint, provider-switch and cloud variable of the harness is removed from what the host passed, and the
/// endpoint comes from the grant alone; on Windows, whatever the case the host wrote the name in.
#[test]
fn an_account_removes_the_endpoint_variables_of_the_host_and_sets_its_own() {
    let dir = scratch("endpoints");
    let log = dir.join("fake.log");
    let mut environment: Environment = [
        ("GROK_CLI_CHAT_PROXY_BASE_URL", "https://evil.example/proxy"),
        ("GROK_MODELS_BASE_URL", "https://evil.example/models"),
        ("GROK_MODELS_LIST_URL", "https://evil.example/list"),
        ("COQUI_ACP_FAKE_STDIO_LOG", log.to_str().expect("text")),
    ]
    .into_iter()
    .collect();
    if cfg!(windows) {
        environment.insert("grok_xai_api_base_url", "https://evil.example/lower");
    }
    let mut settings = Map::new();
    settings.set("proxy_base_url", Value::from("https://good.example/proxy"));
    let grant = AccountGrant::new("a-key", "xai-api-key")
        .settings(Value::from(settings))
        .credentials(Credentials::from_iter([("api_key", "grok-key-value-3f9a")]))
        .state_dir(dir.to_str().expect("text"));
    let request = StartRequest::builder()
        .environment(environment)
        .account(grant)
        .build();
    start_and_close(&provider(StdioHarness::Grok), request, &dir);
    let entries = logged(&log);
    let entry = entries.last().expect("a start");
    assert_eq!(
        entry["endpoints"]["GROK_CLI_CHAT_PROXY_BASE_URL"],
        serde_json::json!("https://good.example/proxy"),
        "{entry}"
    );
    for name in [
        "GROK_MODELS_BASE_URL",
        "GROK_MODELS_LIST_URL",
        "GROK_XAI_API_BASE_URL",
    ] {
        assert!(
            names_without(entry, name),
            "{name} reached the harness: {entry}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same for the variables the host's own process has, which only the spawner can remove.
#[test]
fn an_account_removes_the_endpoint_variables_the_host_process_has() {
    let dir = scratch("inherited");
    let out = Command::new(std::env::current_exe().expect("the test knows itself"))
        .args([
            "--exact",
            "inherited_inside",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("GROK_MODELS_BASE_URL", "https://evil.example/models")
        .env("COQUI_ACP_FAKE_STDIO_LOG", dir.join("fake.log"))
        .env("STDIO_INHERITED_DIR", &dir)
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
fn inherited_inside() {
    let dir = PathBuf::from(std::env::var("STDIO_INHERITED_DIR").expect("a directory"));
    let log = dir.join("fake.log");
    let grok = AccountGrant::new("a-key", "xai-api-key")
        .credentials(Credentials::from_iter([("api_key", "grok-key-value-3f9a")]))
        .state_dir(dir.to_str().expect("text"));
    start_and_close(
        &provider(StdioHarness::Grok),
        StartRequest::builder().account(grok).build(),
        &dir,
    );
    let entries = logged(&log);
    assert_eq!(entries.len(), 1);
    assert!(
        names_without(&entries[0], "GROK_MODELS_BASE_URL"),
        "{}",
        entries[0]
    );
}

/// A renewal that leaves the login of another account in the file is refused and undone; one that keeps the account is
/// answered.
#[test]
fn a_renewal_that_changes_whose_login_it_is_is_refused() {
    let mine = r#"{"account_id":"mine","n":1}"#;
    for (swap, refused) in [(true, true), (false, false)] {
        let dir = scratch(if swap {
            "identity-swap"
        } else {
            "identity-same"
        });
        if swap {
            std::fs::write(dir.join("swap_identity"), "").expect("a marker");
        }
        let grant = AccountGrant::new("an-account", "grok-login")
            .credentials(Credentials::from_iter([("auth_json", mine)]))
            .state_dir(dir.to_str().expect("text"));
        let answer = provider(StdioHarness::Grok).renew(
            coqui_acp_api::RenewRequest::new(grant),
            LocalSpawner::new().files(&dir).into_object(),
        );
        if refused {
            let Err(error) = answer else {
                panic!("a renewal into another account's login was answered");
            };
            assert_eq!(u32::from(error.status), 7, "{}", error.message());
            assert_eq!(
                std::fs::read_to_string(dir.join("auth.json")).expect("the file"),
                mine,
                "the grant's copy was not put back"
            );
        } else {
            let report = answer.expect("a renewal of the same account is answered");
            assert!(report.credentials.is_some(), "{report:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

struct Recording(std::sync::Arc<std::sync::Mutex<(usize, usize)>>);

impl AccountKeeper for Recording {
    fn renewed(&self, _renewal: AccountRenewal) -> Result<(), ProviderError> {
        self.0.lock().expect("lock").0 += 1;
        Ok(())
    }

    fn lost(&self, _loss: AccountLoss) -> Result<(), ProviderError> {
        self.0.lock().expect("lock").1 += 1;
        Ok(())
    }
}

/// A login file the harness is in the middle of writing (half an object) is never a renewal.
#[test]
fn a_half_written_login_is_never_reported_as_a_renewal() {
    let dir = scratch("half");
    let heard = std::sync::Arc::new(std::sync::Mutex::new((0, 0)));
    let grant = AccountGrant::new("an-account", "grok-login")
        .credentials(Credentials::from_iter([(
            "auth_json",
            r#"{"account_id":"mine"}"#,
        )]))
        .state_dir(dir.to_str().expect("text"));
    let provider = provider(StdioHarness::Grok);
    let (sender, _received) = mpsc::channel();
    let channel = provider
        .start(
            StartRequest::builder().account(grant.clone()).build(),
            LocalSpawner::new().files(&dir).into_object(),
            Sink(sender).into_object(),
            Recording(std::sync::Arc::clone(&heard)).into_object(),
        )
        .expect("it starts");
    std::fs::write(dir.join("auth.json"), r#"{"account_id":"mi"#).expect("a half file");
    provider
        .models(
            coqui_acp_api::ModelsRequest::new().account(grant),
            LocalSpawner::new().files(&dir).into_object(),
            Keeper.into_object(),
        )
        .expect("models answers");
    assert_eq!(
        heard.lock().expect("lock").0,
        0,
        "a half-written file was a renewal"
    );
    channel.close(0, "");
    drop(channel);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What an account hands the harness is held in a `Secret`, so it is wiped when it is let go.
#[test]
fn what_is_applied_is_held_as_a_secret() {
    use coqui_acp_api::guatiao::Secret;
    use coqui_acp_harness::stdio::accounts::{apply, grok};
    let grant = AccountGrant::new("a-key", "xai-api-key")
        .credentials(Credentials::from_iter([("api_key", "grok-key-value-3f9a")]))
        .state_dir("/some/dir");
    let applied = apply(&grok::DEF, &grant)
        .expect("it applies")
        .expect("a kind");
    let held: &Secret<Environment> = &applied.environment;
    assert_eq!(
        held.expose().get("XAI_API_KEY"),
        Some("grok-key-value-3f9a")
    );
    assert!(!format!("{applied:?}").contains("grok-key-value-3f9a"));
}
