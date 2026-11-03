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
- `coqui-acp-page`, and `package:coqui_acp/page.dart` for Dart: a person's agents from a page, over `document.acp`.
