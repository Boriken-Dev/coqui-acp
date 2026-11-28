//! A host that holds the provider as `dyn AcpHarness` warms Claude Code ahead of a connection, against the stand-in
//! program, and a provider whose policy keeps none ahead starts nothing.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::time::{Duration, Instant};

use coqui_acp_api::guatiao::Secret;
use coqui_acp_api::{AcpHarness, Environment, ProcessSpawner as _};
use coqui_acp_claude::{ClaudeProvider, ProviderSettings};
use coqui_acp_harness::LocalSpawner;
use support::{Scratch, replay_program};

fn provider(ahead: Option<u64>) -> ClaudeProvider {
    let mut settings = ProviderSettings {
        executable: replay_program().to_string_lossy().into_owned(),
        ..ProviderSettings::default()
    };
    settings.session.limits.warm_ahead = ahead;
    ClaudeProvider::new(settings)
}

fn environment(scratch: &Scratch) -> Environment {
    let mut environment = Environment::new();
    environment.insert("CLAUDE_CONFIG_DIR", scratch.0.to_string_lossy().as_ref());
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
fn a_host_holding_the_kind_warms_a_process_for_a_directory() {
    let scratch = Scratch::new("prewarm-kind");
    let environment = environment(&scratch);
    let cwd = scratch.0.to_string_lossy().into_owned();
    let provider = provider(Some(1));
    assert_eq!(provider.warm_ready(&cwd, &environment), 0);
    let harness: &dyn AcpHarness = &provider;
    harness
        .prewarm(
            &cwd,
            Secret::new(environment.clone()),
            LocalSpawner::new().into_object(),
        )
        .expect("answered");
    until("a process is warm", || {
        provider.warm_ready(&cwd, &environment) == 1
    });
    // Another environment is another process.
    assert_eq!(provider.warm_ready(&cwd, &Environment::new()), 0);
    coqui_acp_harness::drop_all();
}

#[test]
fn a_policy_that_keeps_none_ahead_starts_nothing() {
    let scratch = Scratch::new("prewarm-off");
    let environment = environment(&scratch);
    let cwd = scratch.0.to_string_lossy().into_owned();
    let provider = provider(None);
    let harness: &dyn AcpHarness = &provider;
    harness
        .prewarm(
            &cwd,
            Secret::new(environment.clone()),
            LocalSpawner::new().into_object(),
        )
        .expect("answered");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(provider.warm_ready(&cwd, &environment), 0);
}
