//! A host that holds the provider as `dyn AcpHarness` warms a Codex app server ahead of a connection, against the
//! stand-in program, and a provider whose policy keeps none ahead starts nothing.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::time::{Duration, Instant};

use coqui_acp_api::guatiao::Secret;
use coqui_acp_api::{AcpHarness, Environment, ProcessSpawner as _};
use coqui_acp_codex::{CodexProvider, ProviderSettings};
use coqui_acp_harness::LocalSpawner;
use support::{Scratch, replay_program};

fn provider(ahead: Option<u64>) -> CodexProvider {
    let mut settings = ProviderSettings {
        executable: replay_program().to_string_lossy().into_owned(),
        ..ProviderSettings::default()
    };
    settings.session.limits.warm_ahead = ahead;
    CodexProvider::new(settings)
}

fn environment(scratch: &Scratch) -> Environment {
    let mut environment = Environment::new();
    environment.insert("CODEX_HOME", scratch.0.to_string_lossy().as_ref());
    environment.insert("COQUI_ACP_CODEX_THREAD", "thread-1");
    environment
}

fn until(what: &str, done: impl Fn() -> bool) {
    let limit = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < limit, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_host_holding_the_kind_warms_an_app_server() {
    let scratch = Scratch::new("prewarm-kind");
    let environment = environment(&scratch);
    let provider = provider(Some(1));
    assert_eq!(provider.warm_ready(&environment), 0);
    let harness: &dyn AcpHarness = &provider;
    harness
        .prewarm(
            "",
            Secret::new(environment.clone()),
            LocalSpawner::new().into_object(),
        )
        .expect("answered");
    until("an app server is warm", || {
        provider.warm_ready(&environment) == 1
    });
    // Another environment is another app server.
    assert_eq!(provider.warm_ready(&Environment::new()), 0);
    coqui_acp_harness::drop_all();
}

#[test]
fn a_policy_that_keeps_none_ahead_starts_nothing() {
    let scratch = Scratch::new("prewarm-off");
    let environment = environment(&scratch);
    let provider = provider(None);
    let harness: &dyn AcpHarness = &provider;
    harness
        .prewarm(
            "",
            Secret::new(environment.clone()),
            LocalSpawner::new().into_object(),
        )
        .expect("answered");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(provider.warm_ready(&environment), 0);
}
