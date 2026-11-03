# `coqui-acp-provider` — API header

Header-file-style reference for the `coqui-acp-provider` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `agent-client-protocol`, `futures`, `guatiao` (with `provider`), `guatiao-serde` (no format), `serde`, `serde_json` and `uuid`. Guatiao is pinned by revision. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: everything is at the crate root. `coqui_acp_provider::sdk` is the protocol's own crate, for its transports; `coqui_acp_provider::guatiao` is Guatiao, so a provider names one version of it (`#[provider(AgentProvider, crate = ::coqui_acp_provider::guatiao)]`).

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state.

## Kinds

A kind is a C ABI. Its table carries a hash of every slot as spelled, so renaming a slot or a model breaks every built provider: `AgentProviderVtable::FLOOR_HASH` is `1182182788`, `AgentSessionVtable::FLOOR_HASH` is `1414216972`. The package holds both kinds as data in `include/coqui_acp_provider.kinds.json`, and for Python in `python/coqui_acp_provider_kinds.py`.

```rust
#[guatiao::kind(name = "agent_provider")]
pub trait AgentProvider: Send + Sync {
    fn start(&self, request: StartRequest) -> Result<Object<dyn AgentSession>, ProviderError>;
    fn resume(&self, request: ResumeRequest) -> Result<Object<dyn AgentSession>, ProviderError>;   // default: refuses, unsupported
}
```

A provider owns whatever process it runs; no terminal crosses. `start` may block until the harness runs. `resume` reattaches to an identity this provider issued, as long as it still exists; the identity may come from outside, so a provider checks it.

```rust
#[guatiao::kind(object, name = "agent_session")]
pub trait AgentSession: Send + Sync {
    fn session_id(&self) -> Result<Option<String>, ProviderError>;   // once the provider has one
    fn send_message(&self, message: AgentMessage) -> Result<(), ProviderError>;
    fn next_event(&self, timeout_ms: i64) -> Result<Option<AgentEvent>, ProviderError>;
    fn interrupt(&self) -> Result<(), ProviderError>;
    fn close(&self) -> Result<(), ProviderError>;
}
```

Any method may be called while another runs, from another thread: `next_event` blocks for its timeout, and `interrupt` and `close` have to reach it.

- `next_event` answers `None` when `timeout_ms` expires. A harness's exit must be reported as a `completed` or `failed` event, the `failed` text saying how it ended and what it wrote last: a provider that only returns `None` after end-of-file leaves its consumer waiting forever. A call that takes much longer than `timeout_ms` counts as hung.
- `interrupt` asks the provider to interrupt the running turn, which then ends with an event as any turn does.
- `close` returns once the harness has exited, or after a short wait when that cannot be told: the caller may remove the working directory next. It is called once.

A provider in another language: every method takes and answers plain data. `start`'s request is a read-only value, valid for the call; the Python binding reads it with `request.to_python()`. An event may leave out anything but `kind`. In Python this kind's `close` slot is `close_`, since Guatiao's Python classes keep the name `close` for themselves; `python -m guatiao.kinds` does not know that, so `python/write_kinds.py` writes the module.

## Requests

```rust
#[non_exhaustive]
pub struct StartRequest {
    pub working_directory: String,   // absolute
    pub prompt: String,
    pub context: Vec<String>,        // to establish before the prompt
    pub environment: BTreeMap<String, String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub meta: Option<guatiao::Value>,
}
pub fn StartRequest::new(working_directory: impl Into<String>, prompt: impl Into<String>) -> StartRequest

#[non_exhaustive]
pub struct ResumeRequest {
    pub session_id: String,
    pub working_directory: String,
    pub prompt: Option<String>,      // a message to submit after resuming
    pub context: Vec<String>,        // the standing part, for a harness that may not keep it; never a recap
    pub environment: BTreeMap<String, String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub meta: Option<guatiao::Value>,
}
pub fn ResumeRequest::new(session_id: impl Into<String>, working_directory: impl Into<String>) -> ResumeRequest
```

`environment` may hold credentials: a provider logs and reports none of it, its schema marks it sensitive, and no `Debug` here shows its values. `meta` is the provider's alone: nothing between the client and the provider reads it, so it is untrusted input.

## Events

```rust
pub struct AgentMessage { pub text: String }
pub struct AgentEvent {
    pub kind: String,
    pub text: Option<String>,
    pub data: Option<Vec<u8>>,
    pub limit: Option<UsageLimit>,
    pub usage: Option<TokenUsage>,
    pub tools: Vec<String>,
    pub quota: Vec<QuotaReport>,     // JSON: absent when empty
}
pub fn AgentEvent::new(kind: impl Into<String>, text: Option<String>) -> AgentEvent
pub struct UsageLimit { pub resets_at: Option<i64>, pub window: Option<String> }
pub struct TokenUsage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64, pub cost_micro_usd: u64, pub cumulative: bool }  // Default, Copy
pub struct QuotaReport { pub window: String, pub used_permille: Option<u16>, pub resets_at: Option<i64>, pub status: Option<String> }
```

All derive `ToValue`, `FromValue`, `Schema`, `Serialize`, `Deserialize`. In JSON every key but `quota` is required, `null` where empty. As a Guatiao value only `kind` is required. Their fields are public and the structs are not `non_exhaustive`, so a literal names every field.

Consumers match `kind` as text, so these strings are the contract:

| Constant | Text | Meaning |
| --- | --- | --- |
| `TEXT_EVENT` | `text` | Agent text, in `text`. One message may take any number of events. |
| `TOOL_EVENT` | `tool` | The agent used a tool: `text` is its title for people, `tools` its name. |
| `BLOCKED_EVENT` | `blocked` | The agent stopped short of what it was asked, and `text` says why. The turn goes on, unless `text` is `TURN_CANCELLED`. |
| `COMPLETED_EVENT` | `completed` | The turn finished. Also a harness that exited cleanly. |
| `FAILED_EVENT` | `failed` | The turn failed, and `text` says why; for a harness that exited, how it ended and what it wrote last. |
| `CANCELLED_EVENT` | `cancelled` | The turn stopped because of `interrupt`, or the agent gave it up. |
| `LIMIT_EVENT` | `limit` | The provider refuses further work until a quota resets. In place of `completed` or `failed`, with `limit` set. |
| `DENIED_EVENT` | `denied` | The harness refused calls in this turn by its rules. Before the turn's ending. `text` names the tools, separated by `, `; `tools` stays empty. |
| `TURN_CANCELLED` | `the agent cancelled the turn` | The `text` of a `blocked` event that means `cancelled`. No `completed` follows it. |

A turn ends with exactly one of `completed`, `failed`, `limit`, `cancelled` or `blocked` with `TURN_CANCELLED`. Any other kind (`usage`, `notice`, `permission`) ends nothing and a consumer may pass over it. An event of any kind may carry `tools`, `usage` and `quota`.

## `AcpFacade`

```rust
pub fn AcpFacade::new(provider: Arc<dyn AgentProvider>) -> AcpFacade   // Clone
pub fn with_auth(self, auth: FacadeAuth) -> Self
pub fn with_settings(self, settings: Settings) -> Self
pub fn with_agent_info(self, name: impl Into<String>, version: impl Into<String>) -> Self
pub fn run(&self, transport: impl ConnectTo<sdk::Agent> + 'static) -> Result<(), Error>
pub fn serve(&self, transport: impl ConnectTo<sdk::Agent> + 'static) -> Result<JoinHandle<Result<(), Error>>, Error>
```

`run` serves one connection on the calling thread until its transport closes, then closes its sessions and waits for them, up to `close_timeout`. `serve` is that on a thread. Sessions belong to the connection that opened them: another connection cannot name one, and a client that vanishes mid-turn has its sessions closed.

What ACP sees: `initialize` answers version 1, whatever the client asked. `authenticate` as below. `session/new` checks what it is given and makes no provider call; the first `session/prompt` calls `start` and later ones `send_message`, one turn at a time. `session/cancel` calls `interrupt`, and the prompt is answered `cancelled`, however the provider ended the turn. `session/resume` calls `resume` and its answer waits for the provider's, so a session that no longer exists fails there in the provider's words; its `sessionId` is the provider's identity, and it stays the ACP session's. `session/close` answers when the provider's `close` returned. `session/load` and everything else is method not found. A prompt is text and resource links, a link passing as its address; any other block is refused. A client's `mcpServers` other than empty is refused: no field of the kind carries them.

### What ACP has no field for

```rust
pub const LAUNCH_META_KEY: &str = "dev.boriken.coqui.acp/launch";
pub const EVENTS_CAPABILITY: &str = "dev.boriken.coqui.acp/events";
pub const EVENT_NOTIFICATION: &str = "_coqui_acp/event";
```

`_meta[LAUNCH_META_KEY]` on `session/new` and `session/resume`: `{"context": [text], "environment": {name: text}, "model": text, "effort": text, "meta": any}`, each optional. An unknown key is refused. An environment name not in `Settings::client_environment` is refused.

`EVENT_NOTIFICATION`, advertised under `agentCapabilities._meta[EVENTS_CAPABILITY]`, is `{"sessionId", "providerSessionId", "event"}`: the event as JSON, for what ACP has no update for. With no `event` it says the provider's identity is now known, once; that identity is what a later `session/resume` names.

| Event | On the wire |
| --- | --- |
| `text` | `agent_message_chunk`s, split at `max_update_bytes` |
| `tools` of any event | a `tool_call`, kind `other`, `in_progress`; a `tool_call_update` ends it with the turn |
| `denied` | a `tool_call`, kind `other`, `failed`, per name |
| `usage` with a cost | `usage_update`, in USD |
| `completed` | stop reason `end_turn` |
| `limit` | stop reason `refusal` |
| `cancelled`, `blocked` with `TURN_CANCELLED` | stop reason `cancelled` |
| `failed` | the prompt fails: code `-32603`, the text in `data` |
| any other kind; any `limit`, `usage` or `quota` | `EVENT_NOTIFICATION` |

A failure with no prompt waiting goes as `EVENT_NOTIFICATION`, with kind `failed`.

### `Settings`

`#[non_exhaustive]`, `Default`; set the fields of `Settings::default()`.

| Field | Default | At the limit |
| --- | --- | --- |
| `max_sessions: usize` | 8 | `session/new` and `session/resume` are refused. A closed session holds its place until the provider's last call returns. |
| `poll: Duration` | 100 ms | The timeout given to `next_event`. A provider that returns sooner is called again after it. |
| `start_timeout: Duration` | 60 s | `start` or `resume`: the session ends, and what the call returns later is closed. |
| `call_timeout: Duration` | 30 s | Any other call, past its own timeout: the session ends. |
| `cancel_grace: Duration` | 10 s | A cancelled turn is answered `cancelled`. Its later events are dropped, and a prompt is refused until its ending comes, or the old ending would answer the new prompt. |
| `close_timeout: Duration` | 10 s | `session/close` fails, or `run` answers `Error::Stuck`. |
| `max_prompt_bytes: usize` | 8 MiB | The prompt is refused. |
| `max_turn_bytes: usize` | 64 MiB | What a provider sends in a turn or between turns: the session ends. It bounds what queues for a client that stops reading. |
| `max_update_bytes: usize` | 1 MiB | `text` is split; an event in `EVENT_NOTIFICATION` has its text cut and bytes that do not fit left out. |
| `max_label_bytes: usize` | 4 KiB | A tool's title or a failure text is cut. |
| `max_auth_failures: u32` | 5 | The connection ends. |
| `environment: BTreeMap<String, String>` | empty | Given to every `start` and `resume`. |
| `client_environment: Vec<String>` | empty | The names a client may set, over `environment`. A client that can set `PATH` runs what it likes. |

`Debug` shows the environment's names, never its values.

### `FacadeAuth`

```rust
pub type Credential = dyn Fn(&Map<String, Value>) -> bool + Send + Sync;
pub struct FacadeAuth { pub method: String, pub name: String, pub check: Arc<Credential> }
```

Every request but `initialize` and `authenticate` answers auth required until an `authenticate` naming `method` has `check` accept its `_meta`. A refusal quotes nothing. `check` is the host's, so comparing in constant time is too.

## Errors

`Error` (`#[non_exhaustive]`): `Connection(String)` in the protocol library's words; `Thread(io::Error)`, a thread could not start; `Stuck(usize)`, this many sessions' providers had not returned from `close` when the connection ended.

## Gotchas

- **A provider's text is untrusted when it reaches the client.** The facade carries it as data: a stop reason, an error code, a tool's kind and identifier never come from it, and titles and failures lose control characters. `text` itself passes unchanged, so a client treats it as it treats any agent's output.
- **A tool's kind is always `other`.** A name says nothing a client could rely on, so a client's refused kinds (`coqui_acp`'s `refuse_kinds`) cannot hold a provider to anything: what a provider may do is the provider's to enforce.
- **A thread stuck in a provider cannot be stopped.** The session ends, its thread stays until the call returns, and it keeps its place in `max_sessions`. A provider in the host's process that holds a lock forever, as Python code holding the interpreter does, stops every call that needs it: run one that may do that in a process of its own.
- **A panic stops at the call**, and its words are not told: they can hold anything the provider held. Through a Guatiao table it is a status; in a trait object called directly, `the provider panicked in <call>`.
- **Nothing reaches a provider's environment from a client by default**, and a provider's `meta` is whatever a client wrote.
- **Standard output is the transport** for a facade served on standard input and output, so a provider in that process must not print to it.
- `model` and `effort` come through `LAUNCH_META_KEY`: `coqui_acp`'s `Builder::model` works through config options, which this facade does not offer.
