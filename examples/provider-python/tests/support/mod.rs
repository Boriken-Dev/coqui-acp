//! Guatiao's source of Python libraries, built for the tests from Cargo's own checkout of Guatiao at
//! the workspace's revision, against the Python build.rs found.

#![allow(dead_code, reason = "each test binary uses part of this file")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn guatiao() -> PathBuf {
    PathBuf::from(env!("COQUI_ACP_TEST_GUATIAO"))
}

pub fn python() -> &'static str {
    env!("COQUI_ACP_TEST_PYTHON_EXE")
}

pub fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory under the test's own");
    for entry in std::fs::read_dir(from).expect("a readable checkout") {
        let entry = entry.expect("a readable entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a file copies");
        }
    }
}

/// Builds the source once per test binary. A copy, since a build writes `Cargo.lock` beside the manifest
/// and Cargo's checkout of a dependency is not this test's to write in. The copy is named after the
/// checkout's directory, which Cargo names by revision, so another pin builds from a copy of its own.
pub fn source() -> &'static Path {
    static SOURCE: OnceLock<PathBuf> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let checkout = guatiao();
        let revision = checkout
            .file_name()
            .expect("Cargo's checkout is a directory of its own");
        let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("guatiao-source-py")
            .join(revision);
        let copy = scratch.join("guatiao");
        if !copy.join("Cargo.toml").is_file() {
            // Every directory the root manifest's workspace lists a member in.
            for dir in ["crates", "examples", "benchmarks"] {
                if guatiao().join(dir).is_dir() {
                    copy_tree(&guatiao().join(dir), &copy.join(dir));
                }
            }
            std::fs::copy(guatiao().join("Cargo.toml"), copy.join("Cargo.toml"))
                .expect("the root manifest copies");
        }
        let manifest = copy.join("crates/guatiao-source-py/Cargo.toml");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo)
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--target-dir")
            .arg(scratch.join("target"))
            .env("PYO3_PYTHON", python())
            .output()
            .expect("cargo runs");
        assert!(
            built.status.success(),
            "the Python source did not build:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
        scratch.join("target/debug").join(format!(
            "{}guatiao_source_py{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    })
}
