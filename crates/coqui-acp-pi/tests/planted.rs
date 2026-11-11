//! On Windows npm's `pi.cmd` runs a bare `node`, and cmd.exe looks for it in the project first: a program a project plants
//! must not run when a session opens.
#![cfg(windows)]
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

mod support;

use std::path::Path;

use coqui_acp_pi::{PiSession, PiSettings, PlainSpawn};
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
