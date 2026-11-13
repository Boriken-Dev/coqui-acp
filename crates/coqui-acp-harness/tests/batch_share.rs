//! A `.cmd` file started in a folder on a share runs in that folder: cmd.exe would fall back to the Windows folder, so the
//! program an npm-written file names is started instead.
#![cfg(windows)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::io::Read;
use std::path::{Path, PathBuf};

use coqui_acp_harness::{Launch, PlainSpawn, Spawn};

/// This test program, run as `--ignored --exact prints_its_folder`, says where it was started.
#[test]
#[ignore = "run by the tests below as a child"]
fn prints_its_folder() {
    println!("FOLDER={}", std::env::current_dir().unwrap().display());
}

fn on_share(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let (drive, rest) = text.split_at(2);
    let shared = PathBuf::from(format!(r"\\localhost\{}${rest}", &drive[..1]));
    assert!(shared.is_dir(), "{} does not answer", shared.display());
    shared
}

fn output_of(launch: &Launch) -> std::io::Result<String> {
    let spawned = PlainSpawn.spawn(launch)?;
    let mut text = String::new();
    let mut stdout = spawned.stdout;
    stdout.read_to_string(&mut text).unwrap();
    Ok(text)
}

#[test]
fn a_command_file_runs_in_the_share_folder_it_was_asked_to() {
    let base = std::env::temp_dir().join(format!("coqui-acp-batch-{}", std::process::id()));
    let bin = base.join("bin");
    let work = base.join("work");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), bin.join("node.exe")).unwrap();
    // What npm writes for a package's command, with its own node beside it.
    let shim = concat!(
        "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\n",
        "IF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\" %*\r\n"
    );
    std::fs::write(bin.join("tool.cmd"), shim).unwrap();
    let share = on_share(&work);
    let launch = Launch::new(bin.join("tool.cmd"))
        .args(["--ignored", "--exact", "prints_its_folder", "--nocapture"])
        .cwd(&share);
    let said = output_of(&launch).unwrap();
    let folder = said
        .lines()
        .find_map(|line| line.strip_prefix("FOLDER="))
        .unwrap_or_else(|| panic!("no folder in {said:?}"));
    assert_eq!(
        folder.to_lowercase(),
        share.to_string_lossy().to_lowercase(),
        "it ran in the share folder, not the Windows folder"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn another_batch_file_is_refused_in_a_share_folder_with_its_reason() {
    let base = std::env::temp_dir().join(format!("coqui-acp-batch-other-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("other.cmd"), "@echo off\r\ncall x.cmd %*\r\n").unwrap();
    let error = PlainSpawn
        .spawn(&Launch::new(base.join("other.cmd")).cwd(on_share(&base)))
        .expect_err("refused");
    assert!(error.to_string().contains("batch file"), "{error}");
    // In a local folder the same file starts as it always did.
    assert!(
        PlainSpawn
            .spawn(&Launch::new(base.join("other.cmd")).cwd(&base))
            .is_ok()
    );
    let _ = std::fs::remove_dir_all(&base);
}
