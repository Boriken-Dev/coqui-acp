//! On Windows npm's `pi.cmd` runs a bare `node`, and cmd.exe looks for it in the project first: a program a project plants
//! must not run when a session opens.
#![cfg(windows)]
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

mod support;

use std::path::Path;

use coqui_acp_pi::{Gate, PiSession, PiSettings, PlainSpawn};
use support::{Events, Scratch, launching, npm_install, path_with};

#[test]
fn a_node_planted_in_the_project_is_never_run_when_a_session_opens() {
    let scratch = Scratch::new("planted");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    // No `node.exe` beside the shim, as npm leaves it when node is installed elsewhere.
    let (npm, bin) = npm_install(&scratch);
    for name in ["node.bat", "node.cmd"] {
        std::fs::write(cwd.join(name), "@echo planted > planted.txt\r\n").unwrap();
    }
    let mut launch = launching(&cwd, &home);
    launch.executable = "pi".into();
    // The weak setting: cmd.exe may look in the project, and the project is on PATH as well.
    launch
        .remove_env
        .push("NoDefaultCurrentDirectoryInExePath".into());
    launch.env.insert(
        "PATH".into(),
        path_with(&[&npm, Path::new("."), &cwd, &bin]),
    );
    let (sink, _events) = Events::new();
    let _ = PiSession::start(&launch, &PlainSpawn, PiSettings::new(), sink);
    assert!(
        !cwd.join("planted.txt").exists(),
        "a program the project planted ran"
    );
}

/// Pi looks for a shell with `where bash.exe`, and `where.exe` searches the working directory first whatever
/// NoDefaultCurrentDirectoryInExePath says: a `bash.exe` of the project would be the shell, and run every command and every
/// `!` key lookup. With the guard loaded `where` runs in the gate's directory, on a PATH without the project.
#[test]
fn where_does_not_find_a_shell_in_the_project_once_the_guard_is_loaded() {
    let Some(node_dir) = std::env::var_os("COQUI_ACP_PI_REAL_PATH") else {
        eprintln!("skipped: no Node named (COQUI_ACP_PI_REAL_PATH)");
        return;
    };
    let scratch = Scratch::new("where");
    let (project, elsewhere, gates) = (
        scratch.dir("project"),
        scratch.dir("git-elsewhere"),
        scratch.dir("gates"),
    );
    // Any file will do: `where` lists names and runs nothing.
    for dir in [&project, &elsewhere] {
        std::fs::write(dir.join("bash.exe"), "not a program").unwrap();
    }
    let gate = Gate::write(Some(&gates), None).expect("written");
    let probe = scratch.0.join("probe.cjs");
    std::fs::write(
        &probe,
        "const r = require('child_process').spawnSync('where', ['bash.exe'], { encoding: 'utf8' });\n\
         console.log(r.stdout);\n",
    )
    .unwrap();
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let path = std::env::join_paths([
        elsewhere.clone(),
        Path::new(&system).join("System32"),
        node_dir.clone().into(),
    ])
    .unwrap();
    let run = |guarded: bool| {
        let mut node = std::process::Command::new(Path::new(&node_dir).join("node.exe"));
        if guarded {
            node.arg("--require").arg(gate.guard());
        }
        let out = node
            .arg(&probe)
            .current_dir(&project)
            .env("PATH", &path)
            .env(
                "COQUI_ACP_PI_KEEP_OUT",
                serde_json::json!([project.to_string_lossy()]).to_string(),
            )
            .output()
            .expect("node runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| line.trim().to_lowercase())
            .filter(|line| !line.is_empty())
            .collect()
    };
    let lower = |dir: &Path| dir.join("bash.exe").to_string_lossy().to_lowercase();
    let without = lines(&run(false));
    assert_eq!(
        without.first(),
        Some(&lower(&project)),
        "where.exe no longer starts in the working directory: {without:?}"
    );
    let with = lines(&run(true));
    assert_eq!(
        with,
        [lower(&elsewhere)],
        "the project's bash.exe was found"
    );
}

/// `cmd.exe /c` is given a path under the project in every way cmd.exe reads one: quoted, after `call`, unquoted, with a
/// caret in the name, with forward slashes. With the guard loaded none of them runs.
#[test]
fn cmd_is_not_given_a_command_that_names_a_file_of_the_project() {
    let Some(node_dir) = std::env::var_os("COQUI_ACP_PI_REAL_PATH") else {
        eprintln!("skipped: no Node named (COQUI_ACP_PI_REAL_PATH)");
        return;
    };
    let scratch = Scratch::new("cmd-line");
    let (project, gates) = (scratch.dir("project"), scratch.dir("gates"));
    let marker = scratch.0.join("RAN");
    std::fs::write(
        project.join("x.cmd"),
        format!("@echo ran> \"{}\"\r\n", marker.display()),
    )
    .unwrap();
    let gate = Gate::write(Some(&gates), None).expect("written");
    let probe = scratch.0.join("probe.cjs");
    std::fs::write(
        &probe,
        "require('child_process').spawnSync('cmd.exe', ['/d', '/s', '/c', process.argv[2]], \
         { windowsVerbatimArguments: true, cwd: process.argv[3] });\n",
    )
    .unwrap();
    let dir = project.to_string_lossy().into_owned();
    let lines = [
        format!("\"{dir}\\x.cmd\" a"),
        format!("call \"{dir}\\x.cmd\""),
        format!("{dir}\\x.cmd a"),
        format!("{dir}\\x^.cmd"),
        format!("\"{}\"", format!("{dir}\\x.cmd").replace('\\', "/")),
        format!("c^a^l^l {dir}\\x.cmd"),
    ];
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let path = std::env::join_paths([
        Path::new(&system).join("System32"),
        Path::new(&node_dir).to_path_buf(),
    ])
    .unwrap();
    let run = |line: &str, guarded: bool| {
        let _ = std::fs::remove_file(&marker);
        let mut node = std::process::Command::new(Path::new(&node_dir).join("node.exe"));
        if guarded {
            node.arg("--require").arg(gate.guard());
        }
        node.arg(&probe)
            .arg(line)
            .arg(&project)
            .current_dir(&scratch.0)
            .env("PATH", &path)
            .env(
                "COQUI_ACP_PI_KEEP_OUT",
                serde_json::json!([dir]).to_string(),
            )
            .output()
            .expect("node runs");
        marker.exists()
    };
    assert!(
        run(&lines[0], false),
        "cmd.exe no longer runs the batch file the control starts"
    );
    for line in &lines {
        assert!(
            !run(line, true),
            "cmd.exe ran the project's file for {line}"
        );
    }
}
