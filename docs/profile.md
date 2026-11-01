# ACP across a network

ACP's standard transport is stdio, and its remote transports are an [upstream RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport), not final.
This is what the bridge and these clients speak.

## WebSocket

One JSON-RPC message per text frame under mTLS, at the path of the operator's choosing, which matches the RFD. A browser can use mTLS as it does for any site: when the
server asks, it offers a certificate from the person's store or a smart card, and the person chooses. A page cannot choose or supply one itself, and cannot tell which was
used, so a page that must prove a key of its own, or a person with no certificate installed, proves itself in-band instead. Each message is then one sealed record in a binary
frame, which the RFD does not cover.

## WebTransport: `dev.boriken.coqui.acp-webtransport/v1`

A draft of this project, not an upstream transport. One reliable bidirectional stream the client opens, carrying a little-endian `u32` body length and then one
UTF-8 JSON-RPC message, at most 1 MiB. No datagrams.

## Identity first

Each endpoint uses one proof, chosen by its operator and never negotiated on the connection, so nothing on the path can talk it down. What `coqui-peer` offers can be used:

| Proof | What it shows | Through a proxy that ends TLS |
| --- | --- | --- |
| In-band | Both keys, and every later message is tied to them | Yes, and the proxy can neither read nor inject |
| Client certificate, by pin, store or CA | Both sides, to whatever ends TLS | Only what the proxy vouches for |
| TLS checking only the server | The server, by pin, key or CA. Anyone can be the client | Only as far as the proxy |
| None | Nothing. Whoever can connect gets the agent | Do not |

No proof is for a setup that is already closed, such as a loopback-only tool on one person's machine. A browser is refused it unless its origin is listed, because any page could otherwise drive a local agent. A client must report it for what it is: `identity.verified` is false in [ACP in the browser](browser.md).

A connection proves its peer before the first ACP message:

- **In band**: [coqui-peer-webcrypto](https://github.com/Boriken-Dev/coqui-peer-webcrypto), on the channel, with a pinned peer key. It ends in a session: each ACP message after it is one sealed record, in a binary WebSocket frame or one WebTransport frame, and each side ends with a close record, so a connection cut before it is seen as cut. A plain message after the handshake is never taken. It does not depend on TLS, so a reverse proxy that ends TLS can carry it. On WebSocket a handshake message may be a text or a binary frame.
- **mTLS**: through coqui-peer-tls for a native client, or the browser's own certificate store for a browser. The stream is then plain ACP from its first byte, and protected only as far as that TLS goes.

A URL, a host name or server TLS alone proves no peer.

## No automatic retry

A prompt whose answer was lost has an unknown outcome. After a new connection proves the same peer, the client reconciles by `session/resume` or `session/load`, and a
person or the application decides what to send again.
