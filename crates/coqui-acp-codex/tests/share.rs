//! A session, and a resumed thread, in a folder on a share (`\\server\share\work`). The share is this machine's own
//! administrative share, so nothing leaves it.
#![cfg(windows)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

mod support;

use std::path::{Path, PathBuf};

use coqui_acp_codex::harness::Settings;
use coqui_acp_codex::{CodexAgent, CodexLaunch};
use serde_json::json;
use support::{Peer, Scratch, fixtures, recorded_thread, replay_program};

fn on_share(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let (drive, rest) = text.split_at(2);
    let shared = PathBuf::from(format!(r"\\localhost\{}${rest}", &drive[..1]));
    assert!(shared.is_dir(), "{} does not answer", shared.display());
    shared
}

/// The `resume` recording with the folder its thread was made in replaced by `folder`, as `<scratch>/resume`.
fn recording_in(scratch: &Scratch, folder: &Path) -> PathBuf {
    let from = fixtures().join("resume");
    let json_text = |path: &str| path.replace('\\', "\\\\");
    let stdout = std::fs::read_to_string(from.with_extension("stdout"))
        .unwrap()
        .replace(
            &json_text(r"C:\coqui-fx\w"),
            &json_text(&folder.to_string_lossy()),
        );
    let base = scratch.0.join("resume");
    std::fs::write(base.with_extension("stdout"), stdout).unwrap();
    std::fs::copy(
        from.with_extension("steps.json"),
        base.with_extension("steps.json"),
    )
    .unwrap();
    base
}

fn agent(recording: &Path) -> CodexAgent {
    let mut defaults = CodexLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults
        .env
        .insert("COQUI_ACP_CODEX_REPLAY".into(), recording.into());
    CodexAgent::new().defaults(defaults)
}

fn load(peer: &mut Peer, thread: &str, cwd: &Path) -> serde_json::Value {
    peer.request(
        "session/load",
        json!({"sessionId": thread, "cwd": cwd.to_string_lossy(), "mcpServers": []}),
    )
}

/// A thread recorded in a folder on a share is resumed under a root on that share, and is not under another.
#[test]
fn a_thread_recorded_on_a_share_is_resumed_under_a_share_root_and_not_under_another() {
    let scratch = Scratch::new("share-resume");
    let project = scratch.0.join("project");
    let elsewhere = scratch.0.join("elsewhere");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let folder = on_share(&project);
    let recording = recording_in(&scratch, &folder);
    let thread = recorded_thread("resume");
    let init = json!({"protocolVersion": 1, "clientCapabilities": {}});

    let mut settings = Settings::new();
    settings.working_roots = vec![folder.clone()];
    let mut peer = Peer::serve(agent(&recording), settings);
    peer.request("initialize", init.clone());
    let loaded = load(&mut peer, &thread, &folder);
    assert!(loaded.get("result").is_some(), "{loaded}");
    assert!(
        peer.kinds().iter().any(|kind| kind == "user_message_chunk"),
        "the thread was replayed"
    );

    let mut settings = Settings::new();
    settings.working_roots = vec![on_share(&elsewhere)];
    let mut peer = Peer::serve(agent(&recording), settings);
    peer.request("initialize", init);
    let refused = load(&mut peer, &thread, &on_share(&elsewhere));
    assert!(refused.get("error").is_some(), "{refused}");
    assert!(
        !peer.kinds().iter().any(|kind| kind == "user_message_chunk"),
        "nothing of it was told"
    );
}

#[test]
fn a_session_opens_in_a_share_folder_under_a_share_root_and_a_device_is_refused() {
    let scratch = Scratch::new("share-new");
    let folder = on_share(&scratch.0);
    let mut settings = Settings::new();
    settings.working_roots = vec![folder.clone()];
    let mut peer = Peer::serve(agent(&fixtures().join("resume")), settings);
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let refused = peer.request(
        "session/new",
        json!({"cwd": r"\\.\pipe\x", "mcpServers": []}),
    );
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let outside = peer.request(
        "session/new",
        json!({"cwd": r"\\localhost\C$\Windows", "mcpServers": []}),
    );
    assert_eq!(outside["error"]["code"], -32602, "{outside}");
}
