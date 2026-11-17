//! The harness-neutral access, in Codex's words.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use coqui_acp_codex::CodexLaunch;
use coqui_acp_harness::{ACCESS_META, Access, Approvals, Sandbox};
use serde_json::json;

fn params(access: Access) -> (serde_json::Value, serde_json::Value) {
    let mut launch = CodexLaunch::new("w");
    launch.apply_access(access);
    (
        launch.thread_params(),
        serde_json::Value::Object(launch.config()),
    )
}

#[test]
fn standard_access_runs_with_no_sandbox_and_asks_before_a_command() {
    let (thread, config) = params(Access::STANDARD);
    assert_eq!(thread["sandbox"], "danger-full-access", "{thread}");
    assert_eq!(thread["approvalPolicy"], "untrusted", "{thread}");
    assert!(config.get("sandbox_workspace_write").is_none(), "{config}");
}

#[test]
fn no_access_given_sends_nothing_so_codexs_own_configuration_decides() {
    let thread = CodexLaunch::new("w").thread_params();
    assert!(thread.get("sandbox").is_none() && thread.get("approvalPolicy").is_none());
}

#[test]
fn each_sandbox_and_approval_has_codexs_word() {
    for (sandbox, word) in [
        (Sandbox::Unrestricted, "danger-full-access"),
        (Sandbox::Workspace, "workspace-write"),
        (Sandbox::ReadOnly, "read-only"),
    ] {
        for (approvals, policy) in [
            (Approvals::Ask, "untrusted"),
            (Approvals::Edits, "on-request"),
            (Approvals::Never, "never"),
        ] {
            let mut access = Access::STANDARD;
            access.sandbox = sandbox;
            access.approvals = approvals;
            let (thread, _) = params(access);
            assert_eq!(thread["sandbox"], word);
            assert_eq!(thread["approvalPolicy"], policy);
        }
    }
}

#[test]
fn the_network_is_the_workspace_sandboxs_setting_and_a_named_sandbox_is_kept() {
    let mut access = Access::STANDARD;
    access.sandbox = Sandbox::Workspace;
    access.network = false;
    let (_, config) = params(access);
    assert_eq!(
        config["sandbox_workspace_write"]["network_access"],
        json!(false)
    );
    let mut launch = CodexLaunch::new("w");
    launch.sandbox = Some("read-only".into());
    launch.apply_access(access);
    assert_eq!(launch.thread_params()["sandbox"], "read-only");
}

#[test]
fn the_meta_key_names_the_access_the_agent_reads() {
    assert_eq!(ACCESS_META, "dev.boriken.coqui.acp/access");
}
