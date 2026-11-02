# `coqui-acp-bridge` — API header

Header-file-style reference for the `coqui-acp-bridge` crate: its command line and its small library, so it can be used without reading the source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

No Cargo features. MSRV Rust 1.89. Version `0.0.0-alpha.0`, not published. It depends on `coqui-web`, `coqui-peer`, `coqui-peer-webcrypto`, `coqui-peer-tls`, `coqui-net-acl`, `clap`, `rustls`, `mdns-sd`, `serde_json` and `url`.

No `unsafe` code (`#![forbid(unsafe_code)]`). Public modules: `agent`, `relay`. Proof, pairing, discovery and the companion are `coqui-agentic-peer`'s; see its `AGENTS.md`.

## Command line

```text
coqui-acp-bridge keygen PATH
coqui-acp-bridge serve --identity FILE (--allow PEER... | --allow-store DIR) [--listen ADDR] [--tls-identity FILE] [--webtransport-port PORT]
                       [--origin URL...] [--permit NET...] [--deny NET...] [--proof-seconds N] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge serve --tls-identity FILE (--allow-cert FP... | --allow-cert-store DIR | --client-ca FILE) [--listen ADDR] [--origin URL...] [--permit NET...] [--deny NET...] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge serve --no-peer-proof [--tls-identity FILE] [--listen ADDR] [--origin URL...] [--permit NET...] [--deny NET...] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge dial URL --identity FILE (--peer ID... | --trust-store DIR) [--ca-file FILE [--bearer-file FILE]] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge dial URL --tls-identity FILE (--expect FP... | --ca-file FILE) [--bearer-file FILE] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge dial wss://... (--expect FP... | --ca-file FILE) [--bearer-file FILE] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge dial ws://loopback --no-peer-proof [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge pair-accept --listen ADDR [--identity FILE] [--store DIR] [--tls-identity FILE] [--permit NET...] [--deny NET...]
coqui-acp-bridge pair-dial URL [--identity FILE] [--store DIR] [--pin-file FILE] [--ca-file FILE]
coqui-acp-bridge serve --paired [--listen ADDR] [--tls-identity FILE | --tls-short-lived] ... -- PROGRAM ARGS...
coqui-acp-bridge dial URL --paired [--ca-file FILE [--bearer-file FILE]] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge discover [--seconds N]
coqui-acp-bridge companion install --extension-id ID...
coqui-acp-bridge companion uninstall
coqui-acp-bridge grant PEER TOKEN [--revoke]
coqui-acp-bridge forward URL (--paired | --identity KEY --peer ID) [--local LOCAL=TARGET]... [--allow-target HOST:PORT]...
```

Every `serve` form also takes `--advertise LABEL` and `--advertise-host NAME`. With the in-band proof it also takes `--service TOKEN=PROGRAM...` and `--grant PEER=TOKEN...`, and `dial` takes `--fallback URL...`. With the in-band proof, `--tls-short-lived` replaces `--tls-identity`.

- **Several protocols on one link:** `--service mcp=PROGRAM` serves another protocol beside the agent after `--`, which is `acp/1`. A client opens streams on one proven link, and the first message tells a link from a plain ACP client, which keeps working. A proof admits a peer and grants nothing: `--grant PEER=TOKEN` names what it may open, with everything for an allowed peer when none is listed. Under `--paired`, a peer gets `acp/1` for being paired, and `grant PEER TOKEN` adds or takes away anything else, counted at once on a live link, with no second pairing. A store that cannot be read refuses. `--advertise` then says what is offered, as a hint.
- **Forwarding TCP:** `serve --forward` serves `tcp/1`, and grants nothing: a peer needs `--grant PEER=tcp/1:HOST:PORT` or `grant PEER tcp/1:HOST:PORT`, exact, with no wildcards. `forward --local` listens here and reaches a target from the remote. `serve --reverse LOCAL=PEER=TARGET` and `forward --allow-target` are the other direction, with the dialing side deciding. A listener has to be on loopback, and anyone on this machine who can reach it gets what this peer was granted.
- **Failover:** `dial --fallback URL` tries the next endpoint when one cannot be reached. Each has to prove what the first would, and they share a scheme.
- **One pairing:** `pair-accept` and `pair-dial` default to this user's own identity and trust store (`coqui_agentic_peer::home`), and `serve --paired` and `dial --paired` read them, so one pairing serves every command, with nothing to name.
- **In-band proof (`--identity`):** the WebCrypto handshake, then every message inside its session. It does not depend on TLS, so it works through a reverse proxy that ends TLS, and the proxy can neither read nor inject. The handshake may be text or binary frames; each record after it is one binary frame.
- **Mutual TLS:** the client's certificate by pin, trust store or CA. The stream is plain ACP protected by that TLS, so it ends where TLS ends and a proxy must vouch for the certificate.
- **TLS checking only the server:** `dial` with `--expect` or `--ca-file` and nothing else. `serve --no-peer-proof --tls-identity` is its other end.
- **No proof (`--no-peer-proof`):** whoever can connect gets the agent, which can run what the agent runs. `serve` allows it only on loopback, or with `--permit`, and refuses a browser unless its `--origin` is listed, since any page could otherwise drive it. `dial` allows it only for `ws://` on loopback.
- `serve` with the in-band proof prints its peer id, one line, on stdout. Each proven connection gets an agent process of its own, up to `--max-peers` (default 4), and another is turned away while they run. Handshakes and proofs run apart, so a slow peer holds up nobody; past 64 unproven connections a new one is dropped. An unproven peer may send at most 3072 bytes in a message. Plain `ws` is refused beyond loopback, where it is for a proxy in front. A flag that cannot apply to the mode is refused.
- `--allow-store` is read at each handshake and every second of a live link, so a peer removed from the store loses its link within about a second.
- `dial` keeps the agent across reconnects, ends with the agent's status, and stops on a wrong certificate, a remote that proves the wrong key, or a remote removed from `--trust-store`. A remote it cannot reach is tried again.
- `pair-accept` prints the PIN, then its peer id, and waits until someone uses the PIN or it expires; connections run apart, so one that says nothing costs neither the PIN nor the wait, and one wrong try ends it. `pair-dial` reads the PIN from `--pin-file` or standard input and prints the other side's peer id. Both record the peer in `--store`. A pairing connection never serves an agent: reconnect pinned.
- An identity file holds a P-256 key, created by `keygen`, and by `dial`, `pair-accept` and `pair-dial` when absent. A TLS identity is a separate PEM certificate and key.
- `serve` says `listening on` on standard error only once it can also be found. `--advertise LABEL` is off by default. On loopback it writes a descriptor; otherwise it publishes `_acp-agent._tcp` on every interface the listener is bound to, with no secret in it. `--advertise-host` publishes a host name for a certificate a browser accepts; without it, clients get an address. A killed `serve` leaves a descriptor, which the next search removes. The DNS-SD record is not withdrawn on a kill, and expires.
- `serve --tls-short-lived` makes a certificate for this run, in memory, which a browser can pin over WebTransport with no certificate authority, and advertises its hash as `wth`. Browsers take one valid for 14 days at most, so `serve` ends with status 3 after 13 days: restart it, and a client finds the new hash by searching again. It needs the in-band proof, which is what authenticates.
- `discover` prints one JSON object per line: `label`, `url`, `peer`, `source` (`dns-sd` or `local`), `webtransport`, `webtransportHash`, `acpVersions`. It is what they claim.
- `companion install` registers this program, where it is now, for this user's Chrome, Chromium and Edge: the manifest and, on Windows, the registry keys. A browser starts it with `chrome-extension://…/` as its first argument, which selects native messaging instead of the command line.
- Exit status: 3 when a short-lived certificate is ending; the agent's when it ended the bridge; 2 for a bad configuration, a failed pairing, or a remote that failed its proof.

## `coqui_acp_bridge::relay`

```rust
pub fn relay<C: Channel>(channel: &C, agent: &Agent) -> LinkEnd
pub enum LinkEnd { AgentEnded, AgentStuck, PeerClosed, Failed(Failure), Protocol }  // non_exhaustive
```

Passes complete messages until one side ends, reading none of them. A message holding a raw newline ends the link as `Protocol`, since it would forge a second line. A slow side feels back-pressure; a line that waits past the limit ends the link, as `AgentStuck` when the agent stopped reading. Nothing is replayed.

## `coqui_acp_bridge::agent`

```rust
pub fn Agent::spawn(program: &OsString, args: &[OsString], max_line: usize) -> io::Result<Agent>
pub const fn Agent::with_write_limit(self, limit: Duration) -> Agent   // default 30 s
pub fn Agent::try_status(&self) -> io::Result<Option<ExitStatus>>
pub fn Agent::stop(&self) -> io::Result<ExitStatus>
```

Standard error stays on this process's. Dropping an `Agent` ends the process, not any process it started.

## Gotchas

- A browser takes a self-signed certificate only for WebTransport with `serverCertificateHashes`, so a `wss://` address `discover` gives opens in a browser only with a certificate it accepts. `--tls-short-lived` is the way without one.
- On Windows, the first time this program listens or joins multicast the firewall may ask, and an answer of no leaves an inbound block rule for that path. `discover` then sees agents on this machine and none on others.
- mDNS stays on the link: it does not cross a router or a site-to-site VPN. `discover` does not check that an answer came from the link, so anyone who can reach UDP 5353 here can send a record. A record is a claim either way.
- `serve --advertise` beyond loopback needs the in-band proof, since a client has to be able to check the key a record names; a record naming no key is ignored.
- A browser cannot do the PIN exchange from this crate; `pair-dial` is native.
- Behind a reverse proxy every peer has the proxy's address, so `--permit`, `--deny` and the store's per-source limit on waiting entries see the proxy. Bind to loopback and filter at the proxy.
- `dial` keeps the agent across links, so lines not yet sent go to the next link, which may be another allowed peer.
- A key file's permissions are set only on Unix; elsewhere it takes its directory's. Nothing checks them on reading. Keep keys in a directory only their owner can read.
- Removing a peer from a store does not affect `--allow-cert`, which has no store.
