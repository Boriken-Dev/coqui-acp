# `coqui-acp-harness-kind` — API header

Header-file-style reference for the `coqui-acp-harness-kind` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `guatiao` (with `provider`) and `guatiao-channel`, both pinned by one revision: a host and every provider build against the same one, and a library built against another `guatiao::library::ABI_VERSION` is refused at load. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: everything is at the crate root. `coqui_acp_harness_kind::guatiao` is Guatiao, so a provider names one version of it (`#[provider(AcpHarness, crate = ::coqui_acp_harness_kind::guatiao)]`).

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Kinds

A kind is a C ABI. Its table carries a hash of every slot as spelled, so renaming a slot or a model breaks every built provider: `AcpHarnessVtable::FLOOR_HASH` is `2194037834`. The channel kinds are Guatiao's own, re-exported: `ChannelVtable::FLOOR_HASH` is `2843269355`, `ChannelSinkVtable::FLOOR_HASH` is `3528315804`. The package holds all three as data in `include/coqui_acp_harness_kind.kinds.json`, and for Python in `python/coqui_acp_harness_kinds.py` (written by `python/write_kinds.py`).

```rust
#[guatiao::kind(name = "acp_harness")]
pub trait AcpHarness: Send + Sync {
    fn describe(&self) -> Result<HarnessInfo, ProviderError>;
    fn start(&self, request: StartRequest, sink: Object<dyn ChannelSink>)
        -> Result<Object<dyn Channel>, ProviderError>;
    fn refusal(&self, requirement: Requirement, environment: BTreeMap<String, String>)
        -> Result<Option<String>, ProviderError>;                                     // default: refuses every requirement
    fn models(&self, environment: BTreeMap<String, String>) -> Result<ModelListing, ProviderError>;   // default: unknown
    fn program(&self) -> Result<Option<String>, ProviderError>;                          // default: None
    fn session_record(&self, query: SessionRecordQuery)
        -> Result<Option<SessionRecordFile>, ProviderError>;                             // default: None
}
```

Every method is synchronous; a host calls them from a thread of its own. The kind grows by appending methods with defaults, and its models by fields that default.

- `describe` says who the harness is and what its `initialize` will advertise, with nothing started. Cheap, the same on every call.
- `start` starts one ACP connection. The host is the client, the provider the agent. The sink is handed over: the provider destroys it, even when it refuses. It returns once the channel takes messages; the program it drives may start later, and a failure then is answered in ACP before the provider closes. It refuses with `GUATIAO_ERR_UNSUPPORTED` when it would refuse any of the request's requirements, and `GUATIAO_ERR_BAD_VALUE` for a request it cannot read. It never starts with a requirement left out.
- `refusal` answers why the harness could not hold a connection to one requirement, started with `environment`, in a sentence; `None` when it can. The default refuses every requirement.
- `models` lists what the account behind `environment` may use, with no model turn, or says it cannot. It never carries a value from `environment`.
- `program` names the program the harness starts (a name looked up on `PATH`, or a path), so a host can say it is missing first.
- `session_record` is where the harness keeps its own record of a session, when it keeps one and the file is there.

A provider's configuration (the program's path, its permission mode, its limits) is its own settings type, declared with `#[provider(.., config = Settings)]` and checked by the host against its schema; it never crosses a method of the kind. Whether a provider can work at all is its `available` hook.

### The channel

`Channel` and `ChannelSink` are Guatiao's (`guatiao-channel`), with every rule of theirs; what this kind adds is what the bytes are.

- **One message is one whole JSON-RPC 2.0 message**, as UTF-8 JSON text, either way: a request, a notification or a response, never a batch and never part of one. Whitespace around it is allowed, so a line read from a stdio agent, newline included, is a message.
- **ACP is agreed on the channel.** The host sends `initialize` first; the version is what the answer says. The kind carries no version of ACP.
- **Back-pressure blocks**, as the channel's rules say: `send` returns once the provider has taken the message, `receive` once the host has. A host's `receive` never waits for its own `send`; a provider keeps taking messages while its answers wait, or a cancel cannot reach it.
- **Cancelling a turn is ACP's `session/cancel`**, a message like any other. The kind has no cancellation of its own. A `send` waiting for room ends only with `close`, which answers it `GUATIAO_ERR_GONE`.
- **Closing.** `close(GUATIAO_OK)` from the host is the end of the client's input: the provider ends every session, stops what it started (its whole process tree), then calls `closed`. Another status says why the host gave up (`GUATIAO_ERR_GONE`: its client left). The provider's `closed(GUATIAO_OK)` is the agent ending; another status, that it failed. Before closing on a failure, the provider answers every request it still holds with a JSON-RPC error saying why, since `closed` carries no text. Requests either side was waiting on end unanswered: a host does not resend them.
- **A message over a side's own size limit** ends the connection with `GUATIAO_ERR_BAD_VALUE`. The limit is each side's setting; the kind sets none.

## Requests and answers

```rust
#[non_exhaustive]
pub struct StartRequest {
    pub environment: BTreeMap<String, String>,   // sensitive: logged and reported nowhere
    pub working_roots: Vec<String>,              // every session's cwd and directories lie in one; empty adds none
    pub requirements: Vec<Requirement>,          // what the connection is held to
}
pub fn StartRequest::new() -> StartRequest      // adds nothing to the provider's configuration

#[non_exhaustive]
pub struct Requirement { pub name: String, pub detail: Option<guatiao::Value> }
pub fn Requirement::new(name: impl Into<String>) -> Requirement
pub fn Requirement::about(name: impl Into<String>, detail: impl Into<guatiao::Value>) -> Requirement

pub const READ_ONLY: &str = "read_only";                 // sessions read and answer: no file written, no command run
pub const NO_COMMANDS: &str = "no_commands";             // no command runs; files may be written
pub const RESUME_WIDER: &str = "resume_wider";           // a session held to a requirement resumes, later, without it
pub const ONLY_SKILLS: &str = "only_skills";             // exactly the skills `detail` lists (names), no other
pub const SANDBOXED_SCRIPTS: &str = "sandboxed_scripts"; // the interpreters `detail` lists run in the harness's sandbox

#[non_exhaustive]
pub struct HarnessInfo { pub title: String, pub versions: Vec<u16>, pub takes: Takes }
pub fn HarnessInfo::new(title: impl Into<String>, versions: Vec<u16>) -> HarnessInfo

#[non_exhaustive]
#[derive(Default)]
pub struct Takes {                     // what `initialize` will advertise; every field defaults to false
    pub images: bool, pub embedded_context: bool, pub mcp_http: bool, pub mcp_sse: bool,
    pub load: bool, pub additional_directories: bool, pub system_prompt: bool,
}
pub const fn Takes::new() -> Takes

#[non_exhaustive]
pub struct ModelListing { pub known: bool, pub unknown_because: Option<String>, pub models: Vec<ModelInfo> }
pub fn ModelListing::unknown(reason: impl Into<String>) -> ModelListing
pub fn ModelListing::of(models: Vec<ModelInfo>) -> ModelListing
#[non_exhaustive]
pub struct ModelInfo { pub id: String, pub name: Option<String>, pub description: Option<String> }
pub fn ModelInfo::new(id: impl Into<String>) -> ModelInfo

#[non_exhaustive]
pub struct SessionRecordQuery { pub session_id: String, pub working_directory: String, pub environment: BTreeMap<String, String> }
pub fn SessionRecordQuery::new(session_id: impl Into<String>, working_directory: impl Into<String>) -> SessionRecordQuery
#[non_exhaustive]
pub struct SessionRecordFile { pub path: String, pub format: String }   // format: `claude-code-jsonl`, `codex-rollout-jsonl`
pub fn SessionRecordFile::new(path: impl Into<String>, format: impl Into<String>) -> SessionRecordFile
```

Every model derives `ToValue`, `FromValue` and `Schema`. `StartRequest` and `SessionRecordQuery` print the environment's names, never its values.

**Requirements only narrow.** Each one takes something away from what the provider's configuration allows; none grants anything, and a provider never reads one as permission. `Requirement::name` is an open vocabulary: a provider refuses a name it does not know, both from `refusal` and by refusing `start`. A host asks `refusal` before it starts a connection and passes over a harness that would refuse. Working roots narrow the same way: a session outside them is refused in ACP (`-32602`).

**The host stays the client.** Every request the agent makes of the client (`session/request_permission`, `fs/*`, `terminal/*`, `elicitation/create`) reaches the host, which answers it under its own rules, refusing by default. A provider's answer never overrides the host's refusal.

## A provider in another language

Every method takes and answers plain data; `start` also takes the sink and answers the channel, both objects. The Python module is what `python -m guatiao.kinds` writes from the description: a provider subclasses `AcpHarness` and `Channel`, and calls the sink it is handed (`receive(bytes)`, `closed(status)`, then `destroy()`), from a `threading.Thread` of its own if it likes. A Python `start` that refuses calls `sink.destroy()` before it raises: Guatiao's Python binding lets go of a taken argument only when the call never ran. A provider in a language whose objects are called on one engine thread (QuickJS, Node, Lua) has no thread of its own: it answers inside the `send` that took the message, so it cannot be told `session/cancel` while a turn runs.

## Tests

`tests/contract/mod.rs` is the suite every provider of the kind passes, included with `#[path]`: ACP through the channel and through `coqui_acp::Client`, back-pressure, close waking a waiting `send` and reaching an idle provider, the provider's own close, a sink that has gone, requirements and working roots. `examples/echo_harness.rs` is a provider holding to it; `cargo test` builds it.
