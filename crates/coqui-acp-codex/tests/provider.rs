//! The Codex provider, held to the `acp_harness` kind against a stand-in for Codex's app server.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use coqui_acp_api::LoginPrompter;
use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Status, Value};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ModelsRequest, ProcessSpawner,
    READ_ONLY, Requirement, UsageQuery,
};
use coqui_acp_codex::{CodexProvider, ProviderSettings};
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
        .env("CODEX_HOME", &dir)
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
        .logins(false)
        .holds(vec![Requirement::new(READ_ONLY)])
        .account("harness-login")
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
fn read_only_is_the_read_only_sandbox_and_nothing_else_is_held() {
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
    assert_eq!(refusal(&[READ_ONLY]), None);
    assert!(refusal(&[READ_ONLY, "no_commands"]).is_some_and(|why| why.contains("no_commands")));
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
    assert!(catalog.logins.is_empty());
    let chatgpt = catalog.kind("codex-chatgpt").expect("listed");
    assert!(chatgpt.rotates && chatgpt.state_dir && !chatgpt.history_portable);
    let error = provider()
        .login(
            coqui_acp_api::LoginRequest::new("chatgpt", ""),
            spawner(),
            Prompter.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
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
