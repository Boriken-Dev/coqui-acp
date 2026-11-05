# `coqui-acp-claude` — API header

Header-file-style reference for the `coqui-acp-claude` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `coqui-acp-harness`, `serde_json`, `uuid` and, for the command, `clap`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0. Tested with Claude Code 2.1.318 (`TESTED_VERSIONS`).

Imports: everything is at the crate root, and `parse` is public. `coqui_acp_claude::harness` is `coqui-acp-harness`, for its events, processes and ACP server; `Launch`, `PlainSpawn`, `Spawn` and `Spawned` are re-exported from it.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Launch (`ClaudeLaunch`)

```rust
#[non_exhaustive]                                           // Clone, PartialEq; Debug shows env names only
pub struct ClaudeLaunch {
    pub executable: OsString,              // "claude"
    pub cwd: PathBuf,
    pub session: SessionStart,             // New(uuid) | Resume(id)
    pub model: Option<String>,
    pub effort: Option<String>,            // low, medium, high, xhigh, max
    pub append_system_prompt: Option<String>,
    pub mcp_servers: Vec<McpServer>,
    pub mcp_config_file: Option<PathBuf>,  // the file --mcp-config names
    pub strict_mcp_config: bool,           // false
    pub add_dirs: Vec<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    pub permission_mode: Option<String>,
    pub setting_sources: Option<Vec<String>>,   // None: Claude Code's default; Some(empty): none
    pub max_budget_micro_usd: Option<u64>,
    pub partial_messages: bool,            // true
    pub env: BTreeMap<OsString, OsString>,
    pub clear_env: bool,
    pub extra_args: Vec<OsString>,         // the caller's own, after the rest, unchecked
}
pub fn ClaudeLaunch::new(cwd: impl Into<PathBuf>) -> ClaudeLaunch    // a new session, its id a new UUID
pub fn session_id(&self) -> &str
pub fn launch(&self) -> Launch
#[non_exhaustive] pub enum SessionStart { New(String), Resume(String) }
pub fn mcp_config(servers: &[McpServer]) -> Value        // {"mcpServers": {name: {type, command, args, env} | {type, url, headers}}}
pub fn valid_session_id(id: &str) -> bool                // letters, digits and dashes, 1 to 128
```

`launch` gives `-p --input-format=stream-json --output-format=stream-json --verbose --replay-user-messages --permission-prompts=host --permission-prompt-tool=stdio`, `--include-partial-messages` when set, `--session-id=` or `--resume=`, then `--model=`, `--effort=`, `--append-system-prompt=`, `--permission-mode=`, `--setting-sources=` (comma-separated), `--max-budget-usd=` (dollars, six decimals), `--mcp-config=<mcp_config_file>` when set (MCP servers never go on the command line, since their environment and headers may be credentials), `--strict-mcp-config`, one `--add-dir=` and one `--plugin-dir=` per directory, and `extra_args`. Every value is one `--flag=value` argument, so a value that starts with `-` is never read as an option.

## Session (`ClaudeSession`)

```rust
pub fn ClaudeSession::start(launch: &ClaudeLaunch, spawn: &dyn Spawn, settings: ClaudeSettings,
    sink: impl Fn(Event) -> bool + Send + Sync + 'static) -> Result<ClaudeSession, Error>
pub fn id(&self) -> &str
pub fn version(&self) -> Option<String>
pub fn models(&self) -> Vec<Model>
pub fn prompt(&self, content: &[Content]) -> Result<(), Error>
pub fn interrupt(&self) -> Result<(), Error>
pub fn set_model(&self, model: &str) -> Result<(), Error>
pub fn set_effort(&self, effort: Option<&str>) -> Result<(), Error>      // None: Claude Code's own
pub fn close(&self)                                                      // also on drop
#[non_exhaustive] pub struct Model { pub id: String, pub name: String, pub description: Option<String>, pub efforts: Vec<String> }
pub fn models_of(answer: &Value) -> Vec<Model>
pub fn user_blocks(content: &[Content]) -> Result<Vec<Value>, Error>
```

`start` writes `mcp_config` of the launch's MCP servers to a new file in the system's temporary directory, readable only by this user on Unix, names it as `mcp_config_file` unless the launch names one, and removes it on `close` and drop; it then spawns Claude Code, writes an `initialize` control request, which Claude Code answers with no model turn and before any message, and waits for it up to `initialize_timeout`; it then sends `Event::Commands` with the commands of that answer, and an `Event::Notice` when the version is not one of `TESTED_VERSIONS`. Events reach `sink` from a thread of the session's own; one that answers `false` is not called again. The output's end is one `Event::Exited`, saying how Claude Code ended (`exited with code N`, `was ended by signal N`) and the last lines it wrote that were not protocol, standard error's included; none after `close`. A session that reports an id other than its own is killed and ends with `Event::Exited` saying so; nothing of the other session is told.

`prompt` writes one `user` message: text; an image as a base64 source; a resource link as `[@name](uri)`; a text resource as `<context ref="uri">…</context>`, the address's `&"<>` escaped and every `</context` of the text written `&lt;/context`, so it cannot close the block; an image resource as an image. Any other resource is `Error::Refused`. The first prompt's first line, cut to 80 bytes, is the `Event::Title`. `interrupt` writes an `interrupt` control request and does not wait; the turn's error result then ends it as `Stop::Cancelled`. `set_model` sends `set_model`, `set_effort` sends `apply_flag_settings` with `effortLevel`, each waiting up to `control_timeout`. Each line is one write.

`ClaudeSettings` (`new()`, `Default`; set its fields): `max_line` (64 MiB; a longer output line ends the session), `tail_lines` (8) and `tail_line_bytes` (512) quoted by an exit, `initialize_timeout` (30 s), `control_timeout` (30 s), `exit_wait` (1 s for the exit status), `max_tools` (4096 tool calls kept for their results), `max_transcript_bytes` (64 MiB replayed).

`models_of` reads `models[]` of an `initialize` answer: `value` as the id, `displayName`, `description` with `resolvedModel` when it differs, and `supportedEffortLevels` when `supportsEffort`. The answer's `account` is never read.

## Parser (`coqui_acp_claude::parse`)

```rust
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;
pub type ControlAnswer = (String, Result<Value, String>);
pub const ALLOW_ONCE: &str = "allow";  pub const ALLOW_ALWAYS: &str = "allow_always";  pub const REJECT: &str = "reject";
pub fn Parser::new(writer: Writer, max_tools: usize) -> Parser
pub fn line(&mut self, value: &Value) -> Vec<Event>
pub fn history(&mut self, value: &Value) -> Vec<Event>     // a transcript line: the person's messages told too
pub fn interrupting(&mut self)
pub fn answers(&mut self) -> Vec<ControlAnswer>            // our control requests' answers since the last call
pub fn version(&self) -> Option<&str>;  pub fn session(&self) -> Option<&str>
```

| Line | Events |
| --- | --- |
| any with `session_id` | `SessionId`, when it changes |
| `system` `init` | its `claude_code_version` is kept |
| `system` `compact_boundary`, then a synthetic message of the person's | `Compaction` with the summary |
| `system` `informational`, `notification` | `Notice` |
| `stream_event` text and thinking deltas | `MessageChunk`, `ThoughtChunk`, then `MessageEnd`, `ThoughtEnd`; message ids `<message id>:<block index>` |
| `assistant` | text and thinking not already streamed (an API error message's text is left to the result); each `tool_use` as `Tool` with title, kind, input, locations, and a diff for `Edit` and `Write`; `Bash` and `PowerShell` also `Terminal` with the command; `TodoWrite` as `Plan` instead; another `*_tool_use` block as `Tool` named `TYPE:NAME` |
| `user` tool results | `Tool` completed or failed: `Edit` and `Write` with a diff whose `patch` is made from `structuredPatch`, located at each hunk; a command's output as `Terminal` with exit 0, or the code its failure names; other output as `ToolChunk` text (not `Read`'s); the Task tools' results update the `Plan` |
| `result` | `Usage` (running totals, `total_cost_usd`, `contextWindow` of `modelUsage` as the size, the last request's tokens as context used), `Denied` with `permission_denials` named once each, then `TurnEnd` |
| `rate_limit_event` | `Quota`; `utilization` taken only as a fraction |
| `control_request` `can_use_tool` | `Permission`; any other subtype is answered with an error at once |
| `control_response` | kept for `answers`, never an event |

`TurnEnd`: a usage limit (`rate_limit_event` `rejected`, or an error result naming `usage limit`, `limit reached`, `hit your limit`, or with `api_error_status` 429; a reset after a trailing `|`, in seconds or milliseconds; `five_hour` or `weekly` from the text) is `Limited`; an error result after `interrupting`, or with a `terminal_reason` starting `aborted`, is `Stopped(Cancelled)`; `error_max_turns` is `Stopped(MaxTurnRequests)`; another error is `Failed` with its `result` or `errors`; otherwise `Stopped` by `stop_reason` (`max_tokens`, `refusal`, else `EndTurn`).

A permission request offers `allow` (Allow once), `allow_always` when one of Claude Code's `permission_suggestions` grants something (a mode, an allow rule, a directory), named for every suggestion it applies and where each is kept, such as `Allow, and from now on: accept edits (acceptEdits) for this session` or `Allow, and from now on: allow mcp__echo__echo in local settings`, and `reject`. Its responder answers `{"behavior": "allow", "updatedInput": <input>}`, with `updatedPermissions` the suggestions for `allow_always`; `reject` is `{"behavior": "deny", "message": "The person refused this."}`; `Outcome::Cancelled` also sets `"interrupt": true`. The title is Claude Code's, else `<display name>: <tool title>`; the description its `decision_reason` or `description`.

## History

```rust
pub fn config_dir(env: &BTreeMap<OsString, OsString>, clear_env: bool) -> Option<PathBuf>
pub fn project_directory(cwd: &Path) -> String          // every character but an ASCII letter or digit as `-`
pub fn transcript(config: &Path, cwd: &Path, id: &str) -> Result<PathBuf, Error>
pub fn replay(transcript: &Path, max_bytes: u64, max_tools: usize, sink: &dyn Fn(Event) -> bool) -> Result<(), Error>
```

`config_dir` is `CLAUDE_CONFIG_DIR`, else `.claude` in `USERPROFILE` (Windows) or `HOME`, from the launch's environment over this process's. `transcript` is `projects/<project_directory(cwd)>/<id>.jsonl`, only that one and not through a link, for an id `valid_session_id` takes: `--resume` finds a session only from the directory that recorded it. `replay` sends the person's messages (not command output, interruptions or meta messages), the agent's text and thinking, and tool calls with their results; past `max_bytes` the rest is left out and a `Notice` says so.

## ACP agent (`ClaudeAgent`)

```rust
pub fn ClaudeAgent::new() -> ClaudeAgent                     // also Default; claude from PATH, PlainSpawn
pub fn defaults(self, defaults: ClaudeLaunch) -> Self
pub fn spawn(self, spawn: Arc<dyn Spawn>) -> Self
pub fn settings(self, settings: ClaudeSettings) -> Self
pub const fn client_plugins(self, allowed: bool) -> Self   // default true
pub fn cli_version(&self) -> Option<String>                 // claude --version, once, 10 s
impl Harness for ClaudeAgent
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub const DEFAULT: &str = "default";
```

`agentInfo` is `coqui-acp-claude`, titled `Claude Code <version>`, at this crate's version. It takes images, embedded context, MCP servers over stdio, HTTP and SSE (version 1), load, more directories and an appended system prompt. A session's parts are added to `defaults`: its `cwd`, a new UUID or the id resumed or loaded, `systemPrompt` appended after the defaults' own, `mcpServers`, `additionalDirectories` as `--add-dir`, and `_meta.claudeCode.options.plugins` (`[{"type": "local", "path": absolute}]`, as the Claude Agent SDK's `plugins` option) as `--plugin-dir`, whose skills are `<plugin>:<skill>`. A plugin of another type, a relative path, or any plugin when `client_plugins(false)`, is refused, as is an MCP server named as one of the defaults'. Load replays the transcript, then resumes.

Config options: `model` (category `model`): the models Claude Code lists, and the current one; `effort` (category `thought_level`): `default` and the current model's efforts, else `EFFORTS`. Setting one not offered is refused; setting one sends `set_model` or `apply_flag_settings` for the turns that follow.

## Errors

`Error` (`#[non_exhaustive]`): `Spawn(io::Error)`, `Refused(String)`, `Timeout { what, limit }`, `Harness(String)` in Claude Code's words or saying how it ended. Into `HarnessError`: `Refused` stays refused, the rest is `Failed`.

## Command line

`coqui-acp-claude` serves `ClaudeAgent` on standard input and output. Flags: `--executable` (`COQUI_ACP_CLAUDE_EXECUTABLE`, default `claude`), `--model`, `--effort`, `--permission-mode`, `--setting-sources` (comma-separated, empty for none), `--strict-mcp-config`, `--max-acp-version` (1 or 2, default 2), `--max-sessions` (8). Notices and exits go to standard error. Exit status 0 when the client closes, 1 when reading it failed, 2 for a wrong invocation.

## Environment variables

`COQUI_ACP_CLAUDE_EXECUTABLE`, as `--executable`. Claude Code reads its own (`CLAUDE_CONFIG_DIR`, `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` and others) from the environment the agent runs in, or `ClaudeLaunch::env`.

## Gotchas

- `--permission-prompts host` alone does not route prompts to standard output on Claude Code 2.1.318: the call is refused as not granted. `--permission-prompt-tool=stdio` does, and the launch passes both.
- `--resume` finds a session only from the directory that recorded it.
- With stream-json input Claude Code writes nothing, not even `init`, until it reads a line; `initialize` is answered at once.
- Claude Code 2.1.318 in print mode offers neither `TodoWrite` nor the Task tools (`No such tool available`, `CLAUDE_CODE_ENABLE_TASKS` either way); a plan the model writes with them is still told.
- A permission answered `reject` lets the turn go on; only `Outcome::Cancelled` interrupts it.
- Everything Claude Code writes is the model's or the harness's: untrusted.
- A plugin's hooks and MCP servers run with no permission asked, whatever `--strict-mcp-config` and `--setting-sources` say: `client_plugins(false)` is how a host keeps them out.
- A diagnostic line on standard error longer than `max_line` is noted and the rest of standard error read and dropped, so Claude Code never writes to a closed pipe.
- An appended system prompt goes on the command line: past the platform's limit (128 KiB for one argument on Linux, 32 767 characters for a command line on Windows) Claude Code does not start.
