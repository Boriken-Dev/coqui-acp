# `coqui-acp-bridge` — API header

Header-file-style reference for the `coqui-acp-bridge` crate: its command line and its small library, so it can be used without reading the source. It ships inside the package and is self-contained. Development documentation lives with the source at <https://github.com/Boriken-Dev/coqui-acp>.

No Cargo features. MSRV Rust 1.89. Version `0.0.0-alpha.0`, not published. It depends on `coqui-web`, `coqui-peer`, `coqui-peer-webcrypto`, `coqui-peer-tls`, `coqui-net-acl`, `clap`, `rustls`, `mdns-sd`, `serde_json` and `url`.

No `unsafe` code (`#![forbid(unsafe_code)]`). Public modules: `agent`, `auth`, `companion`, `discovery`, `pairing`, `relay`.

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
coqui-acp-bridge pair-accept --listen ADDR --identity FILE --store DIR [--tls-identity FILE] [--permit NET...] [--deny NET...]
coqui-acp-bridge pair-dial URL --identity FILE --store DIR [--pin-file FILE] [--ca-file FILE]
coqui-acp-bridge discover [--seconds N]
coqui-acp-bridge companion install --extension-id ID...
coqui-acp-bridge companion uninstall
```

Every `serve` form also takes `--advertise LABEL` and `--advertise-host NAME`. With the in-band proof, `--tls-short-lived` replaces `--tls-identity`.

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

## `coqui_acp_bridge::pairing`

```rust
pub fn desk() -> Result<PairingDesk, Error>   // PIN runs only
pub fn respond<C: Channel>(channel: &C, identity: &Identity, store: &TrustStore, desk: &PairingDesk, source: Option<SocketAddr>, limit: Duration) -> Result<Paired, Error>
pub fn join<C: Channel>(channel: &C, identity: &Identity, pin: Pin, store: &TrustStore, limit: Duration) -> Result<Paired, Error>
```

PIN pairing over any channel in binary frames, with the PIN shown by the responder: the WebCrypto handshake admitting anyone who proves a key, which yields a `PairingSession` that can carry nothing but this exchange, sealed and bound to a secret export of it. Only PIN mode with the PIN the responder shows, and a joiner asking for anything else is refused at once. It pairs the WebCrypto key. The connection then ends, and the peer returns pinned.

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

## `coqui_acp_bridge::discovery`

```rust
pub const SERVICE_TYPE: &str = "_acp-agent._tcp.local.";
pub const DEFAULT_PATH: &str = "/acp";
pub const DESCRIPTORS_ENV: &str = "COQUI_ACP_DESCRIPTORS";
pub const MAX_CANDIDATES: usize = 64;
pub struct Candidate { pub label: String, pub url: String, pub peer: Option<String>, pub source: Source, pub webtransport: Option<u16>, pub webtransport_hash: Option<String>, pub acp_versions: Vec<u32> }  // non_exhaustive
pub enum Source { Network, ThisMachine }  // non_exhaustive
pub fn Candidate::to_json(&self) -> Value
pub fn Advertisement::new(label: &str, tls: bool) -> Result<Advertisement, Error>
pub fn Advertisement::with_peer(self, peer: Option<String>) -> Advertisement
pub const fn Advertisement::with_webtransport(self, port: Option<u16>) -> Advertisement
pub fn Advertisement::with_webtransport_hash(self, hash: Option<String>) -> Advertisement
pub fn Advertisement::with_host(self, host: Option<String>) -> Result<Advertisement, Error>
pub fn advertise(advertisement: &Advertisement, bound: SocketAddr) -> Result<Advertised, Error>   // withdrawn on drop
pub fn discover(wait: Duration) -> Vec<Candidate>                 // descriptors, then the link; a failed mDNS start leaves the descriptors
pub fn browse(wait: Duration) -> Result<Vec<Candidate>, Error>    // the link only
pub fn read_descriptors(dir: &Path) -> Vec<Candidate>
pub fn descriptors_dir() -> Option<PathBuf>
pub fn candidate_from(record: &Record<'_>) -> Option<Candidate>
```

`candidate_from` reads an answer anyone on the link can send: it refuses a loopback, link-local or multicast address, anything plain, and an unknown `txtvers` or `proto`, cuts the label down, and treats a malformed peer as none. A descriptor may name only loopback.

## `coqui_acp_bridge::companion`

```rust
pub const HOST_NAME: &str = "dev.boriken.coqui_acp_bridge";
pub const MAX_REQUEST: usize = 65_536;
pub const MAX_REPLY: usize = 1_048_576;
pub fn started_by_browser(first_argument: &str) -> bool
pub fn read_message(input: &mut impl Read) -> io::Result<Option<Vec<u8>>>
pub fn write_message(output: &mut impl Write, message: &[u8]) -> io::Result<()>
pub fn run(input: &mut impl Read, output: &mut impl Write) -> io::Result<()>
pub fn install(extensions: &[String]) -> Result<Vec<String>, Error>
pub fn uninstall() -> Result<(), Error>
```

It answers `{ "op": "discover", "seconds": N }`, at most 10 seconds, and nothing else; it carries no agent traffic.

## Errors

`coqui_acp_bridge::Error` (`#[non_exhaustive]`): `Channel(Failure)`, `Proof(String)`, `Agent(io::Error)`, `Identity(String)`, `Discovery(String)`. A message never quotes a key file.

## Gotchas

- A browser takes a self-signed certificate only for WebTransport with `serverCertificateHashes`, so a `wss://` address `discover` gives opens in a browser only with a certificate it accepts. `--tls-short-lived` is the way without one.
- On Windows, the first time this program listens or joins multicast the firewall may ask, and an answer of no leaves an inbound block rule for that path. `discover` then sees agents on this machine and none on others.
- mDNS stays on the link: it does not cross a router or a site-to-site VPN.
- A browser cannot do the PIN exchange from this crate; `pair-dial` is native.
- Behind a reverse proxy every peer has the proxy's address, so `--permit`, `--deny` and the store's per-source limit on waiting entries see the proxy. Bind to loopback and filter at the proxy.
- `dial` keeps the agent across links, so lines not yet sent go to the next link, which may be another allowed peer.
- A key file's permissions are set only on Unix; elsewhere it takes its directory's. Nothing checks them on reading. Keep keys in a directory only their owner can read.
- Removing a peer from a store does not affect `--allow-cert`, which has no store.
