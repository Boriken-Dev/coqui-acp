//! `acp-harness-check`, the contract as a program, run on the Rust echo's library file as a provider
//! in another repository or language would run it.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::process::Command;

/// The Rust echo's library file, which `cargo test` builds beside the tests.
fn library() -> std::path::PathBuf {
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

fn check(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_acp-harness-check"))
        .arg("--library")
        .arg(library())
        .args(args)
        .output()
        .expect("the checker runs")
}

#[test]
fn the_checker_passes_a_provider_that_keeps_every_rule() {
    let out = check(&["--message-limit", "1048576"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_checker_names_each_rule_a_provider_breaks() {
    // The echo refuses `answer_only`, so a contract saying it holds it fails, and says where.
    let out = check(&["--holds", "answer_only"]);
    assert_eq!(out.status.code(), Some(1));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("requirements only narrow"), "{said}");
}

#[test]
fn the_checker_builds_an_instance_from_settings() {
    let out = check(&["--settings", r#"{"title": "Configured echo"}"#]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let quiet = check(&["--settings", r#"{"imports": false}"#, "--no-imports"]);
    assert!(
        quiet.status.success(),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let refused = check(&["--settings", r#"{"titel": "misspelled"}"#]);
    assert_eq!(refused.status.code(), Some(2));
}

#[test]
fn the_checker_runs_the_scripted_usage_half_on_a_provider_configured_for_it() {
    let out = check(&[
        "--settings",
        r#"{"usage": "scripted"}"#,
        "--message-limit",
        "1048576",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let refused = check(&["--settings", r#"{"usage": "sometimes"}"#]);
    assert_eq!(refused.status.code(), Some(2));
}
