//! A session recorded from the messages a client exchanges with any ACP agent, so a session of an
//! agent with no transcript of its own can be exported.
//!
//! [`SessionRecorder`] is fed every JSON-RPC message the client sends and receives, in the order it
//! sees them, and builds a [`Document`]: the prompts, the agent's updates exactly as it sent them
//! (version 1 and version 2 alike), cancels, permission requests and the client's answers, stop
//! reasons, and the compaction boundaries version 2 reports. It reads only the messages of one
//! session, ignores what it does not know and never changes an update.
//!
//! Recording is passive: a message the recorder cannot use is skipped and counted, never an error
//! for the connection it watches.

use alloc::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::conversation::{Recorder, Stamp, valid_time};
use crate::{
    Body, DEFAULT_MAX_LINE_BYTES, DEFAULT_MAX_LINES, Document, Header, HistoryError, Line, Origin,
    Stop,
};

/// The longest error text kept for a failed turn, in bytes.
const MAX_ERROR_BYTES: usize = 512;

/// Which way a message went, as the client sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Direction {
    /// The client sent it to the agent.
    Sent,
    /// The client received it from the agent.
    Received,
}

/// Builds the history of one session from the JSON-RPC messages of a client's connection.
#[derive(Debug)]
pub struct SessionRecorder {
    header: Header,
    recorder: Recorder,
    max_line_bytes: u64,
    max_lines: u64,
    session: Option<String>,
    /// Requests the client sent that are not answered, by request id.
    initializing: BTreeSet<String>,
    opening: BTreeMap<String, Option<String>>,
    modes: BTreeMap<String, String>,
    /// The prompt whose answer ends the open turn.
    prompt: Option<String>,
    /// The agent's permission requests not yet answered: request id to tool call id.
    asked: BTreeMap<String, String>,
    /// Tool calls with a permission request waiting in the open turn.
    waiting: BTreeSet<String>,
    /// Version 2 compaction summaries being told, by compaction id.
    compactions: BTreeMap<String, String>,
    skipped: u64,
    truncated: bool,
}

impl SessionRecorder {
    /// A recorder of the session the host names `id` (a UUID), started by the harness `origin`
    /// names.
    #[must_use]
    pub fn new(id: impl Into<String>, origin: Origin) -> Self {
        Self {
            header: Header::new(id, origin),
            recorder: Recorder::new(),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_lines: DEFAULT_MAX_LINES,
            session: None,
            initializing: BTreeSet::new(),
            opening: BTreeMap::new(),
            modes: BTreeMap::new(),
            prompt: None,
            asked: BTreeMap::new(),
            waiting: BTreeSet::new(),
            compactions: BTreeMap::new(),
            skipped: 0,
            truncated: false,
        }
    }

    /// Skips a message whose line would be longer than `bytes` (default
    /// [`DEFAULT_MAX_LINE_BYTES`]).
    #[must_use]
    pub fn max_line_bytes(mut self, bytes: u64) -> Self {
        self.max_line_bytes = bytes;
        self
    }

    /// Stops recording at `lines` lines, the header included (default [`DEFAULT_MAX_LINES`]).
    #[must_use]
    pub fn max_lines(mut self, lines: u64) -> Self {
        self.max_lines = lines;
        self
    }

    /// How many messages were left out because their line was over the size limit or the format
    /// refuses it.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Whether recording stopped at the line limit: the history is cut short.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Records `message`, which went the way `direction` says.
    pub fn observe(&mut self, direction: Direction, message: &Value) {
        self.observe_at(direction, message, None);
    }

    /// Records `message` with the time it happened, RFC 3339; a time the format refuses is left
    /// out.
    pub fn observe_at(&mut self, direction: Direction, message: &Value, at: Option<&str>) {
        let Some(object) = message.as_object() else {
            return;
        };
        let at = at.filter(|at| valid_time(at)).map(str::to_owned);
        if self.header.created_at.is_none() {
            self.header.created_at.clone_from(&at);
        }
        let stamp = Stamp::at(at);
        let id = object
            .get("id")
            .filter(|id| id.is_string() || id.is_number());
        let id = id.map(Value::to_string);
        let method = object.get("method").and_then(Value::as_str);
        let params = object.get("params").unwrap_or(&Value::Null);
        match (direction, method, id) {
            (Direction::Sent, Some(method), id) => self.sent(method, id, params, stamp),
            (Direction::Received, Some(method), id) => self.asked_of(method, id, params, stamp),
            (Direction::Received, None, Some(id)) => self.answer_received(&id, object, stamp),
            (Direction::Sent, None, Some(id)) => self.answer_sent(&id, object, stamp),
            (_, None, None) => {}
        }
    }

    /// The history so far, the turn left open if it is open: a recording cut short.
    ///
    /// # Errors
    ///
    /// The history is not one the format accepts, which only a header field the format refuses
    /// can cause.
    pub fn finish(self) -> Result<Document, HistoryError> {
        let mut doc = Document::new(self.header);
        doc.lines = self.recorder.finish();
        doc.check()?;
        Ok(doc)
    }

    fn belongs(&self, params: &Value) -> bool {
        match (
            &self.session,
            params.get("sessionId").and_then(Value::as_str),
        ) {
            (Some(ours), Some(theirs)) => ours == theirs,
            (None, Some(_)) => true,
            (_, None) => false,
        }
    }

    fn adopt(&mut self, session: &str) {
        if self.session.is_none() && !session.is_empty() {
            self.session = Some(session.to_owned());
            self.header.origin.session_id = Some(session.to_owned());
        }
    }

    /// Whether another line fits; counts the one it refuses.
    fn room(&mut self) -> bool {
        // The header is the first line.
        if (self.recorder.len() as u64).saturating_add(1) >= self.max_lines {
            self.truncated = true;
            return false;
        }
        true
    }

    /// Whether `line` is one the format accepts, as the reader would read it, and within the size
    /// limit; counts the one it refuses.
    fn fits(&mut self, line: &Line) -> bool {
        let accepted = line.to_json().is_ok_and(|json| {
            (json.len() as u64) <= self.max_line_bytes
                && serde_json::from_str::<Value>(&json)
                    .is_ok_and(|value| Line::from_value(value, 2).is_ok())
        });
        if !accepted {
            self.skipped += 1;
        }
        accepted
    }

    fn sent(&mut self, method: &str, id: Option<String>, params: &Value, stamp: Stamp) {
        match (method, id) {
            ("initialize", Some(id)) => {
                self.initializing.insert(id);
            }
            ("session/new" | "session/fork", Some(id)) => {
                let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_owned);
                self.opening.insert(id, cwd);
            }
            ("session/load" | "session/resume", Some(id)) => {
                if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
                    self.adopt(session);
                }
                let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_owned);
                self.opening.insert(id, cwd);
            }
            ("session/set_mode", Some(id)) => {
                if self.belongs(params)
                    && let Some(mode) = params.get("modeId").and_then(Value::as_str)
                {
                    self.modes.insert(id, mode.to_owned());
                }
            }
            ("session/prompt", Some(id)) => self.prompted(id, params, stamp),
            ("session/cancel", None) if self.belongs(params) && self.room() => {
                self.recorder.cancel(stamp);
            }
            _ => {}
        }
    }

    fn prompted(&mut self, id: String, params: &Value, stamp: Stamp) {
        if !self.belongs(params) {
            return;
        }
        let Some(blocks) = params.get("prompt").and_then(Value::as_array) else {
            return;
        };
        if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
            self.adopt(session);
        }
        if !self.room() || !self.fits(&Line::prompt(blocks.clone())) {
            return;
        }
        if self.recorder.is_open() {
            // The recorder ends the turn the agent never answered.
            self.waiting.clear();
        }
        self.recorder.prompt(blocks.clone(), stamp);
        self.prompt = Some(id);
    }

    fn asked_of(&mut self, method: &str, id: Option<String>, params: &Value, stamp: Stamp) {
        match method {
            "session/update" if id.is_none() && self.belongs(params) => {
                if let Some(update) = params.get("update") {
                    self.told(update, stamp);
                }
            }
            "session/request_permission" => {
                let (Some(id), true) = (id, self.belongs(params)) else {
                    return;
                };
                let Some(tool_call) = params.get("toolCall").filter(|call| call.is_object()) else {
                    return;
                };
                let Some(options) = params.get("options").and_then(Value::as_array) else {
                    return;
                };
                let call_id = tool_call
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !self.recorder.is_open() || call_id.is_empty() || self.waiting.contains(&call_id)
                {
                    return;
                }
                let line = Line::new(Body::PermissionRequest {
                    tool_call: tool_call.clone(),
                    options: options.clone(),
                });
                if !self.room() || !self.fits(&line) {
                    return;
                }
                self.recorder
                    .permission_request(tool_call.clone(), options.clone(), stamp);
                self.waiting.insert(call_id.clone());
                self.asked.insert(id, call_id);
            }
            _ => {}
        }
    }

    /// The client answered a request of the agent: only a permission is recorded.
    fn answer_sent(&mut self, id: &str, object: &serde_json::Map<String, Value>, stamp: Stamp) {
        let Some(call_id) = self.asked.remove(id) else {
            return;
        };
        if !self.waiting.remove(&call_id) {
            return;
        }
        let outcome = object
            .get("result")
            .and_then(|result| result.get("outcome"))
            .filter(|outcome| outcome.is_object())
            .cloned()
            .unwrap_or_else(|| json!({"outcome": "cancelled"}));
        let line = Line::new(Body::PermissionAnswer {
            tool_call_id: call_id.clone(),
            outcome: outcome.clone(),
        });
        if !self.room() || !self.fits(&line) {
            // The request stays unanswered, which the format reads as cancelled.
            return;
        }
        self.recorder.permission_answer(call_id, outcome, stamp);
    }

    /// The agent answered a request of the client.
    fn answer_received(&mut self, id: &str, object: &serde_json::Map<String, Value>, stamp: Stamp) {
        let result = object.get("result");
        if self.initializing.remove(id) {
            if let Some(result) = result {
                if let Some(version) = result.get("protocolVersion").and_then(Value::as_u64) {
                    self.header.acp_version = Some(version);
                }
                if let Some(version) = result.pointer("/agentInfo/version").and_then(Value::as_str)
                    && self.header.origin.version.is_none()
                {
                    self.header.origin.version = Some(version.to_owned());
                }
            }
            return;
        }
        if let Some(cwd) = self.opening.remove(id) {
            if let Some(result) = result {
                if let Some(session) = result.get("sessionId").and_then(Value::as_str) {
                    self.adopt(session);
                }
                if let Some(mode) = result
                    .pointer("/modes/currentModeId")
                    .and_then(Value::as_str)
                {
                    self.header.mode = Some(mode.to_owned());
                }
                if let Some(model) = result
                    .pointer("/models/currentModelId")
                    .and_then(Value::as_str)
                {
                    self.header.model = Some(model.to_owned());
                }
                if self.header.cwd.is_none()
                    && let Some(cwd) = cwd
                {
                    let mut candidate = self.header.clone();
                    candidate.cwd = Some(cwd.clone());
                    if candidate.to_line().is_ok() {
                        self.header.cwd = Some(cwd);
                    }
                }
            }
            return;
        }
        if let Some(mode) = self.modes.remove(id) {
            if result.is_some() && self.header.mode.as_deref() != Some(mode.as_str()) {
                let update = json!({"sessionUpdate": "current_mode_update", "currentModeId": mode});
                self.told(&update, stamp);
            }
            return;
        }
        if self.prompt.as_deref() == Some(id) {
            self.prompt = None;
            let (stop, error) = match (result, object.get("error")) {
                (Some(result), _) => {
                    match result
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .and_then(Stop::from_name)
                    {
                        Some(stop) => (stop, None),
                        None => (
                            Stop::Error,
                            Some("the agent ended the turn without a stop reason".to_owned()),
                        ),
                    }
                }
                (None, Some(error)) => {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the agent refused the prompt");
                    (Stop::Error, Some(cut(message, MAX_ERROR_BYTES)))
                }
                (None, None) => return,
            };
            if self.recorder.is_open() && self.room() {
                self.recorder.end_turn(stop, error, stamp);
                self.waiting.clear();
                self.asked.clear();
            }
        }
    }

    /// One `session/update` of the agent.
    fn told(&mut self, update: &Value, stamp: Stamp) {
        let Some(kind) = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .filter(|kind| !kind.is_empty())
        else {
            return;
        };
        if !self.room() || !self.fits(&Line::update(update.clone())) {
            return;
        }
        match kind {
            "current_mode_update" => {
                if let Some(mode) = update.get("currentModeId").and_then(Value::as_str) {
                    self.header.mode = Some(mode.to_owned());
                }
            }
            "compaction_summary_chunk" => {
                if let (Some(id), Some(text)) = (
                    update.get("compactionId").and_then(Value::as_str),
                    update.pointer("/content/text").and_then(Value::as_str),
                ) {
                    self.compactions
                        .entry(id.to_owned())
                        .or_default()
                        .push_str(text);
                }
            }
            _ => {}
        }
        self.recorder.update(update.clone(), stamp.clone());
        if kind == "compaction_update" {
            self.compacted(update, stamp);
        }
    }

    /// A version 2 compaction that completed becomes the format's boundary, with its summary.
    fn compacted(&mut self, update: &Value, stamp: Stamp) {
        let Some(id) = update.get("compactionId").and_then(Value::as_str) else {
            return;
        };
        if update.get("status").and_then(Value::as_str) != Some("completed") {
            return;
        }
        let told: String = update
            .get("summary")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .or_else(|| self.compactions.remove(id))
            .unwrap_or_default();
        self.compactions.remove(id);
        let summary = (!told.is_empty()).then_some(told);
        let line = Line::new(Body::Compaction {
            summary: summary.clone(),
            trigger: None,
            pre_tokens: None,
        });
        if self.room() && self.fits(&line) {
            self.recorder.compaction(summary, None, None, stamp);
        }
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
