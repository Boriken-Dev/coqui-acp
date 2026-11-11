//! How Pi is started and what is checked before a session is trusted: the gate must have loaded, a mode the host did not
//! allow is refused, the gate's directory is the person's alone, and on Windows no program is found in the project.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::Event;
use coqui_acp_pi::{Error, GATE_SOURCE, Gate, PiLaunch, PiSession, PiSettings, PlainSpawn};
use support::{Events, Scratch, launching, logged};

fn start(launch: &PiLaunch) -> Result<(PiSession, Events), Error> {
    let (sink, events) = Events::new();
    PiSession::start(launch, &PlainSpawn, PiSettings::new(), sink).map(|session| (session, events))
}

#[test]
fn a_pi_that_did_not_load_the_gate_is_refused_before_it_runs_anything() {
    let scratch = Scratch::new("no-gate");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let mut launch = launching(&cwd, &home);
    launch
        .env
        .insert("COQUI_ACP_FAKE_PI_NO_GATE".into(), "1".into());
    let Err(error) = start(&launch) else {
        panic!("a session without the gate was taken");
    };
    assert!(error.to_string().contains("permission gate"), "{error}");
}

#[test]
fn the_gate_names_a_command_of_its_own_per_launch_and_it_is_not_offered_to_the_person() {
    let scratch = Scratch::new("nonce");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let (_first, mut events) = start(&launching(&cwd, &home)).expect("it starts");
    let (_second, _) = start(&launching(&cwd, &home)).expect("it starts");
    events.drain();
    let commands: Vec<String> = events
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::Commands(commands) => Some(commands.iter().map(|c| c.name.clone())),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(commands, ["fake"], "the gate's command is not the person's");
    let gates: Vec<String> = logged(&home)
        .iter()
        .filter_map(|start| start["gate"].as_str().map(str::to_string))
        .collect();
    assert_eq!(gates.len(), 2);
    assert_ne!(gates[0], gates[1], "each launch has a nonce of its own");
    assert!(
        !gates[0].contains("__NONCE__"),
        "the placeholder was filled"
    );
    assert!(GATE_SOURCE.contains("__NONCE__"));
}

#[test]
fn bypass_is_refused_to_a_launch_that_does_not_allow_it() {
    let scratch = Scratch::new("bypass");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let mut launch = launching(&cwd, &home);
    launch.mode = Some(Mode::Bypass);
    assert!(matches!(start(&launch), Err(Error::Refused(_))));
    launch.allow_bypass = true;
    assert!(start(&launch).is_ok());
}

#[test]
fn pi_is_told_to_ignore_the_project_and_the_gate_directory_is_the_persons_alone() {
    let scratch = Scratch::new("hardening");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let launch = launching(&cwd, &home);
    let (_session, _) = start(&launch).expect("it starts");
    let args = logged(&home)[0]["args"].clone();
    assert!(
        args.as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--no-approve"),
        "{args}"
    );
    // The gate directory is made exclusively and closed to everyone else.
    let gate = Gate::write(Some(&scratch.dir("gates")), None).expect("written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(gate.dir()).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    #[cfg(windows)]
    {
        let shown = std::process::Command::new("icacls")
            .arg(gate.dir())
            .output()
            .expect("icacls");
        let dir = gate.dir().to_string_lossy().to_lowercase();
        let shown = String::from_utf8_lossy(&shown.stdout)
            .to_lowercase()
            .replace(&dir, "");
        assert!(
            !shown.contains("administrators")
                && !shown.contains("system")
                && !shown.contains("users"),
            "{shown}"
        );
    }
}

/// npm installs Pi on Windows as `pi.cmd`, which runs a bare `node`; cmd.exe would look for it in the project first. The
/// launch never goes through cmd.exe: it starts an absolute `node.exe` with the shim's script.
#[cfg(windows)]
mod windows {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_node_planted_in_the_project_is_never_run_and_the_shim_runs_through_an_absolute_node() {
        let scratch = Scratch::new("planted");
        let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
        let (npm, bin) = support::npm_install(&scratch);
        for name in ["node.bat", "node.cmd"] {
            std::fs::write(cwd.join(name), "@echo planted > planted.txt\r\n").unwrap();
        }
        let mut launch = launching(&cwd, &home);
        launch.executable = "pi".into();
        // The vulnerable setting: cmd.exe is allowed to look in the project, and the project is on PATH too.
        launch
            .remove_env
            .push("NoDefaultCurrentDirectoryInExePath".into());
        launch.env.insert(
            "PATH".into(),
            support::path_with(&[&npm, Path::new("."), &cwd, &bin]),
        );
        let (_session, _) = start(&launch).expect("it starts through the shim");
        assert!(
            !cwd.join("planted.txt").exists(),
            "a program of the project ran"
        );
        let started = &logged(&home)[0];
        let args = started["args"].as_array().unwrap();
        assert!(
            args[0].as_str().unwrap().ends_with("cli.js"),
            "the script comes first: {args:?}"
        );
        assert!(args.iter().any(|arg| arg == "--mode"));
        let spawned = launch.launch(None, None).expect("a command");
        assert_eq!(Path::new(&spawned.program), bin.join("node.exe"));
        assert_eq!(
            spawned
                .env
                .get(std::ffi::OsStr::new("NoDefaultCurrentDirectoryInExePath"))
                .map(|v| v.to_string_lossy().into_owned()),
            Some("1".to_string())
        );
        assert!(
            !spawned
                .remove_env
                .iter()
                .any(|name| name.eq_ignore_ascii_case("NoDefaultCurrentDirectoryInExePath"))
        );
    }

    #[test]
    fn a_node_only_the_project_has_is_not_used() {
        let scratch = Scratch::new("only-planted");
        let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
        let (npm, _bin) = support::npm_install(&scratch);
        std::fs::copy(support::fake_program(), cwd.join("node.exe")).unwrap();
        let mut launch = launching(&cwd, &home);
        launch.executable = "pi".into();
        // The system PATH may hold a real node; take it out so that only the project's is left.
        launch
            .env
            .insert("PATH".into(), std::env::join_paths([&npm, &cwd]).unwrap());
        assert!(matches!(
            start(&launch),
            Err(Error::Refused(_)) | Err(Error::Spawn(_))
        ));
        assert!(launch.launch(None, None).is_err());
    }
}
