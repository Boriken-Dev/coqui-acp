# Session history format

One file format for the history of an Agent Client Protocol session, so a session can move between machines and between harnesses (Claude Code, Codex, Antigravity, any ACP agent). Version 1.

The reference implementation is the crate [`coqui-acp-history`](https://github.com/Boriken-Dev/coqui-acp/tree/main/crates/coqui-acp-history). The shared test vectors in `vectors/session_history/` are normative with this document: a reader in any language is correct when it passes them.

## What a history is

A history is the record of one session, in the order it happened: what the client prompted, what the agent sent back, what was cancelled, which permissions were asked and how they were answered, where each turn ended and why, and where the harness folded the conversation into a summary (a compaction). It is written in ACP's own vocabulary: a prompt holds ACP content blocks, an update is an ACP `session/update` notification, a tool call is ACP's `toolCallId`, `kind`, `locations`, `rawInput` and `rawOutput`. This format adds only the envelope around them.

A history is context, not commands. A tool call in it records what happened; an importer never runs it again. A history comes from another machine or another person, so a reader treats all of it as untrusted input.

## The file

- UTF-8 text. One leading byte-order mark is ignored; a writer never writes one.
- JSON Lines: each line is one JSON value that is an object, and each line, the last included, ends with a line feed (U+000A). A carriage return anywhere in the file, an empty line, or a file whose last byte is not a line feed is refused (the last rule is how a cut-short file shows).
- Line 1 is the header. Every other line is a body line. Line numbers count from 1, the header being line 1.
- In every object, a key repeated is refused: two readers would resolve it differently.
- Numbers are JSON numbers. A reader keeps one that does not fit a signed or unsigned 64-bit integer as a double.
- A conventional name is `<id>.jsonl`, where `<id>` is the header's `id`.

## The header

Line 1, an object with these keys and no others. A reader checks `format` first, then `version`, then the rest, so a future version is refused as an unknown version whatever else its first line holds.

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `format` | string | yes | Exactly `coqui-acp-history`. Any other value is `unknown_format`. |
| `version` | unsigned integer | yes | The format version: `1`. Any other number is `unknown_version`; a reader refuses it, it does not guess. |
| `id` | string | yes | The host's UUID for the session: canonical lower-case, 36 characters, hyphens at 8, 13, 18 and 23. |
| `origin` | object | yes | Where the history was recorded; see below. |
| `createdAt` | string | no | When the session began: RFC 3339, at most 40 bytes. |
| `model` | string | no | The model, as the harness names it. At most 256 bytes. |
| `mode` | string | no | The permission mode. At most 256 bytes. |
| `effort` | string | no | The reasoning effort. At most 256 bytes. |
| `cwd` | string | no | The session's absolute working directory. At most 4096 bytes. |
| `acpVersion` | unsigned integer | no | The ACP version of the updates, 1 or more. |
| `digest` | string | no | `sha256:` and 64 lower-case hexadecimal digits: the SHA-256 of the body, every byte after the header line, line feeds included. |
| `redacted` | object | no | `{"rules": n, "replaced": n}`, present when a redaction changed the body; see Redaction. |
| `_meta` | object | no | Anything else. The format reads none of it. |

`origin` holds `harness` (required: 1 to 64 bytes of lower-case letters, digits and `.`, `_`, `-`, `:`, starting with a letter or digit: `claude`, `codex`, `agy`, `acp:name`), `version` (the harness's version, at most 128 bytes) and `sessionId` (the harness's own id for the session, at most 256 bytes). Those are all its keys.

### The session UUID

A session is named by a UUID the host creates. The header's `id` is that UUID and `origin.sessionId` is whatever the origin harness called the session. Importing a history into a harness creates a session there under the header's `id` (a provider maps the UUID to the harness's own id and reports the UUID back), and converting a history to another harness keeps the `id`. A history whose `id` is not a canonical lower-case UUID is refused.

## Body lines

Every body line is an object with a `method` and, for most, `params`; it may also carry `at` (an RFC 3339 time, at most 40 bytes, when it happened) and `_meta` (an object the format does not read). No other top-level key is allowed. A `method` that is not in this table, or no `method`, is `unknown_line`.

| `method` | `params` (required unless noted) | Who and what |
| --- | --- | --- |
| `session/prompt` | `{"prompt": [block, ...]}`: one or more ACP content blocks, each an object with a non-empty string `type` | The client's prompt; opens a turn. |
| `session/update` | `{"update": {...}}`: an ACP update, an object with a non-empty `sessionUpdate` string of at most 64 bytes | The agent's notification: message and thought chunks, tool calls and their updates, plans, mode and option changes. |
| `session/cancel` | none (a `params` is refused) | The client cancelled the turn. |
| `session/request_permission` | `{"toolCall": {...}, "options": [option, ...]}`: `toolCall` an object with a non-empty `toolCallId`; one or more options, each an object with a non-empty `optionId` | The agent asked. |
| `history/permission_answer` | `{"toolCallId": "...", "outcome": {...}}`: `outcome` an object whose `outcome` is `selected` (then with an `optionId`) or `cancelled` | The client's answer to the request for that call. |
| `history/turn_end` | `{"stopReason": "...", "error": "..."}`: `stopReason` one of ACP's `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`, or `error`; `error` optional text | The prompt's response; closes the turn. |
| `history/compaction` | `{"summary": "...", "trigger": "...", "preTokens": n}`, all optional: `summary` non-empty, `trigger` `auto` or `manual`, `preTokens` an unsigned integer | The harness folded the history so far into a summary. |

Objects inside `params` are ACP's and are open: the format checks the fields named above and nothing else in them. Everywhere else, in the header, in the envelope and in `params` itself, a key the format does not define is refused (`unknown_field`). Anything extra goes in `_meta`.

String bounds in the table apply to the field only; the whole line is held to the line bound.

### Ordering rules

A reader keeps one state: whether a turn is open, and which permission requests are waiting.

1. The header is line 1, and only line 1 (`header_repeated`).
2. `session/prompt` opens a turn and is refused while one is open (`prompt_inside_turn`).
3. `history/turn_end` closes the open turn; it, `session/cancel`, `session/request_permission` and `history/permission_answer` are refused with no turn open (`outside_turn`).
4. `session/request_permission` is refused for a `toolCallId` already waiting (`request_repeated`). `history/permission_answer` needs a request waiting for its `toolCallId` and answers it (`answer_without_request`). Requests still waiting when the turn ends are treated as cancelled.
5. `session/update` and `history/compaction` may appear anywhere, outside a turn too: ACP sends some updates between prompts, and a harness can compact in the middle of a turn.
6. A history may end with a turn open: a recording cut short. A reader reports it (`open_turn`); it is not an error.

### Turn boundaries and stop reasons

A turn is a `session/prompt` through its `history/turn_end`. The stop reasons are ACP's; `error` is added for a turn the harness failed (a crash, a limit), with the message in `error`. A turn the client cancelled has a `session/cancel` line and, when the harness confirms, `cancelled`.

### Tools

A tool call is the ACP updates `tool_call` and `tool_call_update` with ACP's own fields: `toolCallId`, `title`, `kind` (`read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`, `other`), `status`, `locations`, `content` (diffs and terminals), `rawInput` and `rawOutput`. A writer fills `kind`, `rawInput` and `rawOutput` for every tool call it can.

What a resume by the same harness needs beyond that (the native tool-use and result ids paired with ACP's `toolCallId`, the exact native input) goes in `_meta` under the single key `dev.boriken.history/native`, an object only the origin harness's codec reads. It is never required to read the history on another harness, and an importer to a different harness ignores it.

Content that is large or binary may be embedded in its block, or left out and known by its digest: a block with empty or absent bytes and, in its `_meta`, `dev.boriken.history/attachment` set to `{"sha256": "sha256:<64 hex>", "bytes": n}`. An importer that cannot recover the bytes reports `attachment_by_digest` in the loss report.

### The normalised tool table

So a tool transfers between harnesses, the table names each harness's built-in tools once. A native tool call is read as `tool` plus input keys renamed to the normalised spelling; writing to another harness picks that harness's tool and renames back. The table renames keys; a change of shape (Codex's `command` as an argument vector, Claude's as a string) is the codec's.

| Tool | ACP `kind` | Normalised input keys | Claude | Codex | agy |
| --- | --- | --- | --- | --- | --- |
| `read` | `read` | `path`, `offset`, `limit` | `Read` (`file_path`) | none | `read_file` |
| `write` | `edit` | `path`, `content` | `Write` (`file_path`) | none | none |
| `edit` | `edit` | `path`, `old`, `new`, `replace_all` | `Edit` (`file_path`, `old_string`, `new_string`) | none | `edit_file` |
| `patch` | `edit` | `patch` | none | `apply_patch` (`input`) | none |
| `shell` | `execute` | `command`, `cwd`, `timeout_ms` | `Bash` (`timeout`) | `shell`, `shell_command` (`workdir`) | `run_command` |
| `grep` | `search` | `pattern`, `path`, `glob` | `Grep` | none | none |
| `glob` | `search` | `pattern`, `path` | `Glob` | none | none |
| `fetch` | `fetch` | `url` | `WebFetch` | none | none |

Claude's and Codex's names and keys are those of their tool schemas; agy's are names only, its input keys taken to be the normalised ones until recorded fixtures say otherwise. A call with no row, or to a harness with no equivalent, is written to the target as a plain note, and counted in the loss report. `vectors/session_history/tools.json` holds the table.

### Compaction boundaries and seeding a session

A `history/compaction` line marks where the harness replaced the history before it with a summary, and carries the harness's own `summary` text when it exposes one (Claude Code's compact summary does; Codex's compaction notification does not carry it, so its boundary has none). The lines before a boundary stay in the file: the file is the whole history, and a boundary says what the harness itself would have kept.

To seed a new session from the current context, a writer:

1. finds the latest boundary that has a `summary`;
2. gives the target that summary, where the target keeps one (Claude's compact summary message after its boundary, Codex's compacted item);
3. then replays the lines after the boundary, turn by turn.

With no boundary that has a summary, the seed is every body line. A later boundary without a summary does not displace an earlier one with a summary; the turns between them are kept. A boundary inside a turn leaves the seed starting with the rest of that turn (`mid_turn`).

The seed's starting point is reported as a line number (the boundary's, else line 2, the first body line) and the SHA-256 of that line in canonical spelling (below), so a later reader can tell it is the same history. A helper reports the latest whole turns that fit a size budget with the summary always kept; what to do when the seed does not fit (start from what fits and ask the harness to compact first) is the caller's policy.

## Size bounds

Every bound is a setting with a default and no floor. A reader holds the file to them while it reads and refuses at the first line past one; it never holds more than one line. A writer holds what it writes to the same bounds and never cuts a line.

| Setting | Default | Refusal |
| --- | --- | --- |
| `max_line_bytes` | 16 MiB (16777216), the line without its line feed | `too_long`, at the line |
| `max_lines` | 5,000,000, the header included | `too_many_lines` |
| `max_total_bytes` | 4 GiB (4294967296), line feeds included | `too_large` |
| `max_depth` | 64, the nesting of arrays and objects in a line | `too_deep`, at the line |

A JSON parser may stop earlier: the reference reader stops at 128 levels whatever `max_depth` says. A reader that verifies the digest does so when it reaches the end (`digest_mismatch`), and a setting turns that off.

## Errors

A reader reports a code, and, where it applies, the line and the JSON pointer of the place. The message never quotes the file: a line may hold a secret.

| Code | Meaning |
| --- | --- |
| `empty` | no header line |
| `not_utf8`, `blank_line`, `line_ending`, `truncated` | the line is not UTF-8, is empty, holds a carriage return, or the file does not end in a line feed |
| `syntax` | not JSON, or text after the value |
| `duplicate_key` | an object repeats a key |
| `shape` | a field is missing, of the wrong type or out of range (pointer given) |
| `unknown_field` | an object has a key the format does not define there (pointer of the object) |
| `unknown_format`, `unknown_version` | line 1 is not a version-1 header |
| `unknown_line` | a body line of a kind the format does not define |
| `order` | the line breaks an ordering rule |
| `too_long`, `too_many_lines`, `too_large`, `too_deep` | a bound |
| `digest_mismatch` | the header's digest is not the body's |

`vectors/session_history/refusals.json` pairs each malformed input with its code, line and pointer.

## Canonical spelling

A writer writes the following, and a writer in any language that does is byte-identical:

- compact JSON, no spaces, strings with the escapes JSON requires and UTF-8 otherwise;
- the header's keys in the order of the table above, the body line's in the order `method`, `params`, `at`, `_meta`;
- every other object's keys sorted by code point.

Reading then writing a history gives it in this spelling. A file that must stay byte for byte as it is (an export left unchanged) is copied line by line after the reader has checked it, never re-spelled.

## Version and evolution

The header's `version` is the format's. A reader refuses a version it does not know, however the file looks. A change that adds a line kind, a key or a rule is a new version; there are no optional extensions to version 1 other than `_meta`, whose namespaced keys (`dev.boriken.history/...`) a codec may add without one. While the format is a pre-release, a version is replaced rather than kept beside the next.

## Export, import and the loss report

- **Export writes the session as it is.** Nothing is dropped, summarised, redacted or cut unless the caller asks. Redaction and size limits are options the caller passes, never defaults.
- **Import** maps each normalised tool onto the target's built-in tools, writes a call with no equivalent as a plain note, seeds from the latest summary, and returns a loss report. It never re-runs a tool.
- **The loss report** is a list of `{kind, count, detail?}`, one entry per kind, `detail` never holding the lost content. The kinds are open (a consumer passes over one it does not know); version 1 defines:

| Kind | Meaning |
| --- | --- |
| `thinking` | signed thinking or encrypted reasoning the target cannot take, dropped |
| `tool_as_note` | a tool call with no equivalent in the target, written as a plain note |
| `attachment_by_digest` | a content block whose bytes the history left out |
| `replayed_as_prompt` | the target has no transcript of its own, so the history was replayed to it as a prompt |
| `summary_missing` | the latest boundary has no summary, so the seed starts earlier or at the start |

## Redaction

Redaction is an option of the caller who exports (the values or patterns to remove); it is off by default. When used it:

- replaces every match, in every string of the header and of the lines' payloads, with a replacement text (default `[redacted]`), leaving keys, the `method`, the time of a line and three identifiers the format pairs lines by (`sessionUpdate`, `toolCallId`, `optionId`) as they are;
- refuses, changing nothing, a removal that would leave a line the format refuses (a pattern that matches `selected`, say);
- reports what it removed as counts per rule, rules named by their place in the settings and never by their text, and per line changed;
- writes `redacted: {"rules": n, "replaced": n}` into the header, so a reader knows the body is not as recorded, and drops the header's digest (a new one is made by sealing).

Values are literal text; patterns are regular expressions that match in linear time and must not match the empty string.

## Settings

The crate's settings are Guatiao values (JSON, TOML or YAML through `guatiao-serde`), with the defaults above:

```json
{
  "bounds": {"max_line_bytes": 16777216, "max_lines": 5000000, "max_total_bytes": 4294967296, "max_depth": 64},
  "verify_digest": true
}
```

and, for redaction, `{"values": [], "patterns": [], "replacement": "[redacted]"}`. Unknown keys are refused. A setting that is not at its default is reported by `non_defaults()`.

## Conformance vectors

`vectors/session_history/` holds JSON files a reader in Rust, Dart or Python runs; text only. Each case has a `name` and a `why`.

- `valid.json`: `cases[]` with `lines` (the text of each line without its line feed; the file is those lines each ended by one) and `expect`: the header's `id`, `harness` and optional fields; the summary (`lines`, `turns`, `updates`, `compactions`, `open_turn`, `last_stop`); the `methods` of the body lines in order; and the `seed` (`line`, `digest` in canonical spelling, `summary`, `first_line`, `mid_turn`, `turns`). `canonical: true` means writing what was read gives the lines back identically; the others are another producer's spelling.
- `refusals.json`: `cases[]` with `lines`, or `text` where the exact bytes matter (a missing final line feed, a carriage return), and `expect` with the error `code`, the `line` (absent when the error has none) and, for `shape` and `unknown_field`, the JSON `pointer`. A reader must also refuse invalid UTF-8 with `not_utf8` at its line; JSON cannot hold such a case.
- `bounds.json`: `cases[]` with `limits` (`max_line_bytes`, `max_lines`, `max_total_bytes`, `max_depth`; a limit not named is its default), `lines`, and `expect` either `{"ok": true}` or an error `code` and `line`.
- `tools.json`: the normalised tool table: each tool's ACP `kind`, `normalise` cases (a harness, a native name and input, the tool and normalised input), `denormalise` cases, and names with no tool.
