//! The harness-neutral access, in Claude Code's words.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use coqui_acp_claude::ClaudeLaunch;
use coqui_acp_harness::{Access, Approvals, Sandbox};

fn args(access: Option<Access>) -> Vec<String> {
    let mut launch = ClaudeLaunch::new("w");
    launch.access = access;
    launch
        .launch()
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn with(sandbox: Sandbox, network: bool, approvals: Approvals) -> Option<Access> {
    let mut access = Access::STANDARD;
    access.sandbox = sandbox;
    access.network = network;
    access.approvals = approvals;
    Some(access)
}

#[test]
fn standard_access_asks_in_the_default_mode_and_starts_no_sandbox() {
    let args = args(Some(Access::STANDARD));
    assert!(
        args.contains(&"--permission-mode=default".to_owned()),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.starts_with("--settings")),
        "{args:?}"
    );
}

#[test]
fn no_access_given_sends_no_mode_so_its_own_settings_decide() {
    let args = args(None);
    assert!(
        !args.iter().any(|arg| arg.starts_with("--permission-mode")),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.starts_with("--settings")),
        "{args:?}"
    );
}

#[test]
fn approvals_and_a_read_only_sandbox_are_permission_modes() {
    for (access, mode) in [
        (
            with(Sandbox::Unrestricted, true, Approvals::Edits),
            "acceptEdits",
        ),
        (
            with(Sandbox::Unrestricted, true, Approvals::Never),
            "bypassPermissions",
        ),
        (with(Sandbox::ReadOnly, true, Approvals::Never), "plan"),
        (with(Sandbox::Workspace, true, Approvals::Ask), "default"),
    ] {
        let args = args(access);
        assert!(
            args.contains(&format!("--permission-mode={mode}")),
            "{mode}: {args:?}"
        );
    }
    let never = args(with(Sandbox::Unrestricted, true, Approvals::Never));
    assert!(never.contains(&"--allow-dangerously-skip-permissions".to_owned()));
}

#[test]
fn a_workspace_sandbox_and_a_closed_network_are_its_sandbox_settings() {
    let find = |access| {
        args(access)
            .into_iter()
            .find_map(|arg| arg.strip_prefix("--settings=").map(str::to_owned))
            .map(|text| serde_json::from_str::<serde_json::Value>(&text).expect("JSON"))
    };
    let workspace = find(with(Sandbox::Workspace, true, Approvals::Ask)).expect("settings");
    assert_eq!(workspace["sandbox"]["enabled"], true);
    assert_eq!(workspace["sandbox"]["allowUnsandboxedCommands"], false);
    assert!(workspace["sandbox"].get("network").is_none());
    let closed = find(with(Sandbox::Unrestricted, false, Approvals::Ask)).expect("settings");
    assert_eq!(closed["sandbox"]["filesystem"]["disabled"], true);
    assert_eq!(closed["sandbox"]["network"]["strictAllowlist"], true);
    assert!(find(with(Sandbox::ReadOnly, true, Approvals::Ask)).is_none());
}
