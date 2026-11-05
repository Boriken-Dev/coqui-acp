# `coqui-acp-harness` — API header

Header-file-style reference for the `coqui-acp-harness` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `base64`, `serde_json` and `uuid`, and `win32job` on Windows or `rustix` on Unix. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: the root re-exports `acp`'s `AgentInfo`, `ConfigOption`, `Conversation`, `EventSink`, `Harness`, `HarnessError`, `McpServer`, `McpTransport`, `Open`, `OpenRequest`, `OptionValue`, `Settings`, `Takes` and `serve`; `event`'s `Event`, `Outcome`, `PermissionRequest` and `Responder`; and `process`'s `Exit`, `Launch`, `PlainSpawn`, `Process`, `Spawn` and `Spawned`. The modules `acp`, `event`, `lines`, `process` and `version` are public.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Processes (`coqui_acp_harness::process`)

```rust
#[non_exhaustive] pub struct Launch { pub program: OsString, pub args: Vec<OsString>, pub cwd: Option<PathBuf>,
    pub env: BTreeMap<OsString, OsString>, pub remove_env: Vec<OsString>, pub clear_env: bool }   // Clone, Default, PartialEq
pub fn Launch::new(program: impl Into<OsString>) -> Launch
pub fn arg(self, arg) -> Self;  pub fn args(self, args) -> Self;  pub fn cwd(self, cwd) -> Self
pub fn env(self, name, value) -> Self;  pub const fn clear_env(self, clear: bool) -> Self
pub fn display(&self) -> String          // the command line, for messages

pub trait Spawn: Send + Sync { fn spawn(&self, launch: &Launch) -> io::Result<Spawned>; }
pub trait Process: Send + Sync { fn kill(&self) -> io::Result<()>; fn try_wait(&self) -> io::Result<Option<Exit>>; }
#[non_exhaustive] pub struct Spawned { pub stdin: Box<dyn Write + Send>, pub stdout: Box<dyn Read + Send>,
    pub stderr: Option<Box<dyn Read + Send>>, pub process: Box<dyn Process> }
pub fn Spawned::new(stdin, stdout, stderr, process) -> Spawned
#[non_exhaustive] pub struct Exit { pub code: Option<i32>, pub signal: Option<i32> }   // Display: "exited with code 3"
pub fn exit_text(process: &dyn Process, wait: Duration) -> String   // how it ended, or "closed its output"
pub struct PlainSpawn;
```

Each argument reaches the program as one argument, with no shell. `Launch`'s `Debug` names the environment's variables and never shows their values.

`PlainSpawn` pipes all three streams. Its `kill` ends the whole tree: on Windows the process is put in a job object of its own just after it starts, and closing the job ends everything in it; on Unix it leads a process group, which is sent `SIGKILL`, and it is not collected before that, so the group id stays its own. A process started in the instant before it joins its job, or one that leaves the group, escapes. `kill` then collects the process; killing one that has ended is no error, and once it is collected nothing is sent to its id or group again; `try_wait` then answers how it ended. On Unix `try_wait` leaves a running process uncollected. Dropping the process kills it as `kill` does.

Implement `Spawn` to run a harness elsewhere: in a container, over SSH, under a terminal.

## Lines (`coqui_acp_harness::lines`)

```rust
pub enum Line { Json(Value), Text(String), TooLong(u64) }
pub fn read_lines(reader: impl Read, max_line: u64, each: impl FnMut(Line) -> bool) -> io::Result<()>
pub fn parse(bytes: &[u8]) -> Option<Line>        // None for a blank line
pub struct Tail;  pub const fn Tail::new(max_lines: usize, max_line_bytes: usize) -> Tail
pub fn push(&mut self, line: &str);  pub fn text(&self) -> String;  pub fn is_empty(&self) -> bool
pub fn cut(text: &str, max: usize) -> &str       // on a character boundary
pub fn one_line(text: &str) -> String            // each control character a space, for a log or a label
pub fn read_diagnostics(reader: impl Read, max_line: u64, tail: &Mutex<Tail>)   // standard error into `tail`, read to its end
```

`read_lines` reads until the end or until `each` answers `false`. A line over `max_line` bytes is handed over as `TooLong` with how much was read, and reading stops. A line ending is dropped, `\r\n` included. `Tail` keeps the last lines that were not protocol, for the message shown when a harness ends.

## Events (`coqui_acp_harness::event`)

`Event` (`#[non_exhaustive]`, `Debug`) is what a harness reports, in no protocol's terms:

| Variant | Carries |
| --- | --- |
| `SessionId(String)` | the harness's session id, once it says it |
| `UserChunk { message, content: Content }`, `UserEnd { message }` | the person's messages, in replayed history |
| `MessageChunk { message, text }`, `MessageEnd { message }` | agent text, and the end of a message |
| `ThoughtChunk { message, text }`, `ThoughtEnd { message }` | reasoning |
| `Tool(ToolCall)` | a tool call announced or changed: a `None` field is unchanged; `content` and `locations` replace the list |
| `ToolChunk { id, content: ToolContent }` | content added to a tool call |
| `Terminal(Terminal)` | a terminal of the agent's own: `id`, `command`, `output` to add, `exit: Option<Option<i32>>` once it ended |
| `Plan(Vec<PlanEntry>)` | the plan, whole |
| `Permission(PermissionRequest)` | a call that needs leave |
| `Usage(Usage)` | tokens (`input`, `output`, `cache_read`, `cache_write`, `cumulative`), `cost_usd`, `context_used`, `context_size` |
| `Quota(Vec<Quota>)` | quota windows: `window`, `used_permille`, `resets_at`, `status` |
| `Commands(Vec<Command>)` | commands by name: `name`, `description`, `hint` |
| `Title(String)` | the session's title |
| `Notice { severity: Severity, title }` | something for the person outside the conversation |
| `Compaction { id, summary: Option<String> }` | the conversation was compacted |
| `Denied(Vec<String>)` | tools the harness refused by its own rules this turn |
| `TurnEnd(TurnEnd)` | `Stopped(Stop)`, `Failed(String)` or `Limited(Limit)` (`resets_at`, `window`, `text`) |
| `Output(String)` | a line that was not protocol |
| `Exited(String)` | the harness ended, how, and what it wrote last |

`context_block(uri, text) -> String` writes a text resource as prompt text that it cannot close.

`Content`: `Text`, `Image { mime_type, data }` (base64), `ResourceLink { uri, name }`, `Resource { uri, mime_type, text, blob }`. `ToolCall` (`Default`): `id`, `name`, `title`, `kind: Option<ToolKind>`, `status: Option<ToolStatus>`, `input`, `output`, `locations: Option<Vec<Location>>`, `content: Option<Vec<ToolContent>>`; `ToolCall::new(id)`. `ToolContent`: `Text`, `Diff(Diff)`, `Terminal(id)`. `Diff`: `path`, `change: FileChange` (`Add`, `Modify`, `Delete`), `old_text`, `new_text`, `patch` (a git patch); `Diff::new(path, change)`. `ToolKind`, `ToolStatus`, `PlanStatus`, `Severity`, `Stop` and `OptionKind` have `as_str`, their names on the wire. Each `#[non_exhaustive]` struct has a `new` or `Default`.

```rust
pub struct PermissionRequest { pub tool: ToolCall, pub title: String, pub description: Option<String>,
    pub options: Vec<PermissionOption>, pub responder: Responder }
pub fn PermissionRequest::new(tool, title, description, options, responder) -> PermissionRequest
pub struct PermissionOption { pub id: String, pub name: String, pub kind: OptionKind }
pub enum Outcome { Selected(String), Cancelled }
pub fn Responder::new(answer: impl FnOnce(Outcome) -> io::Result<()> + Send + 'static) -> Responder
pub fn answer(self, outcome: Outcome) -> io::Result<()>
```

A responder answers once; one dropped unanswered answers `Outcome::Cancelled`, so the harness is never left waiting. A tool call's input and output are the model's, untrusted.

## The ACP agent (`coqui_acp_harness::acp`)

```rust
pub trait Harness: Send + Sync + 'static {
    fn info(&self) -> AgentInfo;
    fn takes(&self) -> Takes;
    fn open(&self, request: OpenRequest, events: EventSink) -> Result<Box<dyn Conversation>, HarnessError>;
}
pub trait Conversation: Send + Sync {
    fn session_id(&self) -> String;
    fn options(&self) -> Vec<ConfigOption>;
    fn prompt(&self, content: Vec<Content>) -> Result<(), HarnessError>;
    fn interrupt(&self) -> Result<(), HarnessError>;
    fn set_option(&self, id: &str, value: &str) -> Result<Vec<ConfigOption>, HarnessError>;
    fn close(&self);
}
pub fn serve(harness: Arc<dyn Harness>, settings: Settings, input: impl Read, output: impl Write + Send + 'static) -> io::Result<()>
pub fn absolute_path(path: &str, what: &str) -> Result<PathBuf, String>
pub struct EventSink;  pub fn send(&self, event: Event) -> bool     // Clone; waits while the queue is full; false once the session is gone
pub type NoticeHook = Arc<dyn Fn(&str) + Send + Sync>;
pub const LIMIT_META_KEY: &str = "dev.boriken.coqui.acp/limit";
pub const PLAN_ID: &str = "plan";
```

`open` returns once the session's id is known; for `Open::Load` it sends the history to `events` first. A turn's events and its `TurnEnd` go to the sink. Any `Conversation` method may be called while another runs.

`OpenRequest` (`new(cwd)`): `cwd`, `open: Open` (`New`, `Resume(id)`, `Load(id)`), `mcp_servers: Vec<McpServer>` (`name`, `transport: McpTransport` of `Stdio { command, args, env }`, `Http { url, headers }`, `Sse { url, headers }`; `Debug` shows names, never values), `system_prompt`, `additional_directories`, `meta` (the request's `_meta`, the client's, untrusted). `Takes` (`new()` and one setter per field): `images`, `embedded_context`, `mcp_http`, `mcp_sse`, `load`, `additional_directories`, `system_prompt`. `ConfigOption::new(id, name, current, values)` with `category` and `description`; `OptionValue::new(id, name, description)`. `AgentInfo::new(name, title, version)`. `HarnessError`: `Refused(String)` is answered invalid params, `Failed(String)` internal error.

`serve` runs on the calling thread until the input ends, then closes every session it opened. Requests that take time run on threads of their own.

- `initialize` answers version 2 to a client that asks for 2 or more, else version 1, up to `Settings::max_version`. Version 1: `agentCapabilities` with `loadSession`, `promptCapabilities`, `mcpCapabilities`, `sessionCapabilities` (`resume`, `close`, and `additionalDirectories` and `systemPrompt` when taken), `agentInfo`, `authMethods: []`. Version 2: `info` and `capabilities.session` (`prompt.image`, `prompt.embeddedContext`, `mcp.stdio`, `mcp.http`, `close`, `additionalDirectories`, `systemPrompt`). Any other request first is refused.
- `authenticate` answers `{}`. `session/new`, `session/load`, `session/resume` (version 2's `replayFrom: {"type": "start"}` replays, as `load`) check every part against `Takes` and `Settings` before the harness is asked: a `cwd`, MCP command or directory that is not absolute by this platform's rules (a drive or UNC path on Windows, `/…` elsewhere), a network or device path (`\\host\share`, `//host/share`, `\\?\…`, `\\.\…`) unless `network_paths`, a directory outside `working_roots` or holding `.` or `..` when roots are set, MCP servers when `client_mcp_servers` is off, two servers of one name, a transport not taken (version 2 has no SSE), a system prompt in a mode other than `append`, or a part not taken is invalid params. `new` answers `{sessionId, configOptions}` before the session's first events; `load` answers `{configOptions}` after its history; `resume` answers at once.
- A session being opened counts against `max_sessions` as an open one. `session/prompt` takes its turn on the thread that reads the client, so a `session/cancel` on the next line finds it; it takes text, images, resource links and resources as taken; any other block is invalid params, as is one larger than `max_prompt_bytes`. One turn at a time: a prompt while one runs is invalid request. Version 1 answers `{stopReason, usage}` at the turn's end; version 2 answers `{messageId}` at once, after a `user_message` and a `running` state update, and ends the turn with an `idle` state update carrying `stopReason` and `usage`. A failed turn is error `-32603` `Internal error` with the harness's words in `data.message` (version 1), or `idle` with `stopReason: "error"` and `error` (version 2). A limit is stop reason `refusal` with `_meta[LIMIT_META_KEY]`, and in version 2 a notice first.
- `session/cancel` calls `interrupt` once per turn, after the turn's prompt has reached the harness; the turn's end is answered `cancelled`. If it has not ended within `cancel_grace` it is answered `cancelled` anyway, its later end is dropped, and a prompt is refused until it comes.
- `session/set_config_option` (`configId`, `value` an id) calls `set_option` and answers `{configOptions}`: `id` in version 1, `configId` in version 2.
- `session/close` answers a running turn `cancelled`, closes the harness's session and lets go of it, and answers `{}`. Anything else with an id is method not found.
- A permission request is `session/request_permission`: version 1 `{sessionId, toolCall, options}`; version 2 `{sessionId, title, description, subject: {type: "tool_call", toolCall}, options}`, with `requires_action` before it and `running` after. An answer naming an option not offered, an error, no answer within `permission_timeout`, or a client that has gone is `Outcome::Cancelled`; so is a question still open when its turn ends, is answered cancelled after `cancel_grace`, or its session closes, and the client's later answer is dropped.

What each event becomes:

| Event | Version 1 | Version 2 |
| --- | --- | --- |
| user, message, thought chunks | `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk` | the same, with `messageId` |
| their ends | nothing | `user_message`, `agent_message`, `agent_thought` with `content: []` |
| `Tool` | `tool_call` first, then `tool_call_update`; a diff as `{path, oldText, newText}`; `cancelled` as `failed` | `tool_call_update` only, with `name`; a diff as `{changes, patch}` |
| `ToolChunk` | `tool_call_update` with the whole content so far | `tool_call_content_chunk` |
| `Terminal` | its output added to its tool call's content | `terminal_update`, output base64-encoded |
| `Plan` | `plan` | `plan_update` with `planId` `plan` |
| `Usage` with context | `usage_update` (`used`, `size`, `cost` in USD) | the same |
| `Commands` | `available_commands_update` | the same, each tagged `type: "command"` |
| `Title` | `session_info_update` | the same |
| `Notice`, `Denied` | nothing | `notice` |
| `Compaction` | nothing | `compaction_summary_chunk`, `compaction_update` |
| `Exited` | the running turn fails | the same |

`Settings` (`#[non_exhaustive]`, `new()` and `Default`; set its fields): `max_line` (64 MiB; a longer client line ends the connection), `max_sessions` (8), `max_requests` (64 at once), `max_prompt_bytes` (16 MiB), `max_version` (2), `cancel_grace` (10 s), `permission_timeout` (`None`: as long as the person takes), `max_tools` (4096 tool calls tracked per session for version 1; past it the oldest is forgotten, and a later report of it is a new `tool_call`), `max_queued_events` (4096 events queued for a client that has not read them; past it the harness's `EventSink::send` waits, so a client that does not read holds the harness back; the same many are kept of what a session says before its answer, and past it they are dropped and the client told how many), `max_open_questions` (64 open at once per session, each holding a thread; past it one is answered cancelled without being asked), `client_mcp_servers` (true), `working_roots` (empty: any; also on `OpenRequest`, for a harness that finds a session by its id alone), `network_paths` (false), `on_notice: Option<NoticeHook>` (called with each notice and each harness exit, for a host that logs them).

## Versions (`coqui_acp_harness::version`)

```rust
pub fn untested(harness: &str, found: &str, tested: &[&str]) -> Option<String>
```

The warning for a version a driver was not tested with, or `None`. Nothing is refused for it.

## Errors

`serve` fails with `io::Error` when reading the client failed, or `InvalidData` for a line over `max_line`. `HarnessError`'s `Display` is `refused: …` or `failed: …`.

## Gotchas

- Version 1 never gets a version 2 kind, and version 2 never gets `tool_call`.
- A `Responder` that is dropped unanswered leaves the harness waiting: `serve` always answers.
- Events sent before `open` returns are told after `session/new`'s answer, since a client drops updates of a session it does not know yet.
