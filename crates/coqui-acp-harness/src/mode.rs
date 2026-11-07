//! The modes a harness is run in, named once for every translator.
//!
//! A translator offers the modes its command-line tool has, as one select option whose id is [`OPTION_ID`] and whose
//! category is `mode`, so a client shows the same control over every harness.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::acp::{ConfigOption, OptionValue};

/// The id of the mode option.
pub const OPTION_ID: &str = "mode";

/// How much a harness asks before it acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Mode {
    /// Asks before everything that is not read-only.
    Manual,
    /// Edits files in the working directory without asking; asks for the rest.
    AcceptEdits,
    /// Decides what to ask on its own, through the tool's own automatic review.
    Auto,
    /// Plans and reads, and changes nothing until the plan is accepted.
    Plan,
    /// Asks for nothing and has no sandbox of its own.
    Bypass,
}

impl Mode {
    /// Every mode, in the order a client cycles them.
    pub const ALL: [Self; 5] = [
        Self::Manual,
        Self::AcceptEdits,
        Self::Auto,
        Self::Plan,
        Self::Bypass,
    ];

    /// The id a client sends.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AcceptEdits => "accept_edits",
            Self::Auto => "auto",
            Self::Plan => "plan",
            Self::Bypass => "bypass",
        }
    }

    /// The name people see.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Manual => "Manual",
            Self::AcceptEdits => "Accept edits",
            Self::Auto => "Auto",
            Self::Plan => "Plan",
            Self::Bypass => "Bypass",
        }
    }

    /// What the mode does, in a line.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Manual => "Asks before it changes anything or runs a command",
            Self::AcceptEdits => "Edits files without asking; asks before other commands",
            Self::Auto => "Decides for itself what needs asking",
            Self::Plan => "Reads and plans; changes nothing",
            Self::Bypass => "Asks for nothing",
        }
    }

    /// The mode a client's `id` names.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == id)
    }
}

/// The mode option, offering `offered` in the order of [`Mode::ALL`] whatever order they are given in.
#[must_use]
pub fn option(current: Mode, offered: &[Mode]) -> ConfigOption {
    let values: Vec<OptionValue> = Mode::ALL
        .into_iter()
        .filter(|mode| offered.contains(mode) || *mode == current)
        .map(|mode| OptionValue::new(mode.id(), mode.name(), Some(mode.description().to_string())))
        .collect();
    ConfigOption::new(OPTION_ID, "Mode", current.id(), values).category("mode")
}

/// The mode `value` of the mode option names, if `offered` holds it.
///
/// # Errors
///
/// The reason, for a refusal: the value is no mode, or this harness does not offer it.
pub fn chosen(value: &str, offered: &[Mode]) -> Result<Mode, String> {
    let mode = Mode::from_id(value).ok_or_else(|| format!("{value} is not a value of mode"))?;
    if offered.contains(&mode) {
        Ok(mode)
    } else {
        Err(format!("this agent does not offer the {} mode", mode.id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_lists_only_what_is_offered_in_the_cycle_order() {
        let option = option(Mode::Manual, &[Mode::Plan, Mode::Manual]);
        let ids: Vec<&str> = option
            .values
            .iter()
            .map(|value| value.id.as_str())
            .collect();
        assert_eq!(ids, ["manual", "plan"]);
        assert_eq!(option.category.as_deref(), Some("mode"));
        assert_eq!(option.current, "manual");
    }

    #[test]
    fn a_mode_in_use_is_listed_even_when_not_offered_for_choosing() {
        let option = option(Mode::Bypass, &[Mode::Manual]);
        assert!(option.values.iter().any(|value| value.id == "bypass"));
    }

    #[test]
    fn a_mode_not_offered_is_refused() {
        assert_eq!(chosen("plan", &[Mode::Plan]), Ok(Mode::Plan));
        assert!(chosen("bypass", &[Mode::Plan]).is_err());
        assert!(chosen("bogus", &Mode::ALL).is_err());
    }

    #[test]
    fn every_id_round_trips() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_id(mode.id()), Some(mode));
        }
    }
}
