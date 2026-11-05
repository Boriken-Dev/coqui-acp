# coqui-acp-claude

[![Version](https://img.shields.io/crates/v/coqui-acp-claude.svg)](https://crates.io/crates/coqui-acp-claude)
[![Rust version](https://img.shields.io/crates/msrv/coqui-acp-claude.svg)](https://crates.io/crates/coqui-acp-claude)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![Docs](https://img.shields.io/docsrs/coqui-acp-claude)](https://docs.rs/coqui-acp-claude)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**Claude Code as an [Agent Client Protocol](https://agentclientprotocol.com) agent, in Rust, with no Node.** It drives the installed
`claude` over its own stream-json protocol on standard input and output, the protocol the Claude Agent SDK speaks, and serves it
as an ACP agent in version 1 or 2, whichever the client asks for. A driver with no ACP in it is public too, for a program that
wants Claude Code's events in its own terms. Documentation: <https://boriken-dev.github.io/coqui-acp/>.

## Features

- `ClaudeLaunch`: model, effort, appended system prompt, MCP servers, more directories, plugin directories (skills), resume,
  permission mode, setting sources and a spending ceiling, as `claude`'s arguments. Every value is one `--flag=value` argument,
  so a value starting with `-` is never read as an option.
- `ClaudeSession`: starts Claude Code through any `Spawn`, by default a process whose whole tree is ended on close (a job object
  on Windows, a process group on Unix); asks `initialize` for its models, efforts, commands and version with no model turn;
  sends prompts with text, images, links and resources; interrupts a turn apart from closing; changes model and effort between
  turns.
- `Parser`: stream-json lines as events: message and thought chunks, tool calls with diffs (and a git patch from Claude Code's
  hunks), locations and terminals, plans from `TodoWrite` and the Task tools, permission requests with a responder, usage and
  context, quota, usage limits, compaction, commands, notices, refused calls and how a turn ended.
- `replay`: a session's history from the transcript Claude Code keeps, for `session/load`.
- `ClaudeAgent`: the ACP agent: `initialize`, `session/new`, `session/load`, `session/resume` (version 2's with replay),
  `session/prompt`, `session/cancel`, `session/close`, `request_permission` with Claude Code's own options (allow once, always
  allow applying the rule Claude Code suggested, reject), and model and effort as config options.
- `coqui-acp-claude`, a command that serves it on standard input and output.
- A Claude Code version it was not tested with runs, with a warning: a notice in version 2, and a line on standard error.

Permission prompts reach the client through `--permission-prompt-tool stdio`, as `can_use_tool` control requests on standard
output. On Claude Code 2.1.318 `--permission-prompts host` alone did not route them: the call was refused as not granted. Both
transcripts are among the fixtures.

## Installation

Not published: `publish = false` until a release is chosen. `cargo build -p coqui-acp-claude` from a checkout builds the library and
the command. There are no Cargo features.

## Quick start

A session in the driver's own terms:

```rust,no_run
use std::sync::mpsc;

use coqui_acp_claude::harness::event::{Content, Event, Outcome};
use coqui_acp_claude::{ClaudeLaunch, ClaudeSession, ClaudeSettings, PlainSpawn};

fn main() -> Result<(), coqui_acp_claude::Error> {
    let mut launch = ClaudeLaunch::new("/work/repo");
    launch.model = Some("haiku".into());
    let (sender, events) = mpsc::channel();
    let session = ClaudeSession::start(&launch, &PlainSpawn, ClaudeSettings::new(), move |event| sender.send(event).is_ok())?;
    session.prompt(&[Content::Text("Say hello.".into())])?;
    for event in events {
        match event {
            Event::MessageChunk { text, .. } => print!("{text}"),
            Event::Permission(request) => {
                // `allow`, `allow_always` or `reject`, the ids of the options the request offers.
                let _ = request.responder.answer(Outcome::Selected("reject".into()));
            }
            Event::TurnEnd(_) => break,
            _ => {}
        }
    }
    session.close();
    Ok(())
}
```

The ACP agent, on any reader and writer:

```rust,no_run
use std::sync::Arc;

use coqui_acp_claude::ClaudeAgent;
use coqui_acp_claude::harness::{Settings, serve};

fn main() -> std::io::Result<()> {
    serve(Arc::new(ClaudeAgent::new()), Settings::new(), std::io::stdin(), std::io::stdout())
}
```

## Command line

```sh
coqui-acp-claude                                   # claude from PATH, its own defaults
coqui-acp-claude --model haiku --effort low        # what every session starts with
coqui-acp-claude --setting-sources "" --strict-mcp-config
```

| Flag | Default | What |
| --- | --- | --- |
| `--executable` (`COQUI_ACP_CLAUDE_EXECUTABLE`) | `claude` | The Claude Code program |
| `--model`, `--effort` | Claude Code's | What every session starts with |
| `--permission-mode` | Claude Code's | `default`, `acceptEdits`, `plan`, `dontAsk`, `bypassPermissions`, `auto` or `manual` |
| `--setting-sources` | every source | Comma-separated, of `user`, `project` and `local`; empty loads none |
| `--strict-mcp-config` | off | Only the MCP servers a session is given |
| `--max-acp-version` | 2 | The highest ACP version answered |
| `--max-sessions` | 8 | Sessions at once |
| `--working-root` | any | A directory a session's `cwd` and more directories must lie in, repeated for more |
| `--no-client-mcp-servers` | off | Refuse a session that a client gives MCP servers |
| `--allow-network-paths` | off | Take a network path as a session's `cwd` or more directories |

Exit status: 0 when the client closes the connection, 1 when reading it failed, 2 for a wrong invocation.

## API overview

| Item | Purpose |
| --- | --- |
| `ClaudeLaunch`, `SessionStart`, `mcp_config` | What Claude Code is started with |
| `ClaudeSession`, `ClaudeSettings`, `Model`, `models_of`, `user_blocks` | A running Claude Code |
| `Parser` (`parse`) | Stream-json lines as events |
| `config_dir`, `transcript`, `replay` | A session's history |
| `ClaudeAgent` | The ACP agent |
| `harness` | `coqui-acp-harness`: events, processes and the ACP server |

## Origin

The stream-json handling, usage-limit and quota reading, and the cases its tests carry over come from AI6's Claude Code
handler, `crates/providers/agents/claude` of <https://github.com/jose-pr/ai6> at commit
`a40abe845ba0bab14e43e364d2da9a3bdba5e1d4`, MPL-2.0, by the same owner. AI6's policy, gate hooks, terminal provider and pricing
are left out.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p coqui-acp-claude
```

The tests run against `examples/replay_claude.rs`, which replays transcripts recorded from Claude Code 2.1.318 on Windows in
`tests/fixtures/2.1.318/`: each `<name>.stdout` is Claude Code's standard output byte for byte, `<name>.steps.json` what was
written to it and when, and `<name>.argv.json` its arguments. They were recorded against a loopback stand-in for the model, so no
model was reached: the turns' text and tool calls are scripted, and what Claude Code does with them is its own.
`turn.transcript.jsonl` holds the message lines of the session transcript Claude Code wrote for `turn`, each unchanged.

## License

MPL-2.0; see [LICENSE](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
