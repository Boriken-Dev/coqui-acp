//! The `acp_harness` provider written in Python, `harness/acp_echo_harness.py`, loaded in this process
//! through Guatiao's source of Python libraries and called as the Rust-declared kind, holds to the same
//! contract as the Rust echo.
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
#[path = "../../../crates/coqui-acp-harness-kind/tests/contract/mod.rs"]
mod contract;

#[cfg(python_ready)]
mod with_python {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use coqui_acp_harness_kind::guatiao::library::Registry;
    use coqui_acp_harness_kind::guatiao::{List, Map};
    use coqui_acp_harness_kind::{AcpHarness, SessionRecordQuery};

    use super::contract;
    use super::support::{guatiao, manifest_dir, python, source};

    fn kinds_crate() -> PathBuf {
        manifest_dir().join("../../crates/coqui-acp-harness-kind")
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
        contract::hold_the_contract("the Python echo", &*harness);
        assert_eq!(harness.program().expect("answered"), None);
        assert_eq!(
            harness
                .session_record(SessionRecordQuery::new("python-echo-1", "/"))
                .expect("answered"),
            None
        );
        let models = harness.models(Default::default()).expect("answered");
        assert!(models.known);
        assert_eq!(models.models[0].id, "echo");
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
