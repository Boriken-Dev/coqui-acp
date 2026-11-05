# `coqui-acp-codex` — API header

Header-file-style reference for the `coqui-acp-codex` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `coqui-acp-harness`, `serde_json` and, for the command, `clap`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0. Tested with Codex 0.164.0 (`TESTED_VERSIONS`).

Imports: everything is at the crate root, and `parse` is public. `coqui_acp_codex::harness` is `coqui-acp-harness`, for its events, processes and ACP server; `Launch`, `PlainSpawn`, `Spawn` and `Spawned` are re-exported from it.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Launch (`CodexLaunch`)

```rust
#[non_exhaustive]                                       // Clone, PartialEq; Debug shows env names only
pub struct CodexLaunch {
    pub executable: OsString,                  // "codex"
    pub cwd: PathBuf,
    pub thread: ThreadStart,                   // New | Resume(id) | Load(id)
    pub model: Option<String>,
    pub effort: Option<String>,
    pub developer_instructions: Option<String>,
    pub mcp_servers: Vec<McpServer>,
    pub additional_directories: Vec<PathBuf>,
    pub working_roots: Vec<PathBuf>,           // a resumed thread's own directory must lie in one; empty takes any
    pub approval_policy: Option<String>,       // untrusted, on-failure, on-request, never
    pub sandbox: Option<String>,               // read-only, workspace-write, danger-full-access
    pub thread_config: Map<String, Value>,     // merged into the thread's config, as given
    pub config_overrides: Vec<(String, Value)>,
    pub env: BTreeMap<OsString, OsString>,
    pub clear_env: bool,
}
pub fn CodexLaunch::new(cwd: impl Into<PathBuf>) -> CodexLaunch
pub fn launch(&self) -> Launch                 // app-server and one --config=key=value per override
pub fn config(&self) -> Map<String, Value>     // mcp_servers, sandbox_workspace_write.writable_roots, thread_config
pub fn skill_roots(&self) -> Vec<String>       // the Agent Skills directory of each
pub fn has_server(&self, name: &str) -> bool   // in mcp_servers or thread_config
pub fn thread_params(&self) -> Value           // thread/start's, or thread/resume's with threadId and excludeTurns
#[non_exhaustive] pub enum ThreadStart { New, Resume(String), Load(String) }
pub fn valid_config_key(key: &str) -> bool;  pub fn toml_value(value: &Value) -> String
pub fn valid_thread_id(id: &str) -> bool       // letters, digits and dashes, 1 to 128
```

The command line holds only `app-server` and the overrides, each one argument `--config=<key>=<value>`: a key part that is not letters, digits, `_` and `-` is a quoted TOML string, and every value is TOML written here (strings with quotes, backslashes and control characters escaped, arrays, objects as inline tables, `null` as `""`), so no value can end its own or start another. MCP servers are `config.mcp_servers.<name>`: `{command, args, env}` for stdio, `{url, http_headers}` for HTTP; Codex has no SSE.

## Session (`CodexSession`)

```rust
pub fn CodexSession::start(launch: &CodexLaunch, spawn: &dyn Spawn, settings: CodexSettings,
    sink: impl Fn(Event) -> bool + Send + Sync + 'static) -> Result<CodexSession, Error>
pub fn thread(&self) -> &str
pub fn version(&self) -> Option<&str>
pub fn models(&self) -> &[Model]
pub fn choice(&self) -> (Option<String>, Option<String>)        // model and effort the next turn names
pub fn set_model(&self, model: Option<&str>);  pub fn set_effort(&self, effort: Option<&str>)
pub fn prompt(&self, content: &[Content]) -> Result<(), Error>
pub fn interrupt(&self) -> Result<(), Error>
pub fn close(&self)                                              // also on drop
#[non_exhaustive] pub struct Model { pub id: String, pub name: String, pub description: Option<String>,
    pub efforts: Vec<String>, pub default_effort: Option<String>, pub is_default: bool }
pub fn models_of(result: &Value) -> Vec<Model>                  // hidden models left out
pub fn version_of(answer: &Value) -> Option<String>             // from initialize's userAgent
pub fn user_input(content: &[Content]) -> Result<Vec<Value>, Error>
```

`start` spawns the app server, sends `initialize` (as `coqui-acp-codex`) and `initialized`, lists `model/list` page by page, sends `skills/extraRoots/set` when there are directories, and `thread/start` or `thread/resume` with `thread_params`. A resumed thread other than the one asked for is `Error::Harness`, and the app server is ended. For `Load` the earlier turns of `thread/resume`'s answer go to `sink` as history, the person's messages included, the latest `max_history_turns` of them. A version not in `TESTED_VERSIONS` is an `Event::Notice`. Events reach `sink` from a thread of the session's own until it answers `false`; the output's end is one `Event::Exited`, saying how Codex ended and the last lines it wrote that were not protocol, standard error's included; none after `close`.

`prompt` sends `turn/start` with the input (text; an image as a data URL; a link as `[@name](uri)`; a text resource in `<context ref="uri">…</context>`; an image resource as an image; any other resource is `Error::Refused`) and the chosen model and effort, and returns once Codex has answered. The first prompt's first line, cut to 80 bytes, is the `Event::Title`. `interrupt` sends `turn/interrupt` with the running turn's id and does not wait; with no turn running it does nothing.

`CodexSettings` (`new()`, `Default`; set its fields): `max_line` (64 MiB), `tail_lines` (8), `tail_line_bytes` (512), `initialize_timeout` (20 s), `request_timeout` (30 s), `exit_wait` (1 s), `max_tools` (4096 items kept), `max_model_pages` (10), `max_history_turns` (1000).

## Parser (`coqui_acp_codex::parse`)

```rust
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;
pub type Answer = (String, Result<Value, String>);
pub const ALLOW_ONCE: &str = "accept";  pub const ALLOW_SESSION: &str = "accept_for_session";
pub const ALLOW_RULE: &str = "accept_with_rule";  pub const ALLOW_ALWAYS: &str = "accept_always";  pub const REJECT: &str = "decline";
pub const ALLOW_NETWORK_RULE: &str = "allow_network_rule";  pub const REJECT_NETWORK_RULE: &str = "reject_network_rule";
pub fn Parser::new(writer: Writer, max_tools: usize) -> Parser
pub fn line(&mut self, message: &Value) -> Vec<Event>
pub fn history(&mut self, turn: &Value) -> Vec<Event>
pub fn turn(&self) -> Option<&str>;  pub fn started(&mut self, turn: &str);  pub fn interrupting(&mut self)
pub fn set_thread(&mut self, thread: &str);  pub fn prompting(&mut self);  pub fn abandon(&mut self)
pub fn answers(&mut self) -> Vec<Answer>
pub fn id_text(id: &Value) -> String
pub fn git_patch(path: &str, change: FileChange, hunks: &str) -> String
```

| Message | Events |
| --- | --- |
| `item/agentMessage/delta`, `item/completed` `agentMessage` | `MessageChunk` by item id, `MessageEnd`; text not streamed is told whole |
| `item/reasoning/summaryTextDelta`, `textDelta`, `item/completed` `reasoning` | `ThoughtChunk`, `ThoughtEnd` |
| `commandExecution` items, `item/commandExecution/outputDelta` | `Tool` (`exec_command`, execute, its terminal) and `Terminal`: the command, output, exit code; `declined` fails |
| `fileChange` items | `Tool` (`apply_patch`, edit) with a diff per change, its `patch` a git patch of Codex's hunks |
| `mcpToolCall`, `dynamicToolCall` | `Tool` named `mcp__<server>__<tool>`, its result or error text as a `ToolChunk` |
| `webSearch`, `imageView` | `Tool` |
| `turn/plan/updated` | `Plan` |
| `thread/tokenUsage/updated` | `Usage` of `last`: cached input split out, not cumulative, no cost, context used and `modelContextWindow` |
| `account/rateLimits/updated` | `Quota` per window (`five_hour`, `weekly`, else its position; `usedPercent` as thousandths), and the exhausted window kept |
| `turn/completed`, `turn/failed` | `TurnEnd`: a limit (`codexErrorInfo` naming a usage or rate limit, or a failed connection with HTTP 429; reset from `resetsAt` else the exhausted window) is `Limited`; `interrupted`, or `failed` after `interrupting`, is `Stopped(Cancelled)`; `failed` is `Failed` with the error's message; else `Stopped(EndTurn)` |
| `error` | kept for the turn's end unless Codex will retry |
| `thread/compacted`, `contextCompaction` items | `Compaction` |
| `warning`, `configWarning`, `deprecationNotice`, `guardianWarning`, an MCP server that failed to start | `Notice` |
| `thread/name/updated` | `Title` |
| an answer to our request | kept for `answers`, never an event |

Approvals are `Event::Permission`; their responders answer the server request by its own id:

| Request | Options | Answer |
| --- | --- | --- |
| `item/commandExecution/requestApproval` | `accept`, `accept_for_session`, `accept_with_rule` when Codex proposed a rule, `decline` | `accept`, `acceptForSession`, `{acceptWithExecpolicyAmendment}`, `decline`; nobody answered: `cancel` |
| `item/fileChange/requestApproval` | `accept`, `accept_for_session`, `decline` | the same words |
| `item/permissions/requestApproval` | `accept` (this turn), `accept_for_session`, `decline` | the permissions asked with `scope`, or none |
| `mcpServer/elicitation/request` of an MCP tool | `accept`, `accept_for_session` and `accept_always` as Codex's `persist` allows, `decline` | `{action: "accept", _meta: {persist}}`, `decline`; nobody answered: `cancel` |
| `execCommandApproval`, `applyPatchApproval` | `accept`, `accept_for_session`, `decline` | `approved`, `approved_for_session`, `{denied}`; nobody answered: `abort` |

Any other server request is answered `-32601` at once.

## ACP agent (`CodexAgent`)

```rust
pub fn CodexAgent::new() -> CodexAgent                    // also Default; codex from PATH, PlainSpawn
pub fn defaults(self, defaults: CodexLaunch) -> Self
pub fn spawn(self, spawn: Arc<dyn Spawn>) -> Self
pub fn settings(self, settings: CodexSettings) -> Self
pub fn cli_version(&self) -> Option<String>               // codex --version, once, 10 s
impl Harness for CodexAgent
pub const DEFAULT: &str = "default";
```

`agentInfo` is `coqui-acp-codex`, titled `Codex <version>`, at this crate's version. It takes images, embedded context, MCP servers over stdio and HTTP, load, more directories and an appended system prompt. A session's parts are added to `defaults`: its `cwd`, `systemPrompt` after the defaults' developer instructions, `mcpServers`, and `additionalDirectories`, each a skill root (the Agent Skills directory inside it) and writable in a `workspace-write` sandbox. An SSE server or a thread id `valid_thread_id` refuses is refused. The session's id is the thread's.

Config options: `model` (category `model`): `default` and the models Codex lists; `effort` (category `thought_level`): `default` and the current model's efforts. Setting one not offered is refused; a choice applies from the next turn.

## Errors

`Error` (`#[non_exhaustive]`): `Spawn(io::Error)`, `Refused(String)`, `Timeout { what, limit }`, `Harness(String)` in Codex's words or saying how it ended. Into `HarnessError`: `Refused` stays refused, the rest is `Failed`.

## Command line

`coqui-acp-codex` serves `CodexAgent` on standard input and output. Flags: `--executable` (`COQUI_ACP_CODEX_EXECUTABLE`, default `codex`), `--model`, `--effort`, `--approval-policy`, `--sandbox`, `--max-acp-version` (1 or 2, default 2), `--max-sessions` (8), `--working-root` (repeated; a session's `cwd` and more directories must lie in one), `--no-client-mcp-servers`, `--allow-network-paths`. Notices and exits go to standard error. Exit status 0 when the client closes, 1 when reading it failed, 2 for a wrong invocation.

## Environment variables

`COQUI_ACP_CODEX_EXECUTABLE`, as `--executable`. Codex reads its own (`CODEX_HOME`, `OPENAI_API_KEY` and others) from the environment the agent runs in, or `CodexLaunch::env`.

## Gotchas

- `turn/interrupt` needs the turn's id, which `turn/start`'s answer and `turn/started` give.
- `availableDecisions` is what a command's approval offers; `decline` is always offered, since 0.164.0 takes it where it lists only `cancel` and goes on with the turn.
- `--config` keys are not TOML: Codex splits the argument at its first `=` and the key at each `.`, and a quote is part of a name. A key holding `=` cannot be written and is refused. A JSON `null` value is the empty string, which is not unset.
- A login file needs `tokens.refresh_token` to be read, or Codex answers every turn `401`. Empty, a second client cannot rotate the person's login, and the access token goes on working until it expires.
- With `additionalDirectories` from a client, `workspace-write` writes where the client says, `/` included, with no approval. A host that does not trust its client gives `--working-root`; `--no-client-mcp-servers` keeps a client from starting programs.
- Whether Codex sends another thread's messages on this connection is not recorded. A notification of another thread is dropped, and a question of another thread says so.
- A question Codex settles itself (`serverRequest/resolved`) stays open at the client until the turn ends; a late answer is harmless.
- An MCP tool call asks through `mcpServer/elicitation/request` with `_meta.codex_approval_kind` `mcp_tool_call`, not a tool approval.
- Codex merges a thread's `config.mcp_servers` with the person's own configuration; a server of the same name in it wins.
- A fresh `CODEX_HOME` makes Codex set the machine up again; give it the login, or use the person's own.
- Everything the app server writes is the model's or Codex's: untrusted.
