# `coqui-acp-secrets` — API header

Header-file-style reference for the `coqui-acp-secrets` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. The wire between the host and the frontend, the sealing and the key bindings are `coqui_acp::secret` (feature `secret` of `coqui-acp`, `docs/secrets.md` of <https://github.com/Boriken-Dev/coqui-acp>); this crate is what the host does with them.

No Cargo features. MSRV Rust 1.89. Dependencies: `coqui-acp` (with `secret`), `serde_json`, `uuid`, `zeroize`, `base64`, `data-encoding`. No `unsafe` (`#![forbid(unsafe_code)]`). Blocking: no async runtime; a tool call blocks its thread until the person answers. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

What it keeps out, and what it does not: a secret value stays out of the model's context, a tool result, a session history or export, and a log. It does not stop an agent that may run a command from reading a value the person lets a command use, or a file the value was written to. So every use is shown to the person in full and needs their yes, and output scrubbing is a backstop for accidents only (a reversed, split, hashed or otherwise transformed value passes it).

## Setup

```rust
pub struct Secrets;                       // Debug (session count only)
impl Secrets {
    pub fn builder(prover: Prover, verifier: Verifier) -> SecretsBuilder;   // the host's own identity; the identities its frontends prove with
    pub fn settings(&self) -> &Settings;
    pub fn open_session(&self, id: &str, config: SessionConfig) -> Result<SessionTools, Error>;   // SessionOpen for an id in use; Path for no roots / a root that is no directory
    pub fn session(&self, id: &str) -> Result<SessionTools, Error>;
    pub fn end_session(&self, id: &str);   // drops (zeroes) every secret and every "for the session" approval; the tools handed out refuse from then on; idempotent
}
// SecretsBuilder: .settings(Settings) .runner(impl Runner) (default ProcessRunner) .clock(impl Fn() -> u64) (Unix seconds; default the system's) .observer(impl Fn(&Event)) .build() -> Secrets
pub struct SessionConfig { pub frontend: Arc<dyn Frontend>, pub roots: Vec<PathBuf> }   // #[non_exhaustive]; SessionConfig::new(frontend, roots)
```

A host calls `end_session` whenever the session ends, however it ends: nothing else clears the store before the `Secrets` is dropped.

## The frontend

```rust
pub trait Frontend: Send + Sync {
    fn identity(&self) -> IdentityId;                 // the only identity whose answers are believed
    fn capability(&self) -> Capability;               // read once at open_session; a method it did not offer has no tool
    fn request(&self, params: &RequestParams) -> Result<RequestReply, Error>;     // _coqui_acp/secret_request
    fn show_open(&self, open: &ShowOpen) -> Result<ShowOpened, Error>;            // _coqui_acp/secret_show_open
    fn show_deliver(&self, deliver: &ShowDeliver) -> Result<ShowDone, Error>;     // _coqui_acp/secret_show_deliver
    fn approve(&self, approval: &Approval) -> Result<Decision, Error>;
}
```

- **Owner-only.** A session has one frontend and nothing else is ever asked; each answer's binding must verify for `identity()` and for the host's own id, so an answer from another attached client is refused (`Error::Secret(UnknownIdentity)`).
- **`approve` is asked for every use and every command- or environment-based show**, whatever the harness's permission mode is. The tools take no mode, so Full access and Bypass cannot answer it, and an implementation must not either. Only an earlier `Decision::Session` for the exact same action on the exact same secret skips the question. A refusal, or no answer, means the action does not run.
- `Approval { session_id, handle: Option<String>, secret: String /* the field */, action: Action }`; `Action::WriteFile { path, mode, mode_applied, overwrite, readable_by_agent }`, `RunCommand { variable, command, cwd }`, `ShowCommand { command, cwd }`, `ShowEnv { name }`. A command is `CommandLine::display()`: one line, quoted, control and bidirectional characters escaped. `Decision { Deny, Once, Session }`. Show the person all of it.
- `Error::Frontend(&'static str)` is the error for a transport failure.

## The tools

`SessionTools` (cheap to clone, `Debug` prints the session and a count):

```rust
pub fn definitions(&self) -> Vec<ToolDefinition>;                  // { name, description, input_schema }; only the tools the capability offers
pub fn call(&self, name: &str, arguments: &Value) -> ToolResult;   // { text: JSON text, is_error }; scrubbed of every secret the session holds
pub fn handle_message(&self, message: &Value) -> Option<Value>;    // MCP over JSON-RPC: initialize, ping, tools/list, tools/call; None for a notification
pub fn handle_line(&self, line: &str) -> Option<String>;
pub fn serve(&self, reader: impl BufRead, writer: impl Write) -> io::Result<()>;   // one message per line until the reader ends
pub fn session_id(&self) -> &str;  pub fn handle_count(&self) -> usize;
```

| Tool | Needs | Arguments | Result |
| --- | --- | --- | --- |
| `secret_request` | `request` | `message`, `schema` (a flat JSON Schema object or its text; at least one `writeOnly` field), `form?` (layout) | `{"status": "accepted", "plain": {..}, "handles": {field: "secret_<hex>"}}`, or `{"status": "declined" \| "cancelled"}` |
| `secret_use` | `request` | `handle` and exactly one of `file: {path, mode?, overwrite?}` or `env: {name, command, cwd?}` | file: `{"status": "written", "path", "readable_by_agent"}`; env: `{"status": "exited" \| "timed_out", "exit_code", "stdout", "stderr", "truncated"}` |
| `secret_show` | `show` | `message`, `schema`, `form?`, `values?` (plain, shown beside), `secrets`: for each secret field exactly one source: `{handle}`, `{file}`, `{env}` or `{command, cwd?}` | `{"status": "shown" \| "declined" \| "cancelled"}` |

A failure is `{"error": "<reason>"}` with `is_error` set; a reason names the rule that refused and holds no value.

- **Files.** Written only under the session's roots and `Settings::write_dirs`, never outside, never through a link (the nearest existing part is resolved and the result checked; `..`, a final link, a path with a control or bidirectional character and a directory that does not exist are refused). The place is resolved again after the person approved and must be the same. Made new unless `overwrite`; mode `Settings::file_mode` (`0o600`) unless `mode` is given within `max_file_mode`, applied on Unix only. `readable_by_agent` is true under the roots: the agent's own tools can read it back.
- **Commands.** `command` is an array (run directly) or a string (the platform shell: `sh -c`, `cmd /C`). Started in `cwd` (under the roots; the first root by default) with the environment cleared except `Settings::inherit_env`, standard input closed, the secret in the one variable `env.name` (a name that changes how programs start, such as `PATH`, `LD_*`, `NODE_OPTIONS`, is refused). Output past `max_output_bytes` is cut after scrubbing. The default `ProcessRunner` kills the child at `command_timeout`, not what it started; a host with a sandbox or terminal supplies its own `Runner`.
- **Show.** The value reaches the person's frontend sealed to a key it made for this request; the tool result says only whether it was shown. A `command` source's output is the value (one trailing line break dropped) and is never returned or logged; a failing command says only that it failed. A `handle` or a `file` under the roots needs no approval; `env` and `command` do.
- **Scrubbing.** In every tool result, the value of every secret of the session and its base64 (standard and URL alphabets, padded or not, and the stable middle of the three alignments inside a longer text), hex (both cases), percent-encoded (both cases, `+` for a space) and JSON-escaped (with `\uXXXX` and `\/` variants) forms are replaced by the secret's handle. Very short secrets make this noisy: it is the person's choice what they type.
- **Request ids** are fresh UUIDs, remembered for `request_memory` (330 s); an id is admitted once and answered once.
- One prompt at a time per session (`Error::Busy`). A handle belongs to its session and lives `handle_lifetime`.

## Settings

`Settings` (`Default`, `#[non_exhaustive]`, fields are public; every one has a safe default) and `non_defaults() -> Vec<NonDefault { setting, value, default }>` for the ones set looser:

`limits` (the extension's form limits), `max_handles` 32, `handle_lifetime` 1 h, `answer_lifetime` 240 s, `request_memory` 330 s, `max_message_bytes` 2048, `max_command_bytes` 8192, `command_timeout` 60 s, `max_output_bytes` 64 KiB, `file_mode` `0o600`, `max_file_mode` `0o700`, `write_dirs` none, `inherit_env` (`PATH`, `HOME`, locale, temp and the Windows system variables), `scrub_min_piece` 8.

## Other items

`Event` (what a host's audit log hears; names handles, fields, paths and commands, never a value), `CommandLine { Argv, Shell }` (`from_value`, `display`), `CommandSpec`, `CommandOutput`, `Runner`, `ProcessRunner`, `Error` (`#[non_exhaustive]`; `Display` of every variant is a fixed reason, `From<coqui_acp::secret::Error>`).

## Gotchas

- Copies a move leaves on the stack, the `serde_json` strings of plain values and the child process's own environment are not zeroed; the store's values and the scrubbing patterns are.
- A command that holds the secret can print it in a form scrubbing does not know; so can a file the agent reads. That is the boundary above, not a bug.
- `Frontend::approve` must reach a person. A host that answers it from a configuration or from its own permission mode has taken the approval away.
- On Windows a file takes its folder's access list; `mode_applied` is false and the approval says so.
