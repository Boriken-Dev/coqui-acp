//! The Claude Code provider, held to the `acp_harness` kind against a stand-in for Claude Code.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{FromValue, Map, Status, ToValue, Value};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, CacheLifetime, Channel, ChannelSink,
    NO_PROJECT_MCP, ProcessSpawner, READ_ONLY, Requirement, StartRequest,
};
use coqui_acp_claude::{ClaudeProvider, ProviderSettings, SettingSource};
use coqui_acp_harness::{LocalSpawner, Mode};
use guatiao_serde::Presentation;
use guatiao_serde::text::json;

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
    ClaudeProvider::new(ProviderSettings {
        executable: fake(),
        ..ProviderSettings::default()
    })
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
        // An empty history cannot be resumed, which the suite asks of an importer, so it checks the refusal;
        // `tests/history.rs` holds the import and export of real histories.
        .imports(false)
        .holds(vec![Requirement::new(NO_PROJECT_MCP)])
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
    let (channel, received, _opened) = connect_opened(provider, request, cwd);
    (channel, received)
}

/// [`connect`], with the answer to `session/new` as well.
fn connect_opened(
    provider: &ClaudeProvider,
    request: StartRequest,
    cwd: &Path,
) -> (
    Object<dyn Channel>,
    mpsc::Receiver<serde_json::Value>,
    serde_json::Value,
) {
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
    (channel, received, opened)
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
    assert!(
        refusal(&["no_such_requirement"])
            .expect("answered")
            .is_some()
    );
}

#[test]
#[cfg(not(feature = "history"))]
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
fn a_usage_read_for_an_account_checks_the_grant_like_a_start_does() {
    let spawner = || -> Object<dyn ProcessSpawner> { LocalSpawner::new().into_object() };
    // A login with no credential or directory is not a grant the kind takes.
    let account = coqui_acp_api::AccountGrant::new("a", "claude-login");
    let error = provider()
        .usage(
            coqui_acp_api::UsageQuery::new().account(account),
            spawner(),
            Keeper.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let error = provider()
        .usage(
            coqui_acp_api::UsageQuery::new()
                .account(coqui_acp_api::AccountGrant::new("a", "no-such-kind")),
            spawner(),
            Keeper.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
}

#[test]
fn it_lists_the_account_kinds_it_measured_and_the_ways_to_sign_in() {
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
    let methods: Vec<&str> = catalog.logins.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        methods,
        ["subscription", "console", "api-key", "auth-token"]
    );
    let login = catalog.kind("claude-login").expect("listed");
    assert!(login.rotates && login.state_dir && !login.history_portable && login.renews);
    let key = catalog.kind("anthropic-api-key").expect("listed");
    assert!(!key.rotates && !key.state_dir && key.history_portable);
    let grant = coqui_acp_api::AccountRef::new("a", "no-such-kind");
    let error = provider()
        .refusal(
            Vec::new(),
            coqui_acp_api::guatiao::Secret::default(),
            Some(grant),
        )
        .expect_err("a kind it does not list");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
}

fn settings_from(text: &str) -> Result<ProviderSettings, guatiao_serde::Error> {
    json::from_str_as::<ProviderSettings>(text, Presentation::new())
}

/// The environment that points the stand-in at `dir` and its log at `log`.
fn stand_in_environment(dir: &Path, log: &Path) -> coqui_acp_api::Environment {
    [
        ("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()),
        (
            "COQUI_ACP_FAKE_CLAUDE_LOG",
            log.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .collect()
}

/// The ids of the `mode` option a session was opened with.
fn offered_modes(opened: &serde_json::Value) -> Vec<String> {
    opened["result"]["configOptions"]
        .as_array()
        .and_then(|options| options.iter().find(|option| option["id"] == "mode"))
        .and_then(|option| option["options"].as_array())
        .expect("a session shows its modes")
        .iter()
        .map(|value| value["value"].as_str().expect("an id").to_string())
        .collect()
}

/// The arguments of the stand-in's last start.
fn last_arguments(log: &Path) -> Vec<String> {
    logged(log).last().expect("it was started")["args"]
        .as_array()
        .expect("arguments")
        .iter()
        .map(|arg| arg.as_str().expect("text").to_string())
        .collect()
}

#[test]
fn modes_and_setting_sources_are_written_as_snake_case_lists_and_round_trip() {
    let settings = settings_from(
        r#"{"modes": ["plan", "accept_edits", "bypass"], "setting_sources": ["user", "local"]}"#,
    )
    .expect("read");
    assert_eq!(
        settings.modes,
        Some(vec![Mode::Plan, Mode::AcceptEdits, Mode::Bypass])
    );
    assert_eq!(
        settings.setting_sources,
        Some(vec![SettingSource::User, SettingSource::Local])
    );
    let written = settings.to_value().expect("written");
    let again = ProviderSettings::from_value(&written).expect("read again");
    assert_eq!(again.to_value().expect("written"), written);
    assert_eq!(again.modes, settings.modes);
    assert_eq!(again.setting_sources, settings.setting_sources);

    let none = settings_from("{}").expect("read");
    assert!(none.modes.is_none() && none.setting_sources.is_none());
    let empty = settings_from(r#"{"setting_sources": []}"#).expect("read");
    assert_eq!(empty.setting_sources, Some(Vec::new()));
}

#[test]
fn a_mode_or_a_setting_source_it_does_not_know_is_refused() {
    for text in [
        r#"{"setting_sources": ["user", "global"]}"#,
        r#"{"setting_sources": ["User"]}"#,
        r#"{"modes": ["yolo"]}"#,
        r#"{"modes": ["acceptEdits"]}"#,
        r#"{"modes": "plan"}"#,
    ] {
        assert!(settings_from(text).is_err(), "{text} was taken");
    }
}

#[test]
fn what_the_settings_name_reaches_the_command_line_and_the_modes_a_client_may_choose() {
    for (allow_bypass, modes) in [
        (false, vec!["manual", "accept_edits", "auto", "plan"]),
        (
            true,
            vec!["manual", "accept_edits", "auto", "plan", "bypass"],
        ),
    ] {
        let dir = scratch("settings");
        let log = dir.join("log.jsonl");
        let provider = ClaudeProvider::new(ProviderSettings {
            executable: fake(),
            allow_bypass,
            setting_sources: Some(vec![SettingSource::User, SettingSource::Project]),
            modes: Some(vec![Mode::AcceptEdits, Mode::Auto, Mode::Bypass]),
            ..ProviderSettings::default()
        });
        let request = StartRequest::builder()
            .environment(stand_in_environment(&dir, &log))
            .build();
        let (channel, _received, opened) = connect_opened(&provider, request, &dir);
        channel.close(0, "");
        assert!(
            last_arguments(&log)
                .iter()
                .any(|arg| arg == "--setting-sources=user,project")
        );
        // Bypass is the allow_bypass switch's alone, whatever `modes` names.
        assert_eq!(offered_modes(&opened), modes, "allow_bypass {allow_bypass}");
        drop(channel);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_empty_list_of_setting_sources_loads_none_and_absent_leaves_the_flag_off() {
    for (sources, flag) in [(Some(Vec::new()), Some("--setting-sources=")), (None, None)] {
        let dir = scratch("sources");
        let log = dir.join("log.jsonl");
        let provider = ClaudeProvider::new(ProviderSettings {
            executable: fake(),
            setting_sources: sources,
            ..ProviderSettings::default()
        });
        let request = StartRequest::builder()
            .environment(stand_in_environment(&dir, &log))
            .build();
        let (channel, _received) = connect(&provider, request, &dir);
        channel.close(0, "");
        let args = last_arguments(&log);
        let found = args
            .iter()
            .find(|arg| arg.starts_with("--setting-sources"))
            .map(String::as_str);
        assert_eq!(found, flag, "{args:?}");
        drop(channel);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// What the recorded `initialize` of Claude Code 2.1.318 gives a host that asks for the models before any session: no
/// turn is made, since the stand-in replays only the recording's control answers.
#[test]
fn models_lists_the_recorded_models_with_versions_and_efforts_and_no_turn() {
    let environment: coqui_acp_api::Environment = [(
        "COQUI_ACP_CLAUDE_REPLAY",
        support::fixtures()
            .join("cli_session")
            .to_string_lossy()
            .into_owned(),
    )]
    .into_iter()
    .collect();
    let provider = ClaudeProvider::new(ProviderSettings {
        executable: support::replay_program().to_string_lossy().into_owned(),
        ..ProviderSettings::default()
    });
    let listing = provider
        .models(
            coqui_acp_api::ModelsRequest::new().environment(environment),
            LocalSpawner::new().into_object(),
            Keeper.into_object(),
        )
        .expect("answered");
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
