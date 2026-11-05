//! What the recordings ran with is what this crate's launch builds: a change to either, or to a flag's form, fails here
//! until the recordings are made again with `tools/record_harnesses/record.py`, against the real Codex.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_codex::CodexLaunch;
use serde_json::json;
use support::fixtures;

fn launch_of(case: &str) -> CodexLaunch {
    let provider = if case == "errors" {
        json!({"base_url": "http://127.0.0.1:{PORT}/v1", "env_key": "STANDIN_API_KEY", "name": "standin",
            "request_max_retries": 0, "stream_max_retries": 0, "wire_api": "responses"})
    } else {
        json!({"base_url": "http://127.0.0.1:{PORT}/v1", "env_key": "STANDIN_API_KEY", "name": "standin",
            "wire_api": "responses"})
    };
    let mut launch = CodexLaunch::new("{ROOT}/w");
    launch.config_overrides = vec![
        ("model_provider".into(), json!("standin")),
        ("model_providers.standin".into(), provider),
    ];
    launch
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
    assert_eq!(cases, 6, "the recordings read");
}

/// Everything the crate says it was tested with has its recordings here.
#[test]
fn the_recordings_are_of_a_tested_version() {
    let directory = fixtures();
    let name = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let version = name.strip_prefix("codex-").unwrap();
    assert!(
        coqui_acp_codex::TESTED_VERSIONS.contains(&version),
        "{version}"
    );
}
