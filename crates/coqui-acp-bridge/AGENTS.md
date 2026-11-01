# `coqui-acp-bridge` — API header

Header-file-style reference for the `coqui-acp-bridge` crate: its command line and its small library, so it can be used without reading the source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

No Cargo features. MSRV Rust 1.89. Version `0.0.0-alpha.0`, not published. It depends on `coqui-web`, `coqui-peer`, `coqui-peer-webcrypto`, `coqui-peer-tls`, `coqui-net-acl`, `clap` and `rustls`.

No `unsafe` code (`#![forbid(unsafe_code)]`). Public modules: `agent`, `auth`, `relay`.

## Command line

```text
coqui-acp-bridge keygen PATH
coqui-acp-bridge serve (--identity FILE --allow PEER... | --allow-cert FP... | --allow-store DIR) [--listen ADDR] [--tls-identity FILE]
                       [--webtransport-port PORT] [--origin URL...] [--permit NET...] [--deny NET...] [--proof-seconds N] [--max-peers N] -- PROGRAM ARGS...
coqui-acp-bridge dial URL --tls-identity FILE (--expect FP... | --trust-store DIR | --ca-file FILE) [--bearer-file FILE] [--max-backoff-seconds N] -- PROGRAM ARGS...
coqui-acp-bridge pair-accept --listen ADDR --tls-identity FILE --store DIR [--permit NET...] [--deny NET...]
coqui-acp-bridge pair-dial URL --tls-identity FILE --store DIR [--pin-file FILE] [--ca-file FILE]
```

- `serve` with `--allow` prints its peer id, one line, on stdout. Each proven connection gets an agent process of its own, up to `--max-peers` (default 4), and another is turned away while they run. Handshakes and proofs run apart, so a slow peer holds up nobody; past 64 unproven connections a new one is dropped. An unproven peer may send at most 3072 bytes in a message. Plain `ws` is refused beyond loopback. `--webtransport-port`, `--identity` and `--proof-seconds` belong to `--allow` and are refused with mutual TLS. `--origin` applies to both.
- `dial` keeps the agent across reconnects, ends with the agent's status, and stops on a wrong remote certificate.
- `pair-accept` prints the PIN, then its fingerprint, and waits until someone uses the PIN or it expires; connections run apart, so one that says nothing costs neither the PIN nor the wait, and one wrong try ends it. `pair-dial` reads the PIN from `--pin-file` or standard input and prints the other side's fingerprint. Both record the peer in `--store`.
- A TLS identity is a PEM certificate and key, created by `dial`, `pair-accept` and `pair-dial` when absent. A `keygen` identity is a separate P-256 key, for the browser method.
- Exit status: the agent's when it ended the bridge; 2 for a bad configuration or a failed pairing.

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

PIN pairing over any channel in binary frames, with the PIN shown by the responder. The responder opens with a random id both ends bind the run to. Only PIN mode: an invitation sends its secret as is and the device flow needs a verified channel, so neither is offered on a channel that proves nothing, and a joiner asking for one is refused.

## `coqui_acp_bridge::auth`

```rust
pub const MAX_PROOF_MESSAGE: usize = 3072;
pub fn read_identity(path: &Path) -> Result<SigningKey, Error>
pub fn create_identity(path: &Path) -> Result<PeerId, Error>
pub fn method(key: SigningKey, allowed: BTreeSet<PeerId>) -> Result<WebCryptoMethod, Error>
pub fn prove<C: Channel>(method: &WebCryptoMethod, channel: &C, limit: Duration, max_message: usize) -> Result<PeerId, Error>
```

`create_identity` never replaces a file. `prove` runs the responder's side within `limit`, refusing a message over 3072 bytes, and sets the channel's limit to `max_message` before the proof's last message goes out. Nothing but the proof crosses until it succeeds.

## Errors

`coqui_acp_bridge::Error` (`#[non_exhaustive]`): `Channel(Failure)`, `Proof(String)`, `Agent(io::Error)`, `Identity(String)`. A message never quotes a key file.

## Gotchas

- Pairing runs in binary messages over the bridge's own WebSocket, so a proxy that ends TLS can carry it: the PIN authenticates both keys, and a relay that swaps a key or changes a message fails on both sides. `pair-dial` with no `--ca-file` takes any certificate for that exchange. Using the stored key through such a proxy afterwards needs the proxy to forward the client certificate.
- A browser cannot do the PIN exchange.
- **`--allow`'s proof shows who holds a key, not who sends what follows.** It binds nothing after itself, so whoever relays the proof holds the channel: a client lured to another server, or anything ending TLS in front. Use it where that cannot happen, until the method binds later messages.
- Removing a peer from a store refuses its next connection, resuming included; a live one stays until it ends.
- `dial` keeps the agent across links, so lines not yet sent go to the next link, which may be another allowed peer.
- A key file's permissions are set by this crate only on Unix for `keygen`; elsewhere it takes its directory's. Nothing checks them on reading. Keep keys in a directory only their owner can read.
