# `coqui-acp-history` — API header

Header-file-style reference for the `coqui-acp-history` crate: every public item with its signature, contract and gotchas, so the crate can be used without reading its source. It ships inside the package and is self-contained. The format itself is specified in `docs/session-history.md` of <https://github.com/Boriken-Dev/coqui-acp>, with shared vectors in `vectors/session_history/` that a Dart or Python reader runs too.

Cargo feature `cli` (off by default) builds the `coqui-acp-history` command and pulls in `clap`. MSRV Rust 1.89. Default dependencies: `agent-client-protocol` (for ACP's `StopReason` and `ToolKind`), `guatiao` (settings), `regex`, `serde`, `serde_json`, `sha2`, `data-encoding`. Version `0.0.0-alpha.0`, not published; the API may change in any release before 1.0.

Imports: everything is at the crate root except the tool table, `coqui_acp_history::tools`. `StopReason` is ACP's (`agent_client_protocol::schema::v1`), re-exported.

No `unsafe` code (`#![forbid(unsafe_code)]`). No process-wide state. Text only: the crate reads and writes UTF-8 JSON Lines; it parses no harness's own files (the Claude and Codex codecs live in their crates).

Dialect: strict UTF-8; one leading byte-order mark ignored on line 1; a repeated object key, a carriage return, an empty line and a missing final line feed are refused; a number outside `i64`/`u64` is read as `f64`; nesting stops at 128 levels whatever `max_depth` is.

## The format in brief

Line 1 is the header (`format` `coqui-acp-history`, `version` 1, `id` the host's session UUID, `origin.harness` and optionally `origin.version`, `origin.sessionId`, `createdAt`, `model`, `mode`, `effort`, `cwd`, `acpVersion`, `digest`, `redacted`, `_meta`). Each body line has a `method`: `session/prompt`, `session/update`, `session/cancel`, `session/request_permission`, `history/permission_answer`, `history/turn_end` (stop reason is ACP's, or `error`), `history/compaction` (optional `summary`, `trigger`, `preTokens`). A line may carry `at` and `_meta`. A prompt opens a turn and is refused while one is open; a turn end, cancel, permission request and answer need an open turn; updates and compactions may appear anywhere.

## Reading

```rust
pub struct Reader<R: Read>;                          // Debug; Iterator<Item = Result<Entry, HistoryError>>
pub fn Reader::builder(source: R) -> ReaderBuilder<R>;
// ReaderBuilder: .settings(ReaderSettings) .bounds(Bounds) .verify_digest(bool) .open() -> Result<Reader<R>, HistoryError>
pub fn Reader::header(&self) -> &Header;
pub fn Reader::header_raw(&self) -> &str;             // line 1 as the file has it
pub fn Reader::summary(&self) -> &Summary;            // what has been read so far
pub fn Reader::next_entry(&mut self) -> Result<Option<Entry>, HistoryError>;
pub fn Reader::copy_to<W: Write>(self, sink: W) -> Result<Summary, HistoryError>;   // lines verbatim, after checking each
#[non_exhaustive] pub struct Entry { pub number: u64, pub line: Line, pub raw: String }
```

`open` reads and checks the header (nothing before it performs I/O). It holds at most one line plus the one being returned. After any error `next_entry` returns `Ok(None)`. A digest in the header is checked when the end is reached, and a mismatch is the error that replaces the `None`. `Document::from_reader`, `from_slice` and `str::parse::<Document>()` read all of a history with the default settings.

## Writing

```rust
pub struct Writer<W: Write>;
pub fn Writer::builder(sink: W) -> WriterBuilder<W>;  // .settings(WriterSettings) .bounds(Bounds) .start(&Header) -> Result<Writer<W>, HistoryError>
pub fn Writer::write(&mut self, line: &Line) -> Result<(), HistoryError>;
pub fn Writer::summary(&self) -> &Summary;
pub fn Writer::body_digest(&self) -> String;          // "sha256:<hex>" of the body so far
pub fn Writer::finish(self) -> Result<(W, Summary), HistoryError>;
```

`start` writes the header (a header carrying a digest is written as given; `Document::seal` makes one). `write` serialises the line, re-reads it with the reader's own checks (shape, order, bounds) and writes it only if they pass; a refused line writes nothing and the writer goes on, but a sink failure poisons it. The output is in canonical spelling: compact, header keys in table order, `method` `params` `at` `_meta` in lines, every other object's keys sorted. Reading and writing a document gives that spelling; use `Reader::copy_to` to keep a file byte for byte.

```rust
#[non_exhaustive] pub struct Document { pub header: Header, pub lines: Vec<Line> }
impl Document { fn new(Header); fn from_reader(Reader<R>); fn from_slice(&[u8]); /* FromStr */
                fn check(&self) -> Result<Summary, HistoryError>; fn to_writer(&self, W); fn to_vec(&self); fn to_string(&self);
                fn seal(&mut self) -> Result<(), HistoryError>; fn seed(&self) -> Seed<'_>; }
```

## Types

```rust
#[non_exhaustive] pub struct Header { pub id: String /*UUID*/, pub origin: Origin, pub created_at, model, mode, effort, cwd: Option<String>,
    pub acp_version: Option<u64>, pub digest: Option<String>, pub redacted: Option<Redacted>, pub meta: Option<serde_json::Value> }
fn Header::new(id: impl Into<String>, origin: Origin) -> Header;  fn Header::to_line(&self) -> Result<String, HistoryError>;
#[non_exhaustive] pub struct Origin { pub harness: String, pub version: Option<String>, pub session_id: Option<String> }  // Origin::new(harness)
#[non_exhaustive] pub struct Line { pub at: Option<String>, pub meta: Option<Value>, pub body: Body }
// Line::new(Body), prompt(Vec<Value>), text_prompt(&str), update(Value), turn_end(Stop), compaction(summary), .at(..), .meta(..), to_json()
#[non_exhaustive] pub enum Body { Prompt { prompt: Vec<Value> }, Update { update: Value }, Cancel,
    PermissionRequest { tool_call: Value, options: Vec<Value> }, PermissionAnswer { tool_call_id: String, outcome: Value },
    TurnEnd { stop: Stop, error: Option<String> }, Compaction { summary: Option<String>, trigger: Option<Trigger>, pre_tokens: Option<u64> } }
#[non_exhaustive] pub enum Stop { Acp(StopReason), Error }       // Stop::name(), Stop::from_name()
#[non_exhaustive] pub enum Trigger { Auto, Manual }
pub const FORMAT: &str; pub const VERSION: u64; pub const NATIVE_META_KEY: &str /* "dev.boriken.history/native" */; pub const ATTACHMENT_META_KEY: &str;
#[non_exhaustive] pub struct Summary { pub lines, bytes, turns, updates, compactions: u64, pub last_boundary, last_summary: Option<Boundary>, pub last_stop: Option<Stop>, pub open_turn: bool }
```

Payloads (`prompt` blocks, `update`, `tool_call`, `options`, `outcome`, `_meta`) are `serde_json::Value`: ACP's shapes are not re-declared here, so a v1 or v2 update passes as long as it is an object with a `sessionUpdate`. The checks a line gets are listed in the format document.

## Seeding

```rust
pub fn Document::seed(&self) -> Seed<'_>;
#[non_exhaustive] pub struct Seed<'a> { pub summary: Option<&'a str>, pub point: Option<SeedPoint>, pub lines: &'a [Line], pub first_line: u64, pub mid_turn: bool }
pub fn Seed::turns(&self) -> u64;  pub fn Seed::fit(&self, max_bytes: u64) -> Fit<'a>;
#[non_exhaustive] pub struct SeedPoint { pub line: u64, pub digest: String }   // guatiao ToValue/FromValue/Schema, Serialize
#[non_exhaustive] pub struct Fit<'a> { pub summary: Option<&'a str>, pub lines: &'a [Line], pub first_line: u64, pub dropped_turns: u64, pub bytes: u64, pub complete: bool }
```

The seed starts at the latest `history/compaction` that has a summary (else at the first body line); `point.digest` is the SHA-256 of that line in canonical spelling. `fit` keeps the summary and the latest whole turns within `max_bytes` (each line as `Line::to_json` writes it, plus a line feed); divide a token window by your own bytes-per-token figure.

## Redaction

```rust
pub fn Redactor::try_new(settings: &RedactionSettings) -> Result<Redactor, HistoryError>;   // Pattern { index } for a bad, huge or empty-matching pattern
pub fn Redactor::header(&mut self, &mut Header) -> Result<(), HistoryError>;
pub fn Redactor::line(&mut self, number: u64, &mut Line) -> Result<(), HistoryError>;
pub fn Redactor::document(&mut self, &mut Document) -> Result<RedactionReport, HistoryError>;   // marks header.redacted, drops the digest
pub fn Redactor::report(&self) -> &RedactionReport;   pub fn Redactor::mark(&self) -> Redacted;
#[non_exhaustive] pub struct RedactionReport { pub replaced: u64, pub lines: u64, pub rules: Vec<RuleReport> }
#[non_exhaustive] pub struct RuleReport { pub kind: RuleKind /*Value|Pattern*/, pub index: usize, pub replaced: u64, pub lines: u64 }
```

Off unless a caller builds a `Redactor`. Strings are changed everywhere in the header (not `format`, `id`, `createdAt`, `digest`, `origin.harness`) and the payloads, except `sessionUpdate`, `toolCallId` and `optionId`, which pair lines. A removal that would leave a line the format refuses (for example a pattern matching `selected`) is an error and leaves the line and the document as they were. The report and the redactor's `Debug` never hold a value or a pattern, only its position. `document` marks `header.redacted` and drops `header.digest`; call `Document::seal` after.

## Loss report

```rust
#[non_exhaustive] pub struct Loss { pub kind: String, pub count: u64, pub detail: Option<String> }   // guatiao ToValue/FromValue/Schema
pub struct LossReport;   // add(kind, count, detail), losses(), count(kind), is_empty(), IntoIterator, Extend/FromIterator<Loss>
pub const THINKING, TOOL_AS_NOTE, ATTACHMENT_BY_DIGEST, REPLAYED_AS_PROMPT, SUMMARY_MISSING: &str;
```

`Loss` is shaped for the `acp_harness` kind's `ImportReport.losses`; an importer fills it, nothing in this crate does. `detail` never holds lost content. The kinds are an open vocabulary.

## Tool table

```rust
#[non_exhaustive] pub enum Tool { Read, Write, Edit, Patch, Shell, Grep, Glob, Fetch }    // Tool::ALL, name(), kind() -> ACP ToolKind, from_name()
pub fn Tool::natives(self) -> &'static [Native];  pub fn Tool::native(self, harness: &str) -> Option<&'static Native>;
pub fn Tool::from_native(harness: &str, name: &str) -> Option<Tool>;
#[non_exhaustive] pub struct Native { pub harness: &'static str, pub name: &'static str, pub keys: &'static [(&'static str, &'static str)] }  // (normalised, native)
pub fn tools::normalise(harness: &str, name: &str, input: &Value) -> Option<(Tool, Value)>;
pub fn tools::denormalise(tool: Tool, harness: &str, input: &Value) -> Option<(&'static str, Value)>;
```

Harness names are `claude`, `codex`, `agy`. The functions rename input keys only; a change of shape is the codec's. agy's input keys are unverified.

## Settings (Guatiao values)

All `#[non_exhaustive]`, `Default`, public fields, unknown keys refused, `ToValue + FromValue + Schema + Serialize`.

| Type | Key | Default |
| --- | --- | --- |
| `Bounds` | `max_line_bytes` | 16777216 |
| | `max_lines` | 5000000 |
| | `max_total_bytes` | 4294967296 |
| | `max_depth` | 64 |
| `ReaderSettings` | `bounds`, `verify_digest` | `Bounds` default, `true` |
| `WriterSettings` | `bounds` | `Bounds` default |
| `RedactionSettings` | `values` (sensitive), `patterns`, `replacement` | `[]`, `[]`, `[redacted]` |

`non_defaults<T: Serialize + Default>(&T) -> Vec<NonDefault { setting, value, default }>` lists, by key path, every setting not at its default; a redaction's `values` are named and shown as `[not shown]`. Precedence for the command: its flags over the defaults; the library reads no environment variable and no file.

## Errors

`HistoryError` (`#[non_exhaustive]`, `Display` one lowercase clause quoting nothing from the document): `Io`, `Empty`, `NotUtf8`, `BlankLine`, `LineEnding`, `Truncated`, `Syntax { line, column }`, `DuplicateKey`, `Shape { line, pointer, expected }`, `UnknownField { line, pointer }`, `UnknownFormat`, `UnknownVersion { version }`, `UnknownLine`, `Order { line, rule: OrderRule }`, `TooLong`, `TooManyLines`, `TooLarge`, `TooDeep`, `DigestMismatch`, `Pattern { index }`. `code() -> &'static str` is the stable name the vectors use, `line()` and `pointer()` the place.

## Command (`--features cli`)

`coqui-acp-history list [DIR]`, `show FILE`, `convert IN -o OUT [--redact-pattern RE]... [--redact-values-file FILE] [--replacement TEXT] [--seal]`, `export --harness H SESSION` and `import --harness H FILE` (exit status 2: they need a harness's codec), and `--max-line-bytes`, `--max-lines`, `--max-total-bytes`, `--max-depth` on the first three. `convert` with no redaction option copies the checked lines verbatim and writes the output whole or not at all (`OUT.part`, then renamed). A secret to remove is read from a file, never an argument. A bound looser than its default is reported on standard error. Exit status 1 for a refused history, 2 for a usage error.
