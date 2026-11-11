//! What each mode lets Pi do without asking.
//!
//! Pi has no permission system of its own, so the modes are this crate's: the permission gate puts every tool call to the
//! translator, which allows it, refuses it as a plan, or asks the person. Pi has no automatic reviewer, so [`Mode::Auto`]
//! is never offered.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use coqui_acp_harness::mode;
pub use coqui_acp_harness::mode::Mode;

/// The tools that only read.
pub const READ_ONLY: [&str; 4] = ["read", "grep", "find", "ls"];
/// The tools that change files.
pub const EDITS: [&str; 2] = ["edit", "write"];

/// What is done with one tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// It runs.
    Allow,
    /// The person is asked.
    Ask,
    /// It is refused: the session is only to plan.
    Plan,
}

/// The mode in use and what the person allowed for the rest of the session.
#[derive(Clone, Debug)]
pub struct Policy {
    mode: Mode,
    always: BTreeSet<String>,
    ask_for_reads: bool,
}

impl Policy {
    /// A policy in `mode`; with `ask_for_reads` even a tool that only reads is asked about in [`Mode::Manual`].
    #[must_use]
    pub const fn new(mode: Mode, ask_for_reads: bool) -> Self {
        Self {
            mode,
            always: BTreeSet::new(),
            ask_for_reads,
        }
    }

    /// The mode in use.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Puts the calls that follow in `mode`.
    pub const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Allows `tool` for the rest of the session, in every mode but Plan.
    pub fn allow_tool(&mut self, tool: &str) {
        self.always.insert(tool.to_string());
    }

    /// What to do with a call of `tool`.
    ///
    /// | Mode | read-only tools | `edit`, `write` | `bash` and any other |
    /// | --- | --- | --- | --- |
    /// | Manual | allowed (asked with `ask_for_reads`) | asked | asked |
    /// | Accept edits | allowed | allowed | asked |
    /// | Plan | allowed | refused | refused |
    /// | Bypass | allowed | allowed | allowed |
    ///
    /// A tool allowed for the session is allowed in every mode but Plan.
    #[must_use]
    pub fn decide(&self, tool: &str) -> Decision {
        let read = READ_ONLY.contains(&tool);
        let edit = EDITS.contains(&tool);
        match self.mode {
            Mode::Bypass => Decision::Allow,
            Mode::Plan if read => Decision::Allow,
            Mode::Plan => Decision::Plan,
            _ if self.always.contains(tool) => Decision::Allow,
            Mode::AcceptEdits if read || edit => Decision::Allow,
            Mode::Manual if read && !self.ask_for_reads => Decision::Allow,
            _ => Decision::Ask,
        }
    }
}

/// The modes a client may choose: the host's own mode and every stricter one, and those the host named besides. Bypass
/// only when allowed, and never Auto, which Pi has nothing to back.
#[must_use]
pub fn offered(loosest: Option<Mode>, named: &[Mode], allow_bypass: bool) -> Vec<Mode> {
    let mut modes = mode::widened(&mode::up_to(loosest, allow_bypass), named, allow_bypass);
    modes.retain(|mode| *mode != Mode::Auto);
    modes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_decides_as_the_table_says() {
        let in_mode = |mode| Policy::new(mode, false);
        assert_eq!(in_mode(Mode::Manual).decide("read"), Decision::Allow);
        assert_eq!(in_mode(Mode::Manual).decide("write"), Decision::Ask);
        assert_eq!(in_mode(Mode::Manual).decide("bash"), Decision::Ask);
        assert_eq!(in_mode(Mode::AcceptEdits).decide("edit"), Decision::Allow);
        assert_eq!(in_mode(Mode::AcceptEdits).decide("bash"), Decision::Ask);
        assert_eq!(
            in_mode(Mode::AcceptEdits).decide("mcp__x__y"),
            Decision::Ask
        );
        assert_eq!(in_mode(Mode::Plan).decide("grep"), Decision::Allow);
        assert_eq!(in_mode(Mode::Plan).decide("write"), Decision::Plan);
        assert_eq!(in_mode(Mode::Plan).decide("bash"), Decision::Plan);
        assert_eq!(in_mode(Mode::Bypass).decide("bash"), Decision::Allow);
        assert_eq!(
            Policy::new(Mode::Manual, true).decide("read"),
            Decision::Ask
        );
    }

    #[test]
    fn a_tool_allowed_for_the_session_is_allowed_except_in_plan() {
        let mut policy = Policy::new(Mode::Manual, false);
        policy.allow_tool("bash");
        assert_eq!(policy.decide("bash"), Decision::Allow);
        assert_eq!(policy.decide("write"), Decision::Ask);
        policy.set_mode(Mode::Plan);
        assert_eq!(policy.decide("bash"), Decision::Plan);
    }

    #[test]
    fn auto_is_never_offered() {
        let modes = offered(Some(Mode::Auto), &[Mode::Auto], true);
        assert!(!modes.contains(&Mode::Auto));
        assert!(modes.contains(&Mode::Bypass));
        assert_eq!(offered(None, &[], false), vec![Mode::Plan]);
    }
}
