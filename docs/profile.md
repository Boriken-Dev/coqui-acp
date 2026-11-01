# ACP across a network

ACP's standard transport is stdio, and its remote transports are an [upstream RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport), not final.
This is what the bridge and these clients speak.

## WebSocket

One JSON-RPC message per text frame, at the path of the operator's choosing. That framing matches the RFD. A browser cannot present a client certificate, so it
proves itself in-band first.

## WebTransport: `dev.boriken.coqui.acp-webtransport/v1`

A draft of this project, not an upstream transport. One reliable bidirectional stream the client opens, carrying a little-endian `u32` body length and then one
UTF-8 JSON-RPC message, at most 1 MiB. No datagrams.

## Identity first

A connection proves its peer before the first ACP message:

- **In band**, for a browser: coqui-peer's WebCrypto method, on the channel, with a pinned peer key.
- **mTLS**, for a native client: through coqui-peer-tls. The stream is then plain ACP from its first byte.

A URL, a host name or server TLS alone proves no peer.

## No automatic retry

A prompt whose answer was lost has an unknown outcome. After a new connection proves the same peer, the client reconciles by `session/resume` or `session/load`, and a
person or the application decides what to send again.
