# `coqui-acp-bridge` — API header

Header-file-style reference for the `coqui-acp-bridge` crate: its command line and its small library, so it can be used without reading the source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

No Cargo features. MSRV Rust 1.89. Version `0.0.0-alpha.0`, not published. It depends on `coqui-web`, `coqui-peer`, `coqui-peer-webcrypto`, `coqui-peer-tls`, `coqui-net-acl`, `clap` and `rustls`.

No `unsafe` code (`#![forbid(unsafe_code)]`). Public modules: `agent`, `auth`, `pairing`, `relay`.

## Command line

```text
coqui-acp-bridge keygen PATH
coqui-acp-bridge serve --identity FILE (--allow PEER... | --allow-store DIR) [--listen ADDR] [--tls-identity FILE] [--webtransport-port PORT]
                       [--origin URL...] [--permit NET...] [--deny NET...] [--proof-seconds N] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge serve --tls-identity FILE --allow-cert FP... [--listen ADDR] [--origin URL...] [--permit NET...] [--deny NET...] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge dial URL --identity FILE (--peer ID... | --trust-store DIR) [--ca-file FILE [--bearer-file FILE]] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge dial URL --tls-identity FILE (--expect FP... | --ca-file FILE) [--bearer-file FILE] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge pair-accept --listen ADDR --identity FILE --store DIR [--tls-identity FILE] [--permit NET...] [--deny NET...]
coqui-acp-bridge pair-dial URL --identity FILE --store DIR [--pin-file FILE] [--ca-file FILE]
```

- **In-band proof (`--identity`):** the WebCrypto handshake, then every message inside its session. It does not depend on TLS, so it works through a reverse proxy that ends TLS, and the proxy can neither read nor inject. The handshake may be text or binary frames; each record after it is one binary frame.
- **Mutual TLS (`--tls-identity`, `--allow-cert`):** the stream is plain ACP protected by that TLS, so it ends where TLS ends and a proxy must vouch for the certificate.
- `serve` with the in-band proof prints its peer id, one line, on stdout. Each proven connection gets an agent process of its own, up to `--max-peers` (default 4), and another is turned away while they run. Handshakes and proofs run apart, so a slow peer holds up nobody; past 64 unproven connections a new one is dropped. An unproven peer may send at most 3072 bytes in a message. Plain `ws` is refused beyond loopback, where it is for a proxy in front. A flag that cannot apply to the mode is refused.
- `--allow-store` is read at each handshake and every second of a live link, so a peer removed from the store loses its link within about a second.
- `dial` keeps the agent across reconnects, ends with the agent's status, and stops on a wrong certificate, a remote that proves the wrong key, or a remote removed from `--trust-store`. A remote it cannot reach is tried again.
- `pair-accept` prints the PIN, then its peer id, and waits until someone uses the PIN or it expires; connections run apart, so one that says nothing costs neither the PIN nor the wait, and one wrong try ends it. `pair-dial` reads the PIN from `--pin-file` or standard input and prints the other side's peer id. Both record the peer in `--store`. A pairing connection never serves an agent: reconnect pinned.
- An identity file holds a P-256 key, created by `keygen`, and by `dial`, `pair-accept` and `pair-dial` when absent. A TLS identity is a separate PEM certificate and key.
- Exit status: the agent's when it ended the bridge; 2 for a bad configuration, a failed pairing, or a remote that failed its proof.

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

## `coqui_acp_bridge::pairing`

```rust
pub fn desk() -> Result<PairingDesk, Error>   // PIN runs only
pub fn respond<C: Channel>(channel: &C, identity: &Identity, store: &TrustStore, desk: &PairingDesk, source: Option<SocketAddr>, limit: Duration) -> Result<Paired, Error>
pub fn join<C: Channel>(channel: &C, identity: &Identity, pin: Pin, store: &TrustStore, limit: Duration) -> Result<Paired, Error>
```

PIN pairing over any channel in binary frames, with the PIN shown by the responder: the WebCrypto handshake admitting anyone who proves a key, which yields a `PairingSession` that can carry nothing but this exchange, sealed and bound to a secret export of it. Only PIN mode, and a joiner asking for another is refused. It pairs the WebCrypto key. The connection then ends, and the peer returns pinned.

## `coqui_acp_bridge::auth`

```rust
pub const MAX_PROOF_MESSAGE: usize = 3072;
pub const RECORD_OVERHEAD: usize = 17;
pub fn read_identity(path: &Path) -> Result<Identity, Error>
pub fn create_identity(path: &Path) -> Result<PeerId, Error>
pub fn prove<M: Method, C: Channel>(method: &M, channel: &C, limit: Duration, max_message: usize) -> Result<Established<<M::Handshake as Handshake>::Output>, Error>
pub fn prove_out<M: Method, C: Channel>(method: &M, channel: &C, limit: Duration, max_message: usize) -> Result<Established<<M::Handshake as Handshake>::Output>, Error>
pub struct Sealed<'a, C>;   // impl Channel
pub fn Sealed::new(channel: &'a C, session: Session) -> Sealed<'a, C>
pub fn Sealed::with_check(self, allowed: impl Fn() -> bool + Send + Sync + 'a) -> Sealed<'a, C>
```

`create_identity` never replaces a file. `prove` answers and `prove_out` connects, within `limit`, refusing a message over 3072 bytes, and set the channel's limit to `max_message` before the proof's last message goes out. A refused proof is `Error::Proof`; silence or a failed channel is `Error::Channel`. **Run everything after a proof through `Sealed`**: a plain message on the same channel comes from whoever carries it. `Sealed` closes the channel on anything that does not open, and on `with_check` saying no, which is asked about once a second. Its `close` sends the session's close record first; `receive` answers `None` only after the peer's, and `Failure::PathLost` for a connection cut before it, which is how a relay would hide what it held back. After `Failure::NoProgress` from its `send`, send the same message again.

## Errors

`coqui_acp_bridge::Error` (`#[non_exhaustive]`): `Channel(Failure)`, `Proof(String)`, `Agent(io::Error)`, `Identity(String)`. A message never quotes a key file.

## Gotchas

- A browser cannot do the PIN exchange from this crate; `pair-dial` is native.
- A joiner that asks to show its own PIN holds a run until the shown PIN expires, since the core's responder waits for an operator. It pairs nobody, and `pair-accept` runs 16 at once, so strangers could hold pairing up for that long: use `--permit`.
- Behind a reverse proxy every peer has the proxy's address, so `--permit`, `--deny` and the store's per-source limit on waiting entries see the proxy. Bind to loopback and filter at the proxy.
- `dial` keeps the agent across links, so lines not yet sent go to the next link, which may be another allowed peer.
- A key file's permissions are set only on Unix; elsewhere it takes its directory's. Nothing checks them on reading. Keep keys in a directory only their owner can read.
- Removing a peer from a store does not affect `--allow-cert`, which has no store.
