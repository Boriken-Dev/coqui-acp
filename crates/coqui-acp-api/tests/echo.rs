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
use coqui_acp_api::guatiao::library::Registry;
use coqui_acp_api::guatiao::{Map, Secret, Value};
use coqui_acp_api::{AcpHarness, SessionRecordQuery};

/// The echo's library file, which `cargo test` builds beside the tests.
pub fn library() -> PathBuf {
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
            .session_record(SessionRecordQuery::new(
                "0f8fad5b-d9cb-469f-a165-70867728950e",
                "/"
            ))
            .expect("answered"),
        None
    );
    let models = harness.models(Secret::default()).expect("answered");
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
