use alloc::string::String;
use serde_json::Value;

/// What happened on a session, whichever protocol version carried it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// Agent text, gathered up to the next tool call or the end of the turn.
    Text(String),
    /// A tool call began.
    Tool {
        /// The agent's identifier of the call.
        id: String,
        /// The human title.
        title: String,
        /// The tool's name, when the agent gave one.
        name: Option<String>,
        /// `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode` or `other`.
        kind: String,
    },
    /// The cost the agent reports so far. ACP carries no token counts.
    Cost {
        /// The amount, in `currency`.
        amount: f64,
        /// The ISO 4217 code the agent gave.
        currency: String,
    },
    /// Something worth telling a person that ends nothing.
    Notice(String),
    /// A permission request was answered.
    Permission {
        /// The tool's title.
        tool: String,
        /// Whether it may run.
        allowed: bool,
        /// Why it was refused.
        reason: Option<String>,
    },
    /// The turn finished: `end_turn`, `max_tokens` or `max_turn_requests`.
    Completed {
        /// The agent's stop reason.
        stop: String,
    },
    /// The turn failed, the agent refused, or the connection was lost.
    Failed(String),
    /// The turn stopped short: it was cancelled, or a permission could not be answered.
    Blocked(String),
    /// A `session/prompt` failed while an extension owns turn endings ([`turn_end_by`]). The extension
    /// may have ended the turn already, so the caller judges this.
    ///
    /// [`turn_end_by`]: crate::Builder::turn_end_by
    PromptFailed(String),
    /// A notification whose method starts with `_`: an extension, passed on untouched.
    Extension {
        /// The method.
        method: String,
        /// Its parameters.
        params: Value,
    },
}
