//! The Claude Code provider, held to the `acp_harness` kind against a stand-in for Claude Code.
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
use coqui_acp_api::guatiao::{Map, Status, Value};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, CacheLifetime, Channel, ChannelSink,
    NO_PROJECT_MCP, ProcessSpawner, READ_ONLY, Requirement, StartRequest,
};
use coqui_acp_claude::{ClaudeProvider, ProviderSettings};
use coqui_acp_harness::LocalSpawner;

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
    let dir = std::env::temp_dir().join(format!("coqui-acp-claude-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> ClaudeProvider {
    let mut settings = ProviderSettings::default();
    settings.executable = fake();
    ClaudeProvider::new(settings)
}

/// The suite, run in a process of its own whose environment names the stand-in's configuration directory, so that no
/// test sets a variable of this one.
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
        .env("CLAUDE_CONFIG_DIR", &dir)
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
    let mut registry = Registry::new("coqui-acp-claude-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    let harness = registry
        .offer::<dyn AcpHarness>("claude")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = Contract::new("claude")
        .program_agent(true)
        .imports(false)
        .logins(false)
        .holds(vec![Requirement::new(NO_PROJECT_MCP)])
        .account("harness-login")
        .run(&harness);
    drop(harness);
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

fn answer_to(received: &mpsc::Receiver<serde_json::Value>, id: u64) -> serde_json::Value {
    loop {
        let message = received
            .recv_timeout(Duration::from_secs(30))
            .expect("an answer");
        if message["id"] == id && message.get("method").is_none() {
            return message;
        }
    }
}

/// Starts a connection and opens one session in `cwd`.
fn connect(
    provider: &ClaudeProvider,
    request: StartRequest,
    cwd: &Path,
) -> (Object<dyn Channel>, mpsc::Receiver<serde_json::Value>) {
    let (sender, received) = mpsc::channel();
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().into_object();
    let channel = provider
        .start(
            request,
            spawner,
            Sink(sender).into_object(),
            Keeper.into_object(),
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
        "params": {"cwd": cwd.to_string_lossy(), "mcpServers": []}}),
    );
    let opened = answer_to(&received, 2);
    assert!(opened["result"]["sessionId"].is_string(), "{opened}");
    (channel, received)
}

fn logged(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a log line"))
        .collect()
}

#[test]
fn a_cache_lifetime_sets_claude_codes_two_variables_and_default_leaves_them() {
    for (lifetime, expected) in [
        (CacheLifetime::Short, Some("5m")),
        (CacheLifetime::Long, Some("1h")),
        (CacheLifetime::Default, None),
    ] {
        let dir = scratch("cache");
        let log = dir.join("log.jsonl");
        let request = StartRequest::builder()
            .environment(
                [
                    ("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()),
                    (
                        "COQUI_ACP_FAKE_CLAUDE_LOG",
                        log.to_string_lossy().into_owned(),
                    ),
                ]
                .into_iter()
                .collect::<coqui_acp_api::Environment>(),
            )
            .cache_lifetime(lifetime)
            .build();
        let (channel, _received) = connect(&provider(), request, &dir);
        channel.close(0, "");
        let started = logged(&log);
        let ttl = &started.last().expect("it was started")["ttl"];
        for name in [
            "CLAUDE_CODE_PROMPT_CACHE_TTL",
            "CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL",
        ] {
            assert_eq!(ttl[name].as_str(), expected, "{name} for {lifetime:?}");
        }
        drop(channel);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn it_describes_what_it_does_with_the_cache_hint_and_the_usage_it_reports() {
    let info = provider().describe().expect("described");
    assert_eq!(info.versions, vec![1, 2]);
    assert!(info.usage.reports() && info.usage.needs_turn);
    let text = info.cache_lifetime.expect("it says what it does");
    assert!(
        text.contains("CLAUDE_CODE_PROMPT_CACHE_TTL") && text.contains("5m") && text.contains("1h")
    );
    assert!(info.takes.resume && info.takes.load && info.takes.images);
}

#[test]
fn it_holds_what_it_can_and_refuses_the_rest_with_the_reason() {
    let provider = provider();
    let refusal = |names: &[&str]| {
        provider.refusal(
            names.iter().map(|n| Requirement::new(*n)).collect(),
            coqui_acp_api::guatiao::Secret::default(),
            None,
        )
    };
    assert_eq!(refusal(&[NO_PROJECT_MCP]).expect("answered"), None);
    let why = refusal(&[NO_PROJECT_MCP, READ_ONLY]).expect("answered");
    assert!(why.is_some_and(|why| why.contains("read_only")));
    assert_eq!(
        refusal(&["no_such_requirement"])
            .expect("answered")
            .is_some(),
        true
    );
}

#[test]
fn what_it_does_not_do_yet_is_refused_unsupported_with_a_reason() {
    let provider = provider();
    let spawner = || -> Object<dyn ProcessSpawner> { LocalSpawner::new().into_object() };
    let keeper = || -> Object<dyn AccountKeeper> { Keeper.into_object() };
    let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let error = provider
        .import_session(
            coqui_acp_api::SessionImport::new(id, "/", String::new()),
            spawner(),
            keeper(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(!error.message().is_empty());
    let error = provider
        .export_session(
            coqui_acp_api::SessionExport::new(id, "/"),
            spawner(),
            keeper(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
}

#[test]
fn it_lists_the_account_kinds_it_measured_and_applies_only_the_harness_login() {
    let catalog = provider().accounts().expect("listed");
    let ids: Vec<&str> = catalog.kinds.iter().map(|k| k.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "harness-login",
            "claude-login",
            "claude-console",
            "claude-oauth-token",
            "anthropic-api-key",
            "anthropic-auth-token"
        ]
    );
    assert!(catalog.logins.is_empty());
    let login = catalog.kind("claude-login").expect("listed");
    assert!(login.rotates && login.state_dir && !login.history_portable && !login.renews);
    let key = catalog.kind("anthropic-api-key").expect("listed");
    assert!(!key.rotates && !key.state_dir && key.history_portable);
    let grant = coqui_acp_api::AccountGrant::new("a", "claude-login").reference();
    let refused = provider()
        .refusal(
            Vec::new(),
            coqui_acp_api::guatiao::Secret::default(),
            Some(grant),
        )
        .expect("answered");
    assert!(refused.is_some_and(|why| why.contains("claude-login")));
}
