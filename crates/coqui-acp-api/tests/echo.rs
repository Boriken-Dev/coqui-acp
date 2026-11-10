//! The Rust echo, `examples/echo_harness.rs`, loaded from its file as a host loads any provider and
//! called as the kind, holds to the contract every `acp_harness` provider is held to.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;
use std::sync::Arc;

use coqui_acp_api::contract::Contract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Secret, Status, Value};
use coqui_acp_api::{
    AccountCatalog, AccountGrant, AccountKeeper, AccountLoss, AccountRef, AccountRenewal,
    AcpHarness, Channel, ChannelSink, Environment, ExportPage, FileInfo, HarnessInfo, ImportReport,
    LoginOutcome, LoginPrompter, LoginRequest, ModelListing, ModelsRequest, ProcessSpawner,
    RenewReport, RenewRequest, Requirement, SessionExport, SessionImport, SessionQuery, Source,
    SpawnSpec, SpawnedProcess, SpawnerInfo, StartRequest, UsageQuery, UsageReport, UsageSupport,
};

/// The echo's library file, which `cargo test` builds beside the tests.
fn library() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    let profile = tests
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary is in target/<profile>/deps");
    profile.join("examples").join(format!(
        "{}echo_harness{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

fn registry() -> Registry {
    let mut registry = Registry::new("coqui-acp-api-tests", "1.0");
    registry.load_file(&library()).unwrap_or_else(|error| {
        panic!("the library loads, once `cargo test` or `cargo build --examples` built it: {error}")
    });
    registry
}

/// The registry stays alive with the test: what it offered points into the loaded file.
fn loaded() -> (Registry, Arc<dyn AcpHarness>) {
    let registry = registry();
    let harness = registry
        .offer::<dyn AcpHarness>("echo_harness")
        .expect("the library offers it")
        .expect("its table is this crate's kind")
        .shared()
        .expect("the provider has a default instance");
    (registry, harness)
}

#[test]
fn the_rust_echo_holds_to_the_contract() {
    let (_registry, harness) = loaded();
    if let Err(failures) = Contract::new("the Rust echo")
        .message_limit(1 << 20)
        .run(&*harness)
    {
        panic!("{failures}");
    }
}

#[test]
fn what_a_provider_leaves_out_has_its_default() {
    let (_registry, harness) = loaded();
    assert_eq!(harness.program().expect("answered"), None);
    assert_eq!(harness.non_defaults().expect("answered"), Vec::new());
    assert_eq!(
        harness
            .session_record(SessionQuery::new(
                "0f8fad5b-d9cb-469f-a165-70867728950e",
                "/"
            ))
            .expect("answered"),
        None
    );
    let models = harness
        .models(ModelsRequest::new(), spawner(), keeper())
        .expect("answered");
    assert!(models.known);
    assert_eq!(models.models[0].id, "echo");
}

/// A host describes a configured harness by building an instance from its settings and asking it.
#[test]
fn a_configured_echo_describes_itself_as_configured() {
    let registry = registry();
    let offer = registry
        .offer::<dyn AcpHarness>("echo_harness")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    assert!(offer.builds_instances());
    let mut settings = Map::new();
    settings.set("title", "Configured echo");
    let instance = offer
        .instantiate(&Value::from(settings))
        .expect("the settings fit the schema");
    assert_eq!(
        instance.describe().expect("described").title,
        "Configured echo"
    );
    let mut wrong = Map::new();
    wrong.set("titel", "a misspelled key");
    assert!(
        offer.instantiate(&Value::from(wrong)).is_err(),
        "a key the settings do not declare was taken"
    );
}

fn instance(registry: &Registry, settings: Map) -> Arc<dyn AcpHarness> {
    let offer = registry
        .offer::<dyn AcpHarness>("echo_harness")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    Arc::new(
        offer
            .instantiate(&Value::from(settings))
            .expect("the settings fit the schema"),
    )
}

fn spawner() -> Object<dyn ProcessSpawner> {
    LocalSpawner.into_object()
}

fn keeper() -> Object<dyn AccountKeeper> {
    Hears.into_object()
}

/// A keeper that hears nothing.
struct Hears;

impl AccountKeeper for Hears {
    fn renewed(&self, _renewal: AccountRenewal) -> Result<(), ProviderError> {
        Ok(())
    }

    fn lost(&self, _loss: AccountLoss) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// A spawner nobody uses: the echo starts nothing.
struct LocalSpawner;

impl ProcessSpawner for LocalSpawner {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError> {
        Ok(SpawnerInfo::new(
            std::env::consts::OS,
            std::env::consts::ARCH,
            true,
        ))
    }

    fn spawn(&self, _spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError> {
        Err(ProviderError::new(
            Status::GUATIAO_ERR_UNSUPPORTED,
            "unused",
        ))
    }

    fn write_file(
        &self,
        _path: &str,
        _bytes: Secret<Vec<u8>>,
        _private: bool,
    ) -> Result<(), ProviderError> {
        Err(no_files())
    }

    fn read_file(&self, _path: &str, _max_bytes: u64) -> Result<Secret<Vec<u8>>, ProviderError> {
        Err(no_files())
    }

    fn file_info(&self, _path: &str) -> Result<Option<FileInfo>, ProviderError> {
        Err(no_files())
    }

    fn remove_file(&self, _path: &str) -> Result<(), ProviderError> {
        Err(no_files())
    }
}

fn no_files() -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_UNSUPPORTED, "no private files")
}

/// A history exported from one harness seeds a session in another, which exports it the same.
#[test]
fn a_history_moves_between_harnesses() {
    let registry = registry();
    let here = instance(&registry, Map::new());
    let there = instance(&registry, Map::new());
    let history = concat!(
        "{\"format\":\"coqui-acp-history\",\"version\":1}\n",
        "{\"method\":\"session/prompt\",\"params\":{\"prompt\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        "{\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"echo: hi\"}}}}\n",
    );
    let first = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let report = here
        .import_session(SessionImport::new(first, "/", history), spawner(), keeper())
        .expect("seeded");
    assert_eq!(report.seeded_from.map(|point| point.line), Some(2));
    assert!(report.losses.is_empty() && !report.compacted);
    let exported = here
        .export_session(SessionExport::new(first, "/"), spawner(), keeper())
        .expect("exported");
    assert_eq!(exported.next, None, "a short history is one page");
    let exported = exported.text;
    let second = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
    there
        .import_session(
            SessionImport::new(second, "/", exported.clone()),
            spawner(),
            keeper(),
        )
        .expect("seeded elsewhere");
    assert_eq!(
        there
            .export_session(SessionExport::new(second, "/"), spawner(), keeper())
            .expect("exported")
            .text,
        exported
    );
    let missing = there
        .export_session(SessionExport::new(first, "/"), spawner(), keeper())
        .expect_err("another harness's session is not this one's");
    assert_eq!(missing.status, Status::GUATIAO_ERR_NOT_FOUND);
}

/// A harness that cannot import says so, with a reason, and holds to the rest of the contract.
#[test]
fn a_harness_that_cannot_import_refuses_with_a_reason() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("imports", false);
    let harness = instance(&registry, settings);
    let refused = harness
        .import_session(
            SessionImport::new("0f8fad5b-d9cb-469f-a165-70867728950e", "/", ""),
            spawner(),
            keeper(),
        )
        .expect_err("it cannot import");
    assert_eq!(refused.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(!refused.message().is_empty());
    if let Err(failures) = Contract::new("an echo that imports nothing")
        .imports(false)
        .run(&*harness)
    {
        panic!("{failures}");
    }
}

/// A harness that reports usage is held to the scripted half of the contract, and is told apart from
/// one that does not.
#[test]
fn a_scripted_echo_reports_usage_as_the_contract_documents() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("usage", "scripted");
    let harness = instance(&registry, settings);
    let info = harness.describe().expect("described");
    assert_eq!(info.usage.sources, vec![Source::Reported]);
    assert!(info.usage.spend);
    if let Err(failures) = Contract::new("the scripted Rust echo")
        .message_limit(1 << 20)
        .run(&*harness)
    {
        panic!("{failures}");
    }
}

#[test]
fn an_echo_that_reports_no_usage_says_so_with_a_reason() {
    let (_registry, harness) = loaded();
    assert!(!harness.describe().expect("described").usage.reports());
    let report = harness
        .usage(
            UsageQuery::new().account(AccountGrant::new("pool/alice", "harness-login")),
            spawner(),
            keeper(),
        )
        .expect("answered");
    assert!(!report.supported && report.buckets.is_empty());
    assert!(
        report
            .unsupported_because
            .as_deref()
            .is_some_and(|why| !why.is_empty())
    );
}

struct Disagrees {
    inner: Arc<dyn AcpHarness>,
    hides_in_describe: bool,
    /// Puts a credential of the account it was given into the refusal it answers.
    leaks: bool,
}

impl AcpHarness for Disagrees {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        let mut info = self.inner.describe()?;
        if self.hides_in_describe {
            info.usage = UsageSupport::none();
        }
        Ok(info)
    }

    fn accounts(&self) -> Result<AccountCatalog, ProviderError> {
        self.inner.accounts()
    }

    fn start(
        &self,
        request: StartRequest,
        spawner: Object<dyn ProcessSpawner>,
        sink: Object<dyn ChannelSink>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn Channel>, ProviderError> {
        let credential = request.account.as_ref().and_then(|grant| {
            grant
                .credentials
                .expose()
                .iter()
                .next()
                .map(|(_, v)| v.clone())
        });
        match (self.inner.start(request, spawner, sink, keeper), credential) {
            (Err(refused), Some(credential)) if self.leaks => Err(ProviderError::new(
                refused.status,
                &format!("{} {credential}", refused.message()),
            )),
            (answer, _) => answer,
        }
    }

    fn refusal(
        &self,
        requirements: Vec<Requirement>,
        environment: Secret<Environment>,
        account: Option<AccountRef>,
    ) -> Result<Option<String>, ProviderError> {
        self.inner.refusal(requirements, environment, account)
    }

    fn program(&self) -> Result<Option<String>, ProviderError> {
        self.inner.program()
    }

    fn import_session(
        &self,
        session: SessionImport,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<ImportReport, ProviderError> {
        self.inner.import_session(session, spawner, keeper)
    }

    fn export_session(
        &self,
        export: SessionExport,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<ExportPage, ProviderError> {
        self.inner.export_session(export, spawner, keeper)
    }

    fn usage(
        &self,
        query: UsageQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<UsageReport, ProviderError> {
        if self.hides_in_describe {
            return self.inner.usage(query, spawner, keeper);
        }
        drop((query, spawner, keeper));
        Ok(UsageReport::unsupported("it says it has none"))
    }

    fn models(
        &self,
        request: ModelsRequest,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<ModelListing, ProviderError> {
        self.inner.models(request, spawner, keeper)
    }

    fn login(
        &self,
        request: LoginRequest,
        spawner: Object<dyn ProcessSpawner>,
        prompter: Object<dyn LoginPrompter>,
    ) -> Result<LoginOutcome, ProviderError> {
        self.inner.login(request, spawner, prompter)
    }

    fn renew(
        &self,
        request: RenewRequest,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<RenewReport, ProviderError> {
        self.inner.renew(request, spawner)
    }
}

/// A provider that reports usage differently from what it says is caught: a harness whose `describe`
/// hides what its connections do, and one whose `usage` slot contradicts its connections.
#[test]
fn the_contract_names_a_provider_whose_usage_disagrees_with_itself() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("usage", "scripted");
    let inner = instance(&registry, settings);
    for hides_in_describe in [false, true] {
        let harness = Disagrees {
            inner: Arc::clone(&inner),
            hides_in_describe,
            leaks: false,
        };
        let failures = Contract::new("a provider that disagrees with itself")
            .run(&harness)
            .expect_err("the contract finds the disagreement");
        let said = failures.to_string();
        assert!(
            said.contains("describes itself") || said.contains("reports usage as it says"),
            "{said}"
        );
    }
}

/// A provider that puts a credential in a refusal is named by the account checks.
#[test]
fn the_contract_names_a_provider_that_says_a_credential() {
    let registry = registry();
    let harness = Disagrees {
        inner: instance(&registry, Map::new()),
        hides_in_describe: false,
        leaks: true,
    };
    let failures = Contract::new("a provider that says a credential")
        .run(&harness)
        .expect_err("the contract finds the credential");
    let said = failures.to_string();
    assert!(said.contains("saying nothing of its credentials"), "{said}");
}

/// Logins off: the catalog lists none, `login` is unsupported, and the suite agrees.
#[test]
fn an_echo_without_logins_holds_to_the_contract() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("logins", false);
    let harness = instance(&registry, settings);
    assert!(harness.accounts().expect("answered").logins.is_empty());
    if let Err(failures) = Contract::new("an echo with no login")
        .logins(false)
        .message_limit(1 << 20)
        .run(&*harness)
    {
        panic!("{failures}");
    }
    // The suite holds a provider that lists methods to say it offers some.
    let failures = Contract::new("an echo with logins, run as if it had none")
        .logins(false)
        .run(&*instance(&registry, Map::new()))
        .expect_err("the catalog lists methods");
    assert!(
        failures.to_string().contains("logins are off"),
        "{failures}"
    );
}

/// One kind alone, as `--account` runs it.
#[test]
fn the_account_checks_run_on_one_kind() {
    let (_registry, harness) = loaded();
    for kind in ["echo-key", "harness-login"] {
        if let Err(failures) = Contract::new("the Rust echo, one kind")
            .account(kind)
            .message_limit(1 << 20)
            .run(&*harness)
        {
            panic!("{kind}: {failures}");
        }
    }
    let failures = Contract::new("the Rust echo, a kind it lacks")
        .account("no-such-kind")
        .run(&*harness)
        .expect_err("the catalog lists no such kind");
    assert!(failures.to_string().contains("no-such-kind"), "{failures}");
}
