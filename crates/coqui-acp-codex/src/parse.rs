//! Codex's app-server messages as events.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use std::io;

use coqui_acp_harness::event::{
    Content, Diff, Event, FileChange, Limit, Location, OptionKind, Outcome, PermissionOption,
    PermissionRequest, PlanEntry, PlanStatus, Responder, Severity, Stop, Terminal, ToolCall,
    ToolContent, ToolKind, ToolStatus, TurnEnd,
};
use serde_json::{Value, json};

use crate::limits;

/// Writes one JSON-RPC message, as one line, to the app server.
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;

/// The answer to one of our requests, by its id as text: its `result`, or the error's message.
pub type Answer = (String, Result<Value, String>);

/// The id of the option that allows one call.
pub const ALLOW_ONCE: &str = "accept";
/// The id of the option that allows calls like it for the rest of the session.
pub const ALLOW_SESSION: &str = "accept_for_session";
/// The id of the option that allows the command and the rule Codex proposed for commands like it.
pub const ALLOW_RULE: &str = "accept_with_rule";
/// The id of the option that always allows an MCP tool.
pub const ALLOW_ALWAYS: &str = "accept_always";
/// The id of the option that refuses the call.
pub const REJECT: &str = "decline";

/// The longest title made from a command or a query, in bytes.
const TITLE_BYTES: usize = 256;

fn title(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let cut = coqui_acp_harness::lines::cut(first, TITLE_BYTES);
    if cut.len() < text.len() {
        format!("{cut}…")
    } else {
        cut.to_string()
    }
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// Reads Codex's app-server lines into [`Event`]s. A server request asking leave becomes an [`Event::Permission`]
/// whose responder writes the answer through the [`Writer`]; any other server request is refused at once.
pub struct Parser {
    writer: Writer,
    max_tools: usize,
    /// Items whose text arrived in pieces, so their whole copy is not told again.
    streamed: VecDeque<String>,
    /// Items started, oldest first, by id: their kind, for their deltas and results.
    items: BTreeMap<String, String>,
    order: VecDeque<String>,
    /// Commands whose output arrived in pieces.
    output_streamed: BTreeSet<String>,
    turn: Option<String>,
    /// The turn that ended last: `turn/start`'s answer can be handled after that turn's end.
    ended: Option<String>,
    interrupted: bool,
    /// The latest window the account reported as used up.
    exhausted: Option<Limit>,
    /// The error the running turn reported before its end.
    error: Option<Value>,
    answers: Vec<Answer>,
}

impl core::fmt::Debug for Parser {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Parser")
            .field("turn", &self.turn)
            .field("items", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl Parser {
    /// A parser that answers server requests through `writer`, keeping the last `max_tools` items it has seen
    /// started (any value is taken; 0 keeps none, and a delta of an item forgotten is told as text of its own).
    #[must_use]
    pub fn new(writer: Writer, max_tools: usize) -> Self {
        Self {
            writer,
            max_tools,
            streamed: VecDeque::new(),
            items: BTreeMap::new(),
            order: VecDeque::new(),
            output_streamed: BTreeSet::new(),
            turn: None,
            ended: None,
            interrupted: false,
            exhausted: None,
            error: None,
            answers: Vec::new(),
        }
    }

    /// The running turn's id, once `turn/started` has said it.
    #[must_use]
    pub fn turn(&self) -> Option<&str> {
        self.turn.as_deref()
    }

    /// Takes `turn` as the running turn, as `turn/start`'s answer names it, unless it has already ended.
    pub fn started(&mut self, turn: &str) {
        if self.ended.as_deref() != Some(turn) {
            self.turn = Some(turn.to_string());
        }
    }

    /// Marks the running turn as interrupted: its end reads as cancelled.
    pub fn interrupting(&mut self) {
        self.interrupted = true;
    }

    /// The answers to our own requests read since the last call.
    pub fn answers(&mut self) -> Vec<Answer> {
        core::mem::take(&mut self.answers)
    }

    fn keep(&mut self, id: &str, kind: &str) {
        if self.max_tools == 0 {
            return;
        }
        if !self.items.contains_key(id) {
            if self.order.len() >= self.max_tools
                && let Some(oldest) = self.order.pop_front()
            {
                self.items.remove(&oldest);
                self.output_streamed.remove(&oldest);
            }
            self.order.push_back(id.to_string());
        }
        self.items.insert(id.to_string(), kind.to_string());
    }

    fn streamed(&mut self, id: &str) {
        if !self.streamed.iter().any(|known| known == id) {
            if self.streamed.len() >= 64 {
                self.streamed.pop_front();
            }
            self.streamed.push_back(id.to_string());
        }
    }

    /// The events of one message of the app server.
    pub fn line(&mut self, message: &Value) -> Vec<Event> {
        let method = message.get("method").and_then(Value::as_str);
        match (method, message.get("id")) {
            (None, Some(id)) => {
                let answer = match message.get("error") {
                    Some(error) => Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the app server refused")
                        .to_string()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                self.answers.push((id_text(id), answer));
                Vec::new()
            }
            (Some(method), Some(id)) => {
                self.request(method, id, message.get("params").unwrap_or(&Value::Null))
            }
            (Some(method), None) => self.notification(method, message),
            (None, None) => Vec::new(),
        }
    }

    fn notification(&mut self, method: &str, message: &Value) -> Vec<Event> {
        let params = message.get("params").unwrap_or(&Value::Null);
        if let Some(window) = limits::exhausted(message) {
            self.exhausted = Some(window);
        }
        match method {
            "turn/started" => {
                self.turn = text(params, "/turn/id").map(str::to_string);
                Vec::new()
            }
            "item/started" => params
                .get("item")
                .map(|item| self.item(item, false, false))
                .unwrap_or_default(),
            "item/completed" => params
                .get("item")
                .map(|item| self.item(item, true, false))
                .unwrap_or_default(),
            "item/agentMessage/delta" => {
                let (Some(id), Some(delta)) = (text(params, "/itemId"), text(params, "/delta"))
                else {
                    return Vec::new();
                };
                self.streamed(id);
                vec![Event::MessageChunk {
                    message: id.to_string(),
                    text: delta.to_string(),
                }]
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let (Some(id), Some(delta)) = (text(params, "/itemId"), text(params, "/delta"))
                else {
                    return Vec::new();
                };
                self.streamed(id);
                vec![Event::ThoughtChunk {
                    message: id.to_string(),
                    text: delta.to_string(),
                }]
            }
            "item/commandExecution/outputDelta" => {
                let (Some(id), Some(delta)) = (text(params, "/itemId"), text(params, "/delta"))
                else {
                    return Vec::new();
                };
                self.output_streamed.insert(id.to_string());
                let mut terminal = Terminal::new(id);
                terminal.output = Some(delta.to_string());
                vec![Event::Terminal(terminal)]
            }
            "item/fileChange/outputDelta" | "item/mcpToolCall/progress" => {
                let id = text(params, "/itemId");
                let said = text(params, "/delta").or_else(|| text(params, "/message"));
                match (id, said) {
                    (Some(id), Some(said)) => vec![Event::ToolChunk {
                        id: id.to_string(),
                        content: ToolContent::Text(said.to_string()),
                    }],
                    _ => Vec::new(),
                }
            }
            "turn/plan/updated" => {
                let entries = params
                    .get("plan")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|step| {
                        let status = match step.get("status").and_then(Value::as_str)? {
                            "completed" => PlanStatus::Completed,
                            "inProgress" | "in_progress" => PlanStatus::InProgress,
                            _ => PlanStatus::Pending,
                        };
                        Some(PlanEntry::new(
                            step.get("step").and_then(Value::as_str)?,
                            status,
                        ))
                    })
                    .collect();
                vec![Event::Plan(entries)]
            }
            "thread/tokenUsage/updated" => limits::usage(message)
                .map(|usage| vec![Event::Usage(usage)])
                .unwrap_or_default(),
            "account/rateLimits/updated" => {
                let quota = limits::quota(message);
                if quota.is_empty() {
                    Vec::new()
                } else {
                    vec![Event::Quota(quota)]
                }
            }
            "error" => {
                if params.get("willRetry").and_then(Value::as_bool) != Some(true) {
                    self.error = params.get("error").cloned();
                }
                Vec::new()
            }
            "turn/completed" | "turn/failed" => {
                let end = self.end(method, message);
                self.ended = self
                    .turn
                    .take()
                    .or_else(|| text(params, "/turn/id").map(str::to_string));
                vec![Event::TurnEnd(end)]
            }
            "thread/compacted" => vec![Event::Compaction {
                id: text(params, "/turnId").unwrap_or("compaction").to_string(),
                summary: None,
            }],
            "thread/name/updated" => text(params, "/threadName")
                .or_else(|| text(params, "/name"))
                .map(|name| vec![Event::Title(name.to_string())])
                .unwrap_or_default(),
            "warning" | "configWarning" | "deprecationNotice" | "guardianWarning" => {
                let said = text(params, "/message")
                    .or_else(|| text(params, "/summary"))
                    .or_else(|| text(params, "/details"));
                let severity = if method == "deprecationNotice" {
                    Severity::Info
                } else {
                    Severity::Warning
                };
                said.map(|said| {
                    vec![Event::Notice {
                        severity,
                        title: said.to_string(),
                    }]
                })
                .unwrap_or_default()
            }
            "mcpServer/startupStatus/updated" if text(params, "/status") == Some("failed") => {
                let name = text(params, "/name").unwrap_or("an MCP server");
                let why = text(params, "/error")
                    .or_else(|| text(params, "/failureReason"))
                    .unwrap_or("no reason given");
                vec![Event::Notice {
                    severity: Severity::Warning,
                    title: format!("MCP server {name} did not start: {why}"),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn end(&mut self, method: &str, message: &Value) -> TurnEnd {
        let interrupted = core::mem::take(&mut self.interrupted);
        let reported = self.error.take();
        let error = limits::error_of(message).cloned().or(reported);
        if let Some(limit) = error
            .as_ref()
            .and_then(|error| limits::limit(error, self.exhausted.as_ref()))
        {
            return TurnEnd::Limited(limit);
        }
        let status = text(message, "/params/turn/status").unwrap_or(if method == "turn/failed" {
            "failed"
        } else {
            "completed"
        });
        match status {
            "interrupted" => TurnEnd::Stopped(Stop::Cancelled),
            "failed" if interrupted => TurnEnd::Stopped(Stop::Cancelled),
            "failed" => TurnEnd::Failed(
                error
                    .as_ref()
                    .and_then(|error| text(error, "/message"))
                    .unwrap_or("the turn failed")
                    .to_string(),
            ),
            _ => TurnEnd::Stopped(Stop::EndTurn),
        }
    }

    /// The items of an earlier turn, as history: the person's messages are told too.
    pub fn history(&mut self, turn: &Value) -> Vec<Event> {
        turn.get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|item| {
                let mut events = self.item(item, false, true);
                events.extend(self.item(item, true, true));
                events
            })
            .collect()
    }

    fn item(&mut self, item: &Value, done: bool, history: bool) -> Vec<Event> {
        let (Some(id), Some(kind)) = (text(item, "/id"), text(item, "/type")) else {
            return Vec::new();
        };
        let id = id.to_string();
        if !done {
            self.keep(&id, kind);
        }
        match kind {
            "userMessage" if history && done => {
                let mut events: Vec<Event> = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|block| {
                        user_content(block).map(|content| Event::UserChunk {
                            message: id.clone(),
                            content,
                        })
                    })
                    .collect();
                if !events.is_empty() {
                    events.push(Event::UserEnd { message: id });
                }
                events
            }
            "agentMessage" if done => {
                let mut events = Vec::new();
                if !self.streamed.iter().any(|known| *known == id)
                    && let Some(said) = text(item, "/text").filter(|said| !said.is_empty())
                {
                    events.push(Event::MessageChunk {
                        message: id.clone(),
                        text: said.to_string(),
                    });
                }
                events.push(Event::MessageEnd { message: id });
                events
            }
            "reasoning" if done => {
                let mut events = Vec::new();
                if !self.streamed.iter().any(|known| *known == id) {
                    let said: Vec<&str> = ["summary", "content"]
                        .iter()
                        .flat_map(|key| {
                            item.get(*key)
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                        })
                        .collect();
                    if said.is_empty() {
                        return Vec::new();
                    }
                    events.push(Event::ThoughtChunk {
                        message: id.clone(),
                        text: said.join("\n\n"),
                    });
                }
                events.push(Event::ThoughtEnd { message: id });
                events
            }
            "commandExecution" => self.command(&id, item, done),
            "fileChange" => {
                let mut call = file_change(&id, item);
                call.status = Some(status(item, done));
                vec![Event::Tool(call)]
            }
            "mcpToolCall" | "dynamicToolCall" => {
                let server = text(item, "/server").or_else(|| text(item, "/namespace"));
                let tool = text(item, "/tool").unwrap_or("tool");
                let mut call = ToolCall::new(id.clone());
                call.name = Some(match server {
                    Some(server) => format!("mcp__{server}__{tool}"),
                    None => tool.to_string(),
                });
                call.title = Some(match server {
                    Some(server) => format!("{server}: {tool}"),
                    None => tool.to_string(),
                });
                call.kind = Some(ToolKind::Other);
                call.input = item.get("arguments").cloned();
                call.status = Some(status(item, done));
                let mut events = Vec::new();
                if done {
                    let said = text(item, "/error/message")
                        .map(str::to_string)
                        .or_else(|| {
                            let blocks =
                                item.pointer("/result/content").and_then(Value::as_array)?;
                            let said: Vec<&str> = blocks
                                .iter()
                                .filter_map(|block| block.get("text").and_then(Value::as_str))
                                .collect();
                            (!said.is_empty()).then(|| said.join("\n"))
                        });
                    if let Some(said) = said {
                        events.push(Event::ToolChunk {
                            id: id.clone(),
                            content: ToolContent::Text(said),
                        });
                    }
                    call.output = item
                        .get("result")
                        .filter(|result| !result.is_null())
                        .cloned();
                }
                events.push(Event::Tool(call));
                events
            }
            "webSearch" => {
                let mut call = ToolCall::new(id);
                call.name = Some("web_search".into());
                call.kind = Some(ToolKind::Fetch);
                call.title = Some(
                    match text(item, "/query").filter(|query| !query.is_empty()) {
                        Some(query) => format!("Search the web for {}", title(query)),
                        None => "Search the web".to_string(),
                    },
                );
                call.status = Some(if done {
                    ToolStatus::Completed
                } else {
                    ToolStatus::InProgress
                });
                vec![Event::Tool(call)]
            }
            "imageView" => {
                let mut call = ToolCall::new(id);
                call.name = Some("view_image".into());
                call.kind = Some(ToolKind::Read);
                let path = text(item, "/path").unwrap_or_default();
                call.title = Some(format!("View {path}"));
                call.locations = Some(vec![Location::new(path, None)]);
                call.status = Some(if done {
                    ToolStatus::Completed
                } else {
                    ToolStatus::InProgress
                });
                vec![Event::Tool(call)]
            }
            "contextCompaction" if done => vec![Event::Compaction { id, summary: None }],
            _ => Vec::new(),
        }
    }

    fn command(&mut self, id: &str, item: &Value, done: bool) -> Vec<Event> {
        let command = text(item, "/command").unwrap_or_default();
        let mut call = ToolCall::new(id);
        call.name = Some("exec_command".into());
        call.kind = Some(ToolKind::Execute);
        call.title = Some(title(command));
        call.input = Some(json!({"command": command, "cwd": item.get("cwd")}));
        call.status = Some(status(item, done));
        call.content = Some(vec![ToolContent::Terminal(id.to_string())]);
        let mut terminal = Terminal::new(id);
        if done {
            if !self.output_streamed.remove(id)
                && let Some(output) =
                    text(item, "/aggregatedOutput").filter(|output| !output.is_empty())
            {
                terminal.output = Some(output.to_string());
            }
            let code = item
                .get("exitCode")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok());
            terminal.exit = Some(code);
            if text(item, "/status") == Some("declined") {
                call.content = Some(vec![
                    ToolContent::Terminal(id.to_string()),
                    ToolContent::Text("The command was declined.".into()),
                ]);
            }
        } else {
            terminal.command = Some(command.to_string());
        }
        if done {
            vec![Event::Terminal(terminal), Event::Tool(call)]
        } else {
            vec![Event::Tool(call), Event::Terminal(terminal)]
        }
    }

    fn request(&mut self, method: &str, id: &Value, params: &Value) -> Vec<Event> {
        let writer = Arc::clone(&self.writer);
        let id = id.clone();
        let reason = text(params, "/reason").map(str::to_string);
        let item = text(params, "/itemId")
            .or_else(|| text(params, "/callId"))
            .unwrap_or("approval")
            .to_string();
        match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let command = match params.get("command") {
                    Some(Value::String(command)) => command.clone(),
                    Some(Value::Array(words)) => words
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                let mut tool = ToolCall::new(item);
                tool.name = Some("exec_command".into());
                tool.kind = Some(ToolKind::Execute);
                tool.title = Some(title(&command));
                tool.input = Some(json!({"command": command, "cwd": params.get("cwd")}));
                let rule = params
                    .get("proposedExecpolicyAmendment")
                    .and_then(Value::as_array)
                    .filter(|rule| !rule.is_empty())
                    .cloned();
                let mut options = vec![
                    PermissionOption::new(ALLOW_ONCE, "Allow", OptionKind::AllowOnce),
                    PermissionOption::new(
                        ALLOW_SESSION,
                        "Allow for this session",
                        OptionKind::AllowAlways,
                    ),
                ];
                if let Some(rule) = &rule {
                    let words: Vec<&str> = rule.iter().filter_map(Value::as_str).collect();
                    options.push(PermissionOption::new(
                        ALLOW_RULE,
                        format!("Always allow `{}`", words.join(" ")),
                        OptionKind::AllowAlways,
                    ));
                }
                options.push(PermissionOption::new(
                    REJECT,
                    "Reject",
                    OptionKind::RejectOnce,
                ));
                let legacy = method == "execCommandApproval";
                let responder = Responder::new(move |outcome| {
                    let decision = match (outcome, legacy) {
                        (Outcome::Selected(chosen), false) if chosen == ALLOW_ONCE => {
                            json!("accept")
                        }
                        (Outcome::Selected(chosen), false) if chosen == ALLOW_SESSION => {
                            json!("acceptForSession")
                        }
                        (Outcome::Selected(chosen), false) if chosen == ALLOW_RULE => {
                            json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": rule.unwrap_or_default()}})
                        }
                        (Outcome::Cancelled, false) => json!("cancel"),
                        (_, false) => json!("decline"),
                        (Outcome::Selected(chosen), true) if chosen == ALLOW_ONCE => {
                            json!("approved")
                        }
                        (Outcome::Selected(chosen), true) if chosen == ALLOW_SESSION => {
                            json!("approved_for_session")
                        }
                        (Outcome::Cancelled, true) => json!("abort"),
                        (_, true) => json!({"denied": {"rejection": "The person refused this."}}),
                    };
                    writer(&json!({"id": id, "result": {"decision": decision}}))
                });
                let ask = format!("Run {}", title(&command));
                vec![Event::Permission(PermissionRequest::new(
                    tool, ask, reason, options, responder,
                ))]
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                let mut tool = match self.items.get(&item).map(String::as_str) {
                    Some("fileChange") => ToolCall::new(item.clone()),
                    _ => legacy_file_change(&item, params),
                };
                tool.kind = Some(ToolKind::Edit);
                if tool.title.is_none() {
                    tool.title = Some("Edit files".into());
                }
                let options = vec![
                    PermissionOption::new(ALLOW_ONCE, "Allow", OptionKind::AllowOnce),
                    PermissionOption::new(
                        ALLOW_SESSION,
                        "Allow for this session",
                        OptionKind::AllowAlways,
                    ),
                    PermissionOption::new(REJECT, "Reject", OptionKind::RejectOnce),
                ];
                let legacy = method == "applyPatchApproval";
                let responder = Responder::new(move |outcome| {
                    let decision = match (outcome, legacy) {
                        (Outcome::Selected(chosen), false) if chosen == ALLOW_ONCE => {
                            json!("accept")
                        }
                        (Outcome::Selected(chosen), false) if chosen == ALLOW_SESSION => {
                            json!("acceptForSession")
                        }
                        (Outcome::Cancelled, false) => json!("cancel"),
                        (_, false) => json!("decline"),
                        (Outcome::Selected(chosen), true) if chosen == ALLOW_ONCE => {
                            json!("approved")
                        }
                        (Outcome::Selected(chosen), true) if chosen == ALLOW_SESSION => {
                            json!("approved_for_session")
                        }
                        (Outcome::Cancelled, true) => json!("abort"),
                        (_, true) => json!({"denied": {"rejection": "The person refused this."}}),
                    };
                    writer(&json!({"id": id, "result": {"decision": decision}}))
                });
                let ask = match text(params, "/grantRoot") {
                    Some(root) => format!("Edit files, and write under {root} from now on"),
                    None => "Edit files".to_string(),
                };
                vec![Event::Permission(PermissionRequest::new(
                    tool, ask, reason, options, responder,
                ))]
            }
            "item/permissions/requestApproval" => {
                let requested = params
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let mut tool = ToolCall::new(item);
                tool.name = Some("permissions".into());
                tool.kind = Some(ToolKind::Other);
                tool.title = Some("More permissions".into());
                tool.input = Some(requested.clone());
                let options = vec![
                    PermissionOption::new(ALLOW_ONCE, "Allow for this turn", OptionKind::AllowOnce),
                    PermissionOption::new(
                        ALLOW_SESSION,
                        "Allow for this session",
                        OptionKind::AllowAlways,
                    ),
                    PermissionOption::new(REJECT, "Reject", OptionKind::RejectOnce),
                ];
                let responder = Responder::new(move |outcome| {
                    let result = match outcome {
                        Outcome::Selected(chosen) if chosen == ALLOW_ONCE => {
                            json!({"permissions": requested, "scope": "turn"})
                        }
                        Outcome::Selected(chosen) if chosen == ALLOW_SESSION => {
                            json!({"permissions": requested, "scope": "session"})
                        }
                        _ => json!({"permissions": {}, "scope": "turn"}),
                    };
                    writer(&json!({"id": id, "result": result}))
                });
                vec![Event::Permission(PermissionRequest::new(
                    tool,
                    "Grant more permissions",
                    reason,
                    options,
                    responder,
                ))]
            }
            "mcpServer/elicitation/request"
                if text(params, "/_meta/codex_approval_kind") == Some("mcp_tool_call") =>
            {
                let persist: Vec<&str> = match params.pointer("/_meta/persist") {
                    Some(Value::String(one)) => vec![one.as_str()],
                    Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
                    _ => Vec::new(),
                };
                let server = text(params, "/serverName").unwrap_or("an MCP server");
                let call = self
                    .order
                    .iter()
                    .rev()
                    .find(|known| self.items.get(*known).map(String::as_str) == Some("mcpToolCall"))
                    .cloned();
                let mut tool = ToolCall::new(call.unwrap_or(item));
                tool.kind = Some(ToolKind::Other);
                tool.input = params.pointer("/_meta/tool_params").cloned();
                let mut options = vec![PermissionOption::new(
                    ALLOW_ONCE,
                    "Allow",
                    OptionKind::AllowOnce,
                )];
                if persist.contains(&"session") {
                    options.push(PermissionOption::new(
                        ALLOW_SESSION,
                        "Allow for this session",
                        OptionKind::AllowAlways,
                    ));
                }
                if persist.contains(&"always") {
                    options.push(PermissionOption::new(
                        ALLOW_ALWAYS,
                        "Always allow",
                        OptionKind::AllowAlways,
                    ));
                }
                options.push(PermissionOption::new(
                    REJECT,
                    "Reject",
                    OptionKind::RejectOnce,
                ));
                let responder = Responder::new(move |outcome| {
                    let result = match outcome {
                        Outcome::Selected(chosen) if chosen == ALLOW_ONCE => {
                            json!({"action": "accept", "content": null, "_meta": null})
                        }
                        Outcome::Selected(chosen) if chosen == ALLOW_SESSION => {
                            json!({"action": "accept", "content": null, "_meta": {"persist": "session"}})
                        }
                        Outcome::Selected(chosen) if chosen == ALLOW_ALWAYS => {
                            json!({"action": "accept", "content": null, "_meta": {"persist": "always"}})
                        }
                        Outcome::Selected(_) => json!({"action": "decline", "content": null}),
                        _ => json!({"action": "cancel", "content": null}),
                    };
                    writer(&json!({"id": id, "result": result}))
                });
                let ask = text(params, "/message")
                    .map_or_else(|| format!("Run a tool of {server}"), str::to_string);
                vec![Event::Permission(PermissionRequest::new(
                    tool, ask, reason, options, responder,
                ))]
            }
            _ => {
                let _ = writer(
                    &json!({"id": id, "error": {"code": -32601, "message": "this client does not take that request"}}),
                );
                Vec::new()
            }
        }
    }
}

/// A request or answer id as text, for matching.
#[must_use]
pub fn id_text(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn status(item: &Value, done: bool) -> ToolStatus {
    match text(item, "/status") {
        Some("completed") => ToolStatus::Completed,
        Some("failed" | "declined") => ToolStatus::Failed,
        _ if done => ToolStatus::Completed,
        _ => ToolStatus::InProgress,
    }
}

fn user_content(block: &Value) -> Option<Content> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => Some(Content::Text(block.get("text")?.as_str()?.to_string())),
        "image" => {
            let url = block.get("url")?.as_str()?;
            let (head, data) = url.strip_prefix("data:")?.split_once(',')?;
            let mime_type = head.strip_suffix(";base64")?;
            Some(Content::Image {
                mime_type: mime_type.to_string(),
                data: data.to_string(),
            })
        }
        _ => None,
    }
}

/// A `fileChange` item as an edit: a diff per change, its patch made from Codex's hunks.
fn file_change(id: &str, item: &Value) -> ToolCall {
    let changes = item
        .get("changes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut call = ToolCall::new(id);
    call.name = Some("apply_patch".into());
    call.kind = Some(ToolKind::Edit);
    let paths: Vec<String> = changes
        .iter()
        .filter_map(|change| text(change, "/path").map(str::to_string))
        .collect();
    call.title = Some(match paths.as_slice() {
        [one] => format!("Edit {one}"),
        [] => "Edit files".to_string(),
        many => format!("Edit {} files", many.len()),
    });
    call.locations = Some(
        paths
            .iter()
            .map(|path| Location::new(path.clone(), None))
            .collect(),
    );
    call.content = Some(
        changes
            .iter()
            .filter_map(|change| {
                let path = text(change, "/path")?;
                let kind = match text(change, "/kind/type") {
                    Some("add") => FileChange::Add,
                    Some("delete") => FileChange::Delete,
                    _ => FileChange::Modify,
                };
                let mut diff = Diff::new(path, kind);
                if let Some(hunks) = text(change, "/diff").filter(|hunks| !hunks.is_empty()) {
                    diff.patch = Some(git_patch(path, kind, hunks));
                }
                Some(ToolContent::Diff(diff))
            })
            .collect(),
    );
    call
}

/// An `applyPatchApproval`'s changes, by path, as an edit.
fn legacy_file_change(id: &str, params: &Value) -> ToolCall {
    let mut call = ToolCall::new(id);
    let paths: Vec<String> = params
        .get("fileChanges")
        .and_then(Value::as_object)
        .map(|changes| changes.keys().cloned().collect())
        .unwrap_or_default();
    call.title = Some(match paths.as_slice() {
        [one] => format!("Edit {one}"),
        _ => "Edit files".to_string(),
    });
    call.locations = Some(
        paths
            .into_iter()
            .map(|path| Location::new(path, None))
            .collect(),
    );
    call
}

/// A git patch of one file from Codex's hunks, which start at `@@`. The path keeps its spelling with forward slashes.
#[must_use]
pub fn git_patch(path: &str, change: FileChange, hunks: &str) -> String {
    let path = path.replace('\\', "/");
    let name = if path
        .bytes()
        .any(|byte| byte == b'"' || byte == b'\\' || byte < 0x20)
    {
        format!("{path:?}")
    } else {
        path
    };
    let mut patch = format!("diff --git {name} {name}\n");
    match change {
        FileChange::Add => patch.push_str(&format!(
            "new file mode 100644\n--- /dev/null\n+++ {name}\n"
        )),
        FileChange::Delete => patch.push_str(&format!(
            "deleted file mode 100644\n--- {name}\n+++ /dev/null\n"
        )),
        _ => patch.push_str(&format!("--- {name}\n+++ {name}\n")),
    }
    patch.push_str(hunks);
    if !hunks.ends_with('\n') {
        patch.push('\n');
    }
    patch
}
