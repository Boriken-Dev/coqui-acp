//! The Rust echo account provider, `examples/echo_account_provider.rs`, loaded from its file as a
//! host loads any provider and called as the kind, holds to the contract every `account_provider`
//! is held to, and the contract names each rule a provider breaks.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use coqui_acp_api::contract::AccountContract;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Value};
use coqui_acp_api::{
    AccountKeeper, AccountOffer, AccountProvider, AccountProviderInfo, AccountUsage,
    AccountUsageQuery, AccountWatch, LoginOutcome, LoginPrompter, LoginRequest, ProcessSpawner,
    RenewReport, RenewRequest, WatchRequest,
};

/// The echo's library file, which `cargo test` builds beside the tests.
fn library() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    let profile = tests
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary is in target/<profile>/deps");
    profile.join("examples").join(format!(
        "{}echo_account_provider{}",
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

fn instance(registry: &Registry, settings: Map) -> Arc<dyn AccountProvider> {
    let offer = registry
        .offer::<dyn AccountProvider>("echo_account_provider")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    Arc::new(
        offer
            .instantiate(&Value::from(settings))
            .expect("the settings fit the schema"),
    )
}

fn echo() -> (Registry, Arc<dyn AccountProvider>) {
    let registry = registry();
    let provider = instance(&registry, Map::new());
    (registry, provider)
}

#[test]
fn the_rust_echo_holds_to_the_contract() {
    let (_registry, provider) = echo();
    if let Err(failures) = AccountContract::new("the Rust echo").run(&*provider) {
        panic!("{failures}");
    }
}

#[test]
fn an_echo_with_no_logins_and_no_usage_holds_to_the_contract_run_that_way() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("logins", false);
    settings.set("usage", false);
    let provider = instance(&registry, settings);
    if let Err(failures) = AccountContract::new("the quiet echo")
        .logins(false)
        .run(&*provider)
    {
        panic!("{failures}");
    }
    // The same provider fails the default contract, which expects a login method.
    let failures = AccountContract::new("the quiet echo")
        .run(&*provider)
        .expect_err("a provider with no login method was held to offering one");
    assert!(
        failures.to_string().contains("lists no login method"),
        "{failures}"
    );
}

/// A host describes a configured provider by building an instance from its settings and asking it.
#[test]
fn a_configured_echo_describes_itself_as_configured() {
    let registry = registry();
    let mut settings = Map::new();
    settings.set("title", "Configured echo");
    let provider = instance(&registry, settings);
    assert_eq!(
        provider.describe().expect("described").title,
        "Configured echo"
    );
    let offer = registry
        .offer::<dyn AccountProvider>("echo_account_provider")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    let mut wrong = Map::new();
    wrong.set("titel", "a misspelled key");
    assert!(
        offer.instantiate(&Value::from(wrong)).is_err(),
        "a key the settings do not declare was taken"
    );
}

/// A provider that is the echo in everything but one rule it breaks.
struct Breaks {
    inner: Arc<dyn AccountProvider>,
    breaking: &'static str,
    // Keeps the library loaded as long as the wrapper lives.
    _registry: Registry,
}

impl AccountProvider for Breaks {
    fn describe(&self) -> Result<AccountProviderInfo, ProviderError> {
        self.inner.describe()
    }

    fn offers(&self) -> Result<AccountOffer, ProviderError> {
        let mut offer = self.inner.offers()?;
        if self.breaking == "provides nothing" {
            for kind in &mut offer.kinds {
                kind.provides.clear();
            }
        }
        if self.breaking == "usage for an unlisted kind" {
            offer.usage.push("no-such-kind".into());
        }
        Ok(offer)
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
        if self.breaking == "renews with nothing and no reason" {
            drop(spawner);
            return Ok(RenewReport::default());
        }
        self.inner.renew(request, spawner)
    }

    fn watch(
        &self,
        request: WatchRequest,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn AccountWatch>, ProviderError> {
        if self.breaking == "watches nothing" {
            drop((spawner, keeper));
            return Ok(Idle.into_object());
        }
        self.inner.watch(request, spawner, keeper)
    }

    fn usage(
        &self,
        query: AccountUsageQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<AccountUsage, ProviderError> {
        if self.breaking == "usage that shows the environment" {
            let shown: String = query
                .environment
                .expose()
                .iter()
                .map(|(_, value)| value.clone())
                .collect();
            drop((spawner, keeper));
            return Ok(AccountUsage::unsupported(shown));
        }
        self.inner.usage(query, spawner, keeper)
    }
}

/// A watch that tells nothing, ever.
struct Idle;

impl AccountWatch for Idle {
    fn stop(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

fn breaking(rule: &'static str) -> Breaks {
    let (registry, inner) = echo();
    Breaks {
        inner,
        breaking: rule,
        _registry: registry,
    }
}

fn broken_by(rule: &'static str, check: &str) {
    let provider = breaking(rule);
    let failures = AccountContract::new("the breaker")
        .run(&provider)
        .expect_err("the provider broke a rule and kept every one");
    assert!(
        failures.failures.iter().any(|f| f.check == check),
        "{rule}: the rule named is not {check:?}: {failures}"
    );
}

#[test]
fn a_provider_that_lists_a_kind_providing_nothing_is_named() {
    broken_by("provides nothing", "lists what it offers");
}

#[test]
fn a_provider_that_lists_usage_for_a_kind_it_does_not_have_is_named() {
    broken_by("usage for an unlisted kind", "lists what it offers");
}

#[test]
fn a_renewal_that_changes_nothing_and_says_nothing_is_named() {
    broken_by("renews with nothing and no reason", "renews as it lists");
}

#[test]
fn a_watch_that_never_tells_is_named() {
    broken_by("watches nothing", "watches as it lists");
}

#[test]
fn a_usage_report_that_shows_the_environment_is_named() {
    broken_by(
        "usage that shows the environment",
        "reports plan usage as it lists",
    );
}

fn check(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_acp-account-check"))
        .arg("--library")
        .arg(library())
        .args(args)
        .output()
        .expect("the checker runs")
}

#[test]
fn the_checker_passes_a_provider_that_keeps_every_rule() {
    let out = check(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_checker_builds_an_instance_from_settings_and_names_a_kind() {
    let out = check(&["--settings", r#"{"title": "Configured echo"}"#]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let quiet = check(&["--settings", r#"{"logins": false}"#, "--no-logins"]);
    assert!(
        quiet.status.success(),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let one = check(&["--account", "echo-key"]);
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    let refused = check(&["--settings", r#"{"titel": "misspelled"}"#]);
    assert_eq!(refused.status.code(), Some(2));
    let missing = check(&["--account", "no-such-kind"]);
    assert_eq!(missing.status.code(), Some(1));
}
