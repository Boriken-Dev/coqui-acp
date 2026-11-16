//! Pi's session file as a standard history, and back.
//!
//! [`export`] reads a session file (`sessions/--<directory>--/<time>_<id>.jsonl`) into a [`Document`];
//! [`import`] writes a session file Pi resumes from a [`Document`]. What Pi keeps beyond the conversation
//! (the system prompt and tool loadout it records, usage, labels, names, extension state, the edits that hide a
//! message from the model) is not part of a history and is not carried.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

use coqui_acp_harness::event::context_block;
use coqui_acp_history::conversation::{
    Boundary, Call, Conversation, Item, Recorder, Stamp, call_note, clean_id, derived_uuid,
    is_uuid, shell_text, valid_time,
};
use coqui_acp_history::{
    ATTACHMENT_BY_DIGEST, ATTACHMENT_META_KEY, Document, Header, LossReport, Origin,
    SUMMARY_MISSING, SeedPoint, Stop, StopReason, THINKING, TOOL_AS_NOTE, Tool,
};
use serde_json::{Map, Value, json};

use crate::Error;

/// The harness name in a history's header and in the `_meta` this codec keeps.
pub const HARNESS: &str = "pi";

/// A loss the tool table has no name for: content the target cannot take at all.
pub const CONTENT_DROPPED: &str = "content_dropped";

/// What the codecs read and write, in bytes: a session file larger than the bound is refused.
pub const DEFAULT_MAX_BYTES: u64 = 256 << 20;

/// The session format version Pi writes, and the newest this codec reads.
pub const SESSION_VERSION: u64 = 3;

/// The text a cancelled tool call's result carries.
const CANCELLED_RESULT: &str = "The tool call did not finish before the history ended.";

/// How much of a file's start is read to find the header of a session: the bound Pi itself scans to.
const HEADER_SCAN_BYTES: u64 = 1 << 20;

/// The `customType` of a note written from a history that did not come from Pi.
const NOTE_TYPE: &str = "history-note";

fn refused(why: impl Into<String>) -> Error {
    Error::Refused(why.into())
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

/// The session in `text` as a history named `id` (the host's UUID).
///
/// The conversation is the active branch: the entries from the file's last one back through `parentId` to
/// the root; when a link is missing from the file, every entry in the file's order. A line that is not JSON is
/// skipped, and so is every entry that is no part of the conversation (the system prompt, usage, labels, names,
/// extension state, `context_edit`).
///
/// # Errors
///
/// [`Error::Refused`] for a file with no session header, one written by a newer Pi than this reads, or a
/// history the format refuses.
pub fn export(text: &str, id: &str) -> Result<Document, Error> {
    let values: Vec<Value> = text
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let session = values
        .iter()
        .find(|value| value.get("type").and_then(Value::as_str) == Some("session"))
        .ok_or_else(|| refused("the file has no session header"))?;
    let version = session.get("version").and_then(Value::as_u64).unwrap_or(1);
    if version > SESSION_VERSION {
        return Err(refused(format!(
            "the session is version {version}, newer than the {SESSION_VERSION} this reads"
        )));
    }
    let entries: Vec<&Value> = values
        .iter()
        .filter(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "session")
        })
        .collect();
    let chain = branch_of(&entries);
    let mut exporter = Exporter {
        cwd: string(session, "cwd").unwrap_or_default(),
        ..Exporter::default()
    };
    for entry in &chain {
        exporter.entry(entry);
    }
    let lines = exporter.finish();
    let mut header = Header::new(id, Origin::new(HARNESS));
    header.origin.session_id = string(session, "id");
    header.created_at = string(session, "timestamp").filter(|at| valid_time(at));
    header.cwd = string(session, "cwd");
    header.model = exporter.model.clone();
    header.effort = exporter.effort.clone();
    header = tidied(header);
    let mut document = Document::new(header);
    document.lines = lines;
    document
        .seal()
        .map_err(|error| refused(format!("the session is not a history: {error}")))?;
    Ok(document)
}

/// `header` with every optional field the format refuses dropped: they come from a file this code does not
/// control.
fn tidied(mut header: Header) -> Header {
    let ok = |header: &Header| header.to_line().is_ok();
    for clear in [
        (|h: &mut Header| h.origin.session_id = None) as fn(&mut Header),
        |h| h.cwd = None,
        |h| h.model = None,
        |h| h.effort = None,
        |h| h.created_at = None,
    ] {
        if ok(&header) {
            break;
        }
        clear(&mut header);
    }
    header
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

/// The entries of the active branch, in order: from the last entry of the file back through `parentId`, or
/// the whole file in its order when the entries carry no ids (a version 1 file) or a link is missing.
fn branch_of<'a>(entries: &[&'a Value]) -> Vec<&'a Value> {
    let index: BTreeMap<&str, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(at, entry)| Some((entry.get("id")?.as_str()?, at)))
        .collect();
    let Some(last) = entries.len().checked_sub(1) else {
        return Vec::new();
    };
    if index.len() != entries.len() {
        return entries.to_vec();
    }
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    let mut at = last;
    loop {
        if !seen.insert(at) {
            return entries.to_vec();
        }
        chain.push(at);
        match entries[at].get("parentId") {
            None | Some(Value::Null) => break,
            Some(Value::String(parent)) => match index.get(parent.as_str()) {
                Some(&next) => at = next,
                None => return entries.to_vec(),
            },
            Some(_) => return entries.to_vec(),
        }
    }
    chain.reverse();
    chain.into_iter().map(|at| entries[at]).collect()
}

#[derive(Default)]
struct Exporter {
    rec: Recorder,
    /// The session's working directory, which a relative path of a tool is against.
    cwd: String,
    /// The history id each Pi call id has now, and every id given so far (a model may repeat one).
    ids: BTreeMap<String, String>,
    used: BTreeSet<String>,
    open_calls: BTreeSet<String>,
    /// Each announced call's tool name, for its result.
    names: BTreeMap<String, String>,
    stop_reason: Option<String>,
    error: Option<String>,
    interrupted: bool,
    /// The last message was the person's or a tool's, so the agent owes an answer.
    owes: bool,
    model: Option<String>,
    effort: Option<String>,
}

impl Exporter {
    fn stamp(value: &Value) -> Stamp {
        Stamp::at(string(value, "timestamp").filter(|at| valid_time(at)))
    }

    fn entry(&mut self, entry: &Value) {
        match entry.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                if let (Some(provider), Some(model)) =
                    (string(entry, "provider"), string(entry, "modelId"))
                {
                    self.model = Some(format!("{provider}/{model}"));
                }
            }
            Some("thinking_level_change") => {
                if let Some(level) = string(entry, "thinkingLevel") {
                    self.effort = Some(level);
                }
            }
            Some("compaction") => {
                self.settle();
                let summary = string(entry, "summary");
                let pre = entry.get("tokensBefore").and_then(Value::as_u64);
                self.rec.compaction(summary, None, pre, Self::stamp(entry));
            }
            Some("branch_summary") => {
                if let Some(summary) = string(entry, "summary") {
                    self.settle();
                    let mut fields = Map::new();
                    fields.insert("kind".into(), json!("branch_summary"));
                    self.note(
                        json!({"type": "text", "text": summary}),
                        Self::stamp(entry).native(HARNESS, fields),
                    );
                }
            }
            Some("custom_message") => self.custom(entry, entry.get("content"), &Self::stamp(entry)),
            Some("message") => {
                if let Some(message) = entry.get("message") {
                    let stamp = Self::stamp(entry);
                    let id = string(entry, "id");
                    match message.get("role").and_then(Value::as_str) {
                        Some("user") => self.user(message, &stamp),
                        Some("assistant") => self.assistant(message, &stamp, id.as_deref()),
                        Some("toolResult") => self.result(message, &stamp),
                        Some("bashExecution") => self.command(message, &stamp, id.as_deref()),
                        Some("custom" | "hookMessage") => {
                            self.custom(message, message.get("content"), &stamp);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// A message of the person's that is no prompt, or an extension's.
    fn custom(&mut self, source: &Value, content: Option<&Value>, stamp: &Stamp) {
        self.settle();
        let blocks = blocks_of(content);
        let mut fields = Map::new();
        if let Some(kind) = string(source, "customType") {
            fields.insert("customType".into(), json!(kind));
        }
        fields.insert(
            "display".into(),
            json!(source.get("display") != Some(&json!(false))),
        );
        for block in blocks.iter().filter_map(prompt_block) {
            self.note(block, stamp.clone().native(HARNESS, fields.clone()));
        }
    }

    /// Ends the turn that is open when the agent owes nothing: what Pi writes between runs (a summary, an extension's
    /// message) is not part of the one before.
    fn settle(&mut self) {
        if !self.owes && self.open_calls.is_empty() {
            self.close_turn(false);
        }
    }

    fn note(&mut self, block: Value, stamp: Stamp) {
        self.rec.note(block, stamp);
    }

    fn user(&mut self, message: &Value, stamp: &Stamp) {
        let blocks: Vec<Value> = blocks_of(message.get("content"))
            .iter()
            .filter_map(prompt_block)
            .collect();
        if blocks.is_empty() {
            return;
        }
        self.close_turn(false);
        self.rec.prompt(blocks, stamp.clone());
        self.owes = true;
    }

    fn assistant(&mut self, message: &Value, stamp: &Stamp, id: Option<&str>) {
        let provider = string(message, "provider");
        let model = string(message, "model");
        let reason = string(message, "stopReason");
        if reason.is_some() {
            self.stop_reason.clone_from(&reason);
        }
        self.owes = false;
        let mut base = Map::new();
        if let Some(id) = id {
            base.insert("entryId".into(), json!(id));
        }
        if let Some(provider) = &provider {
            base.insert("provider".into(), json!(provider));
        }
        if let Some(model) = &model {
            base.insert("model".into(), json!(model));
        }
        if let Some(api) = string(message, "api") {
            base.insert("api".into(), json!(api));
        }
        if let (Some(provider), Some(model)) = (&provider, &model) {
            self.model = Some(format!("{provider}/{model}"));
        }
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for block in &blocks {
            let mut fields = base.clone();
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        self.rec
                            .message(text, stamp.clone().native(HARNESS, fields));
                    }
                }
                Some("thinking") => {
                    if let Some(signature) = block.get("thinkingSignature").and_then(Value::as_str)
                    {
                        fields.insert("signature".into(), json!(signature));
                    }
                    if flag(block, "redacted") {
                        fields.insert("redacted".into(), json!(true));
                    }
                    let text = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.rec
                        .thought(text, stamp.clone().native(HARNESS, fields));
                }
                Some("toolCall") => self.tool_call(block, stamp, fields),
                _ => {}
            }
        }
        match reason.as_deref() {
            Some("error") => {
                self.error = Some(
                    string(message, "errorMessage")
                        .unwrap_or_else(|| "the model call failed".to_owned()),
                );
            }
            Some("aborted") if !self.interrupted => {
                self.interrupted = true;
                self.rec.cancel(stamp.clone());
            }
            _ => {}
        }
        if blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
            && !matches!(reason.as_deref(), Some("error" | "aborted"))
        {
            self.owes = true;
        }
    }

    fn tool_call(&mut self, block: &Value, stamp: &Stamp, fields: Map<String, Value>) {
        let (Some(pi_id), Some(name)) = (
            block.get("id").and_then(Value::as_str),
            block.get("name").and_then(Value::as_str),
        ) else {
            return;
        };
        let id = self.fresh_id(pi_id);
        let input = block
            .get("arguments")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        let view = view(name, &input, &self.cwd);
        let mut call = Call::new(&id);
        call.name = Some(name.to_owned());
        call.title = Some(view.title);
        call.kind = Some(view.kind.to_owned());
        call.status = Some("pending".to_owned());
        call.raw_input = Some(input);
        call.locations = view.locations;
        call.content = view.content;
        self.names.insert(id.clone(), name.to_owned());
        self.open_calls.insert(id);
        self.rec
            .update(call.announce(), stamp.clone().native(HARNESS, fields));
    }

    /// The id Pi's call `pi_id` has in the history: a model may repeat one, and the second use gets `~2`, as the
    /// driver tells it.
    fn fresh_id(&mut self, pi_id: &str) -> String {
        let mut id = pi_id.to_owned();
        let mut again = 1_u64;
        while self.used.contains(&id) {
            again += 1;
            id = format!("{pi_id}~{again}");
        }
        self.used.insert(id.clone());
        self.ids.insert(pi_id.to_owned(), id.clone());
        id
    }

    fn result(&mut self, message: &Value, stamp: &Stamp) {
        let Some(pi_id) = message.get("toolCallId").and_then(Value::as_str) else {
            return;
        };
        let id = self
            .ids
            .get(pi_id)
            .cloned()
            .unwrap_or_else(|| pi_id.to_owned());
        self.open_calls.remove(&id);
        let failed = flag(message, "isError");
        let mut call = Call::new(&id);
        call.status = Some(if failed { "failed" } else { "completed" }.to_owned());
        call.raw_output = message.get("content").cloned();
        let name = self.names.get(&id).map(String::as_str).unwrap_or_default();
        let text = result_text(message.get("content"));
        call.content = if text.is_empty() || name == "read" {
            Vec::new()
        } else {
            vec![json!({"type": "content", "content": {"type": "text", "text": text}})]
        };
        self.rec.update(call.finish(), stamp.clone());
        self.owes = true;
    }

    /// A command the person ran with `!` (not the model's): a call and its result, outside any run.
    fn command(&mut self, message: &Value, stamp: &Stamp, id: Option<&str>) {
        let Some(command) = string(message, "command") else {
            return;
        };
        self.close_turn(false);
        let base = id.map_or_else(|| "command".to_owned(), |id| format!("bash-{id}"));
        let id = self.fresh_id(&base);
        let input = json!({"command": command});
        let view = view("bash", &input, &self.cwd);
        let mut fields = Map::new();
        fields.insert("userCommand".into(), json!(true));
        let mut call = Call::new(&id);
        call.name = Some("bash".to_owned());
        call.title = Some(view.title);
        call.kind = Some(view.kind.to_owned());
        call.status = Some("pending".to_owned());
        call.raw_input = Some(input);
        self.rec.update(
            call.announce(),
            stamp.clone().native(HARNESS, fields.clone()),
        );
        let output = string(message, "output").unwrap_or_default();
        let cancelled = flag(message, "cancelled");
        let code = message.get("exitCode").and_then(Value::as_i64);
        let mut done = Call::new(&id);
        done.status = Some(
            if cancelled {
                "cancelled"
            } else if code.is_some_and(|code| code != 0) {
                "failed"
            } else {
                "completed"
            }
            .to_owned(),
        );
        done.raw_output = Some(json!([{"type": "text", "text": output}]));
        if !output.is_empty() {
            done.content =
                vec![json!({"type": "content", "content": {"type": "text", "text": output}})];
        }
        if let Some(code) = code {
            fields.insert("exitCode".into(), json!(code));
        }
        if flag(message, "truncated") {
            fields.insert("truncated".into(), json!(true));
        }
        self.rec
            .update(done.finish(), stamp.clone().native(HARNESS, fields));
    }

    /// Ends the turn that is open: how it ended is told by the messages it held.
    fn close_turn(&mut self, at_end: bool) {
        if !self.rec.is_open() {
            return;
        }
        let owed = self.owes || !self.open_calls.is_empty();
        if at_end && owed && !self.interrupted && self.error.is_none() {
            // A recording cut short: the turn stays open.
            return;
        }
        let (stop, error) = if let Some(error) = self.error.take() {
            (Stop::Error, Some(error))
        } else if self.interrupted || owed {
            (Stop::Acp(StopReason::Cancelled), None)
        } else {
            let reason = match self.stop_reason.as_deref() {
                Some("length") => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            };
            (Stop::Acp(reason), None)
        };
        self.rec.end_turn(stop, error, Stamp::default());
        self.interrupted = false;
        self.stop_reason = None;
        self.open_calls.clear();
        self.owes = false;
    }

    fn finish(&mut self) -> Vec<coqui_acp_history::Line> {
        self.close_turn(true);
        core::mem::take(&mut self.rec).finish()
    }
}

/// The content blocks of a message's `content`: a string is one text block.
fn blocks_of(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

/// The text of a tool result's content, its text blocks joined.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// An ACP content block of a Pi content block, when it has one.
fn prompt_block(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => Some(json!({"type": "text", "text": block.get("text")?.as_str()?})),
        "image" => Some(json!({"type": "image",
            "mimeType": block.get("mimeType")?.as_str()?,
            "data": block.get("data")?.as_str()?})),
        _ => None,
    }
}

/// What a tool call shows: its ACP kind, a title, where it works and what it changes.
struct View {
    kind: &'static str,
    title: String,
    locations: Vec<Value>,
    content: Vec<Value>,
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    let mut cut: String = line.chars().take(200).collect();
    if cut.len() < line.len() {
        cut.push('…');
    }
    cut
}

/// `path` against the session's `cwd`, written the way `cwd` is: a drive or a root keeps it, else it follows `cwd` with
/// the separator `cwd` uses.
fn joined(cwd: &str, path: &str) -> String {
    let rooted = path.starts_with(['/', '\\'])
        || path.as_bytes().get(..3).is_some_and(|head| {
            head[0].is_ascii_alphabetic() && head[1] == b':' && matches!(head[2], b'/' | b'\\')
        });
    if rooted || cwd.is_empty() {
        return path.to_owned();
    }
    let separator = if cwd.contains('\\') { '\\' } else { '/' };
    let tail: String = path
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\') {
                separator
            } else {
                c
            }
        })
        .collect();
    format!("{}{separator}{tail}", cwd.trim_end_matches(['/', '\\']))
}

fn view(name: &str, input: &Value, cwd: &str) -> View {
    let shown = input.get("path").and_then(Value::as_str).unwrap_or("");
    let pattern = input.get("pattern").and_then(Value::as_str).unwrap_or("");
    let path = (!shown.is_empty()).then(|| joined(cwd, shown));
    let at = |line: Option<u64>| -> Vec<Value> {
        path.iter()
            .map(|path| match line {
                Some(line) => json!({"path": path, "line": line}),
                None => json!({"path": path}),
            })
            .collect()
    };
    let diff = |old: Option<&str>, new: &str| json!({"type": "diff", "path": path.clone().unwrap_or_default(), "oldText": old, "newText": new});
    match name {
        "read" => View {
            kind: "read",
            title: format!("Read {}", one_line(shown)),
            locations: at(input.get("offset").and_then(Value::as_u64)),
            content: Vec::new(),
        },
        "write" => View {
            kind: "edit",
            title: format!("Write {}", one_line(shown)),
            locations: at(None),
            content: input
                .get("content")
                .and_then(Value::as_str)
                .map(|text| diff(None, text))
                .into_iter()
                .collect(),
        },
        "edit" => View {
            kind: "edit",
            title: format!("Edit {}", one_line(shown)),
            locations: at(None),
            content: input
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|edit| {
                    Some(diff(
                        Some(edit.get("oldText")?.as_str()?),
                        edit.get("newText")?.as_str()?,
                    ))
                })
                .collect(),
        },
        "bash" => View {
            kind: "execute",
            title: one_line(input.get("command").and_then(Value::as_str).unwrap_or("")),
            locations: Vec::new(),
            content: Vec::new(),
        },
        "grep" => View {
            kind: "search",
            title: format!("Search {}", one_line(pattern)),
            locations: at(None),
            content: Vec::new(),
        },
        "find" => View {
            kind: "search",
            title: format!("Find {}", one_line(pattern)),
            locations: at(None),
            content: Vec::new(),
        },
        "ls" => View {
            kind: "search",
            title: format!("List {}", one_line(shown)),
            locations: at(None),
            content: Vec::new(),
        },
        other => View {
            kind: "other",
            title: one_line(other),
            locations: Vec::new(),
            content: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

/// How an import is done.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportOptions {
    /// Seed from the latest compaction summary the history has (the default). Off, the seed is every line of
    /// the history and no summary is written.
    pub use_summary: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { use_summary: true }
    }
}

impl ImportOptions {
    /// The same options seeding from the latest summary or, with `false`, from the first line.
    #[must_use]
    pub const fn use_summary(mut self, yes: bool) -> Self {
        self.use_summary = yes;
        self
    }
}

/// A session file written from a history.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Imported {
    /// The session file: JSON Lines, every line ended by a line feed.
    pub text: String,
    /// The time in the file's header, which names the file: `<time>_<id>.jsonl`, with `:` and `.` as `-`.
    pub created: String,
    /// What could not be carried over.
    pub report: LossReport,
    /// Where in the history the seed starts.
    pub seeded_from: Option<SeedPoint>,
    /// How many tool calls the history left open and the session ends as cancelled.
    pub closed_calls: u64,
    /// Whether the history left a turn open and the session ends it as cancelled.
    pub closed_turn: bool,
}

impl Imported {
    /// The name of the session file: `<time>_<id>.jsonl`.
    #[must_use]
    pub fn file_name(&self, id: &str) -> String {
        format!("{}_{id}.jsonl", self.created.replace([':', '.'], "-"))
    }
}

/// The session file of `doc` for session `id`, working in `cwd`.
///
/// The conversation is the seed ([`Conversation::of`]): a compaction summary goes only to the entry Pi keeps for
/// one, tool calls are written as Pi's own when the history's origin is Pi or when the tool table names an
/// equivalent (and as a plain note otherwise), and thinking is written only for a history from Pi. Nothing is
/// run, no permission answer is written as a grant, and no file snapshot is made.
///
/// # Errors
///
/// [`Error::Refused`] for an `id` that is not a UUID.
pub fn import(
    doc: &Document,
    cwd: &str,
    id: &str,
    options: &ImportOptions,
) -> Result<Imported, Error> {
    if !is_uuid(id) {
        return Err(refused("the session id is not a UUID"));
    }
    let conversation = Conversation::of(doc, options.use_summary);
    let mut writer = Writer::new(doc, cwd, id);
    writer.write(&conversation);
    let mut report = writer.report;
    report.add(
        SUMMARY_MISSING,
        u64::from(conversation.summary_missing),
        Some("the latest compaction has no summary, so the seed starts earlier"),
    );
    let mut text = String::new();
    for line in &writer.lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    Ok(Imported {
        text,
        created: writer.created,
        report,
        seeded_from: conversation.seeded_from.clone(),
        closed_calls: conversation.closed_calls,
        closed_turn: conversation.closed_turn,
    })
}

struct Writer<'a> {
    doc: &'a Document,
    session: String,
    created: String,
    lines: Vec<Value>,
    report: LossReport,
    previous: Option<String>,
    used: BTreeSet<String>,
    count: u64,
    time: String,
    /// The assistant entry that still takes blocks: nothing but assistant blocks has come since.
    draft: Option<usize>,
    /// The latest assistant entry of the turn, to give it the stop reason the turn ended with.
    last_assistant: Option<usize>,
    cancelled_turn: bool,
    ids: BTreeMap<String, String>,
}

impl<'a> Writer<'a> {
    fn new(doc: &'a Document, cwd: &str, session: &str) -> Self {
        let created = doc
            .header
            .created_at
            .clone()
            .filter(|at| valid_time(at))
            .or_else(|| {
                doc.lines
                    .iter()
                    .find_map(|line| line.at.clone().filter(|at| valid_time(at)))
            })
            .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned());
        let header = json!({"type": "session", "version": SESSION_VERSION, "id": session,
            "timestamp": created, "cwd": cwd});
        Self {
            doc,
            session: session.to_owned(),
            time: created.clone(),
            created,
            lines: vec![header],
            report: LossReport::new(),
            previous: None,
            used: BTreeSet::new(),
            count: 0,
            draft: None,
            last_assistant: None,
            cancelled_turn: false,
            ids: BTreeMap::new(),
        }
    }

    fn is_pi(&self) -> bool {
        self.doc.header.origin.harness == HARNESS
    }

    fn write(&mut self, conversation: &Conversation) {
        if self.is_pi()
            && let Some((provider, model)) = self
                .doc
                .header
                .model
                .as_deref()
                .and_then(|model| model.split_once('/'))
        {
            let (provider, model) = (provider.to_owned(), model.to_owned());
            let mut fields = Map::new();
            fields.insert("provider".into(), json!(provider));
            fields.insert("modelId".into(), json!(model));
            self.entry("model_change", fields);
            if let Some(level) = self.doc.header.effort.clone() {
                let mut fields = Map::new();
                fields.insert("thinkingLevel".into(), json!(level));
                self.entry("thinking_level_change", fields);
            }
        }
        if let Some(boundary) = &conversation.summary {
            self.boundary(boundary);
        }
        for item in &conversation.items {
            self.item(conversation, item);
        }
    }

    fn note_time(&mut self, stamp: &Stamp) {
        if let Some(at) = stamp.at.as_deref().filter(|at| valid_time(at)) {
            at.clone_into(&mut self.time);
        }
    }

    /// An id of eight hex digits made from the session and a counter, the same each time and not one used.
    fn next_id(&mut self) -> String {
        loop {
            let id = derived_uuid(&self.session, "entry", self.count)[..8].to_owned();
            self.count += 1;
            if self.used.insert(id.clone()) {
                return id;
            }
        }
    }

    /// Pushes an entry of type `kind` with `fields`, chained to the one before.
    fn entry(&mut self, kind: &str, fields: Map<String, Value>) -> usize {
        let id = self.next_id();
        self.entry_with(id, kind, fields)
    }

    fn entry_with(&mut self, id: String, kind: &str, fields: Map<String, Value>) -> usize {
        let mut line = Map::new();
        line.insert("type".into(), json!(kind));
        line.insert("id".into(), json!(id));
        line.insert(
            "parentId".into(),
            self.previous.clone().map_or(Value::Null, Value::String),
        );
        line.insert("timestamp".into(), json!(self.time));
        for (key, value) in fields {
            line.insert(key, value);
        }
        self.previous = Some(id);
        self.lines.push(Value::Object(line));
        self.lines.len() - 1
    }

    fn message(&mut self, message: Value) -> usize {
        let mut fields = Map::new();
        fields.insert("message".into(), message);
        self.entry("message", fields)
    }

    fn boundary(&mut self, boundary: &Boundary) {
        self.note_time(&boundary.stamp);
        // The latest compaction keeps nothing before it: its `firstKeptEntryId` is its own id.
        let id = self.next_id();
        let mut fields = Map::new();
        fields.insert("summary".into(), json!(boundary.summary));
        fields.insert("firstKeptEntryId".into(), json!(id));
        fields.insert(
            "tokensBefore".into(),
            json!(boundary.pre_tokens.unwrap_or(0)),
        );
        self.entry_with(id, "compaction", fields);
    }

    fn item(&mut self, conversation: &Conversation, item: &Item) {
        match item {
            Item::Prompt { blocks, stamp } => {
                self.note_time(stamp);
                self.draft = None;
                self.last_assistant = None;
                self.cancelled_turn = false;
                let content = self.content(blocks);
                if !content.is_empty() {
                    let at = self.millis();
                    self.message(json!({"role": "user", "content": content, "timestamp": at}));
                }
            }
            Item::Note { block, stamp } => {
                self.note_time(stamp);
                self.draft = None;
                let custom = stamp
                    .native_of(HARNESS)
                    .filter(|_| self.is_pi())
                    .and_then(|fields| fields.get("customType"))
                    .and_then(Value::as_str)
                    .unwrap_or(NOTE_TYPE)
                    .to_owned();
                let display = stamp
                    .native_of(HARNESS)
                    .filter(|_| self.is_pi())
                    .and_then(|fields| fields.get("display"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let content = self.content(core::slice::from_ref(block));
                let content = match content.as_slice() {
                    [] => return,
                    [only] if only.get("type").and_then(Value::as_str) == Some("text") => {
                        only["text"].clone()
                    }
                    _ => Value::Array(content),
                };
                let mut fields = Map::new();
                fields.insert("customType".into(), json!(custom));
                fields.insert("content".into(), content);
                fields.insert("display".into(), json!(display));
                self.entry("custom_message", fields);
            }
            Item::Message { text, stamp } => {
                self.note_time(stamp);
                self.assistant(stamp, json!({"type": "text", "text": text}));
            }
            Item::Thought { text, stamp } => {
                self.note_time(stamp);
                self.thought(text, stamp);
            }
            Item::Call { id } => {
                if let Some(call) = conversation.call(id) {
                    self.call(call);
                }
            }
            Item::Result { id } => {
                if let Some(call) = conversation.call(id) {
                    self.result(call);
                }
            }
            Item::Plan { .. } => {}
            Item::Cancel { stamp } => {
                self.note_time(stamp);
                self.cancelled_turn = true;
            }
            Item::TurnEnd {
                stop, error, stamp, ..
            } => {
                self.note_time(stamp);
                self.end_turn(*stop, error.as_deref());
            }
            _ => {}
        }
    }

    /// The time of the entry being written, in milliseconds since 1970.
    fn millis(&self) -> u64 {
        millis(&self.time).unwrap_or(0)
    }

    /// Gives the turn's last assistant message the stop reason the turn ended with. One that holds a tool call
    /// stays a tool use: its results follow it, and Pi leaves an aborted or failed message out of what the model
    /// reads.
    fn end_turn(&mut self, stop: Stop, error: Option<&str>) {
        self.draft = None;
        let stopped = match stop {
            Stop::Acp(StopReason::Cancelled) => Some("aborted"),
            Stop::Acp(StopReason::MaxTokens) => Some("length"),
            Stop::Error => Some("error"),
            _ => None,
        };
        let Some(reason) = stopped else {
            return;
        };
        let plain = self.last_assistant.filter(|&at| {
            self.lines[at]["message"]["content"]
                .as_array()
                .is_none_or(|blocks| blocks.iter().all(|block| block["type"] != "toolCall"))
        });
        match (plain, reason) {
            (Some(at), _) => {
                let message = &mut self.lines[at]["message"];
                message["stopReason"] = json!(reason);
                if let (true, Some(error)) = (reason == "error", error) {
                    message["errorMessage"] = json!(error);
                }
            }
            (None, "error") => {
                // As Pi writes a failed request: an empty message, which Pi's own edit then hides from the model.
                let (provider, api, model) = self.origin_of(&Stamp::default());
                let at = self.millis();
                let target = self.message(json!({"role": "assistant", "content": [],
                    "api": api, "provider": provider, "model": model, "usage": zero_usage(),
                    "stopReason": "error", "errorMessage": error.unwrap_or("the turn failed"),
                    "timestamp": at}));
                let target = self.lines[target]["id"].clone();
                let mut fields = Map::new();
                fields.insert("targetId".into(), target);
                fields.insert("replacement".into(), Value::Null);
                self.entry("context_edit", fields);
            }
            _ => {}
        }
    }

    /// The `(provider, api, model)` an assistant message carries: Pi's own when the history is Pi's, and
    /// otherwise names that no model of Pi has, so Pi reads the message as another model's.
    fn origin_of(&self, stamp: &Stamp) -> (String, String, String) {
        let origin = &self.doc.header.origin.harness;
        let from = |harness: &str, key: &str| {
            stamp
                .native_of(harness)
                .and_then(|fields| fields.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        if self.is_pi() {
            let header = self
                .doc
                .header
                .model
                .as_deref()
                .and_then(|m| m.split_once('/'));
            return (
                from(HARNESS, "provider")
                    .or_else(|| header.map(|(provider, _)| provider.to_owned()))
                    .unwrap_or_else(|| HARNESS.to_owned()),
                from(HARNESS, "api").unwrap_or_else(|| "imported".to_owned()),
                from(HARNESS, "model")
                    .or_else(|| header.map(|(_, model)| model.to_owned()))
                    .unwrap_or_else(|| "unknown".to_owned()),
            );
        }
        (
            origin.clone(),
            "imported".to_owned(),
            from(origin, "model")
                .or_else(|| self.doc.header.model.clone())
                .unwrap_or_else(|| "unknown".to_owned()),
        )
    }

    /// Adds one block to the assistant message in progress, or to a new one.
    fn assistant(&mut self, stamp: &Stamp, block: Value) {
        let at = match self.draft {
            Some(at) => at,
            None => {
                let (provider, api, model) = self.origin_of(stamp);
                let time = self.millis();
                let at = self.message(json!({"role": "assistant", "content": [],
                    "api": api, "provider": provider, "model": model, "usage": zero_usage(),
                    "stopReason": "stop", "timestamp": time}));
                self.draft = Some(at);
                self.last_assistant = Some(at);
                at
            }
        };
        let is_call = block["type"] == "toolCall";
        let message = &mut self.lines[at]["message"];
        if let Some(blocks) = message["content"].as_array_mut() {
            blocks.push(block);
        }
        if is_call {
            message["stopReason"] = json!("toolUse");
        }
    }

    fn thought(&mut self, text: &str, stamp: &Stamp) {
        let native = stamp.native_of(HARNESS).filter(|_| self.is_pi());
        if !self.is_pi() {
            self.report.add(
                THINKING,
                1,
                Some("Pi takes thinking only from the model that wrote it"),
            );
            return;
        }
        let mut block = Map::new();
        block.insert("type".into(), json!("thinking"));
        block.insert("thinking".into(), json!(text));
        if let Some(signature) = native
            .and_then(|fields| fields.get("signature"))
            .and_then(Value::as_str)
        {
            block.insert("thinkingSignature".into(), json!(signature));
        }
        if native.is_some_and(|fields| fields.get("redacted") == Some(&json!(true))) {
            block.insert("redacted".into(), json!(true));
        }
        self.assistant(stamp, Value::Object(block));
    }

    /// How a call is written: Pi's own tool when there is one.
    fn native_call(&self, call: &Call) -> Option<(String, Value)> {
        let name = call.name.as_deref()?;
        let input = call.raw_input.as_ref()?;
        if self.is_pi() {
            return input.is_object().then(|| (name.to_owned(), input.clone()));
        }
        let (tool, normal) =
            coqui_acp_history::tools::normalise(&self.doc.header.origin.harness, name, input)?;
        let (tool, normal) = if tool == Tool::Patch {
            patch_as_write_or_edit(&normal)?
        } else {
            (tool, normal)
        };
        native_input(tool, &normal)
    }

    fn is_user_command(&self, call: &Call) -> bool {
        self.is_pi()
            && coqui_acp_history::conversation::native_of(call.call_meta.as_ref(), HARNESS)
                .and_then(|fields| fields.get("userCommand"))
                == Some(&json!(true))
    }

    fn call_id(&mut self, id: &str) -> String {
        if let Some(known) = self.ids.get(id) {
            return known.clone();
        }
        let clean = clean_id(id, &self.ids);
        self.ids.insert(id.to_owned(), clean.clone());
        clean
    }

    fn call(&mut self, call: &Call) {
        if self.is_user_command(call) {
            return;
        }
        let stamp = Stamp::new(call.call_at.clone(), call.call_meta.clone());
        self.note_time(&stamp);
        if let Some((name, input)) = self.native_call(call) {
            let id = self.call_id(&call.id);
            self.assistant(
                &stamp,
                json!({"type": "toolCall", "id": id, "name": name, "arguments": input}),
            );
        }
    }

    fn result(&mut self, call: &Call) {
        if !call.announced {
            return;
        }
        let stamp = Stamp::new(
            call.result_at.clone().or_else(|| call.call_at.clone()),
            call.result_meta.clone(),
        );
        self.note_time(&stamp);
        self.draft = None;
        if self.is_user_command(call) {
            self.user_command(call, &stamp);
        } else if let Some((name, _)) = self.native_call(call) {
            let id = self.call_id(&call.id);
            let at = self.millis();
            let content = self.result_content(call);
            self.message(
                json!({"role": "toolResult", "toolCallId": id, "toolName": name,
                "content": content, "isError": call.is_failed(), "timestamp": at}),
            );
        } else {
            self.report.add(
                TOOL_AS_NOTE,
                1,
                Some("Pi has no equivalent tool, so the call is written as text"),
            );
            self.assistant(&stamp, json!({"type": "text", "text": call_note(call)}));
        }
    }

    /// A command the person ran, as the message Pi keeps of one.
    fn user_command(&mut self, call: &Call, stamp: &Stamp) {
        let native = stamp.native_of(HARNESS);
        let command = call
            .raw_input
            .as_ref()
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut message = json!({"role": "bashExecution", "command": command,
            "output": call.output_text(),
            "cancelled": call.status.as_deref() == Some("cancelled"),
            "truncated": native.and_then(|f| f.get("truncated")) == Some(&json!(true)),
            "timestamp": self.millis()});
        if let Some(code) = native.and_then(|fields| fields.get("exitCode")) {
            message["exitCode"] = code.clone();
        }
        self.message(message);
    }

    fn result_content(&self, call: &Call) -> Value {
        if call.status.as_deref() == Some("cancelled")
            && call.raw_output.is_none()
            && call.content.is_empty()
        {
            return json!([{"type": "text", "text": CANCELLED_RESULT}]);
        }
        if self.is_pi()
            && let Some(Value::Array(blocks)) = &call.raw_output
        {
            return Value::Array(blocks.clone());
        }
        let text = call.output_text();
        let text = match (&call.raw_output, text.is_empty()) {
            (Some(Value::String(text)), _) => text.clone(),
            (Some(other), true) if !other.is_null() => other.to_string(),
            _ => text,
        };
        json!([{"type": "text", "text": text}])
    }

    /// Pi content blocks of ACP's.
    fn content(&mut self, blocks: &[Value]) -> Vec<Value> {
        let mut out = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        out.push(json!({"type": "text", "text": text}));
                    }
                }
                Some("image") => {
                    let data = block
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let mime = block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    if data.is_empty() {
                        self.left_out(block);
                        out.push(json!({"type": "text", "text": format!("[an attachment of {mime} was left out of the history]")}));
                    } else {
                        out.push(json!({"type": "image", "data": data, "mimeType": mime}));
                    }
                }
                Some("resource_link") => {
                    let uri = block.get("uri").and_then(Value::as_str).unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .unwrap_or(uri);
                    out.push(json!({"type": "text", "text": format!("[@{name}]({uri})")}));
                }
                Some("resource") => {
                    let uri = block
                        .pointer("/resource/uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Some(text) = block.pointer("/resource/text").and_then(Value::as_str) {
                        out.push(json!({"type": "text", "text": context_block(uri, text)}));
                    } else {
                        self.report.add(
                            CONTENT_DROPPED,
                            1,
                            Some("a resource of bytes, which Pi reads only as an image"),
                        );
                    }
                }
                _ => self.report.add(
                    CONTENT_DROPPED,
                    1,
                    Some("a content block of a kind Pi does not take"),
                ),
            }
        }
        out
    }

    fn left_out(&mut self, block: &Value) {
        let by_digest = block
            .get("_meta")
            .and_then(|meta| meta.get(ATTACHMENT_META_KEY))
            .is_some();
        self.report.add(
            if by_digest {
                ATTACHMENT_BY_DIGEST
            } else {
                CONTENT_DROPPED
            },
            1,
            Some("the history left the bytes out"),
        );
    }
}

fn zero_usage() -> Value {
    json!({"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}})
}

/// A patch of one change as the normalised Write or Edit it is.
fn patch_as_write_or_edit(normal: &Value) -> Option<(Tool, Value)> {
    use coqui_acp_history::patch::{FileOp, parse};
    Some(match parse(normal.get("patch")?.as_str()?)? {
        FileOp::Add { path, content } => (Tool::Write, json!({"path": path, "content": content})),
        FileOp::Update { path, old, new } => {
            (Tool::Edit, json!({"path": path, "old": old, "new": new}))
        }
        _ => return None,
    })
}

/// The input of Pi's tool for the normalised `tool`, from its normalised `input`.
fn native_input(tool: Tool, normal: &Value) -> Option<(String, Value)> {
    let mut normal = normal.clone();
    if tool == Tool::Shell {
        // Pi's bash takes a command line as text; an argument vector is joined. It runs where Pi does and has
        // no directory or description of its own.
        if let Some(argv) = normal.get("command").and_then(Value::as_array) {
            let words: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
            normal["command"] = json!(shell_text(&words));
        }
        normal
            .as_object_mut()?
            .retain(|key, _| ["command", "timeout_ms"].contains(&key.as_str()));
    }
    let (name, input) = coqui_acp_history::tools::denormalise(tool, HARNESS, &normal)?;
    Some((name.to_owned(), input))
}

/// The time `at` (RFC 3339) in milliseconds since 1970, when it is one.
fn millis(at: &str) -> Option<u64> {
    let digits = |from: usize, length: usize| at.get(from..from + length)?.parse::<i64>().ok();
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let rest = at.get(19..)?;
    let (fraction, rest) = match rest.strip_prefix('.') {
        Some(after) => {
            let end = after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len());
            let mut digits: String = after[..end].chars().take(3).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            (digits.parse::<i64>().ok()?, &after[end..])
        }
        None => (0, rest),
    };
    let offset = match rest.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let hours = rest.get(1..3)?.parse::<i64>().ok()?;
            let minutes = rest.get(4..6)?.parse::<i64>().ok()?;
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };
    // Days from the civil date, by the proleptic Gregorian calendar.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds * 1000 + fraction).ok()
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// The id in the header of the file at `path`, when it is a session file: its first line is read, up to the bound
/// Pi scans to.
fn header_id(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file.take(HEADER_SCAN_BYTES))
        .read_line(&mut line)
        .ok()?;
    let header: Value = serde_json::from_str(line.trim()).ok()?;
    (header.get("type").and_then(Value::as_str) == Some("session"))
        .then(|| string(&header, "id"))
        .flatten()
}

/// The session file of `id` in `directory`: the file whose header names it, a file named for it first.
///
/// # Errors
///
/// [`Error::Refused`] when the directory cannot be read.
pub fn find(directory: &Path, id: &str) -> Result<Option<PathBuf>, Error> {
    let listing = match std::fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(refused("the sessions directory could not be read")),
    };
    let mut files: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl") && path.is_file())
        .collect();
    files.sort_by_key(|path| {
        !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(id))
    });
    Ok(files
        .into_iter()
        .find(|path| header_id(path).as_deref() == Some(id)))
}

/// The history of session `id` from the session file in `directory`.
///
/// # Errors
///
/// [`Error::Refused`] when there is no such session file, it is larger than `max_bytes`, or it is not one.
pub fn export_file(directory: &Path, id: &str, max_bytes: u64) -> Result<Document, Error> {
    let path = find(directory, id)?.ok_or_else(|| refused("no session has this id"))?;
    let file =
        std::fs::File::open(&path).map_err(|_| refused("the session's file could not be read"))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| refused("the session's file could not be read"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(refused(format!(
            "the session's file is larger than {max_bytes} bytes"
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    export(&text, id)
}

/// Writes the session file of `doc` as session `id` in `cwd`, into `directory` (the one
/// [`PiLaunch::sessions_dir`](crate::PiLaunch::sessions_dir) names, unless Pi was told another), where `pi --session` finds it. A session there
/// already is never replaced.
///
/// # Errors
///
/// [`Error::Refused`] for an `id` that is not a UUID, a session that exists or a directory that cannot be made.
pub fn import_file(
    directory: &Path,
    cwd: &Path,
    id: &str,
    doc: &Document,
    options: &ImportOptions,
) -> Result<Imported, Error> {
    let imported = import(doc, &cwd.to_string_lossy(), id, options)?;
    std::fs::create_dir_all(directory)
        .map_err(|_| refused("the session's directory could not be made"))?;
    if find(directory, id)?.is_some() {
        return Err(refused("the id names a session already"));
    }
    let path = directory.join(imported.file_name(id));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                refused("the id names a session already")
            } else {
                refused("the session's file could not be written")
            }
        })?;
    if file.write_all(imported.text.as_bytes()).is_err() {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(refused("the session's file could not be written"));
    }
    Ok(imported)
}
