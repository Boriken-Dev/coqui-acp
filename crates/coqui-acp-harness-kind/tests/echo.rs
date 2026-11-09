//! The Rust echo, `examples/echo_harness.rs`, loaded from its file as a host loads any provider and
//! called as the kind, holds to the contract every `acp_harness` provider is held to.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

#[path = "contract/mod.rs"]
mod contract;

use std::path::PathBuf;
use std::sync::Arc;

use coqui_acp_harness_kind::guatiao::library::Registry;
use coqui_acp_harness_kind::{AcpHarness, SessionRecordQuery};

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

/// The registry stays alive with the test: what it offered points into the loaded file.
fn loaded() -> (Registry, Arc<dyn AcpHarness>) {
    let mut registry = Registry::new("coqui-acp-harness-kind-tests", "1.0");
    registry.load_file(&library()).unwrap_or_else(|error| {
        panic!("the library loads, once `cargo test` or `cargo build --examples` built it: {error}")
    });
    let harness = registry
        .offer::<dyn AcpHarness>("echo_harness")
        .expect("the library offers it")
        .expect("its table is this crate's kind")
        .shared()
        .expect("the provider is its one instance");
    (registry, harness)
}

#[test]
fn the_rust_echo_holds_to_the_contract() {
    let (_registry, harness) = loaded();
    contract::hold_the_contract("the Rust echo", &*harness);
}

#[test]
fn what_a_provider_leaves_out_has_its_default() {
    let (_registry, harness) = loaded();
    assert_eq!(harness.program().expect("answered"), None);
    assert_eq!(
        harness
            .session_record(SessionRecordQuery::new("echo-1", "/"))
            .expect("answered"),
        None
    );
    let models = harness.models(Default::default()).expect("answered");
    assert!(models.known);
    assert_eq!(models.models[0].id, "echo");
}
