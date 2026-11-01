# coqui-acp-bridge

[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/Boriken-Dev/coqui-acp/test.yml)](https://github.com/Boriken-Dev/coqui-acp/actions/workflows/test.yml)

**Put a stdio [Agent Client Protocol](https://agentclientprotocol.com) agent on the network, under a proven peer identity.** No byte reaches the agent until the peer is proven. The
bridge passes complete messages and reads none of them, so what the agent may do stays with the agent. Documentation: <https://boriken-dev.github.io/coqui-acp/>.

## Features

- `serve`: wait for peers, each with an agent of its own. WebSocket, and WebTransport beside it.
- `dial`: connect out from behind NAT, reconnecting with the agent still running; ends with the agent's exit status.
- Nothing is trusted on first use. A peer is proven by one of: a browser's in-band WebCrypto proof (`--allow`), a pinned client certificate (`--allow-cert`), a paired key in a trust store (`--allow-store`).
- `pair-accept` and `pair-dial`: a PIN makes a full key, over the bridge's own WebSocket, so a reverse proxy can carry it. One wrong try ends the attempt.
- `--permit` and `--deny` drop a source network before its handshake. `--origin` limits browsers.
- A key, a PIN or a token is never an argument.

## Installation

Not published: `publish = false` until a release is chosen. `cargo build -p coqui-acp-bridge` from a checkout.

## Quick start

A browser, on this machine:

```sh
coqui-acp-bridge keygen bridge.pem
coqui-acp-bridge serve --identity bridge.pem --allow sha256:<browser peer id> -- gemini --acp
```

Two machines, paired by a PIN, with the agent behind NAT:

```sh
# On the server: shows a PIN
coqui-acp-bridge pair-accept --listen 127.0.0.1:7443 --tls-identity server.pem --store peers   # behind a proxy; any other address serves wss

# On the agent's machine: asks for it on standard input
coqui-acp-bridge pair-dial wss://server.example/v1/agent/pair --tls-identity agent.pem --store peers

# Then, on each side
coqui-acp-bridge serve --listen 0.0.0.0:7444 --tls-identity server.pem --allow-store peers -- my-gateway
coqui-acp-bridge dial wss://server.example:7444/agent --tls-identity agent.pem --trust-store peers -- gemini --acp
```

## Command line

| Command | Does |
| --- | --- |
| `keygen PATH` | creates a P-256 identity and prints its peer id; never replaces a file |
| `serve` | one agent process per proven connection, up to `--max-peers` (default 4) |
| `dial URL` | `wss://` only; a wrong remote certificate stops it |
| `pair-accept`, `pair-dial` | one PIN exchange, recorded in a trust store |

`dial` trusts by `--expect` (a fingerprint), `--trust-store`, `--ca-file` (a private CA, with the name checked), or a pin together with a CA. `--bearer-file` adds `Authorization: Bearer`.

Exit status: the agent's own when it ends the bridge; 2 for a bad configuration or a refused pairing.

## What it does not protect

- An agent is a program with its own permissions. Allowing a peer gives it whatever that agent will do when asked.
- A live connection is not closed when a peer is removed from the store: the next connection is refused.
- `--allow`'s proof shows who holds a key, not who sends what follows: whoever relays the proof holds the channel, including a server a client was lured to and anything ending TLS in front. Use it where that cannot happen, until the method binds later messages. Client certificates end at TLS, so through a proxy they mean what the proxy vouches for.
- A slow peer is dropped past 64 unproven connections at once; `--permit` is what keeps strangers out of that count.
- The agent's own children are not stopped with it.
- Key file permissions are the caller's to keep, on Windows and when reading on any platform.
- Everything else about the session, what it reads and what it sends to its model provider, is the agent's.

## Development

```sh
COQUI_ACP_TEST_NODE=/path/to/node COQUI_ACP_TEST_WEBCRYPTO=/path/to/coqui-peer/methods/webcrypto/src/index.mjs \
  cargo test -p coqui-acp-bridge
```

The WebCrypto test needs Node 22 or later and coqui-peer's method, and says so when it skips.

## License

MPL-2.0; see [LICENSE](https://github.com/Boriken-Dev/coqui-acp/blob/main/LICENSE).
