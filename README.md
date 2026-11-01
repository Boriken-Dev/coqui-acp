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
| `bindings/dart/coqui_acp` | the Dart client, for browsers and Flutter |
| `bindings/js` | the page side of [ACP in the browser](docs/browser.md), its scope and its conformance checks |
| `vectors/conversations.json` | scripted conversations both clients must pass |
| `docs/` | the wire profiles |

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
(cd bindings/dart/coqui_acp && dart analyze && dart test)
(cd bindings/js && npm test)
```

## License

MPL-2.0; see [LICENSE](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
