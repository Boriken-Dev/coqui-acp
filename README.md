# coqui-acp

[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://boriken-dev.github.io/coqui-acp/)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**Use any [Agent Client Protocol](https://agentclientprotocol.com) agent, from any program, over any distance.** A session library for Rust and Dart, and a
command that puts a stdio agent on the network under a proven identity. It builds on the protocol's own SDK and knows nothing of the application above it.

| Path | What it is |
| --- | --- |
| `crates/coqui-acp` | the Rust library |
| `crates/coqui-acp-bridge` | the bridge command |
| `crates/coqui-acp-claude` | [Claude Code as an ACP agent](docs/harnesses.md), over its stream-json protocol, and a command of the same name |
| `crates/coqui-acp-codex` | [Codex as an ACP agent](docs/harnesses.md), over its app server, and a command of the same name |
| `crates/coqui-acp-harness` | what those translators share: process trees, bounded lines, neutral events, and an ACP agent in versions 1 and 2 |
| `crates/coqui-acp-provider` | [agents as Guatiao providers](docs/provider.md), in any language, and a facade serving one over ACP |
| `examples/provider-python` | an echo agent written in Python, through that facade |
| `bindings/dart/coqui_acp` | the Dart client, for browsers and Flutter |
| `bindings/dart/coqui_acp_chat` | the state of an agent chat built from the client's updates, in pure Dart: messages, tool calls, permissions, the prompt queue and several attached clients |
| `bindings/js` | the page side of [ACP in the browser](docs/browser.md), its scope and its conformance checks |
| `vectors/conversations.json`, `vectors/describe.json`, `vectors/elicitation.json` | scripted conversations, descriptions of agents, and an agent's questions as forms, which both clients must pass |
| `docs/` | the wire profiles |

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
(cd bindings/dart/coqui_acp && dart analyze && dart test)
(cd bindings/dart/coqui_acp_chat && dart analyze && dart test)
(cd bindings/js && npm test)
```

## License

MPL-2.0; see [LICENSE](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
