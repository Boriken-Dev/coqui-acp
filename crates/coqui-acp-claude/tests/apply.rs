//! `coqui_acp_claude::accounts::apply`: what it does to a launch and to a configuration directory, the part of the
//! catalog that is itself a credential, and that the provider's own start goes through the same function.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::{AccountGrant, Credentials, HARNESS_LOGIN, ProcessSpawner};
use coqui_acp_claude::ClaudeLaunch;
use coqui_acp_claude::accounts::{self, apply};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_harness::account::{Accounts, Files};

const CANARY: &str = "sk-canary-2c91d0e5aa";
const LOGIN_FILE: &str =
    r#"{"claudeAiOauth":{"accessToken":"a-1","refreshToken":"r-1","expiresAt":4102444800000}}"#;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-claude-apply-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn files_in(dir: &Path) -> Files {
    Files::new(Arc::new(LocalSpawner::new().files(dir).into_object()))
}

fn with_credentials(kind: &str, pairs: &[(&str, &str)]) -> AccountGrant {
    AccountGrant::new("acct", kind).credentials(pairs.iter().copied().collect::<Credentials>())
}

fn variable(launch: &ClaudeLaunch, name: &str) -> Option<String> {
    launch
        .env
        .get(&OsString::from(name))
        .map(|value| value.to_string_lossy().into_owned())
}

#[test]
fn an_environment_kind_sets_its_variable_and_clears_every_other_credential_variable() {
    let dir = scratch("env");
    let mut launch = ClaudeLaunch::new(".");
    launch
        .env
        .insert("ANTHROPIC_AUTH_TOKEN".into(), "host-token".into());
    launch
        .env
        .insert("ANTHROPIC_BASE_URL".into(), "https://host.example".into());
    launch.env.insert("PATH".into(), "/bin".into());
    let grant = with_credentials(accounts::API_KEY, &[("api_key", CANARY)]);
    let applied = apply(&grant, &mut launch, &files_in(&dir)).expect("applied");
    assert_eq!(
        variable(&launch, "ANTHROPIC_API_KEY").as_deref(),
        Some(CANARY)
    );
    assert_eq!(variable(&launch, "ANTHROPIC_AUTH_TOKEN"), None);
    assert_eq!(
        variable(&launch, "ANTHROPIC_BASE_URL"),
        None,
        "the endpoint is the grant's, not the host's"
    );
    assert_eq!(variable(&launch, "PATH").as_deref(), Some("/bin"));
    let catalog = accounts::catalog();
    for name in &catalog.credential_variables {
        assert!(
            launch
                .remove_env
                .iter()
                .any(|removed| removed == name.as_str()),
            "{name} is removed from what Claude Code starts with"
        );
    }
    assert_eq!(
        launch.setting_sources.as_deref(),
        Some(&["project".to_string(), "local".to_string()][..]),
        "the person's own user settings do not outrank the credential"
    );
    assert!(applied.dir().is_none() && !applied.keeps_files());
    assert!(!format!("{applied:?}").contains(CANARY));
    let held = applied
        .hold(&Arc::new(Accounts::new()), &files_in(&dir))
        .expect("nothing to place");
    assert!(held.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_endpoint_comes_only_from_the_grant_and_a_launch_that_names_sources_keeps_them() {
    let dir = scratch("url");
    let mut launch = ClaudeLaunch::new(".");
    launch.setting_sources = Some(vec!["user".to_string()]);
    let grant = with_credentials(accounts::AUTH_TOKEN, &[("auth_token", CANARY)]).settings(
        coqui_acp_harness::provider::schema(
            &serde_json::json!({"base_url": "https://gateway.example/v1"}),
        ),
    );
    let _applied = apply(&grant, &mut launch, &files_in(&dir)).expect("applied");
    assert_eq!(
        variable(&launch, "ANTHROPIC_BASE_URL").as_deref(),
        Some("https://gateway.example/v1")
    );
    assert_eq!(
        variable(&launch, "ANTHROPIC_AUTH_TOKEN").as_deref(),
        Some(CANARY)
    );
    assert_eq!(launch.setting_sources, Some(vec!["user".to_string()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_login_directory_with_no_copy_in_the_grant_is_used_as_it_is_and_nothing_is_written() {
    let dir = scratch("inplace");
    std::fs::write(dir.join(".credentials.json"), LOGIN_FILE).unwrap();
    let before = std::fs::read(dir.join(".credentials.json")).unwrap();
    let mut launch = ClaudeLaunch::new(".");
    launch
        .env
        .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), "host-token".into());
    let grant = AccountGrant::new("acct", accounts::LOGIN).state_dir(dir.to_string_lossy());
    let applied = apply(&grant, &mut launch, &files_in(&dir)).expect("applied");
    assert_eq!(
        variable(&launch, "CLAUDE_CONFIG_DIR").as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert_eq!(variable(&launch, "CLAUDE_CODE_OAUTH_TOKEN"), None);
    assert!(
        launch.setting_sources.is_none(),
        "a directory with a login of its own keeps the person's settings"
    );
    assert_eq!(applied.dir(), Some(dir.to_string_lossy().as_ref()));
    assert!(!applied.keeps_files());
    let held = applied
        .hold(&Arc::new(Accounts::new()), &files_in(&dir))
        .expect("nothing to place");
    assert!(held.is_none());
    assert_eq!(
        std::fs::read(dir.join(".credentials.json")).unwrap(),
        before
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_login_the_grant_carries_is_placed_by_hold_and_not_by_apply() {
    let dir = scratch("placed");
    let mut launch = ClaudeLaunch::new(".");
    let grant = with_credentials(accounts::LOGIN, &[("credentials_file", LOGIN_FILE)])
        .state_dir(dir.to_string_lossy());
    let applied = apply(&grant, &mut launch, &files_in(&dir)).expect("applied");
    assert!(applied.keeps_files());
    assert!(
        !dir.join(".credentials.json").exists(),
        "apply writes no file"
    );
    assert_eq!(
        variable(&launch, "CLAUDE_CONFIG_DIR").as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    let hold = applied
        .hold(&Arc::new(Accounts::new()), &files_in(&dir))
        .expect("placed")
        .expect("a login is held");
    assert_eq!(
        std::fs::read_to_string(dir.join(".credentials.json")).unwrap(),
        LOGIN_FILE
    );
    hold.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_login_to_place_on_a_spawner_with_no_files_is_unsupported_and_a_kind_with_none_is_bad_value() {
    let dir = scratch("nofiles");
    let none = Files::new(Arc::new(LocalSpawner::new().into_object()));
    let grant = with_credentials(accounts::LOGIN, &[("credentials_file", LOGIN_FILE)])
        .state_dir(dir.to_string_lossy());
    let error = apply(&grant, &mut ClaudeLaunch::new("."), &none).expect_err("no files");
    assert_eq!(error.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let wrong = with_credentials(accounts::API_KEY, &[("token", CANARY)]);
    let error = apply(&wrong, &mut ClaudeLaunch::new("."), &none).expect_err("not the kind's");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    assert!(!error.message().contains(CANARY));
    let nowhere = AccountGrant::new("acct", accounts::LOGIN);
    let error = apply(&nowhere, &mut ClaudeLaunch::new("."), &none).expect_err("no directory");
    assert_eq!(error.status, Status::GUATIAO_ERR_BAD_VALUE);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_login_changes_nothing() {
    let dir = scratch("own");
    let mut launch = ClaudeLaunch::new(".");
    launch
        .env
        .insert("ANTHROPIC_API_KEY".into(), "host-key".into());
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
