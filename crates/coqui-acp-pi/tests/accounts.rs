//! The accounts Pi takes (moved from the stdio provider), held to the kind against the stand-in: each kind through the
//! contract suite, and what the stand-in saw of the arguments and the environment.
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
    Environment, ProcessSpawner, RenewRequest, StartRequest,
};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_pi::{PiProvider, ProviderSettings};

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
        .join(format!("fake_pi{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn library() -> PathBuf {
    profile().join("examples").join(format!(
        "{}pi_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

/// Removes `dir` once the stand-in that ran in it has ended: Windows keeps a directory a process still uses.
fn remove(dir: &Path) {
    for _ in 0..100 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coqui-acp-pi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Runs the suite for one kind in a process of its own, whose environment names the stand-in's home and log, so that no test
/// sets a variable of this one. Answers what the stand-in logged.
fn run_suite(kind: &str) -> String {
    let dir = scratch(&format!("suite-{kind}"));
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
        .env("COQUI_ACP_FAKE_PI_LOG", &log)
        .env("PI_CASE_KIND", kind)
        .env_remove("PI_CODING_AGENT_DIR")
        .output()
        .expect("the test runs itself");
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    remove(&dir);
    assert!(
        out.status.success(),
        "{kind}:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !logged.contains(CANARY),
        "{kind}: the stand-in's log holds a credential"
    );
    assert!(
        logged.lines().count() > 0,
        "{kind}: the stand-in was never started"
    );
    logged
}

#[test]
fn pi_keeps_every_rule_of_the_kind_for_each_kind() {
    for kind in ["harness-login", "pi-api-key", "pi-oauth"] {
        run_suite(kind);
    }
}

#[test]
#[ignore = "run by the tests that set its environment"]
fn the_suite_inside() {
    let kind = std::env::var("PI_CASE_KIND").expect("a kind");
    let mut registry = Registry::new("coqui-acp-pi-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    let provider = registry
        .offer::<dyn AcpHarness>("pi")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = Contract::new(format!("pi {kind}"))
        .program_agent(true)
        .imports(false)
        .logins(false)
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

fn provider() -> PiProvider {
    PiProvider::new(ProviderSettings {
        executable: fake(),
        ..ProviderSettings::default()
    })
}

/// The directories of the permission gates now in the temporary directory.
fn gates() -> std::collections::BTreeSet<std::ffi::OsString> {
    std::fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| {
            name.to_string_lossy()
                .strip_prefix("coqui-acp-pi-")
                .is_some_and(|rest| rest.len() == 36 && rest.matches('-').count() == 4)
        })
        .collect()
}

/// Starts a connection, opens a session on it and closes it, so that the stand-in has logged its start. Returns once the
/// session has ended and removed its gate's directory.
fn start_and_close(provider: &PiProvider, request: StartRequest, dir: &Path) {
    let before = gates();
    start_and_close_inner(provider, request, dir);
    for _ in 0..200 {
        if gates().is_subset(&before) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start_and_close_inner(provider: &PiProvider, request: StartRequest, dir: &Path) {
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
    // The session works beside the directory holding Pi's credentials, not over it.
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let ask = |id: u64, method: &str, params: serde_json::Value| {
        let message =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        channel.send(message.to_string().as_bytes()).expect("taken");
        loop {
            let answer = received
                .recv_timeout(Duration::from_secs(30))
                .expect("an answer");
            if answer["id"] == id && answer.get("method").is_none() {
                return answer;
            }
        }
    };
    ask(1, "initialize", serde_json::json!({"protocolVersion": 1}));
    let opened = ask(
        2,
        "session/new",
        serde_json::json!({"cwd": work.to_string_lossy(), "mcpServers": []}),
    );
    assert!(opened["error"].is_null(), "{opened}");
    channel.close(0, "");
    drop(channel);
}

fn logged(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("the stand-in logged")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON value a line"))
        .filter(|entry: &serde_json::Value| entry.get("args").is_some())
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

fn has(names: &[String], name: &str) -> bool {
    names.iter().any(|n| {
        if cfg!(windows) {
            n.eq_ignore_ascii_case(name)
        } else {
            n == name
        }
    })
}

/// A Pi key goes in the variable its setting names, and in no other; Pi runs in the account's own directory.
#[test]
fn a_pi_key_goes_in_the_variable_named_by_its_setting() {
    let dir = scratch("key");
    let log = dir.join("fake.log");
    let key = "pi-key-value-77c1";
    let environment: Environment = [
        ("OPENAI_API_KEY", "the-hosts-own-key"),
        ("GROQ_API_KEY", "the-hosts-other-key"),
        ("COQUI_ACP_FAKE_PI_LOG", log.to_str().expect("text")),
    ]
    .into_iter()
    .collect();
    let mut settings = Map::new();
    settings.set("variable", Value::from("GROQ_API_KEY"));
    let grant = AccountGrant::new("a-pi-key", "pi-api-key")
        .settings(Value::from(settings))
        .credentials(Credentials::from_iter([("api_key", key)]))
        .state_dir(dir.to_str().expect("text"));
    let request = StartRequest::builder()
        .environment(environment)
        .account(grant)
        .build();
    start_and_close(&provider(), request, &dir);
    let entries = logged(&log);
    let entry = entries.last().expect("a start");
    let names = names_of(entry);
    assert!(has(&names, "GROQ_API_KEY"), "{names:?}");
    assert!(!has(&names, "OPENAI_API_KEY"), "{names:?}");
    assert!(has(&names, "PI_CODING_AGENT_DIR"), "{names:?}");
    assert!(
        !entry.to_string().contains(key),
        "the key is in no argument"
    );
    let args: Vec<&str> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a.as_str())
        .collect();
    assert_eq!(&args[..2], ["--mode", "rpc"]);
    remove(&dir);
}

/// A variable the Pi key setting does not list is refused, so a setting cannot name `PATH`.
#[test]
fn a_pi_key_names_only_a_provider_variable() {
    let dir = scratch("path");
    let mut settings = Map::new();
    settings.set("variable", Value::from("PATH"));
    let grant = AccountGrant::new("a-pi-key", "pi-api-key")
        .settings(Value::from(settings))
        .credentials(Credentials::from_iter([("api_key", "pi-key-value-77c1")]))
        .state_dir(dir.to_str().expect("text"));
    let (sender, _received) = mpsc::channel();
    let refused = provider().start(
        StartRequest::builder().account(grant).build(),
        LocalSpawner::new().files(&dir).into_object(),
        Sink(sender).into_object(),
        Keeper.into_object(),
    );
    let Err(error) = refused else {
        panic!("a variable that is no provider's was taken");
    };
    assert_eq!(u32::from(error.status), 7, "{}", error.message());
    remove(&dir);
}

/// A sign-in kept in `auth.json` is placed in Pi's directory, and the host's own provider keys do not reach Pi beside it.
#[test]
fn a_sign_in_is_placed_in_pis_directory_and_the_hosts_keys_are_cleared() {
    let dir = scratch("oauth");
    let log = dir.join("fake.log");
    let environment: Environment = [
        ("ANTHROPIC_API_KEY", "the-hosts-own-key"),
        ("AWS_PROFILE", "the-hosts-profile"),
        ("COQUI_ACP_FAKE_PI_LOG", log.to_str().expect("text")),
    ]
    .into_iter()
    .collect();
    let grant = AccountGrant::new("a-sign-in", "pi-oauth")
        .credentials(Credentials::from_iter([(
            "auth_json",
            "{\"anthropic\": {\"type\": \"oauth\"}}",
        )]))
        .state_dir(dir.to_str().expect("text"));
    let request = StartRequest::builder()
        .environment(environment)
        .account(grant)
        .build();
    start_and_close(&provider(), request, &dir);
    let entries = logged(&log);
    let names = names_of(entries.last().expect("a start"));
    assert!(
        !has(&names, "ANTHROPIC_API_KEY") && !has(&names, "AWS_PROFILE"),
        "{names:?}"
    );
    assert!(has(&names, "PI_CODING_AGENT_DIR"), "{names:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("auth.json")).expect("placed"),
        "{\"anthropic\": {\"type\": \"oauth\"}}"
    );
    remove(&dir);
}

/// Pi refreshes its own sign-in, so there is nothing for the provider to renew.
#[test]
fn a_renewal_is_unsupported_for_both_kinds_and_checks_the_grant_first() {
    let dir = scratch("renew");
    let grant = AccountGrant::new("a-pi-key", "pi-api-key")
        .credentials(Credentials::from_iter([("api_key", "k")]))
        .state_dir(dir.to_str().expect("text"));
    let mut settings = Map::new();
    settings.set("variable", Value::from("GROQ_API_KEY"));
    let grant = grant.settings(Value::from(settings));
    let error = provider()
        .renew(
            RenewRequest::new(grant),
            LocalSpawner::new().files(&dir).into_object(),
        )
        .expect_err("unsupported");
    assert_eq!(
        error.status,
        coqui_acp_api::guatiao::Status::GUATIAO_ERR_UNSUPPORTED
    );
    assert!(
        error.message().contains("not renewed"),
        "{}",
        error.message()
    );
    // A grant the kind refuses is refused before the answer.
    let bare = AccountGrant::new("a", "pi-oauth").state_dir(dir.to_str().expect("text"));
    let error = provider()
        .renew(
            RenewRequest::new(bare),
            LocalSpawner::new().files(&dir).into_object(),
        )
        .expect_err("refused");
    assert_eq!(
        error.status,
        coqui_acp_api::guatiao::Status::GUATIAO_ERR_BAD_VALUE
    );
    remove(&dir);
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
        .env("ANTHROPIC_BASE_URL", "https://evil.example/anthropic")
        .env("COQUI_ACP_FAKE_PI_LOG", dir.join("fake.log"))
        .env("PI_INHERITED_DIR", &dir)
        .output()
        .expect("the test runs itself");
    remove(&dir);
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
    let dir = PathBuf::from(std::env::var("PI_INHERITED_DIR").expect("a directory"));
    let log = dir.join("fake.log");
    let mut settings = Map::new();
    settings.set("variable", Value::from("GROQ_API_KEY"));
    let grant = AccountGrant::new("a-pi-key", "pi-api-key")
        .settings(Value::from(settings))
        .credentials(Credentials::from_iter([("api_key", "pi-key-value-77c1")]))
        .state_dir(dir.to_str().expect("text"));
    start_and_close(
        &provider(),
        StartRequest::builder().account(grant).build(),
        &dir,
    );
    let entries = logged(&log);
    assert_eq!(entries.len(), 1);
    assert!(
        !has(&names_of(&entries[0]), "ANTHROPIC_BASE_URL"),
        "{}",
        entries[0]
    );
}
