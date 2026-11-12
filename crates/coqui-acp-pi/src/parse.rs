//! Pi's RPC lines as events.
//!
//! Pi writes one JSON object per line: responses to the commands this crate sent, and the events of a run. A run ends at
//! `agent_settled`, not at `agent_end`: a retry, a compaction or a queued message may follow `agent_end`. Pi asks nobody
//! before it runs a tool; the permission gate (see [`crate::GATE_SOURCE`]) puts each call to the parser as an extension
//! dialog, which the parser answers by the mode, or turns into a permission request.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use coqui_acp_api::usage::{DEFAULT_LIMIT_PHRASES, infer_limit};
use coqui_acp_harness::event::{
    Content, Diff, Event, FileChange, Location, OptionKind, Outcome, PermissionOption,
    PermissionRequest, Responder, Severity, Stop, Terminal, ToolCall, ToolContent, ToolKind,
    ToolStatus, TurnEnd, Usage,
};
use coqui_acp_harness::lines::{cut, one_line};
use serde_json::{Map, Value, json};

use crate::gate;
use crate::mode::{Decision, Policy, Scope};

/// Writes one line to Pi.
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;

/// A response to a command of ours: its id, and its data or Pi's refusal.
pub type Answer = (String, Result<Value, String>);

/// The option that lets this call run.
pub const ALLOW: &str = "allow";
/// The option that lets this tool run for the rest of the session.
pub const ALLOW_TOOL: &str = "allow_tool";
/// The option that refuses this call.
pub const REJECT: &str = "reject";

/// What the parser keeps and cuts.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct ParserLimits {
    /// The longest a title is, in bytes. Default 256.
    pub max_title_bytes: usize,
    /// The most of a tool's output text kept for a client, in bytes. Default 16 KiB.
    pub max_told_bytes: usize,
    /// The largest file read for a write's old content, in bytes. Default 1 MiB.
    pub max_old_bytes: u64,
}

impl ParserLimits {
    /// The defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_title_bytes: 256,
            max_told_bytes: 16 * 1024,
            max_old_bytes: 1024 * 1024,
        }
    }
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self::new()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// The text parts of a message's or a result's `content`, joined; a string is itself.
#[must_use]
pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(whole) => whole.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One tool call under way.
#[derive(Debug)]
struct Tool {
    /// The id the client knows it by: Pi's, made unique in the session.
    id: String,
    name: String,
    args: Value,
    kind: ToolKind,
    title: String,
    locations: Vec<Location>,
    /// What the client is shown of it, kept to be sent again with a patch.
    content: Vec<ToolContent>,
    /// The bytes of a command's output already sent.
    sent: usize,
    started: bool,
    asked: bool,
    finished: bool,
}

/// How an unfinished run stands.
#[derive(Debug, Default)]
struct Run {
    /// A run of ours is under way.
    active: bool,
    /// Pi has said `agent_start`.
    began: bool,
    /// 0 while it runs, 1 once the client asked to stop it, 2 once it is over. Shared with the questions asked in it, so
    /// that a question answered cancelled stops it, and one from an earlier run does not stop this one.
    state: Arc<AtomicU8>,
    /// Pi is waiting to retry a failed request.
    retrying: bool,
    /// The last assistant message's stop reason and error.
    last: Option<(String, Option<String>)>,
}

/// Pi's lines as events, for one session. Not thread-safe by itself; the session holds it behind a lock.
pub struct Parser {
    writer: Writer,
    policy: Arc<Mutex<Policy>>,
    /// The gate dialogs shown to a client and not yet answered.
    open: Arc<Mutex<BTreeSet<String>>>,
    limits: ParserLimits,
    max_tools: usize,
    cwd: PathBuf,
    roots: Vec<PathBuf>,
    /// Pi's agent directories, where it keeps its credentials and the code it loads.
    protected: Vec<PathBuf>,
    private: Vec<PathBuf>,
    /// The home directory Pi expands `~` to, when it is not the process's own.
    home: Option<PathBuf>,
    tools: BTreeMap<String, Tool>,
    order: VecDeque<String>,
    /// The ids given out to tool calls, so that a model that repeats one gets another.
    used: BTreeSet<String>,
    answers: Vec<Answer>,
    run: Run,
    /// The assistant messages seen, for message ids.
    messages: u64,
    /// The blocks of the current assistant message: id, whether it is thought, whether any of it was streamed.
    blocks: BTreeMap<u64, (String, bool, bool)>,
    /// The content indexes of the current assistant message that were seen.
    seen: BTreeSet<u64>,
    context_window: Option<u64>,
    cost: f64,
    compactions: u64,
}

impl core::fmt::Debug for Parser {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Parser")
            .field("run", &self.run)
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

impl Parser {
    /// A parser that writes its answers to Pi with `writer` and follows at most `max_tools` tool calls at once, in Manual
    /// mode.
    #[must_use]
    pub fn new(writer: Writer, max_tools: usize) -> Self {
        Self::with_limits(writer, max_tools, ParserLimits::new())
    }

    /// As [`Parser::new`], with these limits.
    #[must_use]
    pub fn with_limits(writer: Writer, max_tools: usize, limits: ParserLimits) -> Self {
        Self {
            writer,
            policy: Arc::new(Mutex::new(Policy::new(
                coqui_acp_harness::Mode::Manual,
                false,
            ))),
            open: Arc::new(Mutex::new(BTreeSet::new())),
            limits,
            max_tools,
            cwd: PathBuf::new(),
            roots: Vec::new(),
            protected: Vec::new(),
            private: Vec::new(),
            home: None,
            tools: BTreeMap::new(),
            order: VecDeque::new(),
            used: BTreeSet::new(),
            answers: Vec::new(),
            run: Run::default(),
            messages: 0,
            blocks: BTreeMap::new(),
            seen: BTreeSet::new(),
            context_window: None,
            cost: 0.0,
            compactions: 0,
        }
    }

    /// The policy that decides each call, shared with whoever sets the mode.
    #[must_use]
    pub fn with_policy(mut self, policy: Arc<Mutex<Policy>>) -> Self {
        self.policy = policy;
        self
    }
    /// Pi's agent directories: files of theirs that hold credentials are refused, and files in them are never written
    /// without a question, since Pi loads code and configuration from there.
    #[must_use]
    pub fn with_protected(mut self, agent_dirs: Vec<PathBuf>) -> Self {
        self.protected = agent_dirs;
        self
    }

    /// Directories of the translator's own (the launch's private one, which holds the gate): the model changes nothing in
    /// them, in any mode but Bypass.
    #[must_use]
    pub fn with_private(mut self, dirs: Vec<PathBuf>) -> Self {
        self.private = dirs;
        self
    }

    /// The home directory Pi expands `~` to: the one of the environment Pi is started with.
    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    fn scope(&self) -> Scope {
        let scope = Scope::new(&self.cwd, self.roots.clone())
            .with_agent_dirs(self.protected.clone())
            .with_private(self.private.clone());
        match &self.home {
            Some(home) => scope.with_home(Some(home.clone())),
            None => scope,
        }
    }

    /// The directory relative paths are against, and the directories a file may be read from for a write's old content.
    #[must_use]
    pub fn with_roots(mut self, cwd: &Path, roots: Vec<PathBuf>) -> Self {
        self.cwd = cwd.to_path_buf();
        self.roots = roots;
        self
    }

    /// The size of the model's context window, which the usage reports are against.
    pub const fn set_context_window(&mut self, size: Option<u64>) {
        self.context_window = size;
    }

    /// Whether a run of ours is under way.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.run.active
    }

    /// Whether Pi is waiting to retry a failed request.
    #[must_use]
    pub const fn retrying(&self) -> bool {
        self.run.retrying
    }

    /// A prompt is about to be sent: a run begins.
    pub fn prompting(&mut self) {
        self.run.state.store(2, Ordering::SeqCst);
        self.run = Run {
            active: true,
            ..Run::default()
        };
    }

    /// The prompt was not taken: no run begins.
    pub const fn abandon(&mut self) {
        self.run.active = false;
    }

    /// Pi took the prompt and ended it at once, without a run (a command of an extension): the turn is over.
    pub fn handled(&mut self) -> Vec<Event> {
        if self.run.active && !self.run.began {
            self.run.active = false;
            return vec![Event::TurnEnd(TurnEnd::stopped(Stop::EndTurn))];
        }
        Vec::new()
    }

    /// The person asked to stop the run: every dialog the client was shown is answered cancelled first, since Pi's abort
    /// does not end an open dialog. Whether a run was under way.
    pub fn interrupting(&mut self) -> bool {
        if !self.run.active {
            return false;
        }
        let _ = self
            .run
            .state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst);
        let open = core::mem::take(&mut *locked(&self.open));
        for dialog in open {
            let _ = (self.writer)(
                &json!({"type": "extension_ui_response", "id": dialog, "cancelled": true}),
            );
        }
        true
    }

    /// The responses to commands that were read since the last call.
    pub fn answers(&mut self) -> Vec<Answer> {
        core::mem::take(&mut self.answers)
    }

    /// One line of Pi's output as events.
    pub fn line(&mut self, message: &Value) -> Vec<Event> {
        let Some(kind) = message.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        match kind {
            "response" => self.response(message),
            "agent_start" => {
                self.run.active = true;
                self.run.began = true;
                Vec::new()
            }
            "message_start" => self.message_start(message),
            "message_update" => self.message_update(message),
            "message_end" => self.message_end(message),
            "tool_execution_start" => self.execution_start(message),
            "tool_execution_update" => self.execution_update(message),
            "tool_execution_end" => self.execution_end(message),
            "extension_ui_request" => self.dialog(message),
            "agent_settled" => {
                let aborted = message.get("aborted").and_then(Value::as_bool) == Some(true);
                self.settle(aborted)
            }
            "auto_retry_start" => {
                self.run.retrying = true;
                let why = text(message, "/errorMessage").unwrap_or("the request failed");
                vec![Event::Notice {
                    severity: Severity::Info,
                    title: format!(
                        "Pi is retrying (attempt {} of {}): {}",
                        message.get("attempt").and_then(Value::as_u64).unwrap_or(1),
                        message
                            .get("maxAttempts")
                            .and_then(Value::as_u64)
                            .unwrap_or(1),
                        self.short(&one_line(why)),
                    ),
                }]
            }
            "auto_retry_end" => {
                self.run.retrying = false;
                if message.get("success").and_then(Value::as_bool) != Some(true)
                    && let Some(why) = text(message, "/finalError")
                {
                    self.run.last = Some(("error".into(), Some(why.to_string())));
                }
                Vec::new()
            }
            "compaction_end" => self.compaction_end(message),
            "session_info_changed" => text(message, "/name")
                .map(|name| vec![Event::Title(self.short(name))])
                .unwrap_or_default(),
            "extension_error" => vec![Event::Notice {
                severity: Severity::Warning,
                title: format!(
                    "A Pi extension failed: {}",
                    self.short(&one_line(
                        text(message, "/error")
                            .or_else(|| text(message, "/message"))
                            .unwrap_or("no reason given")
                    ))
                ),
            }],
            _ => Vec::new(),
        }
    }

    fn short(&self, text: &str) -> String {
        cut(text, self.limits.max_title_bytes).to_string()
    }

    fn told(&self, text: &str) -> String {
        let kept = cut(text, self.limits.max_told_bytes);
        if kept.len() < text.len() {
            format!("{kept}\n… (cut)")
        } else {
            kept.to_string()
        }
    }

    fn response(&mut self, message: &Value) -> Vec<Event> {
        let ok = message.get("success").and_then(Value::as_bool) == Some(true);
        let result = if ok {
            Ok(message.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(text(message, "/error").unwrap_or("Pi refused").to_string())
        };
        match message.get("id").and_then(Value::as_str) {
            Some(id) => self.answers.push((id.to_string(), result)),
            None => {
                if let Err(why) = result {
                    return vec![Event::Notice {
                        severity: Severity::Warning,
                        title: format!("Pi refused a line: {}", self.short(&one_line(&why))),
                    }];
                }
            }
        }
        Vec::new()
    }

    // ---- messages -------------------------------------------------------------------------------------------

    fn message_id(&self, index: u64) -> String {
        format!("m{}.{index}", self.messages)
    }

    fn message_start(&mut self, message: &Value) -> Vec<Event> {
        if text(message, "/message/role") == Some("assistant") {
            self.messages += 1;
            self.blocks.clear();
            self.seen.clear();
        }
        Vec::new()
    }

    fn message_update(&mut self, message: &Value) -> Vec<Event> {
        let Some(update) = message.get("assistantMessageEvent") else {
            return Vec::new();
        };
        let index = update
            .get("contentIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        match update.get("type").and_then(Value::as_str) {
            Some("text_start") => self.open_block(index, false),
            Some("thinking_start") => self.open_block(index, true),
            Some("text_delta") => self.block_delta(index, false, text(update, "/delta")),
            Some("thinking_delta") => self.block_delta(index, true, text(update, "/delta")),
            Some("text_end") => self.block_end(index, false, text(update, "/content")),
            Some("thinking_end") => self.block_end(index, true, text(update, "/content")),
            Some("toolcall_start") => {
                let (Some(id), Some(name)) = (text(update, "/id"), text(update, "/toolName"))
                else {
                    return Vec::new();
                };
                self.seen.insert(index);
                self.announce(id, name, &Value::Null)
            }
            Some("toolcall_end") => {
                let Some(call) = update.get("toolCall") else {
                    return Vec::new();
                };
                let (Some(id), Some(name)) = (text(call, "/id"), text(call, "/name")) else {
                    return Vec::new();
                };
                self.seen.insert(index);
                self.announce(id, name, call.get("arguments").unwrap_or(&Value::Null))
            }
            _ => Vec::new(),
        }
    }

    fn open_block(&mut self, index: u64, thought: bool) -> Vec<Event> {
        self.seen.insert(index);
        let id = self.message_id(index);
        self.blocks.entry(index).or_insert((id, thought, false));
        Vec::new()
    }

    fn block_delta(&mut self, index: u64, thought: bool, delta: Option<&str>) -> Vec<Event> {
        self.open_block(index, thought);
        let Some(delta) = delta.filter(|delta| !delta.is_empty()) else {
            return Vec::new();
        };
        let Some(block) = self.blocks.get_mut(&index) else {
            return Vec::new();
        };
        block.2 = true;
        let message = block.0.clone();
        let text = delta.to_string();
        vec![if thought {
            Event::ThoughtChunk { message, text }
        } else {
            Event::MessageChunk { message, text }
        }]
    }

    /// Ends a block; one that was never streamed is told whole.
    fn block_end(&mut self, index: u64, thought: bool, whole: Option<&str>) -> Vec<Event> {
        self.open_block(index, thought);
        let streamed = self.blocks.get(&index).is_some_and(|block| block.2);
        let mut events = if streamed {
            Vec::new()
        } else {
            self.block_delta(index, thought, whole)
        };
        if let Some((message, _, _)) = self.blocks.remove(&index) {
            events.push(if thought {
                Event::ThoughtEnd { message }
            } else {
                Event::MessageEnd { message }
            });
        }
        events
    }

    fn message_end(&mut self, message: &Value) -> Vec<Event> {
        let Some(body) = message.get("message") else {
            return Vec::new();
        };
        if text(body, "/role") != Some("assistant") {
            return Vec::new();
        }
        let mut events = Vec::new();
        // What the message holds that its updates did not say: a provider that does not stream.
        let content = body.get("content").and_then(Value::as_array);
        for (index, block) in content.into_iter().flatten().enumerate() {
            let index = index as u64;
            match block.get("type").and_then(Value::as_str) {
                Some("text") if !self.seen.contains(&index) => {
                    events.extend(self.block_end(index, false, text(block, "/text")));
                }
                Some("thinking") if !self.seen.contains(&index) => {
                    events.extend(self.block_end(index, true, text(block, "/thinking")));
                }
                Some("toolCall") => {
                    if let (Some(id), Some(name)) = (text(block, "/id"), text(block, "/name")) {
                        events.extend(self.announce(
                            id,
                            name,
                            block.get("arguments").unwrap_or(&Value::Null),
                        ));
                    }
                }
                _ => {}
            }
        }
        // A block still open is over.
        let open: Vec<u64> = self.blocks.keys().copied().collect();
        for index in open {
            if let Some((message, thought, _)) = self.blocks.remove(&index) {
                events.push(if thought {
                    Event::ThoughtEnd { message }
                } else {
                    Event::MessageEnd { message }
                });
            }
        }
        let stop = text(body, "/stopReason").unwrap_or("stop").to_string();
        let error = text(body, "/errorMessage").map(str::to_string);
        self.run.last = Some((stop, error));
        events.extend(self.usage(body));
        events
    }

    fn usage(&mut self, body: &Value) -> Option<Event> {
        let usage = body.get("usage")?;
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let (input, output) = (count("input"), count("output"));
        let (read, write) = (count("cacheRead"), count("cacheWrite"));
        if input + output + read + write == 0 {
            return None;
        }
        self.cost += usage
            .pointer("/cost/total")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let mut report = Usage::default();
        report.input = input;
        report.output = output;
        report.cache_read = read;
        report.cache_write = write;
        report.cost_usd = (self.cost > 0.0).then_some(self.cost);
        report.context_used = Some(input + output + read + write);
        report.context_size = self.context_window;
        Some(Event::Usage(report))
    }

    fn compaction_end(&mut self, message: &Value) -> Vec<Event> {
        if message.get("aborted").and_then(Value::as_bool) == Some(true) {
            return Vec::new();
        }
        if let Some(why) = text(message, "/errorMessage") {
            return vec![Event::Notice {
                severity: Severity::Warning,
                title: format!(
                    "Pi could not compact the conversation: {}",
                    self.short(&one_line(why))
                ),
            }];
        }
        self.compactions += 1;
        vec![Event::Compaction {
            id: format!("compaction-{}", self.compactions),
            summary: text(message, "/result/summary").map(str::to_string),
        }]
    }

    // ---- tool calls -----------------------------------------------------------------------------------------

    fn path_of(&self, args: &Value) -> Option<String> {
        let path = args.get("path")?.as_str()?;
        Some(self.cwd.join(path).to_string_lossy().into_owned())
    }

    /// What kind of tool, its title and where it works.
    fn view(&self, name: &str, args: &Value) -> (ToolKind, String, Vec<Location>) {
        let path = self.path_of(args);
        let shown = args.get("path").and_then(Value::as_str).unwrap_or("");
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
        let at = |line: Option<u64>| {
            path.iter()
                .map(|path| Location::new(path.clone(), line))
                .collect::<Vec<_>>()
        };
        match name {
            "read" => (
                ToolKind::Read,
                self.short(&format!("Read {}", one_line(shown))),
                at(args.get("offset").and_then(Value::as_u64)),
            ),
            "write" => (
                ToolKind::Edit,
                self.short(&format!("Write {}", one_line(shown))),
                at(None),
            ),
            "edit" => (
                ToolKind::Edit,
                self.short(&format!("Edit {}", one_line(shown))),
                at(None),
            ),
            "bash" => {
                let command = args.get("command").and_then(Value::as_str).unwrap_or("");
                let first = command.lines().next().unwrap_or("");
                (ToolKind::Execute, self.short(&one_line(first)), Vec::new())
            }
            "grep" => (
                ToolKind::Search,
                self.short(&format!("Search {}", one_line(pattern))),
                at(None),
            ),
            "find" => (
                ToolKind::Search,
                self.short(&format!("Find {}", one_line(pattern))),
                at(None),
            ),
            "ls" => (
                ToolKind::Search,
                self.short(&format!("List {}", one_line(shown))),
                at(None),
            ),
            other => (ToolKind::Other, self.short(&one_line(other)), Vec::new()),
        }
    }

    /// The entry for Pi's call `id`; a new one when there is none, or when the old one is over and the model reuses the id.
    fn entry(&mut self, pi_id: &str, name: &str) -> &mut Tool {
        let reuse = self.tools.get(pi_id).is_some_and(|tool| tool.finished);
        if reuse || !self.tools.contains_key(pi_id) {
            let mut id = pi_id.to_string();
            let mut again = 1u64;
            while self.used.contains(&id) {
                again += 1;
                id = format!("{pi_id}~{again}");
            }
            self.used.insert(id.clone());
            self.tools.remove(pi_id);
            self.order.retain(|known| known != pi_id);
            while self.order.len() >= self.max_tools {
                if let Some(oldest) = self.order.pop_front() {
                    self.tools.remove(&oldest);
                }
            }
            self.order.push_back(pi_id.to_string());
            self.tools.insert(
                pi_id.to_string(),
                Tool {
                    id,
                    name: name.to_string(),
                    args: Value::Null,
                    kind: ToolKind::Other,
                    title: name.to_string(),
                    locations: Vec::new(),
                    content: Vec::new(),
                    sent: 0,
                    started: false,
                    asked: false,
                    finished: false,
                },
            );
        }
        self.tools.get_mut(pi_id).expect("the entry was just made")
    }

    /// A tool call the model made: pending, until Pi runs it.
    fn announce(&mut self, pi_id: &str, name: &str, args: &Value) -> Vec<Event> {
        let (kind, title, locations) = self.view(name, args);
        let tool = self.entry(pi_id, name);
        if tool.started || tool.finished {
            return Vec::new();
        }
        tool.name = name.to_string();
        if !args.is_null() {
            tool.args = args.clone();
        }
        tool.kind = kind;
        tool.title = title.clone();
        tool.locations = locations.clone();
        let mut call = ToolCall::new(tool.id.clone());
        call.name = Some(name.to_string());
        call.title = Some(title);
        call.kind = Some(kind);
        call.status = Some(ToolStatus::Pending);
        if !args.is_null() {
            call.input = Some(args.clone());
            call.locations = Some(locations);
        }
        vec![Event::Tool(call)]
    }

    fn execution_start(&mut self, message: &Value) -> Vec<Event> {
        let (Some(pi_id), Some(name)) = (text(message, "/toolCallId"), text(message, "/toolName"))
        else {
            return Vec::new();
        };
        let args = message.get("args").cloned().unwrap_or(Value::Null);
        let (kind, title, locations) = self.view(name, &args);
        let content = self.start_content(name, &args);
        let old = if name == "write" {
            self.old_content(&args)
        } else {
            None
        };
        let tool = self.entry(pi_id, name);
        tool.name = name.to_string();
        tool.args = args.clone();
        tool.kind = kind;
        tool.title = title.clone();
        tool.locations = locations.clone();
        tool.started = true;
        let mut shown = content;
        if name == "write"
            && let Some(ToolContent::Diff(diff)) = shown.first_mut()
        {
            diff.change = if old.is_some() {
                FileChange::Modify
            } else {
                FileChange::Add
            };
            diff.old_text = old;
        }
        if kind == ToolKind::Execute {
            shown = vec![ToolContent::Terminal(tool.id.clone())];
        }
        tool.content = shown.clone();
        let mut call = ToolCall::new(tool.id.clone());
        call.name = Some(name.to_string());
        call.title = Some(title);
        call.kind = Some(kind);
        call.status = Some(ToolStatus::InProgress);
        call.input = Some(args.clone());
        call.locations = Some(locations);
        if !shown.is_empty() {
            call.content = Some(shown);
        }
        let mut events = vec![Event::Tool(call)];
        if kind == ToolKind::Execute {
            let mut terminal = Terminal::new(tool.id.clone());
            terminal.command = args
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string);
            events.push(Event::Terminal(terminal));
        }
        events
    }

    /// What a call shows when it starts: the changes an `edit` or a `write` will make.
    fn start_content(&self, name: &str, args: &Value) -> Vec<ToolContent> {
        let Some(path) = self.path_of(args) else {
            return Vec::new();
        };
        match name {
            "edit" => {
                let mut pairs: Vec<(&str, &str)> = args
                    .get("edits")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|edit| Some((text(edit, "/oldText")?, text(edit, "/newText")?)))
                    .collect();
                if pairs.is_empty()
                    && let (Some(old), Some(new)) = (text(args, "/oldText"), text(args, "/newText"))
                {
                    pairs.push((old, new));
                }
                pairs
                    .into_iter()
                    .map(|(old, new)| {
                        let mut diff = Diff::new(path.clone(), FileChange::Modify);
                        diff.old_text = Some(old.to_string());
                        diff.new_text = Some(new.to_string());
                        ToolContent::Diff(diff)
                    })
                    .collect()
            }
            "write" => {
                let mut diff = Diff::new(path, FileChange::Add);
                diff.new_text = args
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                vec![ToolContent::Diff(diff)]
            }
            _ => Vec::new(),
        }
    }

    /// The file a `write` is about to replace, read before Pi writes it: Pi's result has no old content. Only inside the
    /// directories the session works in, and only a plain file of a bounded size.
    fn old_content(&self, args: &Value) -> Option<String> {
        let text = args.get("path")?.as_str()?;
        let scope = self.scope();
        // A credential file is not read for a diff, and a file outside the session's directories is not either; a network
        // path is passed over by how it is written, before anything opens it.
        if scope.secret(text) || !scope.inside(text) {
            return None;
        }
        let path = self.cwd.join(text);
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > self.limits.max_old_bytes {
            return None;
        }
        std::fs::read_to_string(&path).ok()
    }

    fn execution_update(&mut self, message: &Value) -> Vec<Event> {
        let Some(pi_id) = text(message, "/toolCallId") else {
            return Vec::new();
        };
        let Some(tool) = self.tools.get_mut(pi_id) else {
            return Vec::new();
        };
        if tool.kind != ToolKind::Execute {
            return Vec::new();
        }
        let whole = text_of(
            message
                .pointer("/partialResult/content")
                .unwrap_or(&Value::Null),
        );
        Self::terminal_output(tool, &whole)
            .map(Event::Terminal)
            .into_iter()
            .collect()
    }

    /// What of a command's cumulative output has not been sent.
    fn terminal_output(tool: &mut Tool, whole: &str) -> Option<Terminal> {
        let more = match whole.get(tool.sent..) {
            Some(rest) if whole.len() >= tool.sent && !rest.is_empty() => rest.to_string(),
            Some(_) => return None,
            // Not a continuation of what was sent: it is told after it.
            None => format!("\n{whole}"),
        };
        tool.sent = whole.len();
        let mut terminal = Terminal::new(tool.id.clone());
        terminal.output = Some(more);
        Some(terminal)
    }

    fn execution_end(&mut self, message: &Value) -> Vec<Event> {
        let Some(pi_id) = text(message, "/toolCallId") else {
            return Vec::new();
        };
        if !self.tools.contains_key(pi_id) {
            let name = text(message, "/toolName").unwrap_or("tool").to_string();
            let args = message.get("args").cloned().unwrap_or(Value::Null);
            let mut events = self.announce(pi_id, &name, &args);
            events.extend(self.finish(pi_id, message));
            return events;
        }
        self.finish(pi_id, message)
    }

    fn finish(&mut self, pi_id: &str, message: &Value) -> Vec<Event> {
        let failed = message.get("isError").and_then(Value::as_bool) == Some(true);
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        let output = text_of(result.get("content").unwrap_or(&Value::Null));
        let interrupting = self.run.state.load(Ordering::SeqCst) == 1;
        let told = self.told(&output);
        let images = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("image"))
            .count();
        let first_line = result
            .pointer("/details/firstChangedLine")
            .and_then(Value::as_u64);
        let patch = text(&result, "/details/patch").map(str::to_string);
        let exit = result
            .pointer("/structuredContent/exit_code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        let Some(tool) = self.tools.get_mut(pi_id) else {
            return Vec::new();
        };
        tool.finished = true;
        let status = match (failed, interrupting) {
            (false, _) => ToolStatus::Completed,
            (true, true) => ToolStatus::Cancelled,
            (true, false) => ToolStatus::Failed,
        };
        let mut call = ToolCall::new(tool.id.clone());
        call.status = Some(status);
        call.output = Some(result.get("content").cloned().unwrap_or(Value::Null));
        let mut events = Vec::new();
        match tool.kind {
            ToolKind::Execute => {
                let mut terminal = Terminal::new(tool.id.clone());
                if let Some(rest) = Self::terminal_output(tool, &output) {
                    terminal.output = rest.output;
                }
                terminal.exit = Some(exit);
                events.push(Event::Terminal(terminal));
            }
            ToolKind::Edit if !failed => {
                let path = tool.title_path();
                if let Some(patch) = &patch
                    && let Some(ToolContent::Diff(diff)) = tool.content.first_mut()
                {
                    diff.patch = Some(git_patch(&path, patch));
                }
                if let Some(line) = first_line {
                    for location in &mut tool.locations {
                        location.line = Some(line);
                    }
                    call.locations = Some(tool.locations.clone());
                }
                call.content = Some(tool.content.clone());
            }
            _ => {
                let mut shown = Vec::new();
                if failed || tool.kind != ToolKind::Edit {
                    if !told.is_empty() {
                        shown.push(ToolContent::Text(told));
                    }
                    if images > 0 {
                        shown.push(ToolContent::Text(format!(
                            "[{images} image{} not shown]",
                            if images == 1 { "" } else { "s" }
                        )));
                    }
                }
                if !shown.is_empty() {
                    call.content = Some(shown);
                }
            }
        }
        events.push(Event::Tool(call));
        events
    }

    // ---- permission -----------------------------------------------------------------------------------------

    fn respond(&self, dialog: &str, fields: &Value) {
        let mut line = json!({"type": "extension_ui_response", "id": dialog});
        if let (Some(line), Some(fields)) = (line.as_object_mut(), fields.as_object()) {
            line.extend(fields.clone());
        }
        let _ = (self.writer)(&line);
    }

    fn dialog(&mut self, message: &Value) -> Vec<Event> {
        let Some(dialog) = text(message, "/id") else {
            return Vec::new();
        };
        match text(message, "/method") {
            Some("select")
                if text(message, "/title").is_some_and(|t| t.starts_with(gate::PREFIX)) =>
            {
                self.gate(dialog, text(message, "/title").unwrap_or_default())
            }
            Some("select" | "confirm" | "input" | "editor") => {
                // Another extension's question has nobody to ask: it is told so, not left waiting.
                self.respond(dialog, &json!({"cancelled": true}));
                vec![Event::Notice {
                    severity: Severity::Warning,
                    title: format!(
                        "A Pi extension asked \"{}\", which this client cannot show; it was cancelled",
                        self.short(&one_line(text(message, "/title").unwrap_or("a question")))
                    ),
                }]
            }
            Some("notify") => {
                let severity = match text(message, "/notifyType") {
                    Some("error") => Severity::Error,
                    Some("warning") => Severity::Warning,
                    _ => Severity::Info,
                };
                vec![Event::Notice {
                    severity,
                    title: self.short(&one_line(text(message, "/message").unwrap_or(""))),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// The gate asks about a call: the mode answers, or the person is asked. What is decided on, and shown, is the input
    /// the gate saw when it asked, not what the stream announced: another extension may change the input in between.
    fn gate(&mut self, dialog: &str, title: &str) -> Vec<Event> {
        let Some(mut asked) = gate::call_of(title) else {
            self.respond(dialog, &json!({"cancelled": true}));
            return Vec::new();
        };
        // A gate that sent no input (an older one) is decided on what the stream announced.
        if asked.input.is_null() {
            let announced = asked
                .id
                .as_ref()
                .and_then(|id| self.tools.get(id))
                .or_else(|| {
                    self.order
                        .iter()
                        .filter_map(|key| self.tools.get(key))
                        .find(|tool| tool.name == asked.tool && !tool.asked && !tool.finished)
                });
            if let Some(tool) = announced {
                asked.input = tool.args.clone();
            }
        }
        // A question that comes after the person stopped the run is not put to them.
        if self.run.state.load(Ordering::SeqCst) != 0 {
            self.respond(dialog, &json!({"cancelled": true}));
            return Vec::new();
        }
        let name = asked.tool.clone();
        let decision = locked(&self.policy).decide(&name, &asked.input, &self.scope());
        let answer = match decision {
            Decision::Allow => Some("allow"),
            Decision::Plan => Some("plan"),
            Decision::Protected => Some("protected"),
            Decision::Ask => None,
        };
        if let Some(answer) = answer {
            self.respond(dialog, &json!({"value": answer}));
            return Vec::new();
        }
        // The call the stream announced under the id Pi gave the gate, when that one has not been asked about yet; else the
        // oldest of that tool not asked about. A reused id is a call of its own.
        let free = |tool: &Tool| tool.name == name && !tool.asked && !tool.finished;
        let key = match &asked.id {
            Some(id) if self.tools.get(id).is_some_and(free) => Some(id.clone()),
            // An id already asked about, or finished, is reused by the model: a call of its own.
            Some(id) if self.tools.contains_key(id) => None,
            _ => self
                .order
                .iter()
                .find(|key| self.tools.get(*key).is_some_and(free))
                .cloned(),
        };
        let (kind, shown, locations) = self.view(&name, &asked.input);
        let id = match key.as_ref().and_then(|key| self.tools.get_mut(key)) {
            Some(tool) => {
                tool.asked = true;
                tool.id.clone()
            }
            None => format!("gate-{dialog}"),
        };
        // What the stream said, to tell the person when it is not what the gate saw.
        let announced = key
            .as_ref()
            .and_then(|key| self.tools.get(key))
            .map(|tool| tool.args.clone());
        let differs = announced.as_ref().is_some_and(|before| {
            ["command", "path"]
                .iter()
                .any(|field| before.get(field) != asked.input.get(field))
        });
        let command = asked.input.get("command").and_then(Value::as_str);
        let mut told: Vec<String> = Vec::new();
        if differs {
            told.push(
                "The call's input changed after Pi announced it; this is what it will run.".into(),
            );
        }
        if kind == ToolKind::Execute
            && let Some(command) = command
            && (differs || command.len() > shown.len())
        {
            told.push(self.told(command));
        }
        if asked
            .input
            .get("_cut")
            .and_then(Value::as_array)
            .is_some_and(|cut| !cut.is_empty())
        {
            told.push("Some of its input is longer than the gate sends and is cut here.".into());
        }
        let description = (!told.is_empty()).then(|| told.join("\n\n"));
        let question = if kind == ToolKind::Execute {
            format!("Run {shown}")
        } else {
            shown.clone()
        };
        let mut call = ToolCall::new(id);
        call.name = Some(name.clone());
        call.title = Some(shown);
        call.kind = Some(kind);
        call.input = Some(asked.input.clone());
        call.locations = Some(locations);
        call.status = Some(ToolStatus::Pending);
        locked(&self.open).insert(dialog.to_string());
        // Nothing is remembered for a path Pi rewrites (`~`, `@`, a `file:` URL): it is asked about every time.
        let rewritten = asked
            .input
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| self.scope().indirect(path));
        let remember = Policy::remember(&name, &asked.input).filter(|_| !rewritten);
        let responder = self.responder(dialog, remember.as_ref().map(|r| r.key.clone()));
        let mut options = vec![PermissionOption::new(ALLOW, "Allow", OptionKind::AllowOnce)];
        if let Some(remember) = remember {
            options.push(PermissionOption::new(
                ALLOW_TOOL,
                remember.label,
                OptionKind::AllowAlways,
            ));
        }
        options.push(PermissionOption::new(
            REJECT,
            "Reject",
            OptionKind::RejectOnce,
        ));
        let mut details = Map::new();
        details.insert("tool".into(), json!(name));
        vec![Event::Permission(
            PermissionRequest::new(call, question, description, options, responder)
                .details(details),
        )]
    }

    fn responder(&self, dialog: &str, key: Option<String>) -> Responder {
        let (writer, open, policy) = (
            Arc::clone(&self.writer),
            Arc::clone(&self.open),
            Arc::clone(&self.policy),
        );
        let state = Arc::clone(&self.run.state);
        let dialog = dialog.to_string();
        Responder::new(move |outcome| {
            // An abort that cancelled the dialog has answered it already.
            if !locked(&open).remove(&dialog) {
                return Ok(());
            }
            let cancelled = matches!(outcome, Outcome::Cancelled);
            let fields = match outcome {
                Outcome::Selected(chosen) if chosen == ALLOW => json!({"value": "allow"}),
                Outcome::Selected(chosen) if chosen == ALLOW_TOOL => {
                    if let Some(key) = &key {
                        locked(&policy).allow_key(key);
                    }
                    json!({"value": "allow"})
                }
                Outcome::Cancelled => json!({"cancelled": true}),
                _ => json!({"value": "deny"}),
            };
            let mut line = json!({"type": "extension_ui_response", "id": dialog});
            if let (Some(line), Some(fields)) = (line.as_object_mut(), fields.as_object()) {
                line.extend(fields.clone());
            }
            writer(&line)?;
            // A question nobody answered ends the run, as it does for every other harness: Pi would go on with the call
            // blocked. A run already being stopped, or over, has nothing more to stop.
            if cancelled
                && state
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                writer(&json!({"id": "abort-after-cancel", "type": "abort"}))?;
            }
            Ok(())
        })
    }

    // ---- the end of a run -----------------------------------------------------------------------------------

    fn settle(&mut self, aborted: bool) -> Vec<Event> {
        if !self.run.active {
            return Vec::new();
        }
        let mut events = Vec::new();
        // A call that never ran ends with the run.
        for key in self.order.clone() {
            if let Some(tool) = self.tools.get_mut(&key)
                && !tool.finished
            {
                tool.finished = true;
                let mut call = ToolCall::new(tool.id.clone());
                call.status = Some(ToolStatus::Cancelled);
                events.push(Event::Tool(call));
            }
        }
        let interrupting = self.run.state.load(Ordering::SeqCst) == 1;
        let last = self.run.last.take();
        self.run.state.store(2, Ordering::SeqCst);
        self.run = Run::default();
        locked(&self.open).clear();
        self.tools.retain(|_, tool| !tool.finished);
        self.order.retain(|key| self.tools.contains_key(key));
        let end = if aborted || interrupting {
            TurnEnd::stopped(Stop::Cancelled)
        } else {
            match last {
                Some((stop, error)) if stop == "error" => {
                    let why = error.unwrap_or_else(|| "The model failed".to_string());
                    self.failure(&why)
                }
                Some((stop, _)) if stop == "length" => TurnEnd::stopped(Stop::MaxTokens),
                Some((stop, _)) if stop == "aborted" => TurnEnd::stopped(Stop::Cancelled),
                _ => TurnEnd::stopped(Stop::EndTurn),
            }
        };
        events.push(Event::TurnEnd(end));
        events
    }

    /// A run Pi ended on an error: a usage limit when the words say so, else a failure in Pi's words.
    fn failure(&self, why: &str) -> TurnEnd {
        let read = if why.starts_with("429") {
            format!("Error {why}")
        } else {
            why.to_string()
        };
        match infer_limit(&read, DEFAULT_LIMIT_PHRASES) {
            Some(hit) => TurnEnd::Limited(hit),
            None => TurnEnd::Failed(cut(why, 2048).to_string()),
        }
    }

    // ---- history --------------------------------------------------------------------------------------------

    /// The messages of a session Pi opened (`get_messages`) as events, in the order they were said. A message the
    /// protocol has no kind for (Pi's own system prompt, branch markers) is left out.
    pub fn history(&mut self, messages: &[Value]) -> Vec<Event> {
        let mut events = Vec::new();
        for (number, message) in messages.iter().enumerate() {
            match text(message, "/role") {
                Some("user") => events.extend(Self::user(&format!("h{number}"), message)),
                Some("assistant") => events.extend(self.past_assistant(message)),
                Some("toolResult") => {
                    if let Some(id) = text(message, "/toolCallId") {
                        let line = json!({
                            "toolCallId": id,
                            "toolName": message.get("toolName"),
                            "result": {"content": message.get("content"), "details": message.get("details")},
                            "isError": message.get("isError"),
                        });
                        events.extend(self.execution_end(&line));
                    }
                }
                Some("bashExecution") => events.extend(self.past_command(number, message)),
                Some("compactionSummary" | "branchSummary") => {
                    self.compactions += 1;
                    events.push(Event::Compaction {
                        id: format!("compaction-{}", self.compactions),
                        summary: text(message, "/summary").map(str::to_string),
                    });
                }
                _ => {}
            }
        }
        events.extend(self.settle_history());
        events
    }

    fn user(id: &str, message: &Value) -> Vec<Event> {
        let mut events = Vec::new();
        match message.get("content") {
            Some(Value::String(said)) => events.push(Event::UserChunk {
                message: id.to_string(),
                content: Content::Text(said.clone()),
            }),
            Some(Value::Array(parts)) => {
                for part in parts {
                    let content = match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            text(part, "/text").map(|said| Content::Text(said.to_string()))
                        }
                        Some("image") => Some(Content::Image {
                            mime_type: text(part, "/mimeType").unwrap_or("image/png").to_string(),
                            data: text(part, "/data").unwrap_or_default().to_string(),
                        }),
                        _ => None,
                    };
                    if let Some(content) = content {
                        events.push(Event::UserChunk {
                            message: id.to_string(),
                            content,
                        });
                    }
                }
            }
            _ => {}
        }
        events.push(Event::UserEnd {
            message: id.to_string(),
        });
        events
    }

    fn past_assistant(&mut self, message: &Value) -> Vec<Event> {
        self.messages += 1;
        self.blocks.clear();
        self.seen.clear();
        let mut events = Vec::new();
        for (index, block) in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let index = index as u64;
            match block.get("type").and_then(Value::as_str) {
                Some("text") => events.extend(self.block_end(index, false, text(block, "/text"))),
                Some("thinking") => {
                    events.extend(self.block_end(index, true, text(block, "/thinking")))
                }
                Some("toolCall") => {
                    if let (Some(id), Some(name)) = (text(block, "/id"), text(block, "/name")) {
                        let args = block.get("arguments").cloned().unwrap_or(Value::Null);
                        events.extend(self.announce(id, name, &args));
                        let start = json!({"toolCallId": id, "toolName": name, "args": args});
                        events.extend(self.execution_start(&start));
                    }
                }
                _ => {}
            }
        }
        events
    }

    /// A command the person ran through `bash` (not the model), as a finished command.
    fn past_command(&mut self, number: usize, message: &Value) -> Vec<Event> {
        let id = format!("bash-{number}");
        let command = text(message, "/command").unwrap_or_default();
        let start = json!({"toolCallId": id, "toolName": "bash", "args": {"command": command}});
        let mut events = self.execution_start(&start);
        let failed = message
            .get("exitCode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0);
        let end = json!({
            "toolCallId": id,
            "result": {"content": [{"type": "text", "text": text(message, "/output").unwrap_or_default()}],
                       "structuredContent": {"exit_code": message.get("exitCode")}},
            "isError": failed,
        });
        events.extend(self.execution_end(&end));
        events
    }

    /// The end of replayed history: a call whose result is missing stays as it was said.
    fn settle_history(&mut self) -> Vec<Event> {
        self.tools.clear();
        self.order.clear();
        Vec::new()
    }
}

impl Tool {
    /// The path a diff is about, for a patch header.
    fn title_path(&self) -> String {
        self.locations
            .first()
            .map(|location| location.path.clone())
            .unwrap_or_default()
    }
}

/// Pi's unified diff as a git patch of `path`.
#[must_use]
pub fn git_patch(path: &str, hunks: &str) -> String {
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
    patch.push_str(hunks);
    if !hunks.ends_with('\n') {
        patch.push('\n');
    }
    patch
}
