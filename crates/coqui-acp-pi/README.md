# coqui-acp-pi

[![Version](https://img.shields.io/crates/v/coqui-acp-pi.svg)](https://crates.io/crates/coqui-acp-pi)
[![Rust version](https://img.shields.io/crates/msrv/coqui-acp-pi.svg)](https://crates.io/crates/coqui-acp-pi)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![Docs](https://img.shields.io/docsrs/coqui-acp-pi)](https://docs.rs/coqui-acp-pi)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**[Pi](https://pi.dev) as an [Agent Client Protocol](https://agentclientprotocol.com) agent, in Rust, with no adapter.** It drives
the installed `pi` over its own RPC mode, `pi --mode rpc` (JSON lines on standard input and output), and serves it as an ACP
agent in version 1 or 2, whichever the client asks for. Nothing but `pi` has to be installed: no `npx`, no `pi-acp`. A driver
with no ACP in it is public too. Documentation: <https://boriken-dev.github.io/coqui-acp/>.

## Features

- `PiLaunch`: model, thinking level, appended system prompt, resume or load, the mode a session starts in and the modes a
  client may choose. None of the person's text is an argument of the command line.
- `PiSession`: starts `pi --mode rpc` through any `Spawn`, by default a process whose whole tree is ended on close; reads its
  state, models, thinking levels and commands; sends prompts with text, images, links and resources; switches the model and the
  thinking level between turns; stops a run (a question the person has not answered is cancelled first, which Pi's abort does
  not do); replays an earlier session's messages.
- `Parser`: Pi's lines as events: message and reasoning chunks, tools with their diffs (an `edit`'s replacements and a git
  patch, a `write`'s old content read before Pi writes it), locations, a command's terminal (command, output, exit code), usage
  and context, compaction, retries, notices, and how a run ended (a 429 is a usage limit).
- A permission gate. Pi asks nobody before it runs a tool, so this crate ships a Pi extension as text, writes it to a private
  directory made for the launch (nothing goes into the person's Pi configuration) and loads it with `-e`. Each tool call is put
  to the translator as an extension dialog, and the mode decides: it runs, it is refused as a plan, or the person is asked
  through ACP's `session/request_permission`.
- `PiAgent`: the ACP agent: `initialize`, `session/new`, `session/load`, `session/resume`, `session/prompt`, `session/cancel`,
  `session/close`, `request_permission`, and the model, the thinking level and the mode as config options.
- `PiProvider`, an `acp_harness` provider (id `pi`) with Pi's accounts (`pi-api-key`, `pi-oauth`).
- `coqui-acp-pi`, a command that serves it on standard input and output.
- A Pi version it was not tested with runs, with a warning.

## Installation

Not published: `publish = false` until a release is chosen. `cargo build -p coqui-acp-pi` from a checkout builds the library and
the command. There are no Cargo features.

## Quick start

```rust,no_run
use std::sync::mpsc;

use coqui_acp_pi::harness::event::{Content, Event, Outcome};
use coqui_acp_pi::{PiLaunch, PiSession, PiSettings, PlainSpawn};

fn main() -> Result<(), coqui_acp_pi::Error> {
    let launch = PiLaunch::new("/work/repo");
    let (sender, events) = mpsc::channel();
    let session = PiSession::start(&launch, &PlainSpawn, PiSettings::new(), move |event| sender.send(event).is_ok())?;
    session.prompt(&[Content::Text("Say hello.".into())])?;
    for event in events {
        match event {
            Event::MessageChunk { text, .. } => print!("{text}"),
            Event::Permission(request) => {
                // `allow`, `allow_tool` and `reject`, the options the request offers.
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

```rust,no_run
use std::sync::Arc;

use coqui_acp_pi::PiAgent;
use coqui_acp_pi::harness::{Settings, serve};

fn main() -> std::io::Result<()> {
    serve(Arc::new(PiAgent::new()), Settings::new(), std::io::stdin(), std::io::stdout())
}
```

## Command line

```sh
coqui-acp-pi                                  # pi from PATH, its own defaults
coqui-acp-pi --mode accept_edits --effort low
```

| Flag | Default | What |
| --- | --- | --- |
| `--executable` (`COQUI_ACP_PI_EXECUTABLE`) | `pi` | The Pi program; on Windows npm's shim is `pi.cmd` |
| `--model`, `--effort` | Pi's | What every session starts with (`provider/id`, a thinking level) |
| `--mode` | `manual` | The mode sessions start in and the loosest a client may choose: `manual`, `accept_edits` or `plan` |
| `--allow-bypass` | off | Offers the Bypass mode: never ask |
| `--ask-for-reads` | off | Asks before a tool that only reads, too, in Manual mode |
| `--max-acp-version` | 2 | The highest ACP version answered |
| `--max-sessions` | 8 | Sessions at once |
| `--working-root` | any | A directory a session's `cwd` and more directories must lie in, repeated for more |
| `--allow-network-paths` | off | Take a network path as a session's `cwd` or more directories |

Exit status: 0 when the client closes the connection, 1 when reading it failed, 2 for a wrong invocation.

## API overview

| Item | Purpose |
| --- | --- |
| `PiLaunch`, `SessionStart`, `valid_session_id` | What Pi is started with |
| `PiSession`, `PiSettings`, `Model`, `models_of`, `user_input` | A running Pi and its session |
| `Parser` (`parse`) | Pi's lines as events |
| `mode` | What each mode lets Pi do without asking |
| `Gate`, `GATE_SOURCE` | The permission gate, as text and as files for one launch |
| `PiAgent` | The ACP agent |
| `PiProvider`, `ProviderSettings`, `accounts` | The `acp_harness` provider and Pi's accounts |
| `harness` | `coqui-acp-harness`: events, processes and the ACP server |

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p coqui-acp-pi
```

The tests run against `examples/fake_pi.rs`, a stand-in for `pi --mode rpc` that speaks the dialect recorded from Pi 1.5.2 on
Windows in `tests/fixtures/pi-1.5.2/`: each `<name>.stdout` is what Pi wrote to standard output, one JSON object a line, with Pi's
own system message left out. They were recorded against a loopback stand-in for the model API, so no model was reached and no
login was used. `gate_live.stdout` is this crate's permission gate running in Pi itself, answered `allow`, `deny`, `plan`,
cancelled and with a word it does not know. `permission_gate.stdout` is Pi asking through an earlier, simpler extension, its
questions rewritten to the gate's.

## License

MPL-2.0; see [LICENSE](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
