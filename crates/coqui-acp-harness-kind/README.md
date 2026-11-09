# coqui-acp-harness-kind

[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**Drive an [Agent Client Protocol](https://agentclientprotocol.com) agent from a provider written in any language [Guatiao](https://github.com/Boriken-Dev/guatiao) hosts.** The `acp_harness` kind starts one ACP connection over a Guatiao channel: the host is the client, the provider is the agent, and every channel message is one JSON-RPC message. The kind never models ACP, so a harness implements ACP itself, at whichever version `initialize` agrees.

## Features

- `AcpHarness`: `describe` and `start`, then `refusal`, `models`, `program` and `session_record`, each with a default.
- `StartRequest`: what a host adds to the provider's own configuration for one connection: environment, working roots and requirements, which only narrow.
- `Requirement` and its vocabulary (`read_only`, `no_commands`, `resume_wider`, `only_skills`, `sandboxed_scripts`): a harness refuses a name it does not know.
- The kinds as data, `include/coqui_acp_harness_kind.kinds.json`, and as a Python module, `python/coqui_acp_harness_kinds.py`, for providers with no Rust.
- No dependency but Guatiao and its channel kinds; no `unsafe`.

## Installation

Not on crates.io yet:

```sh
cargo add --git https://github.com/Boriken-Dev/coqui-acp coqui-acp-harness-kind
```

There are no Cargo features.

## Usage

A host starts a connection and speaks ACP over it:

```rust,no_run
use coqui_acp_harness_kind::guatiao::library::{Object, ProviderError};
use coqui_acp_harness_kind::{AcpHarness, ChannelSink, StartRequest};

struct Host;

impl ChannelSink for Host {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        // One JSON-RPC message from the agent.
        let _ = bytes;
        Ok(())
    }

    fn closed(&mut self, status: u32) {
        let _ = status;
    }
}

fn connect(harness: &dyn AcpHarness) -> Result<(), ProviderError> {
    let channel = harness.start(StartRequest::new(), Host.into_object())?;
    channel.send(br#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}}"#)?;
    channel.close(0);
    Ok(())
}
```

`examples/echo_harness.rs` is a provider, and the API header `AGENTS.md` holds the whole contract.

## Licence

[MPL-2.0](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
