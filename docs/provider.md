# Agents as providers

Most agents already speak ACP, so a client needs nothing more for them. For anything else, `coqui-acp-provider` is the small contract to write instead: a [Guatiao](https://github.com/Boriken-Dev/guatiao) provider kind that starts sessions and reports what happens as events, and a facade that serves any such provider as an ACP agent. A provider written once, in any language Guatiao hosts, reaches every ACP client.

## The kind

`AgentProvider` has `start` and `resume`. Each takes a working directory, a prompt, context, an environment, a model, an effort and an opaque `meta`, and answers an `AgentSession`. A provider owns whatever process it runs.

`AgentSession` has `session_id`, `send_message`, `next_event`, `interrupt` and `close`. Everything that happens comes out of `next_event` as an event, whose `kind` is text that consumers match on: `text`, `tool`, `blocked`, `completed`, `failed`, `cancelled`, `limit`, `denied`. A harness's exit is reported as `completed` or `failed`, and `close` returns once the harness has exited.

The kinds are in `crates/coqui-acp-provider/include/coqui_acp_provider.kinds.json` as data, which Guatiao's generators read, and in `crates/coqui-acp-provider/python/` for Python.

## The facade

`AcpFacade` serves one provider over any transport, one provider session per ACP session. It speaks ACP version 1: `initialize`, optional `authenticate`, `session/new`, `session/prompt`, `session/cancel`, `session/resume`, `session/close`.

ACP has no field for an environment, a model or an effort, so a client sends them under `_meta["dev.boriken.coqui.acp/launch"]` on `session/new`. ACP has no update for a quota or a usage limit, so those arrive as a `_coqui_acp/event` notification, which a client may ignore.

### What a session's limits are for

The facade is the boundary between a client it may not fully trust and a provider it cannot stop. Each limit is a setting with a safe default.

- **A provider that blocks** costs one session. Every provider call runs on a thread of its own and is timed, so the connection goes on answering. An overdue call fails the turn and ends the session; the thread stays until the call returns, and the session keeps its place among the connection's sessions until then.
- **A client that vanishes** mid-turn has its sessions closed, and sessions belong to the connection that opened them. Keeping an agent for a client to come back to is the [bridge's](profile.md) to do.
- **A provider that fails or panics** fails that turn. A panic's words stay out of what the client hears.
- **A connection** opens eight sessions by default.
- **A client's environment** reaches a provider only for names the host allows, by default none: a client that can set `PATH` runs what it likes.
- **A provider's text** is untrusted when it reaches the client. It is only ever data: a stop reason, an error code and a tool's kind never come from it. A tool's kind is always `other`, so what a provider may do is the provider's to enforce.

## From another ACP client

The facade serves any transport the protocol's SDK has, so a host can put a provider on standard input and output and any client can start it as an agent. `examples/provider-python` does that for an echo agent written in Python, with its tests driving it through `coqui_acp::Client`.

[The API header](https://github.com/Boriken-Dev/coqui-acp/blob/main/crates/coqui-acp-provider/AGENTS.md) has the whole contract.
