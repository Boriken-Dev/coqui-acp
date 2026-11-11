# `coqui-acp-pi` — API header

Header-file-style reference for the `coqui-acp-pi` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `coqui-acp-api`, `coqui-acp-harness` (with its `kind` feature), `serde_json`, `uuid` and, for the command, `clap`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0. Tested with Pi 1.5.2 (`TESTED_VERSIONS`).

Imports: everything is at the crate root; `parse`, `mode` and `accounts` are public. `coqui_acp_pi::harness` is `coqui-acp-harness`, for its events, processes and ACP server; `Launch`, `PlainSpawn`, `Spawn` and `Spawned` are re-exported from it.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

Pi's side is `pi --mode rpc`: JSON lines on standard input and output, commands answered by a `response` and a run reported by events, ending at `agent_settled`. Pi writes nothing before the first command, has no permission system, no command that lists sessions, takes no MCP servers from a client and serves one working directory per process (the process's own).

## Launch (`PiLaunch`)

```rust
#[non_exhaustive]                                       // Clone, PartialEq; Debug shows env names only
pub struct PiLaunch {
    pub executable: OsString,                  // "pi"; on Windows npm's shim is "pi.cmd"
    pub cwd: PathBuf,                          // the process's working directory
    pub session: SessionStart,                 // New | Resume(id) | Load(id)
    pub model: Option<String>,                 // "provider/id" (--model)
    pub effort: Option<String>,                // a thinking level (--thinking)
    pub system_prompt: Option<String>,         // appended to Pi's: written to a file, and --append-system-prompt names the file
    pub additional_directories: Vec<PathBuf>,  // where a write's old content may be read from, besides cwd
    pub working_roots: Vec<PathBuf>,           // replaces cwd and more directories as those; empty takes them
    pub mode: Option<Mode>,                    // the start mode and the loosest a client may choose; Manual when None
    pub allow_bypass: bool,                    // false: the Bypass mode is not offered
    pub modes: Option<Vec<Mode>>,              // those named widen the start mode and stricter ones (Bypass still needs allow_bypass; Auto is never offered)
    pub ask_for_reads: bool,                   // Manual asks before read, grep, find and ls too
    pub gate_dir: Option<PathBuf>,             // where the gate's private directory is made; the system's temporary directory
    pub extra_args: Vec<OsString>,             // the caller's own, last, unchecked
    pub env: BTreeMap<OsString, OsString>,
    pub remove_env: Vec<OsString>,
    pub clear_env: bool,
}
pub fn PiLaunch::new(cwd: impl Into<PathBuf>) -> PiLaunch
pub fn start_mode(&self) -> Mode                // mode, else Manual
pub fn launch(&self, extension: Option<&Path>, prompt: Option<&Path>) -> Launch   // pi --mode rpc [-e ext] [--session id] [--model m] [--thinking l] [--append-system-prompt file] extra
#[non_exhaustive] pub enum SessionStart { New, Resume(String), Load(String) }
pub fn valid_session_id(id: &str) -> bool       // letters, digits and dashes, 1 to 128
```

None of the person's text is an argument: the system prompt is a file, a prompt goes through `prompt`. A model or thinking level that starts with `-` or holds a control character is `Error::Refused`.

## Gate (`Gate`, `GATE_SOURCE`)

```rust
pub const GATE_SOURCE: &str                      // the extension, TypeScript, which Pi loads itself
pub fn Gate::write(parent: Option<&Path>, system_prompt: Option<&str>) -> io::Result<Gate>   // a new directory coqui-acp-pi-<uuid> under parent; owner-only on Unix
pub fn extension(&self) -> &Path;  pub fn prompt(&self) -> Option<&Path>;  pub fn dir(&self) -> &Path
// Drop removes the directory.
```

Pi asks nobody before it runs a tool. The gate is an extension on `tool_call` that puts every call to the program that started Pi as a `select` dialog, titled `coqui-acp-pi-gate {"id":<toolCallId>,"tool":<name>}`, with the options `allow`, `deny` and `plan`. The answer decides: `allow` runs the call; `plan` refuses it with the reason `Plan mode: nothing may be changed or run. Say what you would do instead.`; `deny`, a cancelled dialog, any other answer, or a dialog that fails refuses it with `Blocked by user`. Pi shows the model that reason as the tool's result and goes on. The gate is written to a private directory for one launch and removed with the session; nothing is written to the person's Pi configuration. The file is on the machine of the translator, so a spawner that runs Pi elsewhere is refused by the provider (`describe().local`).

## Session (`PiSession`)

```rust
pub fn PiSession::start(launch: &PiLaunch, spawn: &dyn Spawn, settings: PiSettings,
    sink: impl Fn(Event) -> bool + Send + Sync + 'static) -> Result<PiSession, Error>
pub fn PiSession::read_models(launch: &PiLaunch, spawn: &dyn Spawn, settings: PiSettings) -> Result<Vec<Model>, Error>   // starts pi --mode rpc --no-session with no gate, lists, ends it: no session, no turn
pub fn session(&self) -> &str
pub fn models(&self) -> Vec<Model>
pub fn choice(&self) -> (Option<String>, Option<String>, Vec<String>)   // model "provider/id", thinking level, the levels the model takes
pub fn mode(&self) -> Mode;  pub fn set_mode(&self, mode: Mode)
pub fn set_model(&self, id: &str) -> Result<(), Error>                     // one of models(); the level may change with it
pub fn set_effort(&self, level: &str) -> Result<(), Error>                 // one of the levels; Pi would clamp any other silently
pub fn prompt(&self, content: &[Content]) -> Result<(), Error>
pub fn interrupt(&self) -> Result<(), Error>
pub fn request(&self, command: &str, fields: Value, limit: Duration) -> Result<Value, Error>   // any RPC command, its response's data
pub fn close(&self)                                                        // also on drop
#[non_exhaustive] pub struct Model { pub id: String /* provider/id */, pub provider: String, pub model: String, pub name: String,
    pub reasoning: bool, pub images: bool, pub context_window: Option<u64> }
pub fn models_of(data: &Value) -> Vec<Model>        // get_available_models' data; Pi's placeholder provider "unknown" left out
pub fn user_input(content: &[Content]) -> Result<(String, Vec<Value>), Error>
```

`start` spawns `pi --mode rpc` in the launch's `cwd` with the gate, sends `get_state` (its answer waits for Pi to start), checks that the session Pi opened is the one asked for (Pi matches a partial id), lists `get_available_models` and `get_available_thinking_levels`, sends `get_commands` as `Event::Commands`, and for `Load` replays `get_messages` (the latest `max_history_messages`) as history, the person's messages included. A version not in `TESTED_VERSIONS` is an `Event::Notice`. A session file exists only after the first message, so a session no message was sent in cannot be resumed. Events reach `sink` from a thread of the session's own until it answers `false`; the output's end is one `Event::Exited`, saying how Pi ended and the last lines it wrote that were not protocol, standard error's included; none after `close`.

`prompt` sends `prompt` with the text (blocks joined by a blank line; a link as `[@name](uri)`; a text resource in `<context ref="uri">…</context>`) and the images (`{type: "image", data, mimeType}`; an image resource as an image; any other resource is `Error::Refused`), and returns once Pi has taken it (`disposition` `started`). A prompt Pi refuses (no credential: `No API key found for the selected model`) is `Error::Harness` in Pi's words. A prompt of `/name` runs a command of an extension, a template or a skill; Pi answers `handled` and the turn ends at once with `end_turn`. The first prompt's first line, cut to 80 bytes, is the `Event::Title`. A model that takes no image is given the prompt with its images replaced by Pi's own placeholder.

`interrupt` answers every dialog the person was shown `cancelled` first (Pi's `abort` alone hangs behind an open dialog), then sends `abort_retry` when Pi is waiting to retry and `abort`; the run ends as `cancelled`. With no run it does nothing. A question answered `Cancelled` by the client with no `session/cancel` before it ends the run the same way, as the contract has every harness do.

`PiSettings` (`new()`, `Default`; set its fields): `max_line` (64 MiB), `tail_lines` (8), `tail_line_bytes` (512), `initialize_timeout` (30 s: the first command), `request_timeout` (30 s), `prompt_timeout` (5 min: a command of an extension answers `prompt` when it is done), `exit_wait` (1 s), `max_tools` (4096), `max_history_messages` (5000), `parser` (`ParserLimits`: `max_title_bytes` 256, `max_told_bytes` 16 KiB, `max_old_bytes` 1 MiB), `list_models` (true), `exit_poll` (100 ms), `close_wait` (2 s), `max_queued_writes` (1024), `title_bytes` (80), `version_wait` (10 s; 0 does not ask), `version_bytes` (4096), `known_version: Option<String>` (what `pi --version` said; the agent sets it), `on_line: Option<LineHook>`.

## Parser (`coqui_acp_pi::parse`)

```rust
pub type Writer = Arc<dyn Fn(&Value) -> io::Result<()> + Send + Sync>;
pub type Answer = (String, Result<Value, String>);     // a response's id, its data or Pi's refusal
pub const ALLOW: &str = "allow";  pub const ALLOW_TOOL: &str = "allow_tool";  pub const REJECT: &str = "reject";
pub fn Parser::new(writer: Writer, max_tools: usize) -> Parser            // Manual mode
pub fn Parser::with_limits(writer: Writer, max_tools: usize, limits: ParserLimits) -> Parser
pub fn with_policy(self, policy: Arc<Mutex<Policy>>) -> Self              // the mode and what the person allowed, shared with whoever sets the mode
pub fn with_roots(self, cwd: &Path, roots: Vec<PathBuf>) -> Self          // relative paths are against cwd; a write's old content is read only inside roots (cwd when empty)
pub fn set_context_window(&mut self, size: Option<u64>)
pub fn line(&mut self, message: &Value) -> Vec<Event>
pub fn history(&mut self, messages: &[Value]) -> Vec<Event>               // get_messages' list
pub fn prompting(&mut self);  pub fn abandon(&mut self);  pub fn handled(&mut self) -> Vec<Event>
pub fn interrupting(&mut self) -> bool;  pub fn running(&self) -> bool;  pub fn retrying(&self) -> bool
pub fn answers(&mut self) -> Vec<Answer>
pub fn text_of(content: &Value) -> String;  pub fn git_patch(path: &str, hunks: &str) -> String
```

| Line | Events |
| --- | --- |
| `message_update` `text_*` | `MessageChunk` by block (`m<n>.<index>`), `MessageEnd`; text not streamed is told whole |
| `message_update` `thinking_*` | `ThoughtChunk`, `ThoughtEnd` (Pi ends a thought after the answer's first deltas; the two are separate messages) |
| `toolcall_start`, `toolcall_end` | `Tool` pending: its kind, title and locations once the arguments are known |
| `tool_execution_start` | `Tool` in progress with its content: an `edit`'s replacements as diffs, a `write`'s file as a diff (an add, or a change with the old content read from disk first), a `bash` as a terminal |
| `tool_execution_update` | a command's output added to its `Terminal` (Pi's text is cumulative; only what is new is sent); other tools' updates are left for the end |
| `tool_execution_end` | `Tool` completed or failed (cancelled when the person stopped the run): an `edit` keeps its diffs and gains a git patch of Pi's hunks and the line it changed; a `bash` ends its terminal with the exit code (`structuredContent.exit_code`, none for a killed command); other tools show their text, cut to `max_told_bytes`; an image in a result is counted, not shown |
| `message_end` of the assistant | `Usage` (input, output, cache read and write, the session's cost so far from Pi's prices, context used and the model's window) when it counted tokens |
| `extension_ui_request` of the gate | by the mode: answered `allow` or `plan`, or `Permission` |
| `extension_ui_request` of another extension | `select`, `confirm`, `input` and `editor` are answered cancelled at once with a `Notice`; `notify` is a `Notice`; the rest is ignored |
| `agent_settled` | `TurnEnd`: `aborted` or a stop asked for is `Stopped(Cancelled)`; the last assistant message's `stopReason` `error` is `Limited` when its words say a usage limit (`429`, "rate limit", "quota", …) and `Failed` in Pi's words otherwise; `length` is `Stopped(MaxTokens)`; else `Stopped(EndTurn)`. A call that never ran ends `cancelled` |
| `auto_retry_start`, `auto_retry_end` | a `Notice`; a final failure is the turn's |
| `compaction_end` | `Compaction` with its summary; a failed one a `Notice` |
| `session_info_changed` | `Title` |
| `extension_error` | `Notice` |
| a `response` | kept for `answers`, never an event |

A tool call's id is Pi's, made unique in the session: a model that repeats one gets `<id>~2`. `history` turns user messages into `UserChunk`/`UserEnd`, assistant messages and tool results into the events above, a command the person ran with Pi's `bash` into a finished command, and a compaction or branch summary into `Compaction`; Pi's system message is left out.

## ACP agent (`PiAgent`)

```rust
pub fn PiAgent::new() -> PiAgent                    // also Default; pi from PATH, PlainSpawn
pub fn defaults(self, defaults: PiLaunch) -> Self
pub fn spawn(self, spawn: Arc<dyn Spawn>) -> Self
pub fn settings(self, settings: PiSettings) -> Self
pub fn cli_version(&self) -> Option<String>         // pi --version, once, within PiSettings::version_wait
impl Harness for PiAgent
pub const DEFAULT: &str = "default";
```

`agentInfo` is `coqui-acp-pi`, titled `Pi <version>`, at this crate's version. It takes images, embedded context, load, more directories and an appended system prompt; it takes no MCP server (Pi reads its own from its configuration, so a session that brings one is refused `-32602`) and reports no usage allowances (`usage()` is `none`; a turn's tokens, cost and context still arrive as usage updates). A session's parts are added to `defaults`: its `cwd`, `systemPrompt` after the defaults' own, `additionalDirectories`. A session id `valid_session_id` refuses is refused. The session's id is Pi's, a UUID.

Config options: `model` (category `model`): the models Pi has credentials for, by `provider/id`, with Pi's name, the provider, "reasoning" and the context window; `effort` (category `thought_level`, shown as Thinking): the levels the current model takes (`off` alone for a model that does not reason), re-read after a model change; `mode`. Setting one not offered is refused; a choice applies at once (the model and level by Pi's own commands, which Pi may refuse mid-run).

The mode option (`mode`, category `mode`; ids from `coqui_acp_harness::mode`; `coqui_acp_pi::mode` holds the table). The mode decides what the gate does with each call; Pi has no sandbox, so every mode runs what it lets through with the person's rights.

| Mode | `read`, `grep`, `find`, `ls` | `edit`, `write` | `bash` and any other tool (an extension's, an MCP server's) |
| --- | --- | --- | --- |
| `manual` | runs (asked with `ask_for_reads`) | asked | asked |
| `accept_edits` | runs | runs | asked |
| `plan` | runs | refused as a plan | refused as a plan |
| `bypass` | runs | runs | runs |

`auto` is never offered: Pi has no automatic reviewer to back it. What a client may choose is the start mode (`PiLaunch::mode`, Manual by default) and every stricter one in the order Plan, Manual, Accept edits, Bypass, and what `PiLaunch::modes` names besides; Bypass only with `allow_bypass`. A question offers `allow` (once), `allow_tool` ("Always allow <tool> in this session": every mode but Plan allows that tool from then on, for the rest of the session) and `reject`; a cancelled answer ends the run. Plan refuses without asking and tells the model why.

## Provider (`PiProvider`, id `pi`)

```rust
#[derive(Provider)] pub struct PiProvider;                           // an `acp_harness` provider; Debug
pub fn PiProvider::new(settings: ProviderSettings) -> PiProvider     // also From<ProviderSettings>
pub struct ProviderSettings {                                        // ToValue, FromValue, Schema, Default; every field defaults
    pub executable: String,              // "pi": a path, or a name looked up where the spawner runs it
    pub model: Option<String>, pub effort: Option<String>,
    pub mode: Option<Mode>,              // None: manual
    pub allow_bypass: bool,              // false
    pub modes: Option<Vec<Mode>>,        // None; ids `manual`, `accept_edits`, `plan`, `bypass`
    pub ask_for_reads: bool,             // false
    pub max_acp_version: u64,            // 2 (1 or 2)
    pub max_sessions: u64,               // 8
    pub max_requests: u64,               // 32: requests worked on at once; a request past it is refused
    pub max_message_bytes: u64,          // 64 MiB: one byte more ends the connection GUATIAO_ERR_BAD_VALUE
    pub renew_poll_ms: u64,              // 30000: how often a connection on a login kept in a file looks at the file; 0 looks only at a call and at close
}
pub mod accounts { pub static DEF: HarnessDef;  pub fn catalog() -> AccountCatalog;  pub const API_KEY, OAUTH: &str; }
```

Register it with `Registry::register_local`, or build it into a library with `guatiao::providers!` (`examples/pi_provider.rs`, built by `cargo test`). Each connection serves a `PiAgent` over the channel with `coqui_acp_harness::provider::Serving`; it starts `pi --mode rpc` through the host's spawner. `examples/fake_pi.rs` is a stand-in for Pi that behaves as the contract agent does (it needs `PI_CODING_AGENT_DIR` or the suite's home, keeps sessions there, and logs its arguments, the names (never the values) of its environment and the gate it was given to `COQUI_ACP_FAKE_PI_LOG` and to `fake-pi.log` of the home); the suite runs against it as `acp-harness-check --library <pi_provider> --provider pi --settings '{"executable": "<fake_pi>"}' --program-agent --no-imports --no-logins --holds none`.

- `describe`: title `Pi`, ACP versions 1 and 2, what `PiAgent` takes and `session/resume`, `resumes_wider`, usage none, and the cache hint: `cache_lifetime` says `ignored`, since this provider passes Pi no setting for it.
- `refusal`, `start`: holds no requirement; each is `GUATIAO_ERR_UNSUPPORTED` with its name (Pi has no sandbox, no read-only mode and loads the person's extensions, skills and MCP servers). `start` on a spawner whose programs run elsewhere (`describe().local` false) is `UNSUPPORTED`: the gate is a file on this machine.
- `accounts`: `harness-login` and the two kinds that were the stdio provider's. `pi-api-key` (credential `api_key`; setting `variable`, one of Pi's provider key variables only, so a setting cannot name `PATH`): the variable named carries the key and `PI_CODING_AGENT_DIR` is the `state_dir`, which holds no `auth.json` or `models.json` to override the key or its `baseUrl`. `pi-oauth` (credential `auth_json`, the text of `auth.json`; rotating): the file is placed in `state_dir`, watched and reported as in `coqui_acp_harness::account` (a changed file is `renewed`, a missing or blank one `lost`); Pi refreshes it itself under a lock. A start on any kind but `harness-login` removes every credential, endpoint and cloud-identity variable Pi reads (`accounts::DEF.variables`, also the catalog's `credential_variables`) from what it starts, and from the request's environment, names compared without case on Windows. Not measured: the variables Pi's dependencies add for providers newer than 1.5.0.
- `models`: starts `pi --mode rpc --no-session` through the spawner, lists `get_available_models` and ends it (no session, no turn); no model with a credential is `unknown`. `usage`: `unsupported`, after looking at the account's file so a renewal is told.
- `login`: `GUATIAO_ERR_UNSUPPORTED`, since Pi signs in only inside its terminal (`/login`, `pi auth login`); `renew`: `UNSUPPORTED` for both kinds after the grant is read (a key is not renewed; Pi refreshes a sign-in itself). `import_session`, `export_session`: `GUATIAO_ERR_UNSUPPORTED` with a reason (after checking the account): Pi keeps its sessions as files in its own agent directory, which the spawner's private files do not reach. `session_record`, `non_defaults`: the kind's defaults.

## Errors

`Error` (`#[non_exhaustive]`): `Spawn(io::Error)`, `Refused(String)`, `Timeout { what, limit }`, `Harness(String)` in Pi's words or saying how it ended. Into `HarnessError`: `Refused` stays refused, the rest is `Failed`.

## Command line

`coqui-acp-pi` serves `PiAgent` on standard input and output. Flags: `--executable` (`COQUI_ACP_PI_EXECUTABLE`, default `pi`), `--model`, `--effort`, `--mode`, `--allow-bypass`, `--ask-for-reads`, `--max-acp-version` (1 or 2, default 2), `--max-sessions` (8), `--working-root` (repeated; a session's `cwd` and more directories must lie in one), `--allow-network-paths`. Notices and exits go to standard error. Exit status 0 when the client closes, 1 when reading it failed, 2 for a wrong invocation.

## Environment variables

`COQUI_ACP_PI_EXECUTABLE`, as `--executable`. Pi reads its own (`PI_CODING_AGENT_DIR`, `ANTHROPIC_API_KEY` and others) from the environment the agent runs in, or `PiLaunch::env`.

## Gotchas

- `abort` does not end an open dialog: Pi answers it only after the dialog is answered. Every open question is answered cancelled before the abort.
- `prompt` is answered before anything runs, and a run ends at `agent_settled`, not `agent_end`. `abort`'s response comes after the run's `agent_settled`.
- `set_thinking_level` with a level the model lacks, or with nonsense, succeeds and is clamped; the crate checks against `get_available_thinking_levels`.
- `get_state` with nothing configured answers a placeholder model (`unknown/unknown`) and `get_available_models` an empty list; a prompt is then refused. The model option shows `default`.
- Pi loads `AGENTS.md` and `CLAUDE.md` from the working directory and every parent into its system prompt, and the person's own extensions: an extension's question that this client cannot show is answered cancelled.
- A tool call Pi runs concurrently with another ends in completion order; each has its own question.
- A write's old content is read from disk when Pi starts the call, before the gate answers, so it is the file as it was; the translator reads it only inside the session's directories, and a spawner on another machine shows an add.
- A tool whose result holds an image shows its text only: the protocol's tool content has no image.
- An empty message with an image is taken by Pi 1.5.2.
- Project-local Pi settings are not read in RPC mode unless Pi is given `--approve`; pass it in `PiLaunch::extra_args` if wanted.
- Everything Pi writes is the model's or Pi's: untrusted.
