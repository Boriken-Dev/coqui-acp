# coqui-acp-secrets

[![Version](https://img.shields.io/crates/v/coqui-acp-secrets.svg)](https://crates.io/crates/coqui-acp-secrets)
[![Rust version](https://img.shields.io/crates/msrv/coqui-acp-secrets.svg)](https://crates.io/crates/coqui-acp-secrets)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![Docs](https://img.shields.io/docsrs/coqui-acp-secrets)](https://docs.rs/coqui-acp-secrets)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**The host side of secret prompts for an [Agent Client Protocol](https://agentclientprotocol.com) agent.** An agent that needs a
password asks for it through a tool, the person types it in their own frontend, and the host keeps it under a handle: the model, the
transcript, the session history and the logs hold the handle and never the value. The wire between the host and the frontend, and the
sealing of each value, are [`coqui_acp::secret`](https://docs.rs/coqui-acp); this crate is what the host does with it.

## Features

- `secret_request`: asks the person for a form; the agent gets the plain answers and one handle per secret field.
- `secret_use`: writes a secret to a file under the session's roots, or runs a command with it in an environment variable, after the
  session's permission mode decides: Bypass approves it, any other mode asks the person (allow once or for the session). A session allowance and Bypass both trust the contents of the folder the command runs in.
- `secret_show`: shows the person a value (from a handle, a file, an environment variable or a command) on their own screen.
- A per-session store, cleared and zeroed when the session ends; every limit is a setting with a default.
- Output scrubbing: the value and its base64, hex, URL-encoded and JSON-escaped forms are replaced by the handle in everything a
  tool returns. A backstop for accidents, not a guarantee: a reversed, split or otherwise transformed value passes it.
- Only the session owner's frontend is asked, and an answer from any other identity is refused.
- The tools as an MCP server over JSON-RPC (`SessionTools::handle_message`, `serve`), for any transport.

## Installation

```sh
cargo add coqui-acp-secrets
```

## Quick start

```rust,ignore
let secrets = Secrets::builder(host_prover, verifier).build();
let tools = secrets.open_session("s1", SessionConfig::new(frontend, vec![project_dir]))?;
// Serve `tools` to the agent as an MCP server, for example on a stdio pipe of its own:
tools.serve(reader, writer)?;
// When the session ends, however it ends:
secrets.end_session("s1");
```

The boundary, stated plainly: this keeps a secret out of the model's context, the transcripts and the logs. It does not stop an agent
that may run a command from reading a value the person lets a command use, or a file it was written to. That is why every use is shown
to the person in full.

Documentation: <https://boriken-dev.github.io/coqui-acp/>. License: MPL-2.0.
