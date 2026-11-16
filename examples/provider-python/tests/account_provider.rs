//! The `account_provider` written in Python, `account/acp_echo_account_provider.py`, loaded in this
//! process through Guatiao's source of Python libraries and called as the Rust-declared kind, holds
//! to the contract `coqui_acp_api::contract` ships, as the Rust echo does.
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
fn the_python_account_provider_needs_a_python() {
    panic!("build.rs found no Python to embed: set COQUI_ACP_TEST_PYTHON to one");
}

#[cfg(python_ready)]
mod support;

#[cfg(python_ready)]
mod with_python {
    use std::path::PathBuf;
    use std::sync::Arc;

    use coqui_acp_api::AccountProvider;
    use coqui_acp_api::contract::AccountContract;
    use coqui_acp_api::guatiao::library::Registry;
    use coqui_acp_api::guatiao::{List, Map, Value};

    use super::support::{guatiao, manifest_dir, source};

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
    fn loaded() -> Registry {
        let mut config = Map::new();
        config.set("dirs", texts(&[manifest_dir().join("account")]));
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
        let mut registry = Registry::new("coqui-acp-account-python-tests", "1.0");
        registry
            .add_source_file(source(), &config)
            .unwrap_or_else(|e| panic!("the source was refused: {e}"));
        let changed = registry.refresh();
        assert!(changed.refused.is_empty(), "{changed:?}");
        registry
    }

    fn instance(registry: &Registry, settings: Map) -> Arc<dyn AccountProvider> {
        let offer = registry
            .offer::<dyn AccountProvider>("acp_echo_account_provider_echo_accounts")
            .expect("the source offers it")
            .expect("its table is the Rust-declared kind");
        Arc::new(
            offer
                .instantiate(&Value::from(settings))
                .expect("the settings fit the schema"),
        )
    }

    #[test]
    fn the_python_echo_holds_to_the_contract() {
        let registry = loaded();
        let provider = instance(&registry, Map::new());
        if let Err(failures) = AccountContract::new("the Python echo").run(&*provider) {
            panic!("{failures}");
        }
    }

    /// With its logins and usage off, the Python echo lists none and the suite agrees.
    #[test]
    fn the_python_echo_without_logins_or_usage_holds_to_the_contract() {
        let registry = loaded();
        let mut settings = Map::new();
        settings.set("logins", false);
        settings.set("usage", false);
        let provider = instance(&registry, settings);
        let offer = provider.offers().expect("answered");
        assert!(offer.logins.is_empty() && offer.usage.is_empty());
        if let Err(failures) = AccountContract::new("the Python echo without logins")
            .logins(false)
            .run(&*provider)
        {
            panic!("{failures}");
        }
    }

    #[test]
    fn a_setting_the_python_echo_does_not_take_is_refused() {
        let registry = loaded();
        let offer = registry
            .offer::<dyn AccountProvider>("acp_echo_account_provider_echo_accounts")
            .expect("the source offers it")
            .expect("its table is the Rust-declared kind");
        let mut wrong = Map::new();
        wrong.set("titel", "misspelled");
        assert!(
            offer.instantiate(&Value::from(wrong)).is_err(),
            "a key the settings do not declare was accepted"
        );
    }
}
