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

## Keeping an agent through a reconnect

`coqui-acp-bridge serve --linger SECONDS` keeps a link's agent that long after the link ends, and gives it to the next connection that proves the same key, never to another, and only while that key is still accepted. Nothing else about the connection changes: the bridge reads no ACP, so nothing says an agent was kept, and the client asks with `session/resume`. An agent past its time, or one that ended, is a fresh agent on the new connection, which answers in its own words; what an agent that ended wrote while nobody was connected comes first.

The key is the whole of what is checked. Where several programs on one machine prove the same key, as every tool of a user does under `--paired`, any of them can be given another's kept agent, with its session and what it wrote.

A client talking to a bridge that may do this has to allow for four things:

- **`initialize` comes twice** to the same agent.
- **What the agent wrote while nobody was connected comes first**, up to a limit, ahead of the answer to `initialize`: updates of a session not yet resumed, and requests such as `session/request_permission`. A client takes a `session/update` or a `session/request_permission` only for a session it opened or resumed on this connection, and cancels any other request without asking anyone, so an old session's output never feeds a new one and nobody answers for a session they cannot see. A request of the session about to be resumed that arrives before the client asked to resume it is cancelled too: the turn goes on without that tool.
- **Answers to the old connection's requests arrive on the new one, with their old ids.** In version 1 the answer to a `session/prompt` is the only thing that says a turn ended, so a client that wants to see one finish keeps that id across the reload. A client's ids must not repeat across connections to one agent, or a late answer is taken for a new request's: these clients start each connection's ids at a random point (Dart) or use UUIDs (Rust).
- **A reconnect can always hide a gap.** What a link had accepted and never delivered when it was cut is lost with no notice, since nothing acknowledges it.

### `_dev.boriken.coqui.acp/output_lost`

Past the limit the oldest lines are dropped, since the newest hold the answer that ends a turn. The next connection's first message then says so:

```json
{"jsonrpc":"2.0","method":"_dev.boriken.coqui.acp/output_lost","params":{"lines":12,"bytes":3456}}
```

ACP has no message for this. A notification whose method starts with `_` is the extension point it offers, and a client that does not know this one ignores it, as ACP requires, and sees only the gap it would have had anyway. It is the one message the bridge writes itself, and it travels inside the proven session like any other. A client that reads it reconciles by `session/load`, or tells the person. These clients report it as `Event::OutputLost` (Rust) and `AcpOutputLost` (Dart).
