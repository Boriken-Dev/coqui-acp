//! The Pi account provider: the sign-in Pi's own `/login` wrote is watched for a change nobody told the host of, a
//! provider's key is signed in, and the provider keeps every rule of the kind. It renews nothing and has no plan limits to
//! read.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coqui_acp_api::contract::AccountContract;
use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError, Registry};
use coqui_acp_api::guatiao::{Map, Value};
use coqui_acp_api::{
    AccountGrant, AccountKeeper, AccountLoss, AccountModelsQuery, AccountProvider, AccountRenewal,
    AccountUsageQuery, Credentials, HARNESS_LOGIN, LoginAnswer, LoginForm, LoginPrompter,
    LoginRequest, OpenUrl, ProcessSpawner, RenewRequest, WatchRequest,
};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_pi::{AccountSettings, PiAccounts};

const CANARY: &str = "coqui-acp-contract-canary-6b1d";

fn library() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    tests
        .parent()
        .and_then(Path::parent)
        .expect("a test binary is in target/<profile>/deps")
        .join("examples")
        .join(format!(
            "{}pi_provider{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-pi-accounts-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn fake() -> String {
    library()
        .parent()
        .expect("a file in examples")
        .join(format!("fake_pi{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn provider() -> PiAccounts {
    PiAccounts::new(AccountSettings {
        renew_poll_ms: 100,
        executable: fake(),
        ..AccountSettings::default()
    })
}

fn spawner(dir: &Path) -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().files(dir).into_object()
}

/// A keeper that remembers what it was told.
#[derive(Clone, Default)]
struct Heard {
    renewed: Arc<Mutex<Vec<AccountRenewal>>>,
    lost: Arc<Mutex<Vec<AccountLoss>>>,
}

impl AccountKeeper for Heard {
    fn renewed(&self, renewal: AccountRenewal) -> Result<(), ProviderError> {
        self.renewed.lock().unwrap().push(renewal);
        Ok(())
    }

    fn lost(&self, loss: AccountLoss) -> Result<(), ProviderError> {
        self.lost.lock().unwrap().push(loss);
        Ok(())
    }
}

fn wait_for(what: impl Fn() -> bool) {
    let waited = std::time::Instant::now();
    while !what() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct NoPrompter;

impl LoginPrompter for NoPrompter {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(&self, _form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        Err(ProviderError::new(
            Status::GUATIAO_ERR_GONE,
            "nobody to ask",
        ))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(true)
    }
}

fn sign_in(dir: &Path, text: &str) -> AccountGrant {
    AccountGrant::new("a-sign-in", "pi-oauth")
        .credentials(Credentials::from_iter([("auth_json", text)]))
        .state_dir(dir.to_string_lossy())
}

#[test]
fn a_watch_tells_a_sign_in_pi_renewed_and_one_deleted_is_lost() {
    let provider = provider();
    let dir = scratch("watch");
    let before = r#"{"anthropic":{"type":"oauth","refresh":"r1"}}"#;
    // The harness placed the sign-in before the watch began.
    std::fs::write(dir.join("auth.json"), before).unwrap();
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(sign_in(&dir, before).revision(4)),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    std::thread::sleep(Duration::from_millis(400));
    assert!(heard.renewed.lock().unwrap().is_empty() && heard.lost.lock().unwrap().is_empty());
    let after = r#"{"anthropic":{"type":"oauth","refresh":"r2"}}"#;
    std::fs::write(dir.join("auth.json"), after).unwrap();
    wait_for(|| !heard.renewed.lock().unwrap().is_empty());
    {
        let renewed = heard.renewed.lock().unwrap();
        assert_eq!(renewed.len(), 1);
        assert_eq!(renewed[0].from_revision, 4);
        assert_eq!(
            renewed[0].credentials.expose().get("auth_json"),
            Some(after)
        );
    }
    std::fs::remove_file(dir.join("auth.json")).unwrap();
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_watch_never_puts_the_grants_copy_back_and_tells_a_sign_in_that_is_gone_at_once() {
    let provider = provider();
    let dir = scratch("gone");
    let heard = Heard::default();
    let watch = provider
        .watch(
            WatchRequest::new(sign_in(&dir, r#"{"anthropic":{}}"#)),
            spawner(&dir),
            heard.clone().into_object(),
        )
        .expect("watched");
    wait_for(|| !heard.lost.lock().unwrap().is_empty());
    assert_eq!(heard.lost.lock().unwrap().len(), 1);
    assert!(!dir.join("auth.json").exists());
    watch.stop().expect("stopped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn it_signs_in_only_a_key_renews_nothing_and_reads_no_limits() {
    let provider = provider();
    let dir = scratch("none");
    let error = provider
        .login(
            LoginRequest::new("anything", dir.to_string_lossy()),
            spawner(&dir),
            NoPrompter.into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(error.message().contains("terminal"), "{}", error.message());
    let grant = sign_in(&dir, r#"{"anthropic":{}}"#);
    let error = provider
        .renew(RenewRequest::new(grant.clone()), spawner(&dir))
        .expect_err("unsupported");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    assert!(
        error.message().contains("model turn"),
        "{}",
        error.message()
    );
    let usage = provider
        .usage(
            AccountUsageQuery::new(grant),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("answered");
    assert!(!usage.supported && usage.unsupported_because.is_some());
    assert_eq!(usage.account.as_deref(), Some("a-sign-in"));
    // A grant the kind refuses is refused before the answer.
    let bare = AccountGrant::new("a", "pi-oauth").state_dir(dir.to_string_lossy());
    let error = provider
        .renew(RenewRequest::new(bare), spawner(&dir))
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_key_whose_variable_names_an_api_is_an_account_it_keeps_and_another_providers_kind_is_not() {
    let provider = provider();
    let dir = scratch("foreign");
    let offer = provider.offers().expect("offered");
    let ids: Vec<&str> = offer.kinds.iter().map(|kind| kind.id.as_str()).collect();
    assert_eq!(ids, ["pi-api-key", "pi-oauth"]);
    let logins: Vec<&str> = offer.logins.iter().map(|login| login.id.as_str()).collect();
    assert_eq!(logins, ["api-key"]);
    assert_eq!(offer.logins[0].produces, "pi-api-key");
    assert!(offer.usage.is_empty());
    let key = AccountGrant::new("k", "pi-api-key")
        .settings(coqui_acp_harness::provider::schema(
            &serde_json::json!({"variable": "ANTHROPIC_API_KEY"}),
        ))
        .credentials(Credentials::from_iter([("api_key", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let report = provider
        .usage(
            AccountUsageQuery::new(key.clone()),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("a key is kept");
    assert!(!report.supported);
    // A key has no file Pi rewrites, so there is nothing to watch.
    let error = provider
        .watch(
            WatchRequest::new(key),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .map(|_| ())
        .expect_err("nothing to watch");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let foreign = AccountGrant::new("a", "someone-elses-kind")
        .provides(["pi"])
        .credentials(Credentials::from_iter([("auth_json", CANARY)]))
        .state_dir(dir.to_string_lossy());
    let error = provider
        .watch(
            WatchRequest::new(foreign.clone()),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .map(|_| ())
        .expect_err("not its kind");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let error = provider
        .usage(
            AccountUsageQuery::new(foreign),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect_err("not its kind");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(!error.message().contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A prompter that answers the key form with the key it was made with.
struct Typing(&'static str);

impl LoginPrompter for Typing {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        let schema = coqui_acp_harness::provider::to_json(&form.schema);
        assert_eq!(
            schema["properties"]["api_key"]["writeOnly"],
            serde_json::json!(true),
            "{schema}"
        );
        Ok(LoginAnswer::new(
            Value::from(Map::new()),
            Credentials::from_iter([("api_key", self.0)]),
        ))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

fn key_login(
    variable: &str,
    typed: &'static str,
) -> Result<coqui_acp_api::LoginOutcome, ProviderError> {
    let dir = scratch("key-login");
    let inputs = coqui_acp_harness::provider::schema(&serde_json::json!({"variable": variable}));
    let outcome = provider().login(
        LoginRequest::new("api-key", dir.to_string_lossy()).inputs(inputs),
        spawner(&dir),
        Typing(typed).into_object(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

#[test]
fn the_key_login_keeps_the_key_and_its_variable_and_the_account_provides_that_api() {
    let outcome = key_login("ANTHROPIC_API_KEY", CANARY).expect("signed in");
    assert_eq!(outcome.kind, "pi-api-key");
    assert_eq!(outcome.credentials.expose().get("api_key"), Some(CANARY));
    let settings = coqui_acp_harness::provider::to_json(&outcome.settings);
    assert_eq!(
        settings,
        serde_json::json!({"variable": "ANTHROPIC_API_KEY"})
    );
    assert!(!outcome.checked);
    let catalog = coqui_acp_pi::accounts::catalog();
    let kind = catalog.kind("pi-api-key").expect("listed");
    assert_eq!(
        coqui_acp_harness::stdio::accounts::provided(kind, &outcome.settings),
        [coqui_acp_api::standard::ANTHROPIC_API]
    );
    // A variable of a provider Pi has no API for is kept as it is and provides none.
    let other = key_login("MISTRAL_API_KEY", CANARY).expect("signed in");
    assert!(coqui_acp_harness::stdio::accounts::provided(kind, &other.settings).is_empty());
}

#[test]
fn the_key_login_refuses_a_variable_that_is_none_a_blank_key_and_a_way_it_does_not_have() {
    for variable in ["", "A=B", "1ST", "TWO WORDS", "AOS_X"] {
        let error = key_login(variable, CANARY).expect_err("refused");
        assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE, "{variable:?}");
    }
    for typed in ["", "   ", "two\nlines"] {
        let error = key_login("OPENAI_API_KEY", typed).expect_err("refused");
        assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE, "{typed:?}");
        assert!(!error.message().contains(CANARY));
    }
    let dir = scratch("other-way");
    let error = provider()
        .login(
            LoginRequest::new("browser", dir.to_string_lossy()),
            spawner(&dir),
            Typing(CANARY).into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suite, run in a process of its own whose home is the suite's, so that no test sets a variable of this one.
#[test]
fn the_provider_keeps_every_rule_of_the_kind() {
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
    settings.set("renew_poll_ms", 100);
    settings.set("executable", Value::from(fake().as_str()));
    let provider = registry
        .offer::<dyn AccountProvider>("pi_accounts")
        .expect("the library offers it")
        .expect("its table is this kind")
        .instantiate(&Value::from(settings))
        .expect("the settings are taken");
    let outcome = AccountContract::new("pi")
        .logins(true)
        .program(true)
        .run(&provider);
    drop(provider);
    drop(registry);
    if let Err(failures) = outcome {
        panic!("{failures}");
    }
}

/// The models of the local account, for a provider that runs the stand-in with `environment`.
fn listing_of(environment: coqui_acp_api::Environment) -> coqui_acp_api::ModelListing {
    let dir = scratch("listing");
    let listing = provider()
        .models(
            AccountModelsQuery::new(AccountGrant::new("local", HARNESS_LOGIN))
                .environment(environment),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("answered");
    let _ = std::fs::remove_dir_all(&dir);
    listing
}

#[test]
fn it_offers_the_login_of_its_program_as_the_automatic_local_account() {
    let offer = provider().offers().expect("offered");
    assert!(offer.local && !offer.reports_usage(HARNESS_LOGIN));
}

#[test]
fn a_kind_it_does_not_list_is_refused_by_models_as_a_bad_value() {
    let dir = scratch("refused");
    let error = provider()
        .models(
            AccountModelsQuery::new(AccountGrant::new("a", "no-such-kind")),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_local_account_lists_models_from_pi_with_no_turn_and_has_no_usage() {
    let dir = scratch("models-home");
    let environment: coqui_acp_api::Environment =
        [("PI_CODING_AGENT_DIR", dir.to_string_lossy().into_owned())]
            .into_iter()
            .collect();
    let listing = listing_of(environment.clone());
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
            AccountUsageQuery::new(AccountGrant::new("local", HARNESS_LOGIN))
                .environment(environment),
            spawner(&dir),
            Heard::default().into_object(),
        )
        .expect("answered");
    assert!(
        !report.supported && report.unsupported_because.is_some(),
        "{report:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_model_carries_its_thinking_levels_and_the_others_none() {
    let dir = scratch("models-efforts");
    let environment: coqui_acp_api::Environment =
        [("PI_CODING_AGENT_DIR", dir.to_string_lossy().into_owned())]
            .into_iter()
            .collect();
    let listing = listing_of(environment);
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

/// The kind lists no standard of its own: which API a key is for is the value of its `variable`, and the provider reads it.
#[test]
fn a_key_provides_the_api_of_the_variable_it_names_and_pis_own_login_provides_pi() {
    let provider = provider();
    let key = |variable: &str| {
        let mut settings = Map::new();
        settings.set("variable", variable);
        coqui_acp_api::AccountRef::new("key", "pi-api-key").settings(Value::from(settings))
    };
    assert_eq!(
        provider
            .provides(key("ANTHROPIC_API_KEY"))
            .expect("a key of a variable Pi has an API for"),
        ["anthropic-api"]
    );
    assert_eq!(
        provider
            .provides(key("OPENAI_API_KEY"))
            .expect("a key of a variable Pi has an API for"),
        ["openai-api"]
    );
    assert!(
        provider
            .provides(key("MISTRAL_API_KEY"))
            .expect("a key of a variable with no API")
            .is_empty()
    );
    assert_eq!(
        provider
            .provides(coqui_acp_api::AccountRef::new("local", HARNESS_LOGIN))
            .expect("the login Pi has"),
        ["pi"]
    );
    let unlisted = provider
        .provides(coqui_acp_api::AccountRef::new("a", "no-such-kind"))
        .expect_err("not a kind it lists");
    assert_eq!(unlisted.status, Status::GUATIAO_ERR_BAD_VALUE);
}
