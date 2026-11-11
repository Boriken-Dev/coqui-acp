//! The Codex provider, held to the `acp_harness` kind against a stand-in for Codex's app server.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{FromValue, Map, Status, ToValue, Value};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ModelsRequest, ProcessSpawner,
    READ_ONLY, Requirement, UsageQuery,
};
use coqui_acp_api::{ChannelSink, LoginPrompter};
use coqui_acp_codex::{CodexProvider, ProviderSettings};
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
    let dir = std::env::temp_dir().join(format!("coqui-acp-codex-{name}-{}", std::process::id()));
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

struct Keeper;

impl AccountKeeper for Keeper {
    fn renewed(&self, _renewal: AccountRenewal) -> Result<(), ProviderError> {
        Ok(())
    }

    fn lost(&self, _loss: AccountLoss) -> Result<(), ProviderError> {
        Ok(())
    }
}

fn spawner() -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().into_object()
}

/// The suite, run in a process of its own whose environment names the stand-in's home, so that no test sets a variable
/// of this one.
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
    let mut registry = Registry::new("coqui-acp-codex-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    let harness = registry
        .offer::<dyn AcpHarness>("codex")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = Contract::new("codex")
        .program_agent(true)
        .imports(false)
        .holds(Vec::new())
        .run(&harness);
    drop(harness);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}

#[test]
fn it_says_the_cache_hint_is_ignored_and_that_usage_needs_no_turn() {
    let info = provider().describe().expect("described");
    assert_eq!(info.versions, vec![1, 2]);
    assert!(info.usage.reports() && !info.usage.needs_turn);
    assert!(
        info.cache_lifetime
            .is_some_and(|text| text.starts_with("ignored"))
    );
    assert!(info.takes.resume && info.takes.load && !info.takes.mcp_sse);
}

#[test]
fn nothing_is_held_and_read_only_is_refused_with_why_it_is_not_the_sandbox_alone() {
    let provider = provider();
    let refusal = |names: &[&str]| {
        provider
            .refusal(
                names.iter().map(|n| Requirement::new(*n)).collect(),
                coqui_acp_api::guatiao::Secret::default(),
                None,
            )
            .expect("answered")
    };
    assert!(refusal(&[READ_ONLY]).is_some_and(|why| why.contains("read_only")));
    assert!(refusal(&["no_commands"]).is_some_and(|why| why.contains("no_commands")));
    assert_eq!(refusal(&[]), None);
    let request = coqui_acp_api::StartRequest::builder()
        .requirement(Requirement::new(READ_ONLY))
        .build();
    let (sender, _received) = std::sync::mpsc::channel();
    let started = provider.start(
        request,
        spawner(),
        Sink(sender).into_object(),
        Keeper.into_object(),
    );
    assert!(matches!(started, Err(error) if error.status == Status::GUATIAO_ERR_UNSUPPORTED));
}

#[test]
fn it_lists_the_account_kinds_it_measured_and_refuses_what_it_does_not_apply() {
    let catalog = provider().accounts().expect("listed");
    let ids: Vec<&str> = catalog.kinds.iter().map(|k| k.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "harness-login",
            "codex-chatgpt",
            "codex-api-key",
            "codex-access-token"
        ]
    );
    let methods: Vec<&str> = catalog.logins.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        methods,
        ["chatgpt", "chatgpt-device", "api-key", "access-token"]
    );
    let chatgpt = catalog.kind("codex-chatgpt").expect("listed");
    assert!(chatgpt.rotates && chatgpt.state_dir && !chatgpt.history_portable && chatgpt.renews);
    // A sign-in with no directory to run in is refused.
    let error = provider()
        .login(
            coqui_acp_api::LoginRequest::new("chatgpt", ""),
            spawner(),
            Prompter.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
}

struct Sink(std::sync::mpsc::Sender<Vec<u8>>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(bytes.to_vec());
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

#[test]
fn a_usage_read_for_an_account_checks_the_grant_like_a_start_does() {
    // A login with no credential or directory is not a grant the kind takes.
    let error = provider()
        .usage(
            UsageQuery::new().account(coqui_acp_api::AccountGrant::new("a", "codex-chatgpt")),
            spawner(),
            Keeper.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
}

struct Prompter;

impl LoginPrompter for Prompter {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: coqui_acp_api::OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(
        &self,
        _form: coqui_acp_api::LoginForm,
    ) -> Result<coqui_acp_api::LoginAnswer, ProviderError> {
        Err(ProviderError::new(Status::GUATIAO_ERR_GONE, "nobody"))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(true)
    }
}

#[test]
fn it_lists_models_and_reads_usage_through_the_spawner_it_is_given() {
    {
        let dir = scratch("reads-home");
        let environment: coqui_acp_api::Environment =
            [("CODEX_HOME", dir.to_string_lossy().into_owned())]
                .into_iter()
                .collect();
        let listing = provider()
            .models(
                ModelsRequest::new().environment(environment.clone()),
                spawner(),
                Keeper.into_object(),
            )
            .expect("answered");
        assert!(listing.known, "{listing:?}");
        assert_eq!(listing.models[0].id, "gpt-standin");
        let report = provider()
            .usage(
                UsageQuery::new().environment(environment),
                spawner(),
                Keeper.into_object(),
            )
            .expect("answered");
        assert!(report.supported && !report.buckets.is_empty(), "{report:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

struct Frames(std::sync::mpsc::Sender<serde_json::Value>);

impl ChannelSink for Frames {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

fn settings_from(text: &str) -> Result<ProviderSettings, guatiao_serde::Error> {
    json::from_str_as::<ProviderSettings>(text, Presentation::new())
}

/// The ids of the `mode` option of a session opened on a provider with `settings`, over a connection in `dir`.
fn modes_offered(settings: ProviderSettings, dir: &Path) -> Vec<String> {
    let (sender, received) = std::sync::mpsc::channel();
    let request = coqui_acp_api::StartRequest::builder()
        .environment(
            [("CODEX_HOME", dir.to_string_lossy().into_owned())]
                .into_iter()
                .collect::<coqui_acp_api::Environment>(),
        )
        .build();
    let channel = CodexProvider::new(settings)
        .start(
            request,
            spawner(),
            Frames(sender).into_object(),
            Keeper.into_object(),
        )
        .expect("it starts");
    let ask = |id: u64, method: &str, params: serde_json::Value| {
        let message =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        channel.send(message.to_string().as_bytes()).expect("taken");
        loop {
            let answer = received
                .recv_timeout(std::time::Duration::from_secs(30))
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
        serde_json::json!({"cwd": dir.to_string_lossy(), "mcpServers": []}),
    );
    channel.close(0, "");
    opened["result"]["configOptions"]
        .as_array()
        .and_then(|options| options.iter().find(|option| option["id"] == "mode"))
        .and_then(|option| option["options"].as_array())
        .expect("a session shows its modes")
        .iter()
        .map(|value| value["value"].as_str().expect("an id").to_string())
        .collect()
}

#[test]
fn modes_are_written_as_a_snake_case_list_and_round_trip() {
    let settings = settings_from(r#"{"modes": ["plan", "accept_edits", "bypass"]}"#).expect("read");
    assert_eq!(
        settings.modes,
        Some(vec![Mode::Plan, Mode::AcceptEdits, Mode::Bypass])
    );
    let written = settings.to_value().expect("written");
    let again = ProviderSettings::from_value(&written).expect("read again");
    assert_eq!(again.to_value().expect("written"), written);
    assert_eq!(again.modes, settings.modes);
    assert!(settings_from("{}").expect("read").modes.is_none());
    assert_eq!(
        settings_from(r#"{"modes": []}"#).expect("read").modes,
        Some(Vec::new())
    );
}

#[test]
fn a_mode_it_does_not_know_is_refused() {
    for text in [
        r#"{"modes": ["yolo"]}"#,
        r#"{"modes": ["acceptEdits"]}"#,
        r#"{"modes": "plan"}"#,
    ] {
        assert!(settings_from(text).is_err(), "{text} was taken");
    }
}

#[test]
fn the_modes_it_names_widen_what_a_client_may_choose_and_bypass_needs_its_switch() {
    for (allow_bypass, expected) in [
        (false, vec!["manual", "accept_edits", "auto", "plan"]),
        (
            true,
            vec!["manual", "accept_edits", "auto", "plan", "bypass"],
        ),
    ] {
        let dir = scratch("modes");
        let offered = modes_offered(
            ProviderSettings {
                executable: fake(),
                approval_policy: Some("on-request".into()),
                sandbox: Some("workspace-write".into()),
                allow_bypass,
                modes: Some(vec![Mode::Auto, Mode::Bypass]),
                ..ProviderSettings::default()
            },
            &dir,
        );
        assert_eq!(offered, expected, "allow_bypass {allow_bypass}");
        let narrow = modes_offered(
            ProviderSettings {
                executable: fake(),
                approval_policy: Some("on-request".into()),
                sandbox: Some("workspace-write".into()),
                allow_bypass,
                ..ProviderSettings::default()
            },
            &dir,
        );
        let mut base: Vec<&str> = vec!["manual", "accept_edits", "plan"];
        if allow_bypass {
            base.push("bypass");
        }
        assert_eq!(narrow, base, "allow_bypass {allow_bypass}, no modes");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
