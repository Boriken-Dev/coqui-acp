//! The bounds and waits of a Pi session, as the settings a provider's configuration holds.

use coqui_acp_api::guatiao::{FromValue, Schema, ToValue};
use coqui_acp_harness::SessionLimits;

use crate::session::PiSettings;

/// How a Pi session reads Pi's output, waits for it and ends it: [`SessionLimits`] and what only this translator
/// bounds. Every value is optional and any value is taken; absent, [`PiSettings::new`]'s.
#[derive(ToValue, FromValue, Schema, Debug, Clone, Default, PartialEq, Eq)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct SessionSettings {
    /// The limits every translator has.
    #[map(flatten)]
    pub limits: SessionLimits,
    /// How long Pi may take to answer `prompt`, which a command of an extension answers when it is done, in
    /// milliseconds. Default 300 000.
    pub prompt_timeout_ms: Option<u64>,
    /// The earlier messages a load replays, the latest kept. Default 5000.
    pub max_history_messages: Option<u64>,
    /// Whether to ask for the models and thinking levels when starting. Default on.
    pub list_models: Option<bool>,
    /// The longest a title is, in bytes. Default 256.
    pub max_tool_title_bytes: Option<u64>,
    /// The most of a tool's output text kept for a client, in bytes. Default 16384.
    pub max_told_bytes: Option<u64>,
    /// The largest file read for a write's old content, in bytes. Default 1 MiB.
    pub max_old_bytes: Option<u64>,
}

impl SessionSettings {
    /// The session's settings: [`PiSettings::new`]'s, with the values given here over them.
    #[must_use]
    pub fn settings(&self) -> PiSettings {
        let mut settings = PiSettings::new();
        let limits = &self.limits;
        SessionLimits::bytes(limits.max_line_bytes, &mut settings.max_line);
        SessionLimits::count(limits.tail_lines, &mut settings.tail_lines);
        SessionLimits::count(limits.tail_line_bytes, &mut settings.tail_line_bytes);
        SessionLimits::millis(
            limits.initialize_timeout_ms,
            &mut settings.initialize_timeout,
        );
        SessionLimits::millis(limits.request_timeout_ms, &mut settings.request_timeout);
        SessionLimits::millis(limits.exit_wait_ms, &mut settings.exit_wait);
        SessionLimits::millis(limits.exit_poll_ms, &mut settings.exit_poll);
        SessionLimits::millis(limits.close_wait_ms, &mut settings.close_wait);
        SessionLimits::count(limits.max_tools, &mut settings.max_tools);
        SessionLimits::count(limits.max_queued_writes, &mut settings.max_queued_writes);
        SessionLimits::count(limits.title_bytes, &mut settings.title_bytes);
        SessionLimits::millis(limits.version_wait_ms, &mut settings.version_wait);
        SessionLimits::bytes(limits.version_bytes, &mut settings.version_bytes);
        SessionLimits::millis(self.prompt_timeout_ms, &mut settings.prompt_timeout);
        SessionLimits::count(
            self.max_history_messages,
            &mut settings.max_history_messages,
        );
        if let Some(list) = self.list_models {
            settings.list_models = list;
        }
        SessionLimits::count(
            self.max_tool_title_bytes,
            &mut settings.parser.max_title_bytes,
        );
        SessionLimits::count(self.max_told_bytes, &mut settings.parser.max_told_bytes);
        SessionLimits::bytes(self.max_old_bytes, &mut settings.parser.max_old_bytes);
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    /// A session section with nothing in it is the session defaults; each value given reaches its own limit.
    #[test]
    fn nothing_given_keeps_the_defaults_and_each_value_reaches_its_limit() {
        let defaults = PiSettings::new();
        let none = SessionSettings::default().settings();
        assert_eq!(none.prompt_timeout, Duration::from_secs(300));
        assert_eq!(none.max_history_messages, defaults.max_history_messages);
        assert_eq!(none.parser.max_old_bytes, defaults.parser.max_old_bytes);
        let mut given = SessionSettings::default();
        given.limits.request_timeout_ms = Some(8);
        given.prompt_timeout_ms = Some(0);
        given.max_history_messages = Some(9);
        given.max_old_bytes = Some(10);
        let settings = given.settings();
        assert_eq!(settings.request_timeout, Duration::from_millis(8));
        assert_eq!(settings.prompt_timeout, Duration::ZERO);
        assert_eq!(settings.max_history_messages, 9);
        assert_eq!(settings.parser.max_old_bytes, 10);
        assert_eq!(
            settings.parser.max_told_bytes,
            defaults.parser.max_told_bytes
        );
    }
}
