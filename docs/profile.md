# ACP across a network

ACP's standard transport is stdio, and its remote transports are an [upstream RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport), not final.
This is what the bridge and these clients speak.

## WebSocket

One JSON-RPC message per text frame under mTLS, at the path of the operator's choosing, which matches the RFD. A browser cannot present a client certificate, so it
proves itself in-band, and each message is then one sealed record in a binary frame, which the RFD does not cover.

## WebTransport: `dev.boriken.coqui.acp-webtransport/v1`

A draft of this project, not an upstream transport. One reliable bidirectional stream the client opens, carrying a little-endian `u32` body length and then one
UTF-8 JSON-RPC message, at most 1 MiB. No datagrams.

## Identity first

A connection proves its peer before the first ACP message:

- **In band**: [coqui-peer-webcrypto](https://github.com/Boriken-Dev/coqui-peer-webcrypto), on the channel, with a pinned peer key. It ends in a session: each ACP message after it is one sealed record, in a binary WebSocket frame or one WebTransport frame. A plain message after the handshake is never taken. It does not depend on TLS, so a reverse proxy that ends TLS can carry it. On WebSocket a handshake message may be a text or a binary frame.
- **mTLS**, for a native client: through coqui-peer-tls. The stream is then plain ACP from its first byte, and protected only as far as that TLS goes.

A URL, a host name or server TLS alone proves no peer.

## No automatic retry

A prompt whose answer was lost has an unknown outcome. After a new connection proves the same peer, the client reconciles by `session/resume` or `session/load`, and a
person or the application decides what to send again.
