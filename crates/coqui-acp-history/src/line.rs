//! The lines after the header.

use agent_client_protocol::schema::v1::StopReason;
use serde_json::{Map, Value, json};

use crate::HistoryError;
use crate::shape::{Fields, is_timestamp};

/// The deepest value written, which is what the parser reads back.
const MAX_WRITE_DEPTH: u64 = 128;

/// The `_meta` key under which a codec keeps what only a resume by the same harness needs: the
/// native tool-use ids paired with ACP's, and the exact native input. Opaque to this crate, and
/// never required to read a history on another harness.
pub const NATIVE_META_KEY: &str = "dev.boriken.history/native";

/// The `_meta` key of a content block whose bytes were left out: an object with the content's
/// `sha256` (`sha256:` and 64 hexadecimal digits) and its size in `bytes`.
pub const ATTACHMENT_META_KEY: &str = "dev.boriken.history/attachment";

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    /// One of ACP's stop reasons.
    Acp(StopReason),
    /// The turn failed; [`Body::TurnEnd::error`] may say how.
    Error,
}

impl Stop {
    /// The reason as the file spells it: ACP's `end_turn` and the rest, or `error`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Acp(StopReason::EndTurn) => "end_turn",
            Self::Acp(StopReason::MaxTokens) => "max_tokens",
            Self::Acp(StopReason::MaxTurnRequests) => "max_turn_requests",
            Self::Acp(StopReason::Refusal) => "refusal",
            Self::Acp(StopReason::Cancelled) => "cancelled",
            // ACP's enum can gain a reason; the format names one only by a version bump.
            Self::Acp(_) => "unknown",
            Self::Error => "error",
        }
    }

    /// The reason a file's `stopReason` spells, or `None` for a name the format does not define.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "end_turn" => Self::Acp(StopReason::EndTurn),
            "max_tokens" => Self::Acp(StopReason::MaxTokens),
            "max_turn_requests" => Self::Acp(StopReason::MaxTurnRequests),
            "refusal" => Self::Acp(StopReason::Refusal),
            "cancelled" => Self::Acp(StopReason::Cancelled),
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// What set a compaction off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Trigger {
    /// The harness compacted by itself.
    Auto,
    /// The person asked.
    Manual,
}

/// What one line says.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Body {
    /// `session/prompt`: the client's prompt, as ACP content blocks.
    Prompt {
        /// The blocks; each an object with a `type`.
        prompt: Vec<Value>,
    },
    /// `session/update`: a notification from the agent, in ACP's shape.
    Update {
        /// The update: an object with a `sessionUpdate`.
        update: Value,
    },
    /// `session/cancel`: the client cancelled the turn.
    Cancel,
    /// `session/request_permission`: the agent asked.
    PermissionRequest {
        /// The call, an object with a `toolCallId`.
        tool_call: Value,
        /// The options offered, each an object with an `optionId`.
        options: Vec<Value>,
    },
    /// `history/permission_answer`: the client's answer to the request for `tool_call_id`.
    PermissionAnswer {
        /// The call the request named.
        tool_call_id: String,
        /// ACP's outcome: `selected` with an `optionId`, or `cancelled`.
        outcome: Value,
    },
    /// `history/turn_end`: the turn's prompt response.
    TurnEnd {
        /// How it ended.
        stop: Stop,
        /// What failed, when the turn failed.
        error: Option<String>,
    },
    /// `history/compaction`: the harness folded the history so far into a summary.
    Compaction {
        /// The harness's own summary text, when it exposes one.
        summary: Option<String>,
        /// What set it off.
        trigger: Option<Trigger>,
        /// The context size before compaction, in tokens, when the harness says.
        pre_tokens: Option<u64>,
    },
}

impl Body {
    /// The line's `method`.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "session/prompt",
            Self::Update { .. } => "session/update",
            Self::Cancel => "session/cancel",
            Self::PermissionRequest { .. } => "session/request_permission",
            Self::PermissionAnswer { .. } => "history/permission_answer",
            Self::TurnEnd { .. } => "history/turn_end",
            Self::Compaction { .. } => "history/compaction",
        }
    }
}

/// One line after the header.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Line {
    /// When it happened, as an RFC 3339 timestamp.
    pub at: Option<String>,
    /// Anything else, as an object, such as [`NATIVE_META_KEY`]; the format reads none of it.
    pub meta: Option<Value>,
    /// What it says.
    pub body: Body,
}

impl Line {
    /// A line saying `body`, with no time and no `_meta`.
    #[must_use]
    pub fn new(body: Body) -> Self {
        Self {
            at: None,
            meta: None,
            body,
        }
    }

    /// A prompt of `blocks`.
    #[must_use]
    pub fn prompt(blocks: Vec<Value>) -> Self {
        Self::new(Body::Prompt { prompt: blocks })
    }

    /// A prompt of one text block.
    #[must_use]
    pub fn text_prompt(text: &str) -> Self {
        Self::prompt(vec![json!({"type": "text", "text": text})])
    }

    /// An update from the agent.
    #[must_use]
    pub fn update(update: Value) -> Self {
        Self::new(Body::Update { update })
    }

    /// The turn's end.
    #[must_use]
    pub fn turn_end(stop: Stop) -> Self {
        Self::new(Body::TurnEnd { stop, error: None })
    }

    /// A compaction boundary holding the harness's `summary`.
    #[must_use]
    pub fn compaction(summary: impl Into<String>) -> Self {
        Self::new(Body::Compaction {
            summary: Some(summary.into()),
            trigger: None,
            pre_tokens: None,
        })
    }

    /// The same line stamped `at`.
    #[must_use]
    pub fn at(mut self, at: impl Into<String>) -> Self {
        self.at = Some(at.into());
        self
    }

    /// The same line with `meta`.
    #[must_use]
    pub fn meta(mut self, meta: Value) -> Self {
        self.meta = Some(meta);
        self
    }

    /// The line as its JSON text, with no line feed. Keys inside ACP payloads come out sorted.
    ///
    /// # Errors
    ///
    /// [`HistoryError::Shape`] when the line is not one the format accepts; the writer's check is
    /// the reader's.
    pub fn to_json(&self) -> Result<String, HistoryError> {
        // Measure before copying anything: a value nested deeper than the parser reads is refused
        // here, so no walk of the line recurses on it.
        if self.payload_depth() > MAX_WRITE_DEPTH {
            return Err(HistoryError::Shape {
                line: 0,
                pointer: String::new(),
                expected: "a line nested no deeper than 128 levels",
            });
        }
        let mut envelope = Map::new();
        envelope.insert("method".into(), Value::String(self.body.method().into()));
        if let Some(params) = params_of(&self.body) {
            envelope.insert("params".into(), params);
        }
        if let Some(at) = &self.at {
            envelope.insert("at".into(), Value::String(at.clone()));
        }
        if let Some(meta) = &self.meta {
            envelope.insert("_meta".into(), meta.clone());
        }
        crate::canon::to_string(
            &Value::Object(envelope),
            &["method", "params", "at", "_meta"],
        )
        .ok_or(HistoryError::Shape {
            line: 0,
            pointer: String::new(),
            expected: "a line nested no deeper than 128 levels",
        })
    }

    pub(crate) fn payload_depth(&self) -> u64 {
        let deepest = |values: &[Value]| values.iter().map(crate::strict::depth).max().unwrap_or(0);
        let body = match &self.body {
            Body::Prompt { prompt } => deepest(prompt),
            Body::Update { update } => crate::strict::depth(update),
            Body::PermissionRequest { tool_call, options } => {
                crate::strict::depth(tool_call).max(deepest(options))
            }
            Body::PermissionAnswer { outcome, .. } => crate::strict::depth(outcome),
            _ => 0,
        };
        let meta = self.meta.as_ref().map_or(0, crate::strict::depth);
        body.max(meta) + 2
    }

    /// Checks a parsed body line.
    pub(crate) fn from_value(value: &Value, line: u64) -> Result<Self, HistoryError> {
        let root = Fields::root(value, line)?;
        if root.has("format") {
            return Err(HistoryError::Order {
                line,
                rule: crate::OrderRule::HeaderRepeated,
            });
        }
        let Some(Value::String(method)) = root.raw("method") else {
            return Err(HistoryError::UnknownLine { line });
        };
        let method = method.as_str();
        if !matches!(
            method,
            "session/prompt"
                | "session/update"
                | "session/cancel"
                | "session/request_permission"
                | "history/permission_answer"
                | "history/turn_end"
                | "history/compaction"
        ) {
            return Err(HistoryError::UnknownLine { line });
        }
        root.only(&["method", "params", "at", "_meta"])?;

        let at = root.string("at", 40, "an RFC 3339 timestamp")?;
        if at.is_some_and(|t| !is_timestamp(t)) {
            return Err(root.shape("at", "an RFC 3339 timestamp"));
        }
        let meta = match root.raw("_meta") {
            None => None,
            Some(v @ Value::Object(_)) => Some(v.clone()),
            Some(_) => return Err(root.shape("_meta", "an object")),
        };

        let body = if method == "session/cancel" {
            if root.has("params") {
                return Err(HistoryError::UnknownField {
                    line,
                    pointer: String::new(),
                });
            }
            Body::Cancel
        } else {
            let params = root.require_object("params")?;
            body_of(method, &params)?
        };
        Ok(Self {
            at: at.map(str::to_owned),
            meta,
            body,
        })
    }
}

fn body_of(method: &str, params: &Fields<'_>) -> Result<Body, HistoryError> {
    Ok(match method {
        "session/prompt" => {
            params.only(&["prompt"])?;
            let blocks = params.tagged_array("prompt", "type", true)?;
            Body::Prompt {
                prompt: blocks.clone(),
            }
        }
        "session/update" => {
            params.only(&["update"])?;
            let update = params.require_object("update")?;
            update.require_string("sessionUpdate", 64, "a non-empty string")?;
            Body::Update {
                update: params.raw("update").cloned().unwrap_or(Value::Null),
            }
        }
        "session/request_permission" => {
            params.only(&["toolCall", "options"])?;
            let call = params.require_object("toolCall")?;
            call.require_string("toolCallId", 512, "a non-empty string")?;
            let options = params.tagged_array("options", "optionId", true)?;
            Body::PermissionRequest {
                tool_call: params.raw("toolCall").cloned().unwrap_or(Value::Null),
                options: options.clone(),
            }
        }
        "history/permission_answer" => {
            params.only(&["toolCallId", "outcome"])?;
            let id = params.require_string("toolCallId", 512, "a non-empty string")?;
            let outcome = params.require_object("outcome")?;
            match outcome.require_string("outcome", 16, "selected or cancelled")? {
                "selected" => {
                    outcome.require_string("optionId", 256, "a non-empty string")?;
                }
                "cancelled" => {}
                _ => return Err(outcome.shape("outcome", "selected or cancelled")),
            }
            Body::PermissionAnswer {
                tool_call_id: id.to_owned(),
                outcome: params.raw("outcome").cloned().unwrap_or(Value::Null),
            }
        }
        "history/turn_end" => {
            params.only(&["stopReason", "error"])?;
            let name = params.require_string("stopReason", 32, "a stop reason")?;
            let stop =
                Stop::from_name(name).ok_or_else(|| params.shape("stopReason", "a stop reason"))?;
            Body::TurnEnd {
                stop,
                error: params
                    .string("error", 65_536, "a non-empty string")?
                    .map(str::to_owned),
            }
        }
        "history/compaction" => {
            params.only(&["summary", "trigger", "preTokens"])?;
            let trigger = match params.string("trigger", 16, "auto or manual")? {
                None => None,
                Some("auto") => Some(Trigger::Auto),
                Some("manual") => Some(Trigger::Manual),
                Some(_) => return Err(params.shape("trigger", "auto or manual")),
            };
            Body::Compaction {
                summary: params
                    .string("summary", usize::MAX, "a non-empty string")?
                    .map(str::to_owned),
                trigger,
                pre_tokens: params.unsigned("preTokens")?,
            }
        }
        _ => {
            return Err(HistoryError::UnknownLine {
                line: params.line(),
            });
        }
    })
}

/// The `params` a body is written with; `None` for a line that has none.
fn params_of(body: &Body) -> Option<Value> {
    Some(match body {
        Body::Prompt { prompt } => json!({ "prompt": prompt }),
        Body::Update { update } => json!({ "update": update }),
        Body::Cancel => return None,
        Body::PermissionRequest { tool_call, options } => {
            json!({ "toolCall": tool_call, "options": options })
        }
        Body::PermissionAnswer {
            tool_call_id,
            outcome,
        } => {
            json!({ "toolCallId": tool_call_id, "outcome": outcome })
        }
        Body::TurnEnd { stop, error } => {
            let mut map = Map::new();
            map.insert("stopReason".into(), Value::String(stop.name().into()));
            if let Some(error) = error {
                map.insert("error".into(), Value::String(error.clone()));
            }
            Value::Object(map)
        }
        Body::Compaction {
            summary,
            trigger,
            pre_tokens,
        } => {
            let mut map = Map::new();
            if let Some(summary) = summary {
                map.insert("summary".into(), Value::String(summary.clone()));
            }
            if let Some(trigger) = trigger {
                let name = match trigger {
                    Trigger::Auto => "auto",
                    Trigger::Manual => "manual",
                };
                map.insert("trigger".into(), Value::String(name.into()));
            }
            if let Some(tokens) = pre_tokens {
                map.insert("preTokens".into(), Value::from(*tokens));
            }
            Value::Object(map)
        }
    })
}
