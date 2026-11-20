//! Pi's access table: what each setting of an [`Access`] becomes in Pi's own words.

use coqui_acp_harness::{Access, Approvals, Part, Sandbox, Target, Translation};

/// Pi, as the target of an access.
pub const TARGET: Target = Target::new("pi", translate);

const fn translate(access: Access) -> Translation {
    let sandbox = match access.sandbox {
        Sandbox::Unrestricted => Part::said("Pi has no sandbox"),
        Sandbox::ReadOnly => Part::word("plan", "the Plan mode of Pi's permission gate"),
        _ => Part::cannot(
            "Pi has no sandbox: its permission gate asks about paths outside the session's directories in every mode but Bypass",
        ),
    };
    let network = if access.network {
        Part::said("Pi has no network setting; it is on")
    } else {
        Part::cannot("Pi has no network setting")
    };
    let approvals = match (access.sandbox, access.approvals) {
        (Sandbox::ReadOnly, _) => Part::cannot("the Plan mode asks nothing: it changes nothing"),
        (_, Approvals::Edits) => Part::word(
            "accept_edits",
            "the Accept edits mode of Pi's permission gate",
        ),
        (_, Approvals::Never) => Part::word("bypass", "the Bypass mode of Pi's permission gate"),
        _ => Part::word("manual", "the Manual mode of Pi's permission gate"),
    };
    Translation::new(sandbox, network, approvals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_well_formed() {
        assert_eq!(TARGET.well_formed(), Ok(()));
    }

    #[test]
    fn the_standard_access_is_the_manual_mode() {
        assert_eq!(Access::STANDARD.translate(&TARGET).mode(), Some("manual"));
    }

    #[test]
    fn a_read_only_sandbox_is_the_plan_mode() {
        let mut access = Access::STANDARD;
        access.sandbox = Sandbox::ReadOnly;
        access.approvals = Approvals::Never;
        assert_eq!(access.translate(&TARGET).mode(), Some("plan"));
    }
}
