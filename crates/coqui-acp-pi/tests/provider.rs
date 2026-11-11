//! The Pi provider, held to the `acp_harness` kind against a stand-in for `pi --mode rpc`.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Secret, Status, Value};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ChannelSink, LoginPrompter,
    ModelsRequest, ProcessSpawner, READ_ONLY, Requirement, SessionExport, SessionImport, SpawnSpec,
    SpawnedProcess, SpawnerInfo, StartRequest, UsageQuery,
};
use coqui_acp_harness::{LocalSpawner, Mode};
use coqui_acp_pi::{PiProvider, ProviderSettings};
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coqui-acp-pi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> PiProvider {
    PiProvider::new(ProviderSettings {
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

struct Sink(mpsc::Sender<serde_json::Value>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
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
        .env_remove("PI_CODING_AGENT_DIR")
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
    let mut registry = Registry::new("coqui-acp-pi-tests", "1.0");
    registry
        .load_file(&library())
        .expect("the library loads once cargo built it");
    let mut settings = Map::new();
    settings.set("executable", Value::from(fake().as_str()));
    let harness = registry
        .offer::<dyn AcpHarness>("pi")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = Contract::new("pi")
        .program_agent(true)
        .imports(false)
        .logins(false)
        .holds(Vec::new())
        .run(&harness);
    drop(harness);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}

#[test]
fn it_says_what_pi_takes_and_that_it_reports_no_usage() {
    let info = provider().describe().expect("described");
    assert_eq!(info.title, "Pi");
    assert_eq!(info.versions, vec![1, 2]);
    assert!(!info.usage.reports(), "Pi reports no allowances");
    assert!(
        info.cache_lifetime
            .is_some_and(|text| text.starts_with("ignored"))
    );
    assert!(info.takes.resume && info.takes.load && info.takes.images);
    assert!(
        !info.takes.mcp_http && !info.takes.mcp_sse,
        "Pi reads its MCP servers from its own configuration"
    );
    assert!(info.resumes_wider);
    let one = ProviderSettings {
        max_acp_version: 1,
        ..ProviderSettings::default()
    };
    assert_eq!(PiProvider::new(one).describe().unwrap().versions, vec![1]);
}

#[test]
fn nothing_is_held_and_each_requirement_is_refused_by_name() {
    let provider = provider();
    let refusal = |names: &[&str]| {
        provider
            .refusal(
                names.iter().map(|n| Requirement::new(*n)).collect(),
                Secret::default(),
                None,
            )
            .expect("answered")
    };
    assert!(refusal(&[READ_ONLY]).is_some_and(|why| why.contains("read_only")));
    assert!(refusal(&["no_commands"]).is_some_and(|why| why.contains("no_commands")));
    assert_eq!(refusal(&[]), None);
    let (sender, _received) = mpsc::channel();
    let started = provider.start(
        StartRequest::builder()
            .requirement(Requirement::new(READ_ONLY))
            .build(),
        spawner(),
        Sink(sender).into_object(),
        Keeper.into_object(),
    );
    assert!(matches!(started, Err(error) if error.status == Status::GUATIAO_ERR_UNSUPPORTED));
}

/// A spawner that says its programs run elsewhere.
struct Remote(Object<dyn ProcessSpawner>);

impl ProcessSpawner for Remote {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        let mut info = self.0.describe()?;
        info.local = false;
        Ok(info)
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        self.0.spawn(spec)
    }

    fn write_file(
        &self,
        path: &str,
        bytes: Secret<coqui_acp_api::SecretBytes>,
        private: bool,
    ) -> Result<(), ProviderError> {
        self.0.write_file(path, bytes, private)
    }

    fn read_file(
        &self,
        path: &str,
        max_bytes: u64,
    ) -> Result<Secret<coqui_acp_api::SecretBytes>, ProviderError> {
        self.0.read_file(path, max_bytes)
    }

    fn file_info(&self, path: &str) -> Result<Option<coqui_acp_api::FileInfo>, ProviderError> {
        self.0.file_info(path)
    }

    fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        self.0.remove_file(path)
    }
}

#[test]
fn a_spawner_that_runs_programs_elsewhere_is_refused_because_the_gate_is_a_local_file() {
    let (sender, _received) = mpsc::channel();
    let Err(error) = provider().start(
        StartRequest::new(),
        Remote(spawner()).into_object(),
        Sink(sender).into_object(),
        Keeper.into_object(),
    ) else {
        panic!("a remote spawner was taken");
    };
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(
        error.message().contains("permission gate"),
        "{}",
        error.message()
    );
}

#[test]
fn it_lists_the_account_kinds_it_moved_from_the_stdio_provider_and_no_login() {
    let catalog = provider().accounts().expect("listed");
    let ids: Vec<&str> = catalog.kinds.iter().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, ["harness-login", "pi-api-key", "pi-oauth"]);
    assert!(
        catalog.logins.is_empty(),
        "Pi signs in only inside its terminal"
    );
    let key = catalog.kind("pi-api-key").expect("listed");
    assert!(key.state_dir && !key.rotates && !key.renews);
    let oauth = catalog.kind("pi-oauth").expect("listed");
    assert!(oauth.state_dir && oauth.rotates && !oauth.renews);
    assert!(
        catalog
            .credential_variables
            .iter()
            .any(|name| name == "PI_CODING_AGENT_DIR")
    );
    assert!(
        catalog
            .credential_variables
            .iter()
            .any(|name| name == "ANTHROPIC_API_KEY")
    );
}

#[test]
fn a_sign_in_a_renewal_an_import_and_an_export_answer_unsupported_with_a_reason() {
    let provider = provider();
    let prompter = Prompter.into_object();
    let error = provider
        .login(
            coqui_acp_api::LoginRequest::new("anything", ""),
            spawner(),
            prompter,
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(error.message().contains("terminal"), "{}", error.message());
    let imported = provider.import_session(
        SessionImport::new("3a3a3a3a-3a3a-4a3a-8a3a-3a3a3a3a3a3a", "/w", ""),
        spawner(),
        Keeper.into_object(),
    );
    assert!(matches!(imported, Err(error) if error.status == Status::GUATIAO_ERR_UNSUPPORTED));
    let exported = provider.export_session(
        SessionExport::new("3a3a3a3a-3a3a-4a3a-8a3a-3a3a3a3a3a3a", "/w"),
        spawner(),
        Keeper.into_object(),
    );
    assert!(matches!(exported, Err(error) if error.status == Status::GUATIAO_ERR_UNSUPPORTED));
}

#[test]
fn models_come_from_pi_with_no_turn_and_usage_is_unsupported() {
    let dir = scratch("models-home");
    let environment: coqui_acp_api::Environment =
        [("PI_CODING_AGENT_DIR", dir.to_string_lossy().into_owned())]
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
    let ids: Vec<&str> = listing
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(ids, ["stub/stub-fast", "stub/stub-think"]);
    assert_eq!(listing.models[1].name.as_deref(), Some("Stub Think"));
    assert!(
        listing.models[1]
            .description
            .as_deref()
            .is_some_and(|text| text.contains("reasoning"))
    );
    let report = provider()
        .usage(
            UsageQuery::new().environment(environment),
            spawner(),
            Keeper.into_object(),
        )
        .expect("answered");
    assert!(!report.supported, "{report:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn settings_from(text: &str) -> Result<ProviderSettings, guatiao_serde::Error> {
    json::from_str_as::<ProviderSettings>(text, Presentation::new())
}

#[test]
fn the_settings_are_read_by_id_and_a_mode_that_is_none_is_refused() {
    let settings = settings_from(r#"{"executable": "p", "mode": "accept_edits", "modes": ["plan", "bypass"], "allow_bypass": true}"#)
        .expect("read");
    assert_eq!(settings.mode, Some(Mode::AcceptEdits));
    assert_eq!(settings.modes, Some(vec![Mode::Plan, Mode::Bypass]));
    assert!(settings_from(r#"{"mode": "yolo"}"#).is_err());
    assert_eq!(settings_from("{}").unwrap().executable, "pi");
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
fn the_default_model_carries_its_thinking_levels_and_the_others_none() {
    let dir = scratch("models-efforts");
    let environment: coqui_acp_api::Environment =
        [("PI_CODING_AGENT_DIR", dir.to_string_lossy().into_owned())]
            .into_iter()
            .collect();
    let listing = provider()
        .models(
            ModelsRequest::new().environment(environment),
            spawner(),
            Keeper.into_object(),
        )
        .expect("answered");
    let fast = &listing.models[0];
    let think = &listing.models[1];
    assert!(
        !fast.is_default && fast.efforts.is_empty() && fast.default_effort.is_none(),
        "{fast:?}"
    );
    assert!(think.is_default, "{think:?}");
    assert_eq!(think.efforts, ["off", "minimal", "low", "medium", "high"]);
    assert_eq!(think.default_effort.as_deref(), Some("medium"));
    let _ = std::fs::remove_dir_all(&dir);
}
