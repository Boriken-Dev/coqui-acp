# coqui-acp-api

[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**Drive an [Agent Client Protocol](https://agentclientprotocol.com) agent from a provider written in any language [Guatiao](https://github.com/Boriken-Dev/guatiao) hosts.** The `acp_harness` kind starts one ACP connection over a Guatiao channel: the host is the client, the provider is the agent, and every channel message is one JSON-RPC message. The kind never models ACP, so a harness implements ACP itself, at whichever version `initialize` agrees. The host hands every start a spawner, so the harness's program runs wherever the host chooses.

## Features

- `AcpHarness`: `describe`, `start`, `refusal` and `program`, which every harness answers; `models`, `session_record` and `non_defaults`, whose defaults are true answers for a harness with nothing to say.
- `ProcessSpawner` and `SpawnedProcess`: the host's spawner, here, over SSH or in a container: pipes or a terminal, reads that survive a dropped link, a kill that ends the whole tree.
- `StartRequest`: what a host adds to the provider's own configuration for one connection: a secret environment, working roots, requirements with their values, and the session's UUID.
- `Requirement` and its vocabulary (`read_only`, `no_commands`, `answer_only`, `no_project_settings`, `no_project_mcp`, `files_under`, `read_only_paths`, `denied_paths`, `network`, `sandboxed_scripts`, `only_skills`), which only narrow: a harness refuses a name it does not know.
- `WorkingRoots`: the one way a path is compared with the roots (no `..`, case on Windows, links followed).
- The kinds as data, `include/coqui_acp_api.kinds.json`, the models' schemas, `include/coqui_acp_api.schemas.json`, and a Python module, `python/coqui_acp_api_kinds.py`, for providers with no Rust.
- Feature `contract`: the suite every provider passes, as a module and as `acp-harness-check`, which runs it on a library or a source file in any language.
- No dependency but Guatiao and its channel kinds by default; no `unsafe`.

## Installation

Not on crates.io yet:

```sh
cargo add --git https://github.com/Boriken-Dev/coqui-acp coqui-acp-api
```

`--features contract` adds the suite.

## Usage

A host starts a connection and speaks ACP over it; `coqui_acp_harness::LocalSpawner` runs programs on this machine:

```rust,no_run
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{AcpHarness, ChannelSink, ProcessSpawner, StartRequest};

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

fn connect(
    harness: &dyn AcpHarness,
    spawner: Object<dyn ProcessSpawner>,
) -> Result<(), ProviderError> {
    let request = StartRequest::builder()
        .working_root("/work")
        .session_id("0f8fad5b-d9cb-469f-a165-70867728950e")
        .build();
    if let Some(why) = harness.refusal(request.requirements.clone(), request.environment.clone())? {
        return Err(ProviderError::new(coqui_acp_api::guatiao::Status::GUATIAO_ERR_UNSUPPORTED, &why));
    }
    let channel = harness.start(request, spawner, Host.into_object())?;
    channel.send(br#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}}"#)?;
    channel.close(0);
    Ok(())
}
```

`examples/echo_harness.rs` is a provider, and the API header `AGENTS.md` holds the whole contract.

## Licence

[MPL-2.0](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
