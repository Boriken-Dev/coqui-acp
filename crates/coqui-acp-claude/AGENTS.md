# `coqui-acp-claude` — API header

Header-file-style reference for the `coqui-acp-claude` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `coqui-acp-api`, `coqui-acp-harness` (with its `kind` feature), `serde_json`, `uuid` and, for the command, `clap`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0. Tested with Claude Code 2.1.318 (`TESTED_VERSIONS`).

Imports: everything is at the crate root, and `parse` is public. `coqui_acp_claude::harness` is `coqui-acp-harness`, for its events, processes and ACP server; `Launch`, `PlainSpawn`, `Spawn` and `Spawned` are re-exported from it.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Launch (`ClaudeLaunch`)

```rust
#[non_exhaustive]                                           // Clone, PartialEq; Debug shows env names only
pub struct ClaudeLaunch {
    pub executable: OsString,              // "claude"
    pub cwd: PathBuf,
    pub session: SessionStart,             // New(uuid) | Chosen | Resume(id)
    pub model: Option<String>,
    pub effort: Option<String>,            // low, medium, high, xhigh, max
    pub append_system_prompt: Option<String>,
    pub mcp_servers: Vec<McpServer>,
    pub mcp_config_file: Option<PathBuf>,  // the file --mcp-config names
    pub strict_mcp_config: bool,           // false
    pub add_dirs: Vec<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    pub permission_prompts: bool,          // true: --permission-prompts=host --permission-prompt-tool=stdio
    pub permission_mode: Option<String>,
    pub allow_bypass: bool,                // false: no `--allow-dangerously-skip-permissions`, and no Bypass mode offered
    pub modes: Option<Vec<Mode>>,          // those named widen the launch's mode and stricter ones (Bypass still needs allow_bypass)
    pub setting_sources: Option<Vec<String>>,   // None: Claude Code's default; Some(empty): none
    pub max_budget_micro_usd: Option<u64>,
    pub partial_messages: bool,            // true
    pub env: BTreeMap<OsString, OsString>,
    pub clear_env: bool,
    pub extra_args: Vec<OsString>,         // the caller's own, after the rest, unchecked
}
pub fn ClaudeLaunch::new(cwd: impl Into<PathBuf>) -> ClaudeLaunch    // a new session, its id a new UUID
pub fn session_id(&self) -> Option<&str>               // None when Claude Code is to choose it
pub fn launch(&self) -> Launch
#[non_exhaustive] pub enum SessionStart { New(String), Chosen, Resume(String) }
pub fn mcp_config(servers: &[McpServer]) -> Value        // {"mcpServers": {name: {type, command, args, env} | {type, url, headers}}}
pub fn valid_session_id(id: &str) -> bool                // letters, digits and dashes, 1 to 128
```

`launch` gives `-p --input-format=stream-json --output-format=stream-json --verbose --replay-user-messages --permission-prompts=host --permission-prompt-tool=stdio`, `--include-partial-messages` when set, `--session-id=` or `--resume=`, then `--model=`, `--effort=`, `--append-system-prompt=`, `--permission-mode=`, `--setting-sources=` (comma-separated), `--max-budget-usd=` (dollars, six decimals), `--mcp-config=<mcp_config_file>` when set (MCP servers never go on the command line, since their environment and headers may be credentials), `--strict-mcp-config`, one `--add-dir=` and one `--plugin-dir=` per directory, and `extra_args`. Every value is one `--flag=value` argument, so a value that starts with `-` is never read as an option.

## Session (`ClaudeSession`)

```rust
pub fn ClaudeSession::start(launch: &ClaudeLaunch, spawn: &dyn Spawn, settings: ClaudeSettings,
    sink: impl Fn(Event) -> bool + Send + Sync + 'static) -> Result<ClaudeSession, Error>
pub fn id(&self) -> Option<String>                       // the one asked for, or the one it chose once said
pub fn version(&self) -> Option<String>
pub fn models(&self) -> Option<Vec<Model>>               // None: it listed none at all, not an empty list
pub fn prompt(&self, content: &[Content]) -> Result<(), Error>
pub fn interrupt(&self) -> Result<(), Error>
pub fn set_model(&self, model: &str) -> Result<(), Error>
pub fn set_effort(&self, effort: Option<&str>) -> Result<(), Error>      // None: Claude Code's own
pub fn permission_mode(&self) -> Option<String>                          // `current_permission_mode` of its initialize answer
pub fn set_permission_mode(&self, mode: &str) -> Result<(), Error>       // `set_permission_mode`; takes effect at once, in a running turn too
pub fn on_permission_mode(&self, hook: impl Fn(&str) + Send + Sync + 'static)   // told each mode Claude Code reports or an "allow always" switches it to
pub fn close(&self)                                                      // also on drop
#[non_exhaustive] pub struct Model { pub id: String, pub name: String, pub description: Option<String>, pub efforts: Vec<String> }
pub fn models_of(answer: &Value) -> Option<Vec<Model>>
pub fn user_blocks(content: &[Content]) -> Result<Vec<Value>, Error>
```

`start` writes `mcp_config` of the launch's MCP servers to a new file in the system's temporary directory, readable only by this user on Unix and on Windows taking that directory's access, names it as `mcp_config_file` unless the launch names one, and removes it on `close` and drop; it then spawns Claude Code, writes an `initialize` control request, which Claude Code answers with no model turn and before any message, and waits for it up to `initialize_timeout`; it then sends `Event::Commands` with the commands of that answer, and an `Event::Notice` when the version is not one of `TESTED_VERSIONS`. Events reach `sink` from a thread of the session's own; one that answers `false` is not called again. The output's end is one `Event::Exited`, saying how Claude Code ended (`exited with code N`, `was ended by signal N`) and the last lines it wrote that were not protocol, standard error's included; none after `close`. A session that reports an id other than its own is killed and ends with `Event::Exited` saying so; nothing of the other session is told.

`prompt` writes one `user` message: text; an image as a base64 source; a resource link as `[@name](uri)`; a text resource as `<context ref="uri">…</context>`, the address's `&"<>` escaped and every `</context` of the text written `&lt;/context`, so it cannot close the block; an image resource as an image. Any other resource is `Error::Refused`. The first prompt's first line, cut to 80 bytes, is the `Event::Title`. `interrupt` writes an `interrupt` control request and does not wait; the turn's error result then ends it as `Stop::Cancelled`. `set_model` sends `set_model`, `set_effort` sends `apply_flag_settings` with `effortLevel`, each waiting up to `control_timeout`. Each line is one write.

`ClaudeSettings` (`new()`, `Default`; set its fields): `max_line` (64 MiB; a longer output line ends the session), `tail_lines` (8) and `tail_line_bytes` (512) quoted by an exit, `initialize_timeout` (30 s), `control_timeout` (30 s), `exit_wait` (1 s for the exit status), `max_tools` (4096 tool calls kept for their results), `max_transcript_bytes` (64 MiB replayed). Also: `parser` (`ParserLimits`: `max_title_bytes` 256, `max_streamed` 64, `max_tasks` 1024), `exit_poll` (100 ms; an exit is noticed even with the output held open), `close_wait` (2 s), `max_queued_writes` (1024; past it a write fails rather than waits), `title_bytes` (80), `version_wait` (10 s; 0 does not ask), `version_bytes` (4096), `on_line: Option<LineHook>` (each line before anything reads it; `false` ends Claude Code).

`models_of` reads `models[]` of an `initialize` answer: `value` as the id, `displayName`, `description` with `resolvedModel` when it differs, and `supportedEffortLevels` when `supportsEffort`. The answer's `account` is never read.

## Parser (`coqui_acp_claude::parse`)

```rust
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;
pub type ControlAnswer = (String, Result<Value, String>);
pub const ALLOW_ONCE: &str = "allow";  pub const ALLOW_ALWAYS: &str = "allow_always";  pub const REJECT: &str = "reject";
pub fn Parser::new(writer: Writer, max_tools: usize) -> Parser
pub fn Parser::with_limits(writer: Writer, max_tools: usize, limits: ParserLimits) -> Parser
pub fn line(&mut self, value: &Value) -> Vec<Event>
pub fn history(&mut self, value: &Value) -> Vec<Event>     // a transcript line: the person's messages told too
pub fn prompting(&mut self);  pub fn interrupting(&mut self);  pub fn end_of_output(&mut self) -> Option<TurnEnd>
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
| `result` | `Usage` (`total_cost_usd`, `contextWindow` of `modelUsage` as the size, the last request's tokens as context used) with the session's `Spend`: `modelUsage` summed, one `ModelSpend` row per model with its `costUSD`, `thinkingTokens` as `reasoning`, `total_cost_usd` as `cost_micro_usd` rounded to the nearest (unset when absent, never 0); the `usage` beside them is the turn's own and stands in only when `modelUsage` is absent, and then the cost stays unset (a partial pair), `Denied` with `permission_denials` named once each, then `TurnEnd` |
| `rate_limit_event` | `Buckets`, one per `unifiedWindows` entry (else the `rateLimitType`, else `unknown`), its id the window's name: `permille` of `utilization` taken only as a fraction, a fixed window ending at `resetsAt` whose length comes from the name (`five_hour` 5 h, `seven_day` and `seven_day_*` 7 d, any other none), coverage `only {opus}` for `seven_day_opus` and `only {sonnet}` for `seven_day_sonnet` and unknown for every other name, status `allowed`, `allowed_warning`, `rejected` as `ok`, `warning`, `exhausted`; a rejected one kept for the turn it came in |
| `error` | `TurnEnd`: `Limited` when its words or a rejected event say so, else `Failed`; a `result` after it is no second end |
| `control_request` `can_use_tool` | `Permission`; any other subtype is answered with an error at once |
| `control_response` | kept for `answers`, never an event |

`TurnEnd`: a usage limit (`rate_limit_event` `rejected`, or an error result naming `usage limit`, `limit reached`, `hit your limit`, or with `api_error_status` 429; a reset after a trailing `|`, in seconds or milliseconds; the bucket `five_hour` or `seven_day` from the text, source `inferred`; a hit a rejected event backs, in the same turn or alone, is `reported`, and that event's bucket and reset win over the text's; `retry_after` is never set, Claude Code gives none) is `Limited` with a `LimitHit`; an error result after `interrupting`, or with a `terminal_reason` starting `aborted`, is `Stopped(Cancelled)`; `error_max_turns` is `Stopped(MaxTurnRequests)`; another error is `Failed` with its `result` or `errors`; otherwise `Stopped` by `stop_reason` (`max_tokens`, `refusal`, else `EndTurn`).

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
pub const fn client_plugins(self, allowed: bool) -> Self   // default false; each path held to the working roots and network rule
pub fn cli_version(&self) -> Option<String>                 // claude --version, once, within ClaudeSettings::version_wait
impl Harness for ClaudeAgent
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub const DEFAULT: &str = "default";
```

`agentInfo` is `coqui-acp-claude`, titled `Claude Code <version>`, at this crate's version. It takes images, embedded context, MCP servers over stdio, HTTP and SSE (version 1), load, more directories and an appended system prompt. A session's parts are added to `defaults`: its `cwd`, a new UUID or the id resumed or loaded, `systemPrompt` appended after the defaults' own, `mcpServers`, `additionalDirectories` as `--add-dir`, and `_meta.claudeCode.options.plugins` (`[{"type": "local", "path": absolute}]`, as the Claude Agent SDK's `plugins` option) as `--plugin-dir`, whose skills are `<plugin>:<skill>`. A plugin of another type, a relative path, or any plugin when `client_plugins(false)`, is refused, as is an MCP server named as one of the defaults'. Load replays the transcript, then resumes.

The mode option (`mode`, category `mode`; ids from `coqui_acp_harness::mode`), measured on Claude Code 2.1.318 with `initialize` and `set_permission_mode`, no prompt sent:

| Mode | `permissionMode` | Offered |
| --- | --- | --- |
| `manual` | `default` (`manual` on the command line is `default`) | the launch's mode and every stricter one |
| `accept_edits` | `acceptEdits` | likewise; Claude Code takes the switch |
| `auto` | `auto` | likewise; whether the account may use it shows at the first turn |
| `plan` | `plan` | always |
| `bypass` | `bypassPermissions` | only with `allow_bypass`, whatever the launch's mode: Claude Code refuses the switch otherwise (`bypass_not_launched`), and a launch in it with `allow_bypass` off shows Bypass and cannot be taken back to it |

What a client may choose is capped by the launch: the launch's own `permission_mode` (default `default`, so Manual) and every stricter one, in the order Plan, Manual, Accept edits, Auto, Bypass; `ClaudeLaunch::modes` names more (a host's widening, still with no Bypass unless `allow_bypass`). A launch in a mode no mode names (`dontAsk`) offers Plan only, and shows it as `custom`. The current mode at the start is `current_permission_mode` of the `initialize` answer. A switch is a `set_permission_mode` control request and holds for the turn running and every turn after it. The mode shown is the one Claude Code is really in: the session tells it when Claude Code reports a `permissionMode` (`system` lines, `init` included) and when a person's "allow always" forwards a `setMode` suggestion (the last one wins; `setMode` is never stripped, since the person chose it), and the conversation sends a `config_option_update` for each change. `ClaudeSession::on_permission_mode` is that hook. A `setMode` an "allow always" would apply is left out of what the answer applies when it names a mode outside the offered list (`ClaudeSession::permit_modes`), so a person's "always" cannot loosen what a host capped; an option that would then grant nothing is not offered. The hook is told after the parser's lock is released, never under it. Claude Code's own list of modes is the `--permission-mode` choices (`acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan`) and the error text of an unknown mode (`acceptEdits, auto, bypassPermissions, default, dontAsk, plan`); its `initialize` answer lists none.

Config options: `model` (category `model`): the models Claude Code lists, and the current one; `effort` (category `thought_level`): `default` and the current model's efforts, else `EFFORTS`. Setting one not offered is refused; setting one sends `set_model` or `apply_flag_settings` for the turns that follow.

## Provider (`ClaudeProvider`, id `claude`)

```rust
#[derive(Provider)] pub struct ClaudeProvider;                       // an `acp_harness` provider; Debug
pub fn ClaudeProvider::new(settings: ProviderSettings) -> ClaudeProvider  // also From<ProviderSettings>
pub struct ProviderSettings {                                        // ToValue, FromValue, Schema, Default; every field defaults
    pub executable: String,              // "claude": a path, or a name looked up where the spawner runs it
    pub model: Option<String>, pub effort: Option<String>, pub permission_mode: Option<String>,
    pub allow_bypass: bool,              // false
    pub modes: Option<Vec<Mode>>,        // None; ids `manual`, `accept_edits`, `auto`, `plan`, `bypass`: widen the permission mode's own and every stricter one (Bypass still needs allow_bypass, and is left out without it)
    pub setting_sources: Option<Vec<SettingSource>>,   // None: Claude Code's default; `user`, `project`, `local`; an empty list loads none (`--setting-sources=`)
    pub max_acp_version: u64,            // 2 (1 or 2)
    pub max_sessions: u64,               // 8
    pub max_requests: u64,               // 32: requests worked on at once; a request past it is refused
    pub max_message_bytes: u64,          // 64 MiB: one byte more ends the connection GUATIAO_ERR_BAD_VALUE
    pub allow_client_plugins: bool,      // false
    pub client_mcp_servers: bool,        // true
}
#[non_exhaustive] pub enum SettingSource { User, Project, Local }   // ToValue, FromValue, Schema; written `user`, `project`, `local`; `as_str()` is the name `--setting-sources` takes
pub mod accounts { pub fn catalog() -> AccountCatalog;  pub const LOGIN, CONSOLE, OAUTH_TOKEN, API_KEY, AUTH_TOKEN: &str; }
```

Register it with `Registry::register_local`, or build it into a library with `guatiao::providers!` (`examples/claude_provider.rs` is that library, built by `cargo test`). Each connection serves a `ClaudeAgent` over the channel with `coqui_acp_harness::provider::Serving`; it starts `claude` through the host's spawner. `examples/fake_claude.rs` is a stand-in for Claude Code in its stream-json dialect that behaves as the contract agent does (it needs `CLAUDE_CONFIG_DIR` or the suite's home, keeps sessions as Claude Code does, and with `COQUI_ACP_FAKE_CLAUDE_LOG` logs its arguments, environment names and the two cache variables); the suite runs against it as `acp-harness-check --library <claude_provider> --provider claude --settings '{"executable": "<fake_claude>"}' --program-agent --no-imports --no-logins --holds no_project_mcp --account harness-login`; `--program-agent` runs the checker in a home of its own, which the stand-in uses when `CLAUDE_CONFIG_DIR` is not set.

- `describe`: title `Claude Code`, ACP versions 1 and 2 (to `max_acp_version`), what `ClaudeAgent` takes and `session/resume`, `resumes_wider`, usage `reported` and `inferred` with spend and `needs_turn`, and the cache hint: `cache_lifetime` short sets `CLAUDE_CODE_PROMPT_CACHE_TTL` and `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL` to `5m`, long to `1h`, default leaves both as the environment has them.
- `refusal`, `start`: holds `no_project_mcp` as `--strict-mcp-config` (which starts only the servers `--mcp-config` names, so none of the user's own either; whether it also keeps out the MCP servers of plugins that a project's `.claude/settings.json` enables is unmeasured); every other requirement is `GUATIAO_ERR_UNSUPPORTED` with its name (the flags do not leave instruction files loaded while they drop a directory's settings, and what the rest take away is not measured). The environment is the host's, over nothing; the session ids are Claude Code's UUIDs.
- `accounts`: `harness-login` and the kinds measured on Claude Code 2.1.311 (`claude-login`, `claude-console` with a login file and a directory, rotating; `claude-oauth-token`, `anthropic-api-key` in a variable; `anthropic-auth-token` through a gateway), with every credential variable. `renews` is false for each and no login method is listed. A call with an account of `harness-login` runs as it would with none; a listed kind is `GUATIAO_ERR_UNSUPPORTED` ("does not apply an account of kind ... yet"), an unlisted one or a bad id `GUATIAO_ERR_BAD_VALUE`.
- `models`: starts `claude` through the spawner, reads its `initialize` answer and ends it (no turn); unknown, with the reason, when it could not be asked.
- `usage`: what the connections last saw for the account (`Reports`), as Claude Code learns usage only in a turn.
- `import_session`, `export_session`, `login`, `renew`: `GUATIAO_ERR_UNSUPPORTED` with a reason (`renew` after checking the account). `session_record`, `non_defaults`: the kind's defaults.

## Errors

`Error` (`#[non_exhaustive]`): `Spawn(io::Error)`, `Refused(String)`, `Timeout { what, limit }`, `Harness(String)` in Claude Code's words or saying how it ended. Into `HarnessError`: `Refused` stays refused, the rest is `Failed`.

## Command line

`coqui-acp-claude` serves `ClaudeAgent` on standard input and output. Flags: `--executable` (`COQUI_ACP_CLAUDE_EXECUTABLE`, default `claude`), `--model`, `--effort`, `--permission-mode`, `--allow-bypass`, `--setting-sources` (comma-separated, empty for none), `--strict-mcp-config`, `--max-acp-version` (1 or 2, default 2), `--max-sessions` (8), `--no-permission-prompts`, `--allow-client-plugins`, `--working-root` (repeated; a session's `cwd` and more directories must lie in one), `--no-client-mcp-servers`, `--allow-network-paths`. Notices and exits go to standard error. Exit status 0 when the client closes, 1 when reading it failed, 2 for a wrong invocation.

## Environment variables

`COQUI_ACP_CLAUDE_EXECUTABLE`, as `--executable`. Claude Code reads its own (`CLAUDE_CONFIG_DIR`, `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` and others) from the environment the agent runs in, or `ClaudeLaunch::env`.

## Gotchas

- `--permission-prompts host` alone does not route prompts to standard output on Claude Code 2.1.318: the call is refused as not granted. `--permission-prompt-tool=stdio` does, and the launch passes both unless `permission_prompts` is off.
- `dontAsk` decides before those flags, on Windows and on Linux: a call nothing granted is refused where it is made, no question comes, and one allowed runs (recording `dont_ask`). With no prompt flags and no mode, the same refusal (recording `no_prompts`).
- Claude Code learns usage only during a turn: `describe` says `usage.needs_turn` and a read with no turn answers what the connection last saw. It writes `rate_limit_event` only for a subscription login. Its `result` `usage` is the turn's own while `modelUsage` and `total_cost_usd` are running totals (measured: 11 input in `usage` beside 88 in `modelUsage` after two turns), so `Spend` takes the latter; `Usage::cumulative` still marks the former.
- A `rate_limit_event` is the only place a limit's reset is a number; the `result` after it says a local clock time. A rejected one becomes the limit of a turn that then fails or whose output ends; one whose turn is answered anyway is no failure (recording `rate_limit`). Claude Code writes these only with a subscription's login.
- No recording holds a `type: error` line: nothing a stand-in did drew one from 2.1.318. It is handled as a failed turn, its words read from `result`, `error.message`, the first block of `message`, `message` or `error`.
- Claude Code's output does not outlive it: on Windows and on Linux, with a background `Bash` still running, killing Claude Code alone ended its output at once, and closing standard input made it wait for that `Bash`, then exit and end its output together. What can hold the output open past an exit is a `Spawn` or a wrapper; the watcher is for that.
- `close` ends what Claude Code started on Windows, its job object taking a background `Bash` with it. On Linux that `Bash` runs in a session of its own, outside Claude Code's process group, and outlives `close` until it ends by itself.
- A prompt's blocks keep the order the client gave them: a host that wants images first, as the model's guidance has it, puts them first.
- `partial_messages` is on since ACP streams; a host that wants whole messages turns it off.
- `--resume` finds a session only from the directory that recorded it.
- With stream-json input Claude Code writes nothing, not even `init`, until it reads a line; `initialize` is answered at once.
- Claude Code 2.1.318 in print mode offers neither `TodoWrite` nor the Task tools (`No such tool available`, `CLAUDE_CODE_ENABLE_TASKS` either way); a plan the model writes with them is still told.
- A permission answered `reject` lets the turn go on; only `Outcome::Cancelled` interrupts it.
- Everything Claude Code writes is the model's or the harness's: untrusted.
- A plugin's hooks and MCP servers run with no permission asked, whatever `--strict-mcp-config` and `--setting-sources` say: they are off until a host says `client_plugins(true)`, or the binary is given `--allow-client-plugins`.
- A diagnostic line on standard error longer than `max_line` is noted and the rest of standard error read and dropped, so Claude Code never writes to a closed pipe.
- An appended system prompt goes on the command line: past the platform's limit (128 KiB for one argument on Linux, 32 767 characters for a command line on Windows) Claude Code does not start.
