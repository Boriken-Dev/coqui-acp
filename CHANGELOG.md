# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `coqui-acp`: drive any Agent Client Protocol version 1 agent from blocking code, with permission deciding, resume, extensions and line transports.
- `coqui_acp` for Dart: a client that speaks version 1 and version 2, whichever the agent selects. `openSession`, `setConfigOption`, `setMode` and `AcpCancel` for a client that shows what an agent offers. `listSessions` and `reopenSession` for earlier sessions, and `AcpAttachment` with `takesImages` and `takesFiles` for what goes with a prompt.
- `coqui-acp-bridge`: a stdio ACP agent on the network under coqui-peer identity.
- `coqui-acp-bridge`: `serve --advertise` publishes a DNS-SD record, or a descriptor on loopback; `discover` finds both; `companion install` lets a browser extension ask, over native messaging; `serve --tls-short-lived` lets a browser pin WebTransport with no certificate authority.
- `coqui-acp-bridge`: `serve --paired` and `dial --paired`, on this user's own identity and trust store, so one pairing serves every command. Proof, pairing, discovery and the companion moved to `coqui-agentic-peer`.
- `coqui-acp-bridge`: `serve --service` and `--grant` for several protocols on one link, `grant` for adding one to a paired peer at once, and `dial --fallback` for another endpoint.
- `coqui-acp-bridge`: `serve --forward`, `serve --reverse` and `forward`, for TCP over a link in either direction, each target its own grant.
- `coqui-acp-provider`: `AgentProvider` and `AgentSession`, a Guatiao provider kind for agents in any language Guatiao hosts, pinned at Guatiao `a67fd4be`; `AgentEvent` and its string contract (`text`, `tool`, `blocked`, `completed`, `failed`, `cancelled`, `limit`, `denied`); `AcpFacade`, which serves any such provider as an ACP version 1 agent over any transport, with a `Settings` limit for sessions per connection (8), provider calls (`start` 60 s, others 30 s), cancel (10 s), close (10 s), prompt size (8 MiB), output per turn (64 MiB) and per connection (256 MiB), and failed authentication (5), and optional working roots. `coqui-acp` itself does not depend on Guatiao.
- `coqui-acp-bridge`: `mcp-config` and `mcp-shim --peer`, so an MCP client with no ACP session reaches a peer's own tools from one stable entry. A peer keeps a stream of `mcp-offer/1` standing, with the metadata `standing/1`, and each program gets a stream of its own.
- `coqui-acp-page`, and `package:coqui_acp/page.dart` for Dart: a person's agents from a page, over `document.acp`.
- `coqui-acp-bridge`: `serve --linger` keeps a link's agent for a time and gives it back to the same proven peer on a new connection, with `--linger-per-peer`, `--linger-total`, `--linger-buffer-bytes` and `--linger-settle-ms`. Off by default. Output dropped while nobody was connected is said in a `_dev.boriken.coqui.acp/output_lost` notification, first on the next link.
- `coqui-acp` and `coqui_acp` for Dart: `Event::OutputLost` and `AcpOutputLost` report `_dev.boriken.coqui.acp/output_lost`.
- `coqui-acp`: `Builder::ends_turn` says which extension notifications end a turn, so a connection ending after one is no `Event::Failed`, and `Builder::disconnect_detail` says what became of a lost agent, in that event's text.

### Changed

- `coqui-acp` and `coqui_acp` for Dart: a `session/update` of a session other than the client's own is dropped, and a `session/request_permission` of one is answered cancelled without asking the decider, so what an agent kept from an earlier connection says of its session never feeds this one.
- `coqui_acp` for Dart: request ids start at a random point on each connection, so they do not repeat those of an earlier connection to the same agent.
- **Breaking**, `coqui-acp` and `coqui_acp` for Dart: stop reason `cancelled` is `Event::Cancelled` and `AcpCancelled`, and no longer `Event::Blocked` and `AcpBlocked`, which now mean only a permission that could not be answered. It ends the turn, so a connection lost after it is no `Failed`.
- **Breaking**, `coqui-acp`: the function `exchange` is now `Exchange::new()...run(transport, calls)`, a builder with `client_info`, `authenticate`, `initialize_timeout` and `request_timeout`, so a caller can name itself rather than always being `coqui-acp`. Its waits default to those of `Client::builder`.
