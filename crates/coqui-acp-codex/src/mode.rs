//! The modes of `coqui_acp_harness::mode`, as Codex's approval policy, sandbox, approvals reviewer and collaboration
//! mode.
//!
//! Measured on Codex 0.166.0 (`app-server generate-json-schema`, then `thread/start` and `collaborationMode/list`
//! with no turn): the approval policy is `untrusted`, `on-request` or `never`; the sandbox `read-only`,
//! `workspace-write` or `danger-full-access`; the reviewer `user`, `auto_review` or `guardian_subagent`; the
//! collaboration modes `plan` and `default`. `turn/start` takes each of them, so a mode holds from the next turn.

use alloc::string::String;
use alloc::vec::Vec;

use coqui_acp_harness::mode::Mode;
use serde_json::{Map, Value, json};

/// What Codex is told for a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeSettings {
    /// `approvalPolicy`.
    pub approval: &'static str,
    /// `approvalsReviewer`.
    pub reviewer: &'static str,
    /// The sandbox, as `sandboxPolicy`'s `type`.
    pub sandbox: &'static str,
    /// `collaborationMode`'s `mode`.
    pub collaboration: &'static str,
}

/// The one table of modes: each mode's settings.
#[must_use]
pub const fn settings(mode: Mode) -> ModeSettings {
    let (approval, reviewer, sandbox, collaboration) = match mode {
        Mode::Manual => ("untrusted", "user", "workspaceWrite", "default"),
        Mode::AcceptEdits => ("on-request", "user", "workspaceWrite", "default"),
        Mode::Auto => ("on-request", "auto_review", "workspaceWrite", "default"),
        Mode::Plan => ("on-request", "user", "readOnly", "plan"),
        Mode::Bypass => ("never", "user", "dangerFullAccess", "default"),
        _ => ("untrusted", "user", "readOnly", "default"),
    };
    ModeSettings {
        approval,
        reviewer,
        sandbox,
        collaboration,
    }
}

/// The modes Codex offers: every one, Bypass only when the host allows it.
#[must_use]
pub fn offered(allow_bypass: bool) -> Vec<Mode> {
    let mut modes = alloc::vec![Mode::Manual, Mode::AcceptEdits, Mode::Auto, Mode::Plan];
    if allow_bypass {
        modes.push(Mode::Bypass);
    }
    modes
}

/// The mode a thread is already in, from what Codex answered to `thread/start` or `thread/resume`; `None` for a
/// combination the table has no name for (`never` with a sandbox, a granular policy).
#[must_use]
pub fn of_thread(opened: &Value) -> Option<Mode> {
    let approval = opened.get("approvalPolicy")?.as_str()?;
    let reviewer = opened
        .get("approvalsReviewer")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let sandbox = opened.pointer("/sandbox/type")?.as_str()?;
    match (approval, sandbox) {
        ("never", "dangerFullAccess") => Some(Mode::Bypass),
        ("untrusted", _) => Some(Mode::Manual),
        ("on-request", "readOnly") if reviewer == "user" => Some(Mode::Manual),
        ("on-request", "workspaceWrite") if reviewer == "auto_review" => Some(Mode::Auto),
        ("on-request", "workspaceWrite") if reviewer == "user" => Some(Mode::AcceptEdits),
        _ => None,
    }
}

/// The `turn/start` parameters that put a turn in `mode`. `model` and `effort` fill the collaboration mode's
/// settings, which name both; `workspace` is the thread's own `sandbox_workspace_write` table, so a workspace
/// sandbox keeps the writable roots and network setting the host gave the thread.
#[must_use]
pub fn turn_params(
    mode: Mode,
    model: &str,
    effort: Option<&str>,
    workspace: &Map<String, Value>,
) -> Map<String, Value> {
    let table = settings(mode);
    let mut sandbox = Map::new();
    sandbox.insert("type".into(), json!(table.sandbox));
    if table.sandbox == "workspaceWrite" {
        if let Some(roots) = workspace.get("writable_roots") {
            sandbox.insert("writableRoots".into(), roots.clone());
        }
        if let Some(network) = workspace.get("network_access").and_then(Value::as_bool) {
            sandbox.insert("networkAccess".into(), json!(network));
        }
    }
    let mut params = Map::new();
    params.insert("approvalPolicy".into(), json!(table.approval));
    params.insert("approvalsReviewer".into(), json!(table.reviewer));
    params.insert("sandboxPolicy".into(), Value::Object(sandbox));
    params.insert(
        "collaborationMode".into(),
        json!({"mode": table.collaboration,
               "settings": {"model": model, "developer_instructions": null,
                            "reasoning_effort": effort.map(String::from)}}),
    );
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_in_the_state_a_mode_sets_is_in_that_mode() {
        for mode in Mode::ALL {
            let table = settings(mode);
            let opened = json!({"approvalPolicy": table.approval, "approvalsReviewer": table.reviewer,
                                "sandbox": {"type": table.sandbox}});
            // A thread has no collaboration mode of its own to read, so Plan reads as the read-only Manual it sits on.
            let expected = if mode == Mode::Plan {
                Mode::Manual
            } else {
                mode
            };
            assert_eq!(of_thread(&opened), Some(expected), "{mode:?}");
        }
    }

    #[test]
    fn a_policy_with_no_name_in_the_table_is_no_mode() {
        let opened = json!({"approvalPolicy": "never", "approvalsReviewer": "user",
                            "sandbox": {"type": "workspaceWrite"}});
        assert_eq!(of_thread(&opened), None);
        assert_eq!(
            of_thread(
                &json!({"approvalPolicy": {"granular": {}}, "sandbox": {"type": "readOnly"}})
            ),
            None
        );
    }
}
