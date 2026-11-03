//! Finds a Python the test can embed, and says so loudly when there is none: the test is then ignored, which
//! a run reports, instead of passing without having run.
//!
//! `COQUI_ACP_TEST_PYTHON` names the interpreter; otherwise `python3`, `python` and `py -3` are tried.

use std::path::PathBuf;
use std::process::Command;

/// Prints the interpreter, its version and where a host finds its shared library. Windows needs
/// `python3.dll`, for the stable ABI; elsewhere a host links the library the build found.
const PROBE: &str = r#"
import os, sys, sysconfig
if os.name == "nt":
    lib = os.path.join(sys.base_prefix, "python3.dll")
else:
    lib = os.path.join(sysconfig.get_config_var("LIBDIR") or "", sysconfig.get_config_var("LDLIBRARY") or "")
print(sys.executable)
print(sys.base_prefix)
print(lib)
print(sys.version_info[0] * 100 + sys.version_info[1])
"#;

struct Python {
    executable: String,
    prefix: String,
    library: PathBuf,
    version: u32,
}

fn probe(program: &str, args: &[&str]) -> Option<Python> {
    let output = Command::new(program)
        .args(args)
        .args(["-I", "-c", PROBE])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    Some(Python {
        executable: lines.next()?.to_owned(),
        prefix: lines.next()?.to_owned(),
        library: PathBuf::from(lines.next()?),
        version: lines.next()?.parse().ok()?,
    })
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=COQUI_ACP_TEST_PYTHON");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rustc-check-cfg=cfg(python_ready)");
    // Published by guatiao's build script for its dependents: `<checkout>/crates/guatiao/include`.
    let include = std::env::var("DEP_GUATIAO_INCLUDE").expect("guatiao says where its header is");
    let checkout = PathBuf::from(include)
        .ancestors()
        .nth(3)
        .expect("the header is in <checkout>/crates/guatiao/include")
        .to_path_buf();
    println!(
        "cargo:rustc-env=COQUI_ACP_TEST_GUATIAO={}",
        checkout.display()
    );

    let named = std::env::var("COQUI_ACP_TEST_PYTHON").ok();
    let found = match &named {
        Some(program) => probe(program, &[]),
        None => probe("python3", &[])
            .or_else(|| probe("python", &[]))
            .or_else(|| probe("py", &["-3"])),
    };
    let why_not = match &found {
        None => Some("no Python was found; set COQUI_ACP_TEST_PYTHON to one".to_owned()),
        Some(python) if python.version < 309 => Some(format!(
            "the Python at {} is older than 3.9",
            python.executable
        )),
        Some(python) if !python.library.is_file() => Some(format!(
            "the Python at {} has no shared library at {}, which embedding needs; \
             install its development package, or set COQUI_ACP_TEST_PYTHON to another",
            python.executable,
            python.library.display()
        )),
        Some(_) => None,
    };
    if let Some(why) = why_not {
        println!("cargo:warning=NOT RUN: the Python provider's tests are ignored: {why}");
        return;
    }
    let python = found.expect("checked above");
    println!("cargo:rustc-cfg=python_ready");
    println!(
        "cargo:rustc-env=COQUI_ACP_TEST_PYTHON_EXE={}",
        python.executable
    );
    println!(
        "cargo:rustc-env=COQUI_ACP_TEST_PYTHON_PREFIX={}",
        python.prefix
    );
}
