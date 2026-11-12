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
        assert_eq!(
            spawned
                .env
                .get(std::ffi::OsStr::new("PATHEXT"))
                .map(|v| v.to_string_lossy().into_owned()),
            Some(".COM;.EXE;.BAT;.CMD".to_string()),
            "no script extension in the program search of Pi's own spawner"
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

#[test]
fn the_guard_is_loaded_through_node_options_and_the_session_directories_are_kept_out_of_every_search()
 {
    let scratch = Scratch::new("guard-env");
    let (cwd, home, more) = (
        scratch.dir("work"),
        scratch.dir("home"),
        scratch.dir("more"),
    );
    let mut launch = launching(&cwd, &home);
    launch.additional_directories.push(more.clone());
    launch
        .env
        .insert("NODE_OPTIONS".into(), "--max-old-space-size=64".into());
    let guard = scratch.dir("gates").join("guard.cjs");
    let made = launch
        .launch_guarded(None, None, Some(&guard))
        .expect("a command");
    let options = made
        .env
        .get(std::ffi::OsStr::new("NODE_OPTIONS"))
        .expect("NODE_OPTIONS")
        .to_string_lossy()
        .into_owned();
    let written = guard.to_string_lossy().replace('\\', "/");
    assert_eq!(
        options,
        format!("--require {written} --max-old-space-size=64"),
        "the guard comes first and the person's options stay"
    );
    let keep: Vec<String> = serde_json::from_str(
        &made
            .env
            .get(std::ffi::OsStr::new("COQUI_ACP_PI_KEEP_OUT"))
            .expect("the directories")
            .to_string_lossy(),
    )
    .unwrap();
    for dir in [&cwd, &more] {
        assert!(
            keep.iter().any(|kept| kept == &dir.to_string_lossy()),
            "{dir:?} is not kept out: {keep:?}"
        );
    }
    // A path with a space is quoted for NODE_OPTIONS.
    let spaced = scratch.dir("a gate").join("guard.cjs");
    let made = launch
        .launch_guarded(None, None, Some(&spaced))
        .expect("a command");
    let options = made
        .env
        .get(std::ffi::OsStr::new("NODE_OPTIONS"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        options.starts_with(&format!(
            "--require \"{}\"",
            spaced.to_string_lossy().replace('\\', "/")
        )),
        "{options}"
    );
}

#[test]
fn a_session_starts_pi_with_the_guard_in_the_gate_directory_and_removes_the_directory_with_it() {
    let scratch = Scratch::new("guard-session");
    let (cwd, home, gates) = (
        scratch.dir("work"),
        scratch.dir("home"),
        scratch.dir("gates"),
    );
    let mut launch = launching(&cwd, &home);
    launch.gate_dir = Some(gates.clone());
    let (session, _events) = start(&launch).expect("it starts");
    let names = logged(&home)[0]["env_names"].clone();
    for wanted in ["NODE_OPTIONS", "COQUI_ACP_PI_KEEP_OUT"] {
        assert!(
            names.as_array().unwrap().iter().any(|n| n == wanted),
            "{wanted} is not in Pi's environment: {names}"
        );
    }
    let held: Vec<_> = std::fs::read_dir(&gates).unwrap().flatten().collect();
    assert_eq!(held.len(), 1, "one directory for one session");
    assert!(held[0].path().join("guard.cjs").is_file());
    drop(session);
    assert_eq!(
        std::fs::read_dir(&gates).unwrap().count(),
        0,
        "the gate's directory outlived the session"
    );
}

#[test]
fn a_launch_whose_working_directory_holds_the_places_credentials_are_kept_is_refused_unless_allowed()
 {
    let scratch = Scratch::new("credential-root");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    // The scratch directory holds the agent directory (`home`) with its `auth.json`, as a person's home holds `.ssh`.
    let mut launch = launching(&scratch.0, &home);
    let Err(error) = start(&launch) else {
        panic!("a session was opened over the credentials");
    };
    assert!(matches!(error, Error::Refused(_)), "{error}");
    assert!(error.to_string().contains("credentials"), "{error}");
    launch.allow_credential_roots = true;
    assert!(start(&launch).is_ok());
    // A project beside it is fine, and so is one with more directories beside it.
    let launch = launching(&cwd, &home);
    assert!(start(&launch).is_ok());
}

#[test]
fn a_tilde_in_the_agent_directory_is_the_home_pi_expands_it_to() {
    let scratch = Scratch::new("tilde-agent");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let mut launch = PiLaunch::new(&cwd);
    for name in ["HOME", "USERPROFILE"] {
        launch.env.insert(name.into(), home.clone().into());
    }
    launch
        .env
        .insert("PI_CODING_AGENT_DIR".into(), "~/elsewhere".into());
    assert_eq!(launch.agent_dirs(), [home.join("elsewhere")]);
    launch.env.insert("PI_CODING_AGENT_DIR".into(), "~".into());
    assert_eq!(launch.agent_dirs(), std::slice::from_ref(&home));
    launch
        .env
        .insert("PI_CODING_AGENT_DIR".into(), "plain/dir".into());
    assert_eq!(launch.agent_dirs(), [cwd.join("plain").join("dir")]);
}

#[test]
fn the_persons_node_options_keep_only_modules_written_in_full() {
    let scratch = Scratch::new("node-options");
    let (cwd, home) = (scratch.dir("work"), scratch.dir("home"));
    let absolute = if cfg!(windows) {
        "C:/tools/trace.js"
    } else {
        "/tools/trace.js"
    };
    let mut launch = launching(&cwd, &home);
    launch.env.insert(
        "NODE_OPTIONS".into(),
        format!(
            "--max-old-space-size=64 --require evil -r evil2 --import=evil3 --require {absolute} --import ./local.mjs --loader x"
        )
        .into(),
    );
    let guard = scratch.dir("gates").join("guard.cjs");
    let made = launch
        .launch_guarded(None, None, Some(&guard))
        .expect("a command");
    let options = made
        .env
        .get(std::ffi::OsStr::new("NODE_OPTIONS"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let written = guard.to_string_lossy().replace('\\', "/");
    assert_eq!(
        options,
        format!("--require {written} --max-old-space-size=64 --require {absolute}")
    );
}

#[test]
fn the_listing_of_the_models_loads_the_guard_and_works_in_an_empty_directory() {
    let scratch = Scratch::new("listing");
    let (cwd, home, gates) = (
        scratch.dir("work"),
        scratch.dir("home"),
        scratch.dir("gates"),
    );
    let mut launch = launching(&cwd, &home);
    launch.gate_dir = Some(gates.clone());
    let _ = PiSession::read_models(&launch, &PlainSpawn, PiSettings::new());
    let started = &logged(&home)[0];
    let names = started["env_names"].as_array().unwrap();
    for wanted in ["NODE_OPTIONS", "COQUI_ACP_PI_KEEP_OUT"] {
        assert!(names.iter().any(|n| n == wanted), "{wanted} is missing");
    }
    let ran_in = std::path::PathBuf::from(started["cwd"].as_str().unwrap());
    assert!(
        ran_in != cwd && ran_in.starts_with(&gates),
        "Pi listed the models in {ran_in:?}, not in a directory of its own"
    );
    assert_eq!(
        std::fs::read_dir(&gates).unwrap().count(),
        0,
        "the directory outlived the listing"
    );
}

/// The list is the person's: a working directory is refused for what the list names, and not for what they took out of it.
#[test]
fn a_working_directory_over_a_protected_place_is_refused_by_the_persons_list() {
    let scratch = Scratch::new("persons-list");
    let (home, agent) = (scratch.dir("home"), scratch.dir("agent"));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let launch_in = |cwd: &std::path::Path, protected: Option<Vec<String>>| {
        let mut launch = PiLaunch::new(cwd);
        launch.executable = support::fake_program().into();
        for name in ["HOME", "USERPROFILE"] {
            launch.env.insert(name.into(), home.clone().into());
        }
        launch
            .env
            .insert("PI_CODING_AGENT_DIR".into(), agent.clone().into());
        launch.protected = protected;
        launch
    };
    // The recommendations name `~/.ssh`: the home directory holds a place, so it is refused.
    assert!(launch_in(&home, None).credential_root().is_some());
    // A list without it, or one that names nothing of the home directory, does not refuse it.
    let without = Some(vec!["secret.key".to_string()]);
    assert!(launch_in(&home, without).credential_root().is_none());
    assert!(
        launch_in(&home, Some(Vec::new()))
            .credential_root()
            .is_none()
    );
    // A list with an entry of the person's own refuses the directory that holds it.
    let own = Some(vec!["~/.claude/notes".to_string()]);
    assert!(launch_in(&home, own).credential_root().is_some());
    // And what the person allows again is not a place.
    let allowed = Some(vec!["~/.claude".to_string(), "!~/.claude".to_string()]);
    assert!(launch_in(&home, allowed).credential_root().is_none());
}
