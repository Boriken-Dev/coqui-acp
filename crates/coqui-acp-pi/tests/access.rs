//! The harness-neutral access, as the mode Pi's permission gate starts in.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use coqui_acp_harness::{Access, Approvals, Mode, Sandbox};
use coqui_acp_pi::PiLaunch;

fn start(sandbox: Sandbox, approvals: Approvals) -> Mode {
    let mut launch = PiLaunch::new(".");
    let mut access = Access::STANDARD;
    access.sandbox = sandbox;
    access.approvals = approvals;
    launch.access = Some(access);
    launch.start_mode()
}

#[test]
fn standard_access_starts_in_manual_and_a_launch_with_no_access_does_too() {
    assert_eq!(start(Sandbox::Unrestricted, Approvals::Ask), Mode::Manual);
    assert_eq!(PiLaunch::new(".").start_mode(), Mode::Manual);
}

#[test]
fn approvals_and_a_read_only_sandbox_choose_the_start_mode() {
    assert_eq!(
        start(Sandbox::Unrestricted, Approvals::Edits),
        Mode::AcceptEdits
    );
    assert_eq!(start(Sandbox::Workspace, Approvals::Never), Mode::Bypass);
    assert_eq!(start(Sandbox::ReadOnly, Approvals::Never), Mode::Plan);
}

#[test]
fn a_named_mode_is_kept() {
    let mut launch = PiLaunch::new(".");
    launch.mode = Some(Mode::Plan);
    launch.access = Some(Access::STANDARD);
    assert_eq!(launch.start_mode(), Mode::Plan);
}
