# Harnesses as agents

Coding agents' own command lines, served as ACP agents by translators written in Rust, with no Node and no SDK of the agent's.
Each runs the installed command line over the protocol it already speaks on standard input and output, and translates.

| Crate and command | Runs | Protocol it drives | Tested with |
| --- | --- | --- | --- |
| `coqui-acp-claude` | `claude` | stream-json: `-p --input-format stream-json --output-format stream-json` | Claude Code 2.1.318 |
| `coqui-acp-codex` | `codex app-server` | the app server's JSON-RPC | Codex 0.164.0 and 0.166.0 |

`coqui-acp-harness` holds what they share: starting the process and ending it with everything it started, reading its lines within
bounds, events in no protocol's terms, and the ACP agent itself. A version not tested runs, with a warning: a notice in ACP version
2, a line on standard error, and a notice to the host through the spawner (`ProcessSpawner::notice`), which is also how it
hears that a harness ended mid-session.

## What a session takes

| ACP | Claude Code | Codex |
| --- | --- | --- |
| `cwd` | the process's directory | the thread's `cwd` |
| `mcpServers` (stdio, HTTP; SSE in version 1 for Claude) | `--mcp-config` | the thread's `mcp_servers` configuration |
| `systemPrompt`, appended | `--append-system-prompt` | developer instructions |
| `additionalDirectories` | `--add-dir` | the Agent Skills directory of each one, and writable roots |
| skills | `_meta.claudeCode.options.plugins` as `--plugin-dir`, skills named `<plugin>:<skill>`, once the host allows client plugins | `additionalDirectories`, as above |
| model and effort | config options `model` and `effort`, changed between turns | the same |
| `session/load`, version 2 `replayFrom` | the transcript Claude Code keeps, then `--resume` | the thread's turns |
| `session/cancel` | an `interrupt` control request | `turn/interrupt` |

## Permissions

A tool call that needs leave reaches the client as `session/request_permission` with the harness's own options: for Claude Code
allow once, always allow (applying the rule Claude Code suggests) and reject; for Codex its own decisions. Neither side can
override the other's refusal: the harness's permission mode and rules still apply, and a call the client refuses does not run.

On Claude Code 2.1.318, `--permission-prompts host` alone does not route a prompt to standard output: the call is refused as not
granted. With `--permission-prompt-tool stdio`, the Claude Agent SDK's own route, it arrives as a `can_use_tool` control request,
and the translator passes both flags, unless told not to with `--no-permission-prompts` or `permission_prompts`. `dontAsk` decides
before either: a call nothing granted is refused where it is made, and no question comes. Both were tried on Windows and on Linux.

Each question says what it grants: the host a network approval opens, the paths and network of a permissions request, where a
moved file goes, the root an edit asks to write under. Version 1 has only a tool call to say it with, so the question is its title,
or comes first in its content. Codex's command approvals offer what it lists in `availableDecisions`. An MCP server's own
elicitation is refused; only Codex's approval of a tool call running on that server is asked, under the server's name.

## What a host can narrow

`--working-root`, `--no-client-mcp-servers` and `--allow-network-paths` on each binary, or `Settings` in a library. A working root is compared with links followed. Plugins from a client are off for Claude Code until the host allows them with `--allow-client-plugins` or `client_plugins(true)`, and are held to the same roots. With none given
a client's `cwd` and `additionalDirectories` may be anywhere local, and under Codex's `workspace-write` an additional directory is
writable with no approval.

## Recordings

`tools/record_harnesses/record.py` makes each crate's fixtures from the real tool against a stand-in model on loopback, spending
nothing and seeing no one's login. `tests/argv.rs` in each crate compares every recording's arguments with what its launch builds,
so a change of flags fails until the recordings are made again. A new version of a tool: record into a new directory, run the tests,
and read what changed.

## Versions 1 and 2

Each answers the version the client asks for. Version 2's shapes differ: tool calls are upserts, chunks name their message and a
message ends with a whole-message update, a prompt is answered at once and the turn ends with an `idle` state update, a permission
request names its subject, options are `configId`, and there are notices, terminals of the agent's own, plans with ids and
compaction. Version 1 gets none of version 2's kinds.
