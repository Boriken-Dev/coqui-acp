//! The `acp_harness` provider written in Python, `harness/acp_echo_harness.py`, loaded in this process
//! through Guatiao's source of Python libraries and called as the Rust-declared kind, holds to the
//! contract `coqui_acp_api::contract` ships, as the Rust echo does.
//!
//! Where build.rs found no Python the one test left is ignored, and a run counts it as not run.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

#[cfg(not(python_ready))]
#[test]
#[ignore = "NOT RUN: build.rs found no Python to embed, and said why in a warning"]
fn the_python_harness_needs_a_python() {
    panic!("build.rs found no Python to embed: set COQUI_ACP_TEST_PYTHON to one");
}

#[cfg(python_ready)]
mod support;

#[cfg(python_ready)]
mod with_python {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use coqui_acp_api::contract::Contract;
    use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
    use coqui_acp_api::guatiao::{List, Map, Secret, Status, Value};
    use coqui_acp_api::{
        AccountGrant, AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, FileInfo,
        ModelsRequest, ProcessSpawner, SessionQuery, Source, SpawnSpec, SpawnedProcess,
        SpawnerInfo, UsageQuery,
    };

    use super::support::{guatiao, manifest_dir, python, source};

    /// A spawner nobody uses: the echo starts nothing.
    struct Unused;

    impl ProcessSpawner for Unused {
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

        fn read_file(
            &self,
            _path: &str,
            _max_bytes: u64,
        ) -> Result<Secret<Vec<u8>>, ProviderError> {
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

    fn unused_spawner() -> Object<dyn ProcessSpawner> {
        Unused.into_object()
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

    fn keeper() -> Object<dyn AccountKeeper> {
        Hears.into_object()
    }

    fn kinds_crate() -> PathBuf {
        manifest_dir().join("../../crates/coqui-acp-api")
    }

    fn texts(items: &[PathBuf]) -> List {
        let mut list = List::new();
        for item in items {
            list.push(item.to_string_lossy().as_ref());
        }
        list
    }

    /// The registry stays alive with the test: what it offered points into the source's library.
    fn loaded() -> (Registry, Arc<dyn AcpHarness>) {
        let mut config = Map::new();
        config.set("dirs", texts(&[manifest_dir().join("harness")]));
        config.set(
            "paths",
            texts(&[
                guatiao().join("bindings/python/guatiao/src"),
                kinds_crate().join("python"),
            ]),
        );
        if cfg!(windows) {
            // Where the source finds python3.dll, and through it the interpreter.
            config.set(
                "import_dirs",
                texts(&[PathBuf::from(env!("COQUI_ACP_TEST_PYTHON_PREFIX"))]),
            );
        }
        let mut registry = Registry::new("coqui-acp-harness-python-tests", "1.0");
        registry
            .add_source_file(source(), &config)
            .unwrap_or_else(|e| panic!("the source was refused: {e}"));
        let changed = registry.refresh();
        assert!(changed.refused.is_empty(), "{changed:?}");
        let harness = registry
            .offer::<dyn AcpHarness>("acp_echo_harness_echo_harness")
            .expect("the source offers it")
            .expect("its table is the Rust-declared kind")
            .shared()
            .expect("the provider is its one instance");
        (registry, harness)
    }

    #[test]
    fn the_python_echo_holds_to_the_contract_and_leaves_the_rest_to_the_defaults() {
        let (_registry, harness) = loaded();
        if let Err(failures) = Contract::new("the Python echo")
            .message_limit(1 << 20)
            .run(&*harness)
        {
            panic!("{failures}");
        }
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
            .models(ModelsRequest::new(), unused_spawner(), keeper())
            .expect("answered");
        assert!(models.known);
        assert_eq!(models.models[0].id, "echo");
    }

    /// Scripted, the Python echo reports usage as the contract documents, through the kind and through ACP.
    #[test]
    fn a_scripted_python_echo_reports_usage_as_the_contract_documents() {
        let (registry, plain) = loaded();
        assert!(!plain.describe().expect("described").usage.reports());
        let report = plain
            .usage(
                UsageQuery::new().account(AccountGrant::new("pool/alice", "harness-login")),
                unused_spawner(),
                keeper(),
            )
            .expect("answered");
        assert!(!report.supported && report.unsupported_because.is_some());
        let offer = registry
            .offer::<dyn AcpHarness>("acp_echo_harness_echo_harness")
            .expect("the source offers it")
            .expect("its table is the Rust-declared kind");
        let mut settings = Map::new();
        settings.set("usage", "scripted");
        let scripted: Arc<dyn AcpHarness> = Arc::new(
            offer
                .instantiate(&Value::from(settings))
                .expect("the settings fit the schema"),
        );
        let info = scripted.describe().expect("described");
        assert_eq!(info.usage.sources, vec![Source::Reported]);
        assert!(info.usage.spend);
        if let Err(failures) = Contract::new("the scripted Python echo")
            .message_limit(1 << 20)
            .run(&*scripted)
        {
            panic!("{failures}");
        }
        let mut wrong = Map::new();
        wrong.set("usage", "sometimes");
        assert!(
            offer.instantiate(&Value::from(wrong)).is_err(),
            "a value the settings do not take was accepted"
        );
    }

    /// With its logins off, the Python echo lists none and refuses `login`, and the suite agrees.
    #[test]
    fn the_python_echo_without_logins_holds_to_the_contract() {
        let (registry, _) = loaded();
        let offer = registry
            .offer::<dyn AcpHarness>("acp_echo_harness_echo_harness")
            .expect("the source offers it")
            .expect("its table is the Rust-declared kind");
        let mut settings = Map::new();
        settings.set("logins", false);
        let harness: Arc<dyn AcpHarness> = Arc::new(
            offer
                .instantiate(&Value::from(settings))
                .expect("the settings fit the schema"),
        );
        assert!(harness.accounts().expect("answered").logins.is_empty());
        if let Err(failures) = Contract::new("the Python echo without logins")
            .logins(false)
            .message_limit(1 << 20)
            .run(&*harness)
        {
            panic!("{failures}");
        }
    }

    #[test]
    fn the_committed_python_kinds_are_what_the_description_writes() {
        let checked = Command::new(python())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONPATH", guatiao().join("bindings/python/guatiao/src"))
            .arg(kinds_crate().join("python/write_kinds.py"))
            .arg("--check")
            .output()
            .expect("Python runs");
        assert!(
            checked.status.success(),
            "regenerate them: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
}
