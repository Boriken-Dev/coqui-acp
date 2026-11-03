# `coqui-acp` — API header

Header-file-style reference for the `coqui-acp` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

There are no Cargo features. MSRV Rust 1.89. A default build depends on `agent-client-protocol`, `futures` and `serde_json`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: the crate root holds everything but `lines`. `coqui_acp::sdk` is the protocol's own crate, for its transports.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state. No async runtime is required: each session runs a `futures` executor on a thread of its own.

## `Client`, `Builder` (crate root)

```rust
pub fn Client::builder(cwd: impl Into<String>) -> Builder
```
Starts a session description; `cwd` is the agent's absolute working directory.

```rust
pub fn client_info(self, name: impl Into<String>, version: impl Into<String>) -> Self
pub fn resume(self, session: impl Into<String>) -> Self
pub fn load(self, session: impl Into<String>) -> Self
pub fn mcp_server(self, server: Value) -> Self
pub fn meta(self, key: impl Into<String>, value: Value) -> Self
pub fn model(self, model: impl Into<String>) -> Self
pub fn effort(self, effort: impl Into<String>) -> Self
pub fn model_option(self, id: impl Into<String>) -> Self
pub fn effort_option(self, id: impl Into<String>) -> Self
pub fn prompt(self, text: impl Into<String>) -> Self
pub fn refuse_kinds(self, kinds: impl IntoIterator<Item = impl Into<String>>) -> Self
pub fn authenticate(self, authenticate: Authenticate) -> Self
pub fn turn_end_by(self, key: impl Into<String>) -> Self
pub fn ends_turn(self, ends: impl Fn(&str, &Value) -> bool + Send + Sync + 'static) -> Self
pub fn disconnect_detail(self, detail: impl FnOnce() -> Option<String> + Send + 'static) -> Self
pub const fn initialize_timeout(self, limit: Option<Duration>) -> Self
pub const fn request_timeout(self, limit: Option<Duration>) -> Self
pub const fn close_grace(self, limit: Option<Duration>) -> Self
pub const fn max_pending_events(self, events: usize) -> Self
pub fn decider(self, decider: Arc<dyn Decider>) -> Self
pub const fn close_on_drop(self, close: bool) -> Self
pub const fn fail_on_disconnect(self, fail: bool) -> Self
pub fn on_close(self, on_close: impl FnOnce() + Send + 'static) -> Self
pub fn connect(self, transport: impl ConnectTo<sdk::Client> + 'static) -> Result<Session, Error>
```

- `resume`: `session/resume`, else `session/load` with its replayed history dropped. `load`: `session/load` whatever else the agent offers, delivering what it replays.
- `meta` sets a key of the session request's `_meta`. `model` and `effort` go through the agent's config options: by the id given with `model_option`/`effort_option`, else the option of category `model`/`thought_level`, else the id `model`/`effort`. What cannot be applied is an `Event::Notice`.
- `refuse_kinds`: refused without asking the decider. A tool of such a kind that runs, whether it asked or not, ends the session with `Event::Failed`; a tool that reports `failed` ends nothing. A kind once announced as refused stays so, whatever a later message claims.
- `turn_end_by(key)`: when the agent advertises `key` in `agentCapabilities._meta`, its notifications carry turn endings: standard updates and config options are left alone, and a failed prompt is `Event::PromptFailed`.
- `ends_turn(ends)`: while `turn_end_by` is active, says which of the extension's notifications end a turn, given their method and parameters, so a connection that ends after one is no `Event::Failed`. It runs on the connection's thread, in order and ahead of the connection's end, so it cannot race a reader; it must not block. Turns are counted: no turn runs only once as many endings have arrived as turns were sent, so a turn sent before the last one's ending arrived still counts as running. `PromptFailed` is still delivered after such a turn: the caller judges it.
- `disconnect_detail(detail)`: appended to the `Event::Failed` of a lost connection as `<why>: <detail>`. Asked at most once, on the connection's thread, only when that event is due; it may wait a while for the agent's exit.
- Defaults: 20 s `initialize`, 30 s other requests, 2 s close grace; `None` waits without limit. 100000 pending events, then the session ends with `Event::Failed`. `RefuseAll`. `close_on_drop` and `fail_on_disconnect` on.
- `connect` initializes, authenticates, opens the session and sends the first prompt. It errors on the agent's refusal or silence, a protocol version other than 1, or a capability the session needs, such as HTTP MCP servers.

## `Session` (crate root)

Every method takes `&self`, so one thread can read events while another sends.

```rust
pub fn id(&self) -> Option<String>
pub fn extension_active(&self) -> bool
pub fn connection_lost(&self) -> bool
pub fn send(&self, text: impl Into<String>) -> Result<(), Error>
pub fn notify(&self, method: impl Into<String>, params: Value) -> Result<(), Error>
pub fn next_event(&self, timeout: Duration) -> Option<Event>
pub fn cancel(&self) -> Result<(), Error>
pub fn close(&self)
```

`next_event` answers `None` when none came in time or the session has ended. `send` is never retried: after a lost connection its outcome is unknown. `close` waits no longer than the close grace, then runs `on_close`.

Only this session's traffic counts: a `session/update` whose `sessionId` is not this session's is dropped, and a `session/request_permission` naming another session is answered cancelled without asking the decider. Before the session is known nothing is this session's, except that `resume` and `load` know it from the start. That is what an agent kept by a bridge (`coqui-acp-bridge serve --linger`) sends of an earlier connection's session. Request ids are the SDK's UUIDs, so they do not repeat across connections.

## `Event` (crate root)

`#[non_exhaustive]`. `Text(String)`: agent text, gathered until the next tool or the end of the turn, and flushed at 64 KiB. `Tool { id, title, name: Option<String>, kind }`. `Cost { amount: f64, currency }`. `Notice(String)`. `Permission { tool, allowed: bool, reason: Option<String> }`. `Completed { stop }`: `end_turn`, `max_tokens` or `max_turn_requests`. `Failed(String)`: the turn failed, the agent refused, or the connection was lost. `Cancelled`: the agent ended the turn as cancelled, whether asked to or by itself. `Blocked(String)`: a permission request could not be answered, so the agent was told it was cancelled; the turn goes on until the agent ends it. `PromptFailed(String)`: see `turn_end_by`. `OutputLost { lines: u64, bytes: u64 }`: `_dev.boriken.coqui.acp/output_lost`, which a bridge sends first on a connection when it dropped the kept agent's output while nobody was connected; what the session shows has a gap there, so reconcile by `load` or tell the person. `Extension { method, params: Value }`: any other notification whose method starts with `_`, untouched.

## `Decider` (crate root)

```rust
pub trait Decider: Send + Sync { fn decide(&self, request: &PermissionRequest) -> Decision; }
pub enum Decision { Allow, Deny(String) }
pub struct RefuseAll;
```

A decider may block: it runs on a thread of its own. `PermissionRequest` (`#[non_exhaustive]`): `session_id`, `tool_call_id`, `title: Option<String>`, `kind: Option<String>`, `raw_input: Option<Value>`, `options: Vec<PermissionOption>`. `PermissionOption`: `option_id`, `kind`. `raw_input` is what the agent wants to do: untrusted.

## `Exchange` (crate root)

```rust
pub fn Exchange::new() -> Exchange      // also Default
pub fn client_info(self, name: impl Into<String>, version: impl Into<String>) -> Self
pub fn authenticate(self, authenticate: Authenticate) -> Self
pub const fn initialize_timeout(self, limit: Option<Duration>) -> Self
pub const fn request_timeout(self, limit: Option<Duration>) -> Self
pub fn run(
    self,
    transport: impl ConnectTo<sdk::Client> + 'static,
    calls: Vec<(String, Value)>,
) -> Result<Vec<Result<Value, RpcFailure>>, Error>
pub struct Authenticate { pub method: String, pub meta: Map<String, Value> }
pub struct RpcFailure { pub code: i32, pub message: String }
```

Initializes, authenticates when asked, and makes each call in turn: for questions that are no session. Each call has its own result; `code` is `-1` when no answer came in time. Defaults: calls itself `coqui-acp` at this crate's version, nothing to authenticate, 20 s `initialize`, 30 s each call; `None` waits without limit.

## Lines (`coqui_acp::lines`)

```rust
pub const DEFAULT_MAX_LINE: u64 = 16 * 1024 * 1024;
pub type IoLines;       // the transport
pub type Diagnostics = Arc<Mutex<VecDeque<String>>>;
pub fn over_io(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, diagnostic_lines: usize, max_line: u64) -> io::Result<(IoLines, Diagnostics)>
```

A transport over blocking I/O, such as a process's pipes, one JSON-RPC message per line. A line that does not start a JSON value is kept in `Diagnostics`, the last `diagnostic_lines` of them, for the message shown when the peer dies. A line over `max_line` ends the transport.

## Errors

`Error` (`#[non_exhaustive]`): `Refused(String)` in the agent's own words, `Unsupported(String)`, `Timeout { what, limit }` naming the operation and how long it had, `Closed`, `Thread`.

## Gotchas

- The SDK turns on `serde_json`'s `preserve_order` for the whole build.
- ACP version 1 only here; the SDK refuses a peer that negotiates 2. The Dart client speaks both.
- ACP carries no token counts and no usage limits. Cost appears only when the agent reports it.
