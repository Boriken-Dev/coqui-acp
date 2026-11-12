# `coqui-acp-secrets` — API header

Header-file-style reference for the `coqui-acp-secrets` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. The wire between the host and the frontend, the sealing and the key bindings are `coqui_acp::secret` (feature `secret` of `coqui-acp`, `docs/secrets.md` of <https://github.com/Boriken-Dev/coqui-acp>); this crate is what the host does with them.

No Cargo features. MSRV Rust 1.89. Dependencies: `coqui-acp` (with `secret`), `serde_json`, `uuid`, `zeroize`, `base64`, `data-encoding`, `unicode-normalization`, and `rustix` (Unix) or `win32job` (Windows) to end a command's whole tree. No `unsafe` (`#![forbid(unsafe_code)]`). Blocking: no async runtime; a tool call blocks its thread until the person answers. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

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
- **A use is a permission, and the session's mode decides.** `Frontend::mode()` (default `Mode::Ask`) is read at every use. In `Mode::Bypass` (Full access) a use is approved without asking (`Event::Bypassed`); in any other mode `approve` is asked, with allow once or allow for the session, as on a permission card. A host with a permission decider answers `mode()` from the same one. Only an earlier `Decision::Session` for the exact same action on the exact same secret skips the question. A refusal, or no answer, means the action does not run. **A session allowance and Bypass both trust the contents of the folder the command runs in**: the scripts, hooks and configuration there belong to the agent, which can change them, and whatever the command runs from there can read the value. Every approval of a command carries `notice: Some(FOLDER_NOTICE)`; show it.
- `Approval { session_id, handle: Option<String>, secret: String /* the field */, notice: Option<&'static str>, action: Action }`; `Action::WriteFile { path, mode, mode_applied, overwrite, under_roots }`, `RunCommand { variable, command, program, target, cwd }`, `ShowCommand { command, program, target, cwd }`, `ShowEnv { name }`. `program` is the program that will start, by full path (for shell text the platform shell), found on a search path with no relative entry and none under the session's roots; a program named by a path is shown as the file it is. The program is started under the name it was found by, never as its link target (`cargo` is not `rustup`); `target` is what it links to, when that is another file. On Windows a name takes its `PATHEXT` file before the extensionless one, and app execution aliases are allowed. The secret's field name, the program and the target are shown with hidden characters written out (a resolved path that has one is refused), and `\` and `<` are escaped in a command, so two commands that read alike are two; "for the session" is keyed on the command as given. A command is `CommandLine::display()`: one line, quoted; control, format (Cf), separator (Zl, Zp) and non-ASCII whitespace characters are written as `\u{..}` and a run of more than three spaces as `<N spaces>`. `Decision { Deny, Once, Session }`. Show the person all of it. `under_roots` is true when the file is under the session's roots, where the host's agent is assumed to read and change files with its own tools: the assumption is the host's, not checked here.
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
| `secret_use` | `request` | `handle` and exactly one of `file: {path, mode?, overwrite?}` or `env: {name, command, cwd?}` | file: `{"status": "written", "path", "under_roots"}`; env: `{"status": "exited" \| "timed_out", "exit_code", "stdout", "stderr", "truncated"}` |
| `secret_show` | `show` | `message`, `schema`, `form?`, `values?` (plain, shown beside), `secrets`: for each secret field exactly one source: `{handle}`, `{file}`, `{env}` or `{command, cwd?}` | `{"status": "shown" \| "declined" \| "cancelled"}` |

A failure is `{"error": "<reason>"}` with `is_error` set; a reason names the rule that refused and holds no value.

- **Files.** Written only under the session's roots and `Settings::write_dirs`, never outside, never through a link (the nearest existing part is resolved and the result checked; `..`, a final link, a path with a control or bidirectional character and a directory that does not exist are refused). Only drive paths on Windows (no network share, device or other verbatim path), checked as text against the roots before any file is touched. The place is resolved again after the person approved and must be the same. A new file is made exclusively; an existing one (`overwrite` required) is replaced by renaming a new file over it, never written in place, so a file it is a hard link of keeps its contents; mode `Settings::file_mode` (`0o600`) unless `mode` is given within `max_file_mode`, applied on Unix only. `under_roots` is true under the roots: the agent's own tools can read it back.
- **Commands.** `command` is an array (run directly) or a string (the platform shell: `sh -c`, `cmd /C`). Started by full path in `cwd` (under the roots; the first root by default) with the environment cleared except `Settings::inherit_env` (`PATH` is rebuilt without relative entries or entries under the roots, and Windows is told not to look in the current directory), standard input closed, the secret in the one variable `env.name` (refused when it is on `Settings::denied_variables`, a small default of names that change how programs start; the person extends it, and it is no catalogue). The raw output is cut at `max_output_bytes`; when it was cut, the end is moved back to the start of anything that could begin a form of a secret, and what is kept is scrubbed, so no piece of a secret that straddles the cut is left. The default `ProcessRunner` ends the child and what it started when the command ends or at `command_timeout` (a process group on Unix, a job object on Windows), and starts Windows' shell as `cmd /D /C`; a host with a sandbox or terminal supplies its own `Runner`.
- **Show.** The value reaches the person's frontend sealed to a key it made for this request; the tool result says only whether it was shown. A `command` source's output is the value (one trailing line break dropped) and is never returned or logged; a failing command says only that it failed. A `handle` or a `file` under the roots needs no approval; `env` and `command` do.
- **Scrubbing.** In every tool result, the value of every secret of the session and its base64 (standard and URL alphabets, padded or not, and the stable middle of the three alignments inside a longer text), hex (both cases), percent-encoded (both cases, `+` for a space) and JSON-escaped (with `\uXXXX` and `\/` variants) forms are replaced by the secret's handle. Also found: base64 and hex wrapped over lines or spaces, hex in mixed case, and the secret's NFC and NFD spellings. Very short secrets make this noisy: it is the person's choice what they type. An agent that may run a command can ask it whether a guess of the value is right (a guessing oracle); scrubbing is no defence against that.
- **Request ids** are fresh UUIDs, remembered for `request_memory` (330 s); an id is admitted once and answered once.
- One prompt at a time per session (`Error::Busy`). A handle belongs to its session and lives `handle_lifetime`.

## Settings

`Settings` (`Default`, `#[non_exhaustive]`, fields are public; every one has a safe default) and `non_defaults() -> Vec<NonDefault { setting, value, default }>` for the ones set looser:

`limits` (the extension's form limits), `max_handles` 32, `handle_lifetime` 1 h, `max_values_bytes` 64 KiB, `answer_lifetime` 240 s, `request_memory` 330 s, `max_message_bytes` 2048, `max_command_bytes` 8192, `command_timeout` 60 s, `max_output_bytes` 64 KiB, `file_mode` `0o600`, `max_file_mode` `0o700`, `write_dirs` none, `inherit_env` (`PATH`, `HOME`, locale, temp and the Windows system variables), `denied_variables` (names, or prefixes ending `*`), `scrub_min_piece` 8.

## Other items

`Event` (what a host's audit log hears; names handles, fields, paths and commands, never a value), `CommandLine { Argv, Shell }` (`from_value`, `display`), `CommandSpec`, `CommandOutput`, `Runner`, `ProcessRunner`, `Error` (`#[non_exhaustive]`; `Display` of every variant is a fixed reason, `From<coqui_acp::secret::Error>`).

## Gotchas

- Copies a move leaves on the stack, the `serde_json` strings of plain values and the child process's own environment are not zeroed; the store's values and the scrubbing patterns are.
- A command that holds the secret can print it in a form scrubbing does not know; so can a file the agent reads. That is the boundary above, not a bug.
- `Frontend::approve` must reach a person; the permission mode belongs in `Frontend::mode`, which is where it may answer.
- A value copied for a use is not used once the session has ended. One use or prompt at a time per session (`Error::Busy`). Values are zeroed where this crate holds them, not in copies the runtime or a child's environment make.
- On Windows a file takes its folder's access list; `mode_applied` is false and the approval says so.
