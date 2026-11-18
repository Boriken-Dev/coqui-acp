# `coqui-acp-hub` — API header

Header-file-style reference for the `coqui-acp-hub` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

One Cargo feature, off by default: `serde` (`Serialize` and `Deserialize` for `Settings`, every field defaulted). MSRV Rust 1.89. Depends on `serde_json` only. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: everything is at the crate root. No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state, no I/O, no threads, no timers, no async runtime.

## What it is

A `Hub` holds the agent side of **one** ACP session that is already open (initialized, authenticated, `session/new` or `session/load` done by the host), independent of any client, and serves it to any number of clients that attach and detach. It is a state machine over JSON-RPC messages (`serde_json::Value`): the host gives it what arrives from the agent and from each client, and sends the returned `Output`s over its own links. It speaks to clients as an ACP agent and to the agent as an ACP client, so it works for ACP v1 and v2 alike: it parses only the envelope and the few fields below, and passes every other field through. A client must speak the version the agent speaks (the hub answers `initialize` with the agent's own result).

It never ends a session and never answers a permission request on its own. Detaching, or having no client at all, leaves the agent running. One hub per session; a host with many sessions holds many hubs. A hub is `Send` but takes `&mut self`: wrap it in a mutex to drive it from several threads.

## `Hub`

```rust
pub fn Hub::builder(session: impl Into<String>) -> Builder
// Builder: .initialize_result(Value) .settings(Settings) .policy(Arc<dyn Policy>)
//          .max_log_entries(usize) .max_log_bytes(usize) .max_queued_prompts(usize)
//          .cancel_drops_queue(bool) .permission_window_ms(Option<u64>) .build() -> Hub

pub fn connect(&mut self, client: Client) -> Result<(), HubError>
pub fn disconnect(&mut self, client: &ClientId) -> Result<Vec<Output>, HubError>
pub fn from_client(&mut self, client: &ClientId, message: &Value) -> Result<Vec<Output>, HubError>
pub fn from_agent(&mut self, message: &Value) -> Vec<Output>
pub fn agent_closed(&mut self) -> Vec<Output>
pub fn poll(&mut self, now_ms: u64) -> Vec<Output>
pub fn decide(&mut self, request: u64, result: Value) -> Result<Vec<Output>, HubError>

pub fn session_id(&self) -> &str
pub fn settings(&self) -> &Settings
pub fn attached_count(&self) -> usize
pub fn attached(&self) -> impl Iterator<Item = &Client>
pub fn last_activity(&self) -> u64
pub fn head(&self) -> Cursor
pub fn log_len(&self) -> usize
pub fn queued_prompts(&self) -> usize
pub fn turn_running(&self) -> bool
pub fn ended(&self) -> bool
pub fn pending_permissions(&self) -> impl Iterator<Item = &PermissionRequest>
```

- **Driving it.** `connect` when a client's link is established (the policy's `Action::Attach` is checked here, and an identity already connected is `HubError::Duplicate`); `from_client` for every message that client sends; `disconnect` when its link ends. `from_agent` for every message from the agent; `agent_closed` when its connection ends. Each returns `Output`s to send, in order.
- **`initialize_result`**: the agent's `initialize` result, which the hub answers every client's `initialize` with, adding `agentCapabilities.loadSession: true` and `agentCapabilities.sessionCapabilities.resume` (v1 names) since the hub provides both whatever the agent does. Without it a minimal `{ protocolVersion: 1, agentCapabilities: {} }`.
- **Idle and lifecycle (the host's).** `attached_count()` and `last_activity()` are what a host decides to end a session from; the hub has no TTL and never reaps itself. `last_activity` is the hub's clock when anything last arrived (from the agent or a client, a connect or a disconnect). The hub has no clock of its own: `poll(now_ms)` sets it (any monotonic millisecond origin; it never goes backwards), so call `poll` with the time before feeding an input. `poll` also returns what became due (below).
- **Settings** (`Settings`, `#[non_exhaustive]`, `Default`; every value is taken as given, none has a floor): `max_log_entries` 100000, `max_log_bytes` 64 MiB (serialized), `max_queued_prompts` 32, `cancel_drops_queue` false, `permission_window_ms` None. `Settings::non_defaults() -> Vec<NonDefault { setting, value, default }>` lists what differs, for the host to show.

## Client identity

```rust
pub struct ClientId(String);          // From<String>, From<&str>, Display, as_str(), Ord, Hash
#[non_exhaustive] pub struct Client { pub id: ClientId, pub label: String, pub role: String, pub subject: Option<String> }
pub fn Client::new(id: impl Into<ClientId>) -> Client          // label = id, role = "client", subject = None
pub fn label(self, impl Into<String>) -> Self; pub fn role(self, impl Into<String>) -> Self; pub fn subject(self, impl Into<String>) -> Self
```

All of it is given by the host and carried unchecked: `id` is stable and opaque and is what every action is attributed to (it is the same on every reconnect when the host keeps it so); `label` is for a person to read; `role` is a kind tag the host's policy distinguishes (`operator`, `station`, `runner`); `subject` is the person or compartment the host maps the client to. The hub shows `{id, label, role}` to the other clients (presence), never `subject`, and hands the whole `Client` to the policy; it never derives identity from a message's contents.

## Cursor

```rust
pub struct Cursor(u64);              // Copy, Ord, Default, Display (decimal), FromStr (CursorError), Cursor::START
```

Opaque, monotonic, stable across reconnects, never a byte offset: one is issued per logged message, in the order they were fanned out, and replay from the start and from a cursor deliver the same messages in the same order (the latter is the tail of the former). Clients treat it as an opaque string. It travels as `params._meta["dev.boriken.coqui.acp/hub"].seq` (decimal string) on every logged message and is `result._meta[...].cursor` (the log's head) in the answer to `session/load` and `session/resume`. To resume from one, a client puts `params._meta["dev.boriken.coqui.acp/hub"].cursor` in its `session/load` or `session/resume` request: the hub replays everything after it. `META_KEY` is that key. A hub restarted without its log issues cursors from 1 again; a cursor from the earlier log is then refused (below), never replayed from. Keeping the log across a restart: "Keeping the log".

## Keeping the log

```rust
pub struct LogSnapshot { pub head: Cursor, pub entries: Vec<Value> }     // Clone, Debug, PartialEq
pub fn LogSnapshot::new(head: Cursor, entries: Vec<Value>) -> Self
pub fn LogSnapshot::to_json(&self) -> Value                              // { "head": "<cursor>", "entries": [...] }
pub fn LogSnapshot::from_json(&Value) -> Result<LogSnapshot, SnapshotError>
#[non_exhaustive] pub enum SnapshotError { NoCursor(usize), OutOfOrder(usize), PastHead(usize), Malformed(&'static str) }   // Display, Error

pub fn Hub::log_snapshot(&self) -> LogSnapshot
pub fn Hub::log_after(&self, cursor: Cursor) -> impl Iterator<Item = &Value>
pub fn Builder::restore_log(self, LogSnapshot) -> Result<Builder, SnapshotError>
```

The hub keeps no file: the host does. `log_snapshot` is the whole kept log (every entry exactly as it was fanned out, with its `seq`, and the head cursor, which can be past the last entry when the bounds dropped it). A host that writes after every call that returned outputs appends `log_after(<cursor it last wrote>)` instead of rewriting everything. `Builder::restore_log` starts a hub with that log: its head is the snapshot's, so the next update is the one after it and every cursor a client holds stays valid; prompt numbers go on after the highest kept. The settings' bounds are applied to what is restored (a smaller `max_log_entries` drops the oldest at once). It is refused when an entry has no `seq`, is not after the one before it, or is past `head`. Only the log is restored: connected clients, the running turn, the queue and open permission requests are not, since they belong to links and an agent turn that did not survive the restart.

A load or resume naming a cursor the log cannot continue from is a JSON-RPC error (code -32602) and attaches nothing, never a partial or shifted replay. `error.data.reason` says which: `cursor_ahead` (the cursor is past the head: it came from a log that is gone; `head`), `cursor_expired` (the entries after it were dropped by the bounds; `oldest`, the first kept, and `head`) or `bad_cursor` (not a cursor). The client may then load again without a cursor, which replays everything kept and says `truncated` when the bounds dropped the oldest.

## Client side: what a client sends

| Message | Hub's answer |
| --- | --- |
| `initialize` | the agent's result, with `loadSession` and `sessionCapabilities.resume` added |
| `authenticate` | `{}` (the host already decided who connects) |
| `session/new` | error: this connection serves one running session |
| `session/load` | attach; replay everything after `_meta[..].cursor`, or all retained when none; then the result `{ _meta: { <META_KEY>: { cursor, truncated } } }`; then every open permission request the client may answer; then presence. Replayed and live updates are the same messages |
| `session/resume` | the same, but replays only when a cursor is given |
| `session/prompt` | policy `Action::Prompt`; error when the session id is not this hub's, the queue is full, or the session has ended. Otherwise echoed to the other attached clients and logged as `user_message_chunk` updates (one per content block) stamped `_meta[..] = { client, label, prompt }` (the sender sees no echo of its own prompt live, but replays include it), then forwarded to the agent when no turn runs (with `_meta[..] = { client, prompt }` merged into the request), else queued in arrival order. The sender gets the agent's answer under its own request id when its turn ends |
| `session/cancel` (notification) | policy `Action::Cancel`; forwarded to the agent when a turn runs; with `cancel_drops_queue` also answers the queued prompts `stopReason: "cancelled"` |
| any other request | policy `Action::Request { method }`; forwarded to the agent under a hub id, the answer returned to the asker (`session/set_mode`, `session/set_config_option`, extensions) |
| response `hub-perm-<n>` | an answer to a permission request (below) |
| response to a request the agent made | passed to the agent under the agent's own id |

Before attaching, only `initialize` and `authenticate` are answered; other requests get an error and the hub never forwards a message of an unattached client. `truncated: true` means the bounds dropped the oldest entries of a replay that named no cursor, so it starts mid-session (tell the person); a cursor the bounds passed is an error instead ("Keeping the log"). The session id in `session/load`, `session/resume` and `session/prompt` must be the hub's.

## Agent side: what the agent sends

- Any notification (`session/update` and extension notifications): logged with a cursor and sent to every attached client unchanged but for the `_meta` stamp `seq`.
- `session/request_permission`: see below.
- Any other request (`fs/*`, `terminal/*`, `elicitation/create`): put to the client whose prompt is running, when attached; its answer returns to the agent. With none, the agent gets an error. (Not fanned out: only permissions are.)
- The response to the running prompt: sent to its sender, then `_dev.boriken.coqui.acp/hub_turn_ended` is logged and fanned out to the others, then the next queued prompt starts.
- `agent_closed()`: every unanswered prompt gets an error response, pending permissions are dropped, `_dev.boriken.coqui.acp/hub_ended` is logged and fanned out, `Note::Ended` is returned and later prompts are refused. The host decides what to do with the hub.

## Permissions

The agent's request is put to every attached client the policy allows to answer (`Action::Answer`), as a `session/request_permission` request with id `hub-perm-<n>` (the agent's params untouched, `_meta[..].request` = `n`). A client that attaches later is asked what is still open. The first valid answer wins: an outcome of `cancelled`, or `selected` with an `optionId` among the request's `options` (when it lists any; v2's shape without `options` takes any id). It goes to the agent under the agent's id, `_dev.boriken.coqui.acp/hub_permission_closed` `{ sessionId, requestId, by, outcome }` is logged and fanned out so the others close theirs, and `Note::PermissionAnswered { request, by }` returns for audit (`Answerer::Client(id)`, `Host`, or `Declined`). A late or unaddressed answer is `Note::Ignored`; an invalid one is ignored and the request stays open.

A client's `cancelled` answer (or an error response) is that client declining, not an answer for everyone: when every attached client the request was put to has declined, the agent is told `cancelled` (`Answerer::Declined`). With nobody attached a request waits for as long as it takes. `Settings::permission_window_ms`: when set, `poll` returns `Note::PermissionUnanswered { request, waiting_ms }` once for each request that waited that long; it stays open, and the host may `decide(request, result)` it (`result` is the agent's `RequestPermissionResponse`) or leave it. The hub never answers on its own.

## `Policy`

```rust
pub trait Policy: Send + Sync { fn check(&self, client: &Client, action: &Action<'_>) -> Verdict; }  // also any matching closure
#[non_exhaustive] pub enum Action<'a> { Attach, Prompt, Cancel, Request { method: &'a str }, Answer { request: &'a PermissionRequest } }
pub enum Verdict { Allow, Deny(String) }
pub struct AllowAll;   // the default
```

A denial reaches the client as a JSON-RPC error (code -32000, the reason as its message) for a request, and `Note::Refused { client, action, reason }` for the host; `connect` returns `HubError::Refused`. The policy is asked about `Answer` both when a request is put to a client and when it answers, so a policy that changes its mind takes effect.

## `Output`, `Note`

```rust
#[non_exhaustive] pub enum Output { ToClient { client: ClientId, message: Value }, ToAgent(Value), Note(Note) }
#[non_exhaustive] pub enum Note {
    Refused { client, action: String, reason: String }, Ignored { client: Option<ClientId>, reason: String },
    PermissionAnswered { request: u64, by: Answerer }, PermissionUnanswered { request: u64, waiting_ms: u64 }, Ended,
}
#[non_exhaustive] pub enum Answerer { Client(ClientId), Host, Declined }
#[non_exhaustive] pub struct PermissionRequest { pub id: u64, pub params: Value, pub asked_at: u64 }
#[non_exhaustive] pub enum HubError { Refused(String), Duplicate(ClientId), UnknownClient(ClientId), UnknownRequest(u64), InvalidAnswer }
```

A `ToClient` for a client that is no longer connected is not produced. Send `ToClient`/`ToAgent` in order: a `session/load` answer comes after the replay it follows.

## Extension messages (all under the coqui-acp key, none a new ACP field)

Constants: `META_KEY` = `dev.boriken.coqui.acp/hub`; `PRESENCE` = `_dev.boriken.coqui.acp/hub_presence` (to every attached client when one attaches or detaches: `{ sessionId, clients: [{ id, label, role }] }`, not logged); `TURN_ENDED` `{ sessionId, prompt, client, stopReason, error }`, `PERMISSION_CLOSED`, `ENDED` (logged and fanned out). Everything the hub adds to a standard message is in `params._meta[META_KEY]`: `seq`, and on a prompt echo or forwarded prompt `client`, `label`, `prompt` (the hub's number for the prompt).

## Bounds

The log keeps `max_log_entries` entries and `max_log_bytes` serialized bytes, oldest first out. The queue holds `max_queued_prompts`. Pending permission requests, connected clients and routed requests are not bounded by the hub: a host that faces peers it does not trust bounds clients itself (`Action::Attach`) and the agent's requests by its own means.
