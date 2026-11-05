//! What the recordings ran with is what this crate's launch builds: a change to either, or to a flag's form, fails here
//! until the recordings are made again with `tools/record_harnesses/record.py`, against the real Claude Code.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::PathBuf;

use coqui_acp_claude::{ClaudeLaunch, SessionStart};
use support::fixtures;

const ROOT: &str = "{ROOT}";

fn at(path: &str) -> PathBuf {
    PathBuf::from(format!("{ROOT}/{path}"))
}

/// A launch as most recordings use: the small model, nobody's settings, and only the servers given.
fn base(id: &str) -> ClaudeLaunch {
    let mut launch = ClaudeLaunch::new(at("w"));
    launch.session = SessionStart::New(format!("6f2c1b0e-0000-4000-8000-{id}"));
    launch.model = Some("claude-haiku-5-5".into());
    launch.setting_sources = Some(Vec::new());
    launch.strict_mcp_config = true;
    launch
}

fn launch_of(case: &str) -> ClaudeLaunch {
    match case {
        "turn" => {
            let mut launch = base("00000000c0de");
            launch.effort = Some("low".into());
            launch.append_system_prompt = Some("Answer briefly.".into());
            launch.add_dirs = vec![at("extra")];
            launch
        }
        "interrupt" => base("00000000057a"),
        "errors" => base("0000000e5505"),
        "compact" => base("0000000c0a9c"),
        "tasks" => base("0000000007a5"),
        "parts" => {
            let mut launch = base("00000000fa57");
            launch.mcp_config_file = Some(at("mcp.json"));
            launch.plugin_dirs = vec![at("canoa-profile")];
            launch
        }
        "resume" => {
            let mut launch = base("00000000a5e5");
            launch.session = SessionStart::Resume("6f2c1b0e-0000-4000-8000-00000000a5e5".into());
            launch
        }
        "probe_host" => {
            let mut launch = base("000000000001");
            launch.permission_prompts = false;
            launch.extra_args = vec!["--permission-prompts=host".into()];
            launch
        }
        "probe_tool" => {
            let mut launch = base("000000000002");
            launch.permission_prompts = false;
            launch.extra_args = vec!["--permission-prompt-tool=stdio".into()];
            launch
        }
        "dont_ask" => {
            let mut launch = base("000000000003");
            launch.permission_mode = Some("dontAsk".into());
            launch
        }
        "no_prompts" => {
            let mut launch = base("000000000004");
            launch.permission_prompts = false;
            launch
        }
        "rate_limit" => base("000000000005"),
        "refused_control" => base("000000000006"),
        "cli_session" => {
            let mut launch = base("000000000000");
            launch.session = SessionStart::Chosen;
            launch
        }
        other => panic!("no launch for the recording {other}: add one here"),
    }
}

#[test]
fn every_recording_ran_with_what_the_launch_builds() {
    let mut cases = 0;
    for file in std::fs::read_dir(fixtures()).unwrap() {
        let path = file.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(case) = name.strip_suffix(".argv.json") else {
            continue;
        };
        let recorded: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let built: Vec<String> = launch_of(case)
            .launch()
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(built, recorded, "the recording {case}");
        cases += 1;
    }
    assert_eq!(cases, 14, "the recordings read");
}

/// Everything the crate says it was tested with has its recordings here.
#[test]
fn the_recordings_are_of_a_tested_version() {
    let directory = fixtures();
    let version = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        coqui_acp_claude::TESTED_VERSIONS.contains(&version.as_str()),
        "{version}"
    );
}

/// Every recording can be made again from a case in the repository.
#[test]
fn every_recording_has_its_case() {
    let cases = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/record_harnesses/cases/claude");
    for file in std::fs::read_dir(fixtures()).unwrap() {
        let name = file.unwrap().file_name().to_string_lossy().into_owned();
        let Some(case) = name.strip_suffix(".argv.json") else {
            continue;
        };
        assert!(
            cases.join(format!("{case}.json")).is_file(),
            "the recording {case} has no case in {}",
            cases.display()
        );
    }
}
