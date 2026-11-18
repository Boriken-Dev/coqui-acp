//! `coqui_acp_codex::accounts::apply`: what it does to a launch and to a home directory, the key or token it saves with
//! `codex login` (the secret on standard input only), and the part of the catalog that is itself a credential.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::{AccountGrant, Credentials, HARNESS_LOGIN, ProcessSpawner};
use coqui_acp_codex::CodexLaunch;
use coqui_acp_codex::accounts::{self, ApplyOptions, apply, apply_with};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_harness::account::{Accounts, Files};

const CANARY: &str = "sk-canary-8d02e61f3b";
const LOGIN_FILE: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"i","access_token":"stand-in-access-1","refresh_token":"stand-in-refresh-1"}}"#;

fn profile() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    tests
        .parent()
        .and_then(Path::parent)
        .expect("a test binary is in target/<profile>/deps")
        .to_path_buf()
}

fn fake() -> OsString {
    profile()
        .join("examples")
        .join(format!("fake_codex{}", std::env::consts::EXE_SUFFIX))
        .into_os_string()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-codex-apply-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn files_in(dir: &Path) -> Files {
    Files::new(Arc::new(LocalSpawner::new().files(dir).into_object()))
}

fn launch() -> CodexLaunch {
    let mut launch = CodexLaunch::new(".");
    launch.executable = fake();
    launch
}

fn with_credentials(kind: &str, pairs: &[(&str, &str)]) -> AccountGrant {
    AccountGrant::new("acct", kind).credentials(pairs.iter().copied().collect::<Credentials>())
}

fn variable(launch: &CodexLaunch, name: &str) -> Option<String> {
    launch
        .env
        .get(&OsString::from(name))
        .map(|value| value.to_string_lossy().into_owned())
}

fn logins(dir: &Path, flag: &str) -> usize {
    std::fs::read_to_string(dir.join("fake-codex.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("a log line"))
        .filter(|entry| entry["args"] == serde_json::json!(["login", flag]))
        .count()
}

#[test]
fn a_key_is_saved_by_codex_login_on_standard_input_through_the_files_spawner() {
    let dir = scratch("key");
    let mut launch = launch();
    launch
        .env
        .insert("OPENAI_API_KEY".into(), "host-key".into());
    launch
        .env
        .insert("OPENAI_BASE_URL".into(), "https://host.example".into());
    let grant = with_credentials(accounts::API_KEY, &[("api_key", CANARY)])
        .state_dir(dir.to_string_lossy());
    let applied = apply(&grant, &mut launch, &files_in(&dir)).expect("applied");
    assert_eq!(applied.dir(), dir.to_string_lossy());
    assert!(!applied.keeps_files());
    assert_eq!(
        variable(&launch, "CODEX_HOME").as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert_eq!(variable(&launch, "OPENAI_API_KEY"), None);
    assert_eq!(variable(&launch, "OPENAI_BASE_URL"), None);
    let catalog = accounts::catalog();
    for name in &catalog.credential_variables {
        assert!(
            launch
                .remove_env
                .iter()
                .any(|removed| removed == name.as_str()),
            "{name}"
        );
    }
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).unwrap()).unwrap();
    assert_eq!(saved["OPENAI_API_KEY"], CANARY);
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        "cli_auth_credentials_store = \"file\"\n"
    );
    assert_eq!(logins(&dir, "--with-api-key"), 1);
    let log = std::fs::read_to_string(dir.join("fake-codex.log")).unwrap();
    assert!(
        !log.contains(CANARY),
        "the key is in no argument or variable"
    );
    assert!(!format!("{applied:?}").contains(CANARY));
    let held = applied
        .hold(&Arc::new(Accounts::new()), &files_in(&dir))
        .expect("nothing to place");
    assert!(held.is_none());
    // A key `auth.json` already holds is not saved again.
    let mut again = self::launch();
    let _ = apply(&grant, &mut again, &files_in(&dir)).expect("applied again");
    assert_eq!(logins(&dir, "--with-api-key"), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_token_is_saved_with_the_flag_that_takes_one() {
    let dir = scratch("token");
    let mut launch = launch();
    let grant = with_credentials(accounts::ACCESS_TOKEN, &[("access_token", CANARY)])
        .state_dir(dir.to_string_lossy());
    let options = ApplyOptions::new().timeout(Duration::from_secs(30));
    let _applied = apply_with(&grant, &mut launch, &files_in(&dir), &options).expect("applied");
    assert_eq!(logins(&dir, "--with-access-token"), 1);
    let log = std::fs::read_to_string(dir.join("fake-codex.log")).unwrap();
    assert!(!log.contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_key_codex_refuses_is_bad_value_and_the_answer_names_no_key() {
    let dir = scratch("refused");
    std::fs::write(dir.join("fake-login"), "refuse").unwrap();
    let grant = with_credentials(accounts::API_KEY, &[("api_key", CANARY)])
        .state_dir(dir.to_string_lossy());
    let error = apply(&grant, &mut launch(), &files_in(&dir)).expect_err("refused");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(
        error.message().starts_with("login refused"),
        "{}",
        error.message()
    );
    assert!(!error.message().contains(CANARY));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chatgpt_home_with_no_copy_in_the_grant_is_used_as_it_is_and_nothing_is_written() {
    let dir = scratch("inplace");
    std::fs::write(dir.join("auth.json"), LOGIN_FILE).unwrap();
    let mut launch = launch();
    launch
        .env
        .insert("CODEX_ACCESS_TOKEN".into(), "host-token".into());
    let grant = AccountGrant::new("acct", accounts::CHATGPT).state_dir(dir.to_string_lossy());
    // No file served: nothing is placed, so none is asked of the spawner.
    let none = Files::new(Arc::new(LocalSpawner::new().into_object()));
    let applied = apply(&grant, &mut launch, &none).expect("applied");
    assert_eq!(
        variable(&launch, "CODEX_HOME").as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert_eq!(variable(&launch, "CODEX_ACCESS_TOKEN"), None);
    assert!(!applied.keeps_files());
    assert!(
        !dir.join("config.toml").exists(),
        "the person's own home is not given a configuration"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("auth.json")).unwrap(),
        LOGIN_FILE
    );
    assert!(!dir.join("fake-codex.log").exists(), "no program was run");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chatgpt_login_the_grant_carries_is_placed_by_hold_and_not_by_apply() {
    let dir = scratch("placed");
    let grant = with_credentials(accounts::CHATGPT, &[("auth_file", LOGIN_FILE)])
        .state_dir(dir.to_string_lossy());
    let applied = apply(&grant, &mut launch(), &files_in(&dir)).expect("applied");
    assert!(applied.keeps_files());
    assert!(!dir.join("auth.json").exists(), "apply places no login");
    assert!(dir.join("config.toml").exists());
    let hold = applied
        .hold(&Arc::new(Accounts::new()), &files_in(&dir))
        .expect("placed")
        .expect("a login is held");
    assert_eq!(
        std::fs::read_to_string(dir.join("auth.json")).unwrap(),
        LOGIN_FILE
    );
    hold.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_credential_to_keep_in_a_home_with_no_files_is_unsupported_and_a_wrong_one_is_bad_value() {
    let dir = scratch("nofiles");
    let none = Files::new(Arc::new(LocalSpawner::new().into_object()));
    let key = with_credentials(accounts::API_KEY, &[("api_key", CANARY)])
        .state_dir(dir.to_string_lossy());
    let error = apply(&key, &mut launch(), &none).expect_err("no files");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let wrong = with_credentials(accounts::API_KEY, &[("access_token", CANARY)])
        .state_dir(dir.to_string_lossy());
    let error = apply(&wrong, &mut launch(), &files_in(&dir)).expect_err("not the kind's");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(!error.message().contains(CANARY));
    let nowhere = with_credentials(accounts::API_KEY, &[("api_key", CANARY)]);
    let error = apply(&nowhere, &mut launch(), &files_in(&dir)).expect_err("no directory");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_login_changes_nothing() {
    let dir = scratch("own");
    let mut launch = launch();
    launch
        .env
        .insert("OPENAI_API_KEY".into(), "host-key".into());
    let before = launch.clone();
    let applied = apply(
        &AccountGrant::new("acct", HARNESS_LOGIN),
        &mut launch,
        &files_in(&dir),
    )
    .expect("applied");
    assert_eq!(launch, before);
    assert!(!applied.keeps_files());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_secret_variables_are_credentials_cleared_by_a_start_and_leave_out_endpoints() {
    let catalog = accounts::catalog();
    for name in &catalog.secret_variables {
        assert!(catalog.credential_variables.contains(name), "{name}");
    }
    for secret in ["CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_ACCESS_TOKEN"] {
        assert!(
            catalog.secret_variables.iter().any(|name| name == secret),
            "{secret}"
        );
    }
    for other in [
        "OPENAI_BASE_URL",
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        "CODEX_APP_SERVER_LOGIN_ISSUER",
        "CODEX_SQLITE_HOME",
    ] {
        assert!(
            catalog
                .credential_variables
                .iter()
                .any(|name| name == other),
            "{other}"
        );
        assert!(
            !catalog.secret_variables.iter().any(|name| name == other),
            "{other}"
        );
    }
}
