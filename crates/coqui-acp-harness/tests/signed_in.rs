//! Whether each stdio harness's own login exists, asked of its account provider with fixture homes and fixture environments and
//! never the person's own: Grok, OpenCode and Antigravity (with a stand-in keyring), and the answer shows no value.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};

use coqui_acp_api::{AccountProvider, Environment, SignedInCheck, SignedInQuery, SignedInState};
use coqui_acp_harness::stdio::{StdioAccountSettings, StdioAccounts};
use coqui_acp_harness::vendors::{grok, opencode};

const CANARY: &str = "sk-canary-signed-in-5d2e";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coqui-acp-harness-signed-in-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// An environment that places the home in `home` and empties every variable in `variables` (an empty value is no login).
fn environment(home: &Path, variables: &[&str], extra: &[(&str, &str)]) -> Environment {
    let mut environment = Environment::new();
    for name in variables {
        environment.insert(*name, "");
    }
    for name in ["HOME", "USERPROFILE"] {
        environment.insert(name, home.to_string_lossy().into_owned());
    }
    for (name, value) in extra {
        environment.insert(*name, *value);
    }
    environment
}

fn ask(
    def: &'static coqui_acp_harness::stdio::StdioDef,
    environment: Environment,
) -> SignedInCheck {
    StdioAccounts::new(def, StdioAccountSettings::new())
        .signed_in(SignedInQuery::new().environment(environment))
        .expect("it answers")
}

fn grok_variables() -> &'static [&'static str] {
    &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY", "GROK_HOME"]
}

#[test]
fn grok_is_signed_in_by_an_auth_file_in_its_home_and_signed_out_without_one() {
    let home = scratch("grok");
    let grok_home = home.join("grok-home");
    std::fs::create_dir_all(&grok_home).unwrap();
    let at = |extra: &[(&str, &str)]| {
        let mut all = vec![("GROK_HOME", grok_home.to_string_lossy().into_owned())];
        all.extend(extra.iter().map(|(a, b)| (*a, (*b).to_owned())));
        let pairs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (*a, b.as_str())).collect();
        ask(
            &grok::STDIO,
            environment(&home, &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"], &pairs),
        )
    };
    let missing = at(&[]);
    assert_eq!(missing.state, SignedInState::SignedOut);
    let reason = missing.reason.unwrap();
    assert!(
        reason.contains("auth.json") && reason.contains("XAI_API_KEY"),
        "{reason}"
    );
    assert!(!reason.contains(&*home.to_string_lossy()), "{reason}");
    std::fs::write(grok_home.join("auth.json"), "{}").unwrap();
    assert_eq!(at(&[]).state, SignedInState::SignedOut, "an empty object");
    std::fs::write(
        grok_home.join("auth.json"),
        format!("{{\"token\":\"{CANARY}\"}}"),
    )
    .unwrap();
    let found = at(&[]);
    assert_eq!(found, SignedInCheck::signed_in());
    assert!(!format!("{found:?}").contains(CANARY));
    // A key variable alone is a login, named and never shown.
    std::fs::remove_file(grok_home.join("auth.json")).unwrap();
    let by_key = at(&[("XAI_API_KEY", CANARY)]);
    assert_eq!(by_key, SignedInCheck::by_variable("XAI_API_KEY"));
    assert!(!format!("{by_key:?}").contains(CANARY));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn grok_with_no_home_named_is_unknown_when_its_default_folder_has_nothing() {
    let home = scratch("grok-default");
    // The default folder is assumed and not measured, so a file missing there is not a signed-out answer.
    let missing = ask(&grok::STDIO, environment(&home, grok_variables(), &[]));
    assert_eq!(missing.state, SignedInState::Unknown, "{missing:?}");
    assert!(!missing.reason.unwrap().contains(&*home.to_string_lossy()));
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    std::fs::write(home.join(".grok").join("auth.json"), "{\"a\":1}").unwrap();
    assert_eq!(
        ask(&grok::STDIO, environment(&home, grok_variables(), &[])),
        SignedInCheck::signed_in()
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn opencode_is_signed_in_by_its_auth_file_a_provider_key_or_inline_auth() {
    let home = scratch("opencode");
    let data = home.join("data");
    std::fs::create_dir_all(data.join("opencode")).unwrap();
    let mut variables = opencode::accounts::KEY_VARIABLES.to_vec();
    variables.push("OPENCODE_AUTH_CONTENT");
    let at = |extra: &[(&str, &str)]| {
        let place = data.to_string_lossy().into_owned();
        let mut pairs = vec![("XDG_DATA_HOME", place.as_str())];
        pairs.extend_from_slice(extra);
        ask(&opencode::STDIO, environment(&home, &variables, &pairs))
    };
    let missing = at(&[]);
    assert_eq!(missing.state, SignedInState::SignedOut);
    assert!(!missing.reason.unwrap().contains(&*home.to_string_lossy()));
    std::fs::write(data.join("opencode").join("auth.json"), "{}").unwrap();
    assert_eq!(at(&[]).state, SignedInState::SignedOut);
    std::fs::write(
        data.join("opencode").join("auth.json"),
        format!("{{\"anthropic\":{{\"type\":\"api\",\"key\":\"{CANARY}\"}}}}"),
    )
    .unwrap();
    let found = at(&[]);
    assert_eq!(found, SignedInCheck::signed_in());
    assert!(!format!("{found:?}").contains(CANARY));
    std::fs::remove_file(data.join("opencode").join("auth.json")).unwrap();
    for name in ["ANTHROPIC_API_KEY", "OPENCODE_AUTH_CONTENT"] {
        let answer = at(&[(name, CANARY)]);
        assert_eq!(answer, SignedInCheck::by_variable(name), "{name}");
        assert!(!format!("{answer:?}").contains(CANARY));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn opencode_without_xdg_data_home_looks_below_the_home() {
    let home = scratch("opencode-default");
    let mut variables = opencode::accounts::KEY_VARIABLES.to_vec();
    variables.extend(["OPENCODE_AUTH_CONTENT", "XDG_DATA_HOME"]);
    assert_eq!(
        ask(&opencode::STDIO, environment(&home, &variables, &[])).state,
        SignedInState::SignedOut
    );
    let folder = home.join(".local").join("share").join("opencode");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("auth.json"), "{\"x\":1}").unwrap();
    assert_eq!(
        ask(&opencode::STDIO, environment(&home, &variables, &[])),
        SignedInCheck::signed_in()
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_wait_is_a_setting_of_the_provider_and_the_answer_does_not_depend_on_a_file_read() {
    let settings = StdioAccountSettings {
        signed_in_timeout_ms: 0,
        ..StdioAccountSettings::new()
    };
    let home = scratch("wait");
    let answer = StdioAccounts::new(&grok::STDIO, settings)
        .signed_in(SignedInQuery::new().environment(environment(
            &home,
            &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"],
            &[("GROK_HOME", &home.to_string_lossy())],
        )))
        .expect("it answers");
    assert_eq!(answer.state, SignedInState::SignedOut);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(feature = "antigravity")]
mod antigravity {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use coqui_acp_harness::antigravity::Keyring;
    use coqui_acp_harness::vendors::antigravity::signed_in_with;

    use super::*;

    const LOGIN: &str = r#"{"token":{"access_token":"agy-canary-access","refresh_token":"agy-canary-refresh","expiry":"2030-01-01T00:00:00Z"},"auth_method":"consumer"}"#;

    fn files(home: &Path, text: &str) {
        let dir = home.join(".gemini");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("jetski-standalone-oauth-token"), text).unwrap();
    }

    fn with(home: &Path, keyring: Keyring) -> SignedInCheck {
        signed_in_with(
            &environment(home, &[], &[]),
            Duration::from_millis(200),
            keyring,
        )
    }

    fn stored(bytes: &'static [u8]) -> Keyring {
        Keyring::Given(Arc::new(move |_: &str, _: &str| Ok(Some(bytes.to_vec()))))
    }

    fn empty() -> Keyring {
        Keyring::Given(Arc::new(|_: &str, _: &str| Ok(None)))
    }

    #[test]
    fn a_whole_login_in_the_keyring_or_in_a_file_is_signed_in_and_neither_is_shown() {
        let home = scratch("agy-found");
        let by_keyring = with(&home, stored(LOGIN.as_bytes()));
        assert_eq!(by_keyring, SignedInCheck::signed_in());
        files(&home, LOGIN);
        let by_file = with(&home, empty());
        assert_eq!(by_file, SignedInCheck::signed_in());
        for shown in [format!("{by_keyring:?}"), format!("{by_file:?}")] {
            assert!(!shown.contains("agy-canary"), "{shown}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn nothing_stored_or_a_login_with_no_refresh_token_is_signed_out() {
        let home = scratch("agy-none");
        let none = with(&home, empty());
        assert_eq!(none.state, SignedInState::SignedOut);
        assert!(!none.reason.unwrap().contains(&*home.to_string_lossy()));
        files(&home, r#"{"token":{"access_token":"a"}}"#);
        assert_eq!(with(&home, empty()).state, SignedInState::SignedOut);
        assert_eq!(
            with(&home, Keyring::Off).state,
            SignedInState::SignedOut,
            "a keyring that is not looked at"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_keyring_that_does_not_answer_or_fails_is_unknown_unless_a_file_has_the_login() {
        let home = scratch("agy-slow");
        let timed_out = Keyring::Given(Arc::new(|_: &str, _: &str| {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        }));
        let slow = with(&home, timed_out);
        assert_eq!(slow.state, SignedInState::Unknown);
        assert!(slow.reason.unwrap().contains("did not answer in time"));
        let denied = Keyring::Given(Arc::new(|_: &str, _: &str| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }));
        assert_eq!(with(&home, denied).state, SignedInState::Unknown);
        files(&home, LOGIN);
        let failing = Keyring::Given(Arc::new(|_: &str, _: &str| {
            Err(io::Error::from(io::ErrorKind::Other))
        }));
        assert_eq!(with(&home, failing), SignedInCheck::signed_in());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_keyring_is_asked_for_gemini_antigravity() {
        let home = scratch("agy-asked");
        let asked = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&asked);
        let keyring = Keyring::Given(Arc::new(move |service: &str, user: &str| {
            assert_eq!((service, user), ("gemini", "antigravity"));
            seen.store(true, Ordering::SeqCst);
            Ok(None)
        }));
        let answer = with(&home, keyring);
        assert!(asked.load(Ordering::SeqCst));
        assert_eq!(answer.state, SignedInState::SignedOut);
        let _ = std::fs::remove_dir_all(&home);
    }
}
