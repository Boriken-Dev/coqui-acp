//! What the tests share: the library's commands behind a command line, built beside the tests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// The `bridge` example, `target/debug/examples/bridge`, built once for the run.
pub fn bridge() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let status = Command::new(env!("CARGO"))
                .args(["build", "-p", "coqui-acp-bridge", "--example", "bridge"])
                .status()
                .unwrap();
            assert!(status.success());
            let mut path = std::env::current_exe().unwrap();
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path.join("examples")
                .join(format!("bridge{}", std::env::consts::EXE_SUFFIX))
        })
        .clone()
}
