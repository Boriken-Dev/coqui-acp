//! A session in a folder on a share (`\\server\share\work`) opens like any other and Claude Code runs in that folder. The
//! share is this machine's own administrative share, so nothing leaves it.
#![cfg(windows)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

mod support;

use std::path::{Path, PathBuf};

use coqui_acp_claude::{ClaudeAgent, ClaudeLaunch};
use coqui_acp_harness::Settings;
use serde_json::json;
use support::{Peer, Scratch, replay_program};

fn on_share(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let (drive, rest) = text.split_at(2);
    let shared = PathBuf::from(format!(r"\\localhost\{}${rest}", &drive[..1]));
    assert!(shared.is_dir(), "{} does not answer", shared.display());
    shared
}

fn agent() -> ClaudeAgent {
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    ClaudeAgent::new().defaults(defaults)
}

#[test]
fn a_session_opens_in_a_share_folder_under_a_share_root_and_nowhere_else() {
    let scratch = Scratch::new("share");
    let project = scratch.0.join("project");
    let other = scratch.0.join("other");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let root = on_share(&project);
    let mut settings = Settings::new();
    settings.working_roots = vec![root.clone()];
    let mut peer = Peer::serve(agent(), settings);
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let mut open = |cwd: String| peer.request("session/new", json!({"cwd": cwd, "mcpServers": []}));
    for taken in [
        root.to_string_lossy().into_owned(),
        on_share(&project.join("src"))
            .to_string_lossy()
            .into_owned(),
    ] {
        let opened = open(taken.clone());
        assert!(opened.get("result").is_some(), "{taken}: {opened}");
    }
    for refused in [
        on_share(&other).to_string_lossy().into_owned(),
        format!(r"{}\..\..\other", root.display()),
        r"\\.\pipe\x".to_owned(),
        project.join("src").to_string_lossy().into_owned(),
    ] {
        let answer = open(refused.clone());
        assert_eq!(answer["error"]["code"], -32602, "{refused}: {answer}");
    }
}

/// Without roots a share is still a folder and a device still is not.
#[test]
fn without_roots_a_share_folder_opens_and_a_device_does_not() {
    let scratch = Scratch::new("share-open");
    let folder = on_share(&scratch.0);
    let mut peer = Peer::serve(agent(), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": folder.to_string_lossy(), "mcpServers": []}),
    );
    assert!(opened.get("result").is_some(), "{opened}");
    let refused = peer.request(
        "session/new",
        json!({"cwd": r"\\.\pipe\x", "mcpServers": []}),
    );
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
}
