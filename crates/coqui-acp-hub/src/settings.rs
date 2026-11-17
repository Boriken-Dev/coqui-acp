/// What the hub keeps and queues. Every value is taken as given: a limit is a setting, not a floor.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(default)
)]
#[non_exhaustive]
pub struct Settings {
    /// Updates kept for replay; the oldest go first. Default 100000.
    pub max_log_entries: usize,
    /// Serialized bytes of updates kept for replay. Default 64 MiB.
    pub max_log_bytes: usize,
    /// Prompts waiting behind a running turn; one more is refused to its sender. Default 32.
    pub max_queued_prompts: usize,
    /// Whether a `session/cancel` also ends the prompts queued behind the running turn (they answer `cancelled`). Default
    /// false: the queue goes on.
    pub cancel_drops_queue: bool,
    /// Milliseconds a permission request may wait before `Hub::poll` reports it as a `Note::PermissionUnanswered`. The hub
    /// never answers one on its own. Default none.
    pub permission_window_ms: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_log_entries: 100_000,
            max_log_bytes: 64 * 1024 * 1024,
            max_queued_prompts: 32,
            cancel_drops_queue: false,
            permission_window_ms: None,
        }
    }
}

/// A setting that is not at its default.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NonDefault {
    /// The setting's field name.
    pub setting: &'static str,
    /// Its value, as text.
    pub value: String,
    /// The default, as text.
    pub default: String,
}

impl Settings {
    /// Every setting that differs from the default, for the host to show.
    #[must_use]
    pub fn non_defaults(&self) -> Vec<NonDefault> {
        let d = Self::default();
        let mut out = Vec::new();
        let mut add = |differs: bool, setting: &'static str, value: String, default: String| {
            if differs {
                out.push(NonDefault {
                    setting,
                    value,
                    default,
                });
            }
        };
        add(
            self.max_log_entries != d.max_log_entries,
            "max_log_entries",
            self.max_log_entries.to_string(),
            d.max_log_entries.to_string(),
        );
        add(
            self.max_log_bytes != d.max_log_bytes,
            "max_log_bytes",
            self.max_log_bytes.to_string(),
            d.max_log_bytes.to_string(),
        );
        add(
            self.max_queued_prompts != d.max_queued_prompts,
            "max_queued_prompts",
            self.max_queued_prompts.to_string(),
            d.max_queued_prompts.to_string(),
        );
        add(
            self.cancel_drops_queue != d.cancel_drops_queue,
            "cancel_drops_queue",
            self.cancel_drops_queue.to_string(),
            d.cancel_drops_queue.to_string(),
        );
        add(
            self.permission_window_ms != d.permission_window_ms,
            "permission_window_ms",
            format!("{:?}", self.permission_window_ms),
            format!("{:?}", d.permission_window_ms),
        );
        out
    }
}
