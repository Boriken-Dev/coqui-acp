# `coqui-acp-harness-kind` — API header

Header-file-style reference for the `coqui-acp-harness-kind` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

MSRV Rust 1.89. A default build depends on `guatiao` (with `provider`) and `guatiao-channel`, both pinned by one revision: a host and every provider build against the same one, and a library built against another `guatiao::library::ABI_VERSION` is refused at load. Feature `contract` (off by default) adds the contract suite, `contract::Contract`, and the program `acp-harness-check`, with `serde_json`, `coqui-acp` and Guatiao's `load`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: everything is at the crate root, but the suite (`contract`). `coqui_acp_harness_kind::guatiao` is Guatiao, so a provider names one version of it (`#[provider(AcpHarness, crate = ::coqui_acp_harness_kind::guatiao)]`).

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Kinds

A kind is a C ABI. Its table carries a hash of every slot as spelled, so adding, removing or respelling a slot, or renaming a model, breaks every built provider:

| kind | table | `FLOOR_HASH` | who implements it |
| --- | --- | --- | --- |
| `acp_harness` | `AcpHarnessVtable` | `165281080` | the provider |
| `channel` | `ChannelVtable` | `2843269355` | the provider (Guatiao's, re-exported) |
| `channel_sink` | `ChannelSinkVtable` | `3528315804` | the host (Guatiao's, re-exported) |
| `process_spawner` | `ProcessSpawnerVtable` | `1466370631` | the host |
| `spawned_process` | `SpawnedProcessVtable` | `3306532587` | the host |

The package holds all five as data in `include/coqui_acp_harness_kind.kinds.json`, every model's JSON Schema in `include/coqui_acp_harness_kind.schemas.json`, and the kinds for Python in `python/coqui_acp_harness_kinds.py` (written by `python/write_kinds.py`).

**How the kind changes.** Until its first release every method a harness has to answer is required, and a method added or changed is a break: every provider rebuilds, and the hash moves. No compatibility code is kept: no default so that an older provider loads, no deprecated alias, no reading of an older request's shape. After its first release the kind grows only by appending, as Guatiao provides for released kinds.

**The hash does not cover a model's fields**: a model crosses as a map, and a reader generated in another language skips a key it does not know, so a renamed field can be dropped with no error. Two things gate that. `tests/kinds.rs` pins every model's keys on the declaring side, so a field renamed here fails it. And every provider (in Rust, in Python, the translators) runs the contract suite at each change of this crate's revision or of Guatiao's: a provider reading a field under its old name fails a check.

```rust
#[guatiao::kind(name = "acp_harness")]
pub trait AcpHarness: Send + Sync {
    fn describe(&self) -> Result<HarnessInfo, ProviderError>;
    fn start(&self, request: StartRequest, spawner: Object<dyn ProcessSpawner>, sink: Object<dyn ChannelSink>)
        -> Result<Object<dyn Channel>, ProviderError>;
    fn refusal(&self, requirements: Vec<Requirement>, environment: Secret<Environment>)
        -> Result<Option<String>, ProviderError>;
    fn program(&self) -> Result<Option<String>, ProviderError>;
    fn models(&self, environment: Secret<Environment>) -> Result<ModelListing, ProviderError>;     // default: unknown
    fn session_record(&self, query: SessionRecordQuery) -> Result<Option<SessionRecordFile>, ProviderError>; // default: None
    fn non_defaults(&self) -> Result<Vec<NonDefault>, ProviderError>;                               // default: empty
}
```

Every method is synchronous; a host calls them from a thread of its own. Every method answers for the provider **as configured**: a host builds an instance from the provider's settings (`offer.instantiate(&settings)`) and asks it, so `describe` and `refusal` are a configured harness's answers. The four without a default are the ones a host acts on for every harness; the three defaults are true answers for a harness with nothing to say (no way to list models, no record kept, settings its schema shows in full).

- `describe`: who the harness is and what its `initialize` will advertise, with nothing started. Cheap, the same on every call.
- `start`: one ACP connection. The host is the client, the provider the agent. `spawner` and `sink` are handed over: the provider destroys both, even when it refuses, and the spawner by the time it calls `closed`. It returns once the channel takes messages; the program may start later, and a failure then is answered in ACP before the provider closes. It refuses exactly when `refusal` would for the request's requirements, with the same status: `GUATIAO_ERR_UNSUPPORTED` for a requirement it cannot hold, `GUATIAO_ERR_BAD_VALUE` for a request it cannot read. It never starts with a requirement left out.
- `refusal`: why the harness could not hold a connection to **all of** `requirements` together, started with `environment`, in a sentence; `None` when it can. An empty environment asks about the configuration alone and is answered. `GUATIAO_ERR_BAD_VALUE` for requirements `start` would refuse as unreadable. A host asks before it starts a connection and passes over a harness that would refuse.
- `program`: the program the harness starts through the spawner (a path, or a name looked up on the `PATH` where it runs, the start's environment's `PATH` when that sets one); `None` for one that starts none.
- `models`: what the account behind `environment` may use, with no model turn, or why it cannot say.
- `session_record`: where the harness keeps its own record of a session, by the UUID it reported.
- `non_defaults`: how a configured harness departs from its defaults in ways its settings' schema cannot show (the detail of a rule a setting holds as text). The rest a host reads from the schema itself, with Guatiao's `schema::differences`.

**Nothing a provider answers carries a value of an environment**: not an answer, a refusal, an error or a log line. `Secret<Environment>` prints `[redacted]`, and its schema marks it `writeOnly` and `x-sensitive`. The copies Guatiao's proxy makes of an argument while converting it are not wiped.

A provider in-process is trusted code: the kind cannot check that it keeps a requirement, and a requirement it accepts is its promise.

### The channel

`Channel` and `ChannelSink` are Guatiao's (`guatiao-channel`), with every rule of theirs; what this kind adds is what the bytes are.

- **One message is one whole JSON-RPC 2.0 message**, as UTF-8 JSON text, either way: a request, a notification or a response, never a batch and never part of one. Whitespace around it is allowed, so a line read from a stdio agent, newline included, is a message.
- **ACP is agreed on the channel.** The host sends `initialize` first; the version is what the answer says. `describe` is advance notice; `initialize` governs.
- **Back-pressure blocks**, as the channel's rules say. A provider's intake never waits on its agent's work, so a `session/cancel` reaches a running turn; it may wait on the host's sink. A host's `receive` never waits for its own `send`.
- **Cancelling a turn is ACP's `session/cancel`**, a message like any other: the turn ends with `stopReason` `cancelled`. `$/cancel_request` passes through the same way. The kind has no cancellation of its own; a `send` waiting for room ends only with `close`, answered `GUATIAO_ERR_GONE`.
- **What is not a message is answered, and the connection goes on**: the provider answers text that is not JSON `-32700`, a batch or anything not one JSON-RPC message `-32600` (id null), a request whose `method` is not text `-32600`, params of the wrong type `-32602`, an unknown method `-32601`; an unknown notification is ignored. A host receiving text that is not JSON, or a batch, refuses it from `receive` with `GUATIAO_ERR_BAD_VALUE`, which ends the connection.
- **Closing.** On the host's `close(status)` the provider answers every request it still holds (a running prompt with `stopReason` `cancelled`, any other with an error), stops what it started (its whole process tree), destroys the spawner, then calls `closed(status)`. On its own end it does the same and calls `closed` with its status. After `closed`, the next `send` answers `GUATIAO_ERR_GONE`.

| the end | `closed` status |
| --- | --- |
| the host closed with `s` | `s` |
| the agent ended normally, or the provider ended the connection | `GUATIAO_OK` |
| the agent exited with a failure or could not start | `GUATIAO_ERR_INTERNAL`, after answering what is pending with errors saying why |
| a message over the provider's size limit | `GUATIAO_ERR_BAD_VALUE`; the `send` that carried it answers the same |
| the sink refused a message | the sink's status |

A message's size limit is each side's own setting; the kind sets none. Requests either side was waiting on end unanswered when the other closes; a host does not resend them.

### The spawner

```rust
#[guatiao::kind(object, name = "process_spawner")]
pub trait ProcessSpawner: Send + Sync {
    fn describe(&self) -> Result<SpawnerInfo, ProviderError>;
    fn spawn(&self, spec: SpawnSpec) -> Result<Object<dyn SpawnedProcess>, ProviderError>;
}
#[guatiao::kind(object, name = "spawned_process")]
pub trait SpawnedProcess: Send + Sync {
    fn write(&self, bytes: &[u8]) -> Result<(), ProviderError>;           // all of it; GONE once input closed or ended
    fn close_input(&self) -> Result<(), ProviderError>;
    fn read(&self, dst: &mut [u8], timeout_ms: u64) -> Result<i64, ProviderError>;        // stdout, or the terminal
    fn read_error(&self, dst: &mut [u8], timeout_ms: u64) -> Result<i64, ProviderError>;  // stderr when Separate; else 0
    fn resize(&self, size: TerminalSize) -> Result<(), ProviderError>;    // UNSUPPORTED without a terminal
    fn wait(&self, timeout_ms: u64) -> Result<ProcessExit, ProviderError>; // 0 polls
    fn kill(&self) -> Result<(), ProviderError>;                          // the whole tree; idempotent
    fn link(&self) -> Result<LinkState, ProviderError>;
}
pub const READ_TIMED_OUT: i64 = -1;   // nothing within the timeout
pub const READ_LINK_DOWN: i64 = -2;   // the link is down; the process may run on, its output kept
```

The host hands a spawner to every `start`: where and how the harness's programs run, on this machine, over SSH, in a container or a virtual machine. A provider starts its programs through it, or ignores it and destroys it. `read` answers the byte count, `0` once no more can come, or one of the two codes; it never skips or repeats a byte across a dropped link. Destroying a process kills its tree. `spawn` refuses `GUATIAO_ERR_UNSUPPORTED` what `describe` says it cannot give, `GUATIAO_ERR_NOT_FOUND` a missing program, `GUATIAO_ERR_BAD_VALUE` a spec it cannot read. A local spawner is `coqui_acp_harness::LocalSpawner` (feature `kind` of `coqui-acp-harness`).

```rust
#[non_exhaustive] pub struct SpawnSpec { pub program: String, pub args: Vec<String>, pub cwd: Option<String>,
    pub environment: Secret<Environment>, pub clear_environment: bool, pub remove_environment: Vec<String>,
    pub terminal: Option<TerminalSize>, pub error_output: ErrorOutput }
pub fn SpawnSpec::new(program) -> SpawnSpec;  .arg(a)  .cwd(d)  .environment(env)  .clear_environment(b)  .terminal(size)  .error_output(o)
#[non_exhaustive] pub enum ErrorOutput { Separate /* default */, Merged, Discarded }   // "separate" | "merged" | "discarded"
#[non_exhaustive] pub struct TerminalSize { pub rows: u16, pub cols: u16 }   TerminalSize::new(rows, cols)
#[non_exhaustive] pub struct SpawnerInfo { pub os: String, pub arch: String, pub local: bool, pub terminal: bool,
    pub separate_error: bool, pub survives_link_loss: bool, pub kills_escaped: bool, pub ends_with_host: bool }
pub fn SpawnerInfo::new(os, arch, local: bool) -> SpawnerInfo   // the rest false
#[non_exhaustive] pub struct ProcessExit { pub running: bool, pub code: Option<i64>, pub signal: Option<i64> }
ProcessExit::running();  ProcessExit::ended(code, signal)
#[non_exhaustive] pub struct LinkState { pub up: bool, pub drops: u64 }   LinkState::up()
```

`os` and `arch` are spelled as `std::env::consts` spells them: which rules the programs' paths follow (`PathStyle::of_os`). `kills_escaped` says whether `kill` reaches processes that left the program's group or session; `ends_with_host`, whether the tree ends when the host dies without a kill.

## Requests and answers

What a host sends (`StartRequest`, `Requirement`, `SessionRecordQuery`, and the provider's `SpawnSpec`) **refuses a key it does not declare** (`GUATIAO_ERR_BAD_VALUE`), so a narrowing a newer side adds is never dropped by an older one: a new key is a break. What is answered is read leniently.

```rust
#[non_exhaustive] pub struct StartRequest {
    pub environment: Secret<Environment>,   // over the configuration's; sensitive
    pub working_roots: Vec<String>,         // every session's directories lie in one; empty adds none
    pub requirements: Vec<Requirement>,     // what the connection is held to; each name once
    pub session_id: Option<String>,         // the UUID the first session/new creates its session under
}
StartRequest::new();  StartRequest::builder() -> StartRequestBuilder   // .environment(env) .working_root(r) .requirement(r) .session_id(id) .build()

#[non_exhaustive] pub struct Requirement { pub name: String, pub value: Option<Value> }
Requirement::new(name);  Requirement::with_value(name, value);  requirement.narrowing() -> Result<Narrowing, RequirementError>
#[non_exhaustive] pub enum Narrowing { ReadOnly, NoCommands, AnswerOnly, NoProjectSettings, NoProjectMcp,
    FilesUnder(Vec<String>), ReadOnlyPaths(Vec<String>), DeniedPaths(Vec<String>), Network(Vec<String>),
    SandboxedScripts(Vec<String>), OnlySkills(Vec<String>) }
Narrowing::read(&Requirement);  Narrowing::read_all(&[Requirement]) -> Result<Vec<Narrowing>, RequirementError>   // a repeated name refused
impl From<Narrowing> for Requirement
#[non_exhaustive] pub enum RequirementError { Unknown { name }, BadValue { name, why }, Repeated { name } }
error.status()   // UNSUPPORTED for Unknown, BAD_VALUE otherwise; impl From<RequirementError> for ProviderError
pub const NAMES: [&str; 11];

pub struct Environment;   // BTreeMap<String, String> inside; Debug names the variables only; Wipe zeroes names and values
Environment::new(); .insert(name, value); .get(name); .iter(); .names(); .len(); .is_empty(); FromIterator; From<BTreeMap<String, String>>

#[non_exhaustive] pub struct HarnessInfo { pub title: String, pub versions: Vec<u16>, pub takes: Takes, pub resumes_wider: bool }
HarnessInfo::new(title, versions)
#[non_exhaustive] #[derive(Default)] pub struct Takes { pub images: bool, pub audio: bool, pub embedded_context: bool,
    pub mcp_http: bool, pub mcp_sse: bool, pub load: bool, pub resume: bool, pub additional_directories: bool, pub system_prompt: bool }
#[non_exhaustive] pub struct ModelListing { pub known: bool, pub unknown_because: Option<String>, pub models: Vec<ModelInfo> }
ModelListing::unknown(reason);  ModelListing::of(models)
#[non_exhaustive] pub struct ModelInfo { pub id: String, pub name: Option<String>, pub description: Option<String> }   ModelInfo::new(id)
#[non_exhaustive] pub struct SessionRecordQuery { pub session_id: String, pub working_directory: String, pub environment: Secret<Environment> }
SessionRecordQuery::new(session_id, working_directory).environment(env)
#[non_exhaustive] pub struct SessionRecordFile { pub path: String, pub format: String }   // `claude-code-jsonl`, `codex-rollout-jsonl`
#[non_exhaustive] pub struct NonDefault { pub setting: String, pub value: String, pub default: String }   NonDefault::new(setting, value, default)
```

`resumes_wider`: a session held to requirements can be reopened (`session/load` or `session/resume`) on a later connection started with fewer, and then has what that connection allows. A host that changes what a session may do starts a new connection with the new requirements and reopens the session there.

### Requirements

**Requirements only narrow.** Each takes something away from what the provider's configuration allows; none grants anything, and a provider never reads one as permission. A name the provider does not know is refused; a known name with a value it does not take (one missing included), or a name given twice, is a bad value. What a session is given (more directories, a plugin, context) travels in ACP: `additionalDirectories` and `_meta` on `session/new`.

| name | value | the connection's sessions |
| --- | --- | --- |
| `read_only` | none | create, change and remove no file, by the agent or what it runs; commands may still run where the file system is read-only to them |
| `no_commands` | none | start no command or program: no shell, script or terminal, sandboxed or not |
| `answer_only` | none | only answer from the prompt: no tool of any kind (no file read or write, command, MCP tool from any source, web search or fetch); no settings, MCP servers, skills, hooks or instruction files from the session's directories; what the harness runs to answer is sandboxed, writes no file and reaches no network but the harness's own model endpoint. Holds every name above and below that takes no value |
| `no_project_settings` | none | read no configuration from the session's directories or their parents (settings, permission rules, hooks, commands, sub-agents); instruction files (`AGENTS.md`, `CLAUDE.md`) still load |
| `no_project_mcp` | none | start no MCP server declared in the session's directories or their parents |
| `files_under` | list of paths | create, change and remove files only under these; an empty list allows none |
| `read_only_paths` | list of paths | create, change or remove nothing under these |
| `denied_paths` | list of paths | read, list, create, change or remove nothing under these |
| `network` | list of `host`, `host:port`, `*.domain`, `[v6]:port` | reach only these, besides the harness's own model endpoint; an empty list reaches nothing else |
| `sandboxed_scripts` | list of program names | run only these interpreters, inside the harness's sandbox; no other command |
| `only_skills` | list of skill names | load at most these skills; an empty list loads none |

A path in a value is absolute (either platform's form) with no `.` or `..` part. A spending limit is not a requirement: the host keeps its own.

### Working roots and session ids

```rust
pub struct WorkingRoots;   // Clone, Debug, PartialEq
WorkingRoots::new(roots: Vec<String>)   // this platform's rules, links followed
    .style(PathStyle) .follow_links(bool) .is_empty() .admits(path: &str) -> Result<(), PathRefusal>
pub enum PathStyle { Windows, Posix }   PathStyle::local();  PathStyle::of_os(os)
#[non_exhaustive] pub enum PathRefusal { NotPlainAbsolute, Outside, LinkLeadsOutside }
pub fn is_plain_absolute(path: &str) -> bool
pub fn is_session_id(text: &str) -> bool   // a UUID, lowercase, 8-4-4-4-12
```

A session's `cwd` and every one of its `additionalDirectories`, on `session/new`, `session/load` and `session/resume`, lie in a working root, or the request is answered `-32602`. A path is in a root when it is absolute, has no `.` or `..` part (refused, never normalised), and its parts begin with every part of the root (`/work-beside` is not in `/work`); on Windows `\` and `/` are one separator and names compare without regard to case. Following links, the path and the roots are resolved through the links that exist, and the resolved path must lie in a resolved root too, so a link inside a root leading out of it is refused. A provider whose agent runs elsewhere compares by that platform's style and follows no link of this machine's.

**Session ids are UUIDs.** Every session id a provider reports, in a `session/new` answer, a `session/update` and wherever else ACP carries one, and every one `session_record` is asked about, is a UUID as `is_session_id` spells it. A provider whose harness names sessions otherwise maps them, and keeps the mapping where it outlives the connection. With `StartRequest::session_id`, the connection's first `session/new` creates its session under that id: one the provider already has is refused in ACP, never reused. `session/load` and `session/resume` take a UUID: the provider resolves it through its mapping, and failing that tries it as the harness's own id where the harness's ids are UUIDs, or refuses it in ACP.

**The host stays the client.** Every request the agent makes of the client (`session/request_permission`, `fs/*`, `terminal/*`, `elicitation/create`) reaches the host, which answers it under its own rules, refusing by default. The agent asks only for what `initialize`'s `clientCapabilities` offered. A permission answered with anything but a selected option the request offered is a refusal; a cancelled outcome ends the turn `cancelled`. A provider's answer never overrides the host's refusal.

## A provider in another language

Every method takes and answers plain data; `start` also takes the spawner and the sink and answers the channel, all objects. The Python module is what `python -m guatiao.kinds` writes from the description: a provider subclasses `AcpHarness` and `Channel`, calls the sink it is handed (`receive(bytes)`, `closed(status)`, then `destroy()`), from a `threading.Thread` of its own if it likes, and calls `destroy()` on the spawner it does not keep. A Python `start` that refuses destroys both before it raises: Guatiao's Python binding lets go of a taken argument only when the call never ran. The models' schemas in `include/coqui_acp_harness_kind.schemas.json` say which keys a request may have. A provider in a language whose objects are called on one engine thread (QuickJS, Node, Lua) has no thread of its own: it answers inside the `send` that took the message, so it cannot be told `session/cancel` while a turn runs.

## The contract suite (feature `contract`)

```rust
pub struct contract::Contract;   // #[must_use]
Contract::new(name) .version(u16 /* 1 */) .holds(Vec<Requirement> /* read_only, no_commands */) .message_limit(bytes)
    .run(&dyn AcpHarness) -> Result<(), contract::Failures>
#[non_exhaustive] pub struct contract::Failure { pub check: &'static str, pub message: String }
pub struct contract::Failures { pub provider: String, pub failures: Vec<Failure> }   // Display lists each
```

Every provider of the kind passes it, configured to drive the **contract agent**: a real harness supplies a stand-in for its program that behaves as the module's documentation says (`close`, `wait`, `ask`, `read PATH`, else `echo: TEXT`). It checks the methods, requirements, secrets, ACP and its errors, the host's session ids, reopening, cancel, permission answers, client capabilities, back-pressure, every close, working roots, the size limit and a real `coqui_acp::Client`. It makes and removes a directory under the system's temporary one, with a link (a junction on Windows) when the platform allows.

`acp-harness-check` runs it on a provider with no Rust of the provider's own: `--library FILE`, or `--source SOURCE_LIBRARY --source-config JSON` for one a Guatiao source loads; `--provider KEY`, `--settings JSON` (build an instance), `--version N`, `--holds NAME[=JSON]` (repeated), `--message-limit N`. Exits 0, 1 listing each rule broken, or 2 when the provider could not be loaded.

## Tests

`examples/echo_harness.rs` is the contract agent in Rust, built by `cargo test` into a `cdylib` the tests load, with settings (`title`, `max_message_bytes`) a host builds instances from. `examples/provider-python/harness/acp_echo_harness.py` in the repository is the same in Python.
