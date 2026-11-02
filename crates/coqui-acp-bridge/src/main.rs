//! `coqui-acp-bridge`: argument parsing and process concerns; the logic is in the library.
#![forbid(unsafe_code)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs
)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use clap::{Parser, Subcommand};
use coqui_acp_bridge::agent::Agent;
use coqui_acp_bridge::relay::{LinkEnd, relay};
use coqui_agentic_peer::auth::Sealed;
use coqui_agentic_peer::discovery::{Advertised, Advertisement};
use coqui_agentic_peer::forward::{self, Target};
use coqui_agentic_peer::link::{self, Accepted, Prefilled, Services};
use coqui_agentic_peer::mux::{self, LinkStream, Side};
use coqui_agentic_peer::peers::{FilePeers, NoEvents, PeerRecord, PeerStore};
use coqui_agentic_peer::{Error, auth, companion, discovery, home, pairing};
use coqui_net_acl::{Acl, Action, IpNet, Rule};
use coqui_peer_tls as tls;
use coqui_peer_webcrypto::pair::{CodeAlphabet, Pin};
use coqui_peer_webcrypto::{Identity, PeerId, Trust, TrustStore, WebCrypto};
use coqui_web::websocket::Frames;
use coqui_web::{Channel, Dialer, Failure, Kind, Limits, websocket, webtransport};
use url::Url;

const MAX_LINE: usize = 16 * 1024 * 1024;
/// Connections that have not proved themselves yet, each a thread for at most the handshake and proof limits.
const MAX_UNPROVEN: usize = 64;
/// Connections to one `pair-accept` at once.
const MAX_PAIRING_RUNS: usize = 16;
/// How long one connection may take to pair. `pair-dial` asks for the PIN before it connects, so no person is waited for.
const PAIRING_RUN: Duration = Duration::from_secs(60);
/// How long a remote may take to prove itself to `dial`.
const DIAL_PROOF: Duration = Duration::from_secs(15);
/// At most what a browser pins, with a margin.
const SHORT_LIVED_VALIDITY: Duration = Duration::from_secs(14 * 86_400 - 60);
const SHORT_LIVED_RUN: Duration = Duration::from_secs(13 * 86_400);
const EXIT_CERTIFICATE_ENDING: i32 = 3;
/// Messages an unproven peer may have waiting.
const UNPROVEN_PENDING: usize = 8;

/// Put a stdio Agent Client Protocol agent on the network, under a proven peer identity.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Show a PIN and wait for one peer to pair with it. Prints the PIN, then this side's fingerprint. One wrong try ends it.
    PairAccept(PairAccept),
    /// Pair with a waiting `pair-accept`, using its PIN. Prints the other side's fingerprint.
    PairDial(PairDial),
    /// Create an identity file and print its peer id.
    Keygen {
        /// Where to write the key. An existing file is never replaced.
        path: PathBuf,
    },
    /// Wait for peers, prove each, and give it an agent of its own.
    Serve(Serve),
    /// Connect out to a peer, for an agent behind NAT. Reconnects and keeps the agent running.
    Dial(Dial),
    /// Forward TCP over a link: listen here for what the far side was asked to reach, and let it reach what is named here. Runs
    /// until stopped, connecting again when the link goes.
    Forward(Forward),
    /// Let a peer this user paired with open a protocol, or with `--revoke` no longer. It counts at once, on a live link too,
    /// with no second pairing.
    Grant {
        /// The peer id.
        peer: String,
        /// The protocol token, or `tcp/1:HOST:PORT` for a target a forward may reach, which also grants `tcp/1`. Taking a target away
        /// keeps `tcp/1`, which reaches nothing without one.
        protocol: String,
        /// Take it away instead.
        #[arg(long)]
        revoke: bool,
    },
    /// Search this machine and the link for agents, and print each as one line of JSON. What it prints is what they claim.
    Discover {
        /// How long to listen on the link, in seconds.
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// Let a browser extension ask this program who is around, over native messaging.
    Companion {
        #[command(subcommand)]
        action: CompanionAction,
    },
}

#[derive(Subcommand, Debug)]
enum CompanionAction {
    /// Register this program, where it is now, for this user's Chrome, Chromium and Edge.
    Install {
        /// An extension allowed to start it, by its 32-letter id. Repeat for more.
        #[arg(long = "extension-id", required = true)]
        extensions: Vec<String>,
    },
    /// Remove that registration.
    Uninstall,
}

#[derive(clap::Args, Debug)]
struct PairAccept {
    /// The address to wait on, for a WebSocket. On loopback it is plain, for a reverse proxy in front; any other address
    /// needs `--tls-identity`. The PIN authenticates either way.
    #[arg(long)]
    listen: SocketAddr,
    /// This side's identity, whose key is the one paired. Created if absent. Without it, this user's own.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// A TLS identity for wss://, when this address is not loopback.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
    /// The directory of the trust store that records the peer. Without it, this user's own, which `serve --paired` reads.
    #[arg(long)]
    store: Option<PathBuf>,
    /// How long the PIN waits for someone to use it, in seconds.
    #[arg(long, default_value_t = 900)]
    timeout_seconds: u64,
    /// A source network allowed to connect at all. Repeat for more. With any listed, everything else is dropped first.
    #[arg(long = "permit")]
    permit: Vec<String>,
    /// A source network dropped before its handshake. Repeat for more. Takes precedence over `--permit`.
    #[arg(long = "deny")]
    deny: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct PairDial {
    /// The URL of the `pair-accept`, any path, such as wss://host/v1/agent/pair. ws:// is for loopback only.
    url: Url,
    /// This side's identity, whose key is the one paired. Created if absent. Without it, this user's own.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// The directory of the trust store that records the peer. Without it, this user's own, which `dial --paired` reads.
    #[arg(long)]
    store: Option<PathBuf>,
    /// A file holding the PIN; without it, standard input. A PIN is never an argument, since arguments are visible to
    /// other users.
    #[arg(long)]
    pin_file: Option<PathBuf>,
    /// A PEM bundle the server's certificate must chain to. Without it any certificate is taken for this exchange, since the
    /// PIN authenticates the peer, not TLS.
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// How long the exchange may take, in seconds.
    #[arg(long, default_value_t = 900)]
    timeout_seconds: u64,
}

#[derive(clap::Args, Debug)]
struct Serve {
    /// The address to listen on. Plain WebSocket is refused beyond loopback.
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: SocketAddr,
    /// This bridge's identity (see `keygen`), for `--allow` and `--allow-store`.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// A peer id allowed in, proved in-band. Repeat for more. With nobody allowed, the bridge refuses to start: nothing
    /// is trusted on first use.
    #[arg(long = "allow", required_unless_present_any = ["allow_cert", "allow_cert_store", "client_ca", "allow_store", "no_peer_proof", "paired"])]
    allow: Vec<String>,
    /// Allow in whoever this user has paired with, proved in-band, under this user's own identity: one pairing serves every
    /// command. Takes the place of `--identity` and `--allow-store`.
    #[arg(long, conflicts_with_all = ["identity", "allow", "allow_store"])]
    paired: bool,
    /// A trust store whose accepted peers are allowed in, proved in-band, read on each connection and while it lasts, so a
    /// peer paired or removed later takes effect at once.
    #[arg(long)]
    allow_store: Option<PathBuf>,
    /// A TLS identity (certificate and key, PEM) for wss:// and WebTransport. Without it, plain ws:// on loopback only, where
    /// a reverse proxy may stand in front: the in-band proof and its session do not depend on TLS.
    #[arg(long, conflicts_with = "tls_short_lived")]
    tls_identity: Option<PathBuf>,
    /// Make a certificate for this run and keep it in memory, for a browser to pin over WebTransport with no certificate
    /// authority. Browsers take only one valid for 14 days at most, so `serve` ends with status 3 after 13: restart it, and a
    /// client finds the new hash by looking for it again. Needs the in-band proof, which is what authenticates.
    #[arg(long)]
    tls_short_lived: bool,
    /// A client certificate fingerprint allowed in over mutual TLS, in place of the in-band proof. Repeat for more. Needs
    /// `--tls-identity`. The stream is then plain ACP protected only by that TLS, so it ends where TLS ends.
    #[arg(long = "allow-cert")]
    allow_cert: Vec<String>,
    /// A trust store of client certificates allowed in over mutual TLS, read on each connection. Needs `--tls-identity`.
    #[arg(long)]
    allow_cert_store: Option<PathBuf>,
    /// A PEM bundle of certificate authorities a client certificate may chain to, over mutual TLS. Needs `--tls-identity`.
    #[arg(long)]
    client_ca: Option<PathBuf>,
    /// Prove nothing about the peer: whoever can connect gets the agent. Only on loopback, or with `--permit` naming who can
    /// reach it, and a browser needs its `--origin` listed, or any page it opens could drive the agent. With `--tls-identity` a
    /// client can still pin this server.
    #[arg(long)]
    no_peer_proof: bool,
    /// Serve WebTransport on this UDP port too; needs `--tls-identity`, and the in-band proof.
    #[arg(long)]
    webtransport_port: Option<u16>,
    /// A browser origin allowed in. Repeat for more. A non-browser client sends none. With none listed, no origin is checked.
    #[arg(long = "origin")]
    origins: Vec<String>,
    /// A source network allowed to connect at all, like 10.6.0.0/16. Repeat for more. With any listed, everything else is
    /// dropped before its handshake. Checked first, in the order given, against `--deny`.
    #[arg(long = "permit")]
    permit: Vec<String>,
    /// A source network dropped before its handshake. Repeat for more. Takes precedence over `--permit`.
    #[arg(long = "deny")]
    deny: Vec<String>,
    /// How long a peer may take to prove itself in-band, in seconds. Default 15.
    #[arg(long)]
    proof_seconds: Option<u64>,
    /// Proven peers served at once, each with an agent of its own. Another is turned away while this many run.
    #[arg(long, default_value_t = 4)]
    max_peers: usize,
    /// Serve another protocol on the same link, as TOKEN=PROGRAM, such as mcp=my-server. Repeat for more. The agent after `--` is
    /// `acp/1`. A client opens streams on one proven link, with no second proof and no second pairing.
    #[arg(long = "service", value_name = "TOKEN=PROGRAM")]
    services: Vec<String>,
    /// What a peer may open, as PEER=TOKEN. Repeat for more. With any listed, a peer may open only what names it. With none, every
    /// peer allowed in may open everything; `--paired` keeps its grants with `grant` instead.
    #[arg(long = "grant", value_name = "PEER=TOKEN")]
    grants: Vec<String>,
    /// Serve `tcp/1`, forwarding to the targets a peer is granted and to no others. Nothing is granted by this. Name each with
    /// `--grant PEER=tcp/1:HOST:PORT` or, with `--paired`, the `grant` command.
    #[arg(long)]
    forward: bool,
    /// Listen on this machine and reach a target from a peer that forwards for this one, as LOCAL=PEER=TARGET. LOCAL has to be a
    /// loopback address. Repeat for more. That peer decides from its own grants.
    #[arg(long = "reverse", value_name = "LOCAL=PEER=TARGET")]
    reverses: Vec<String>,
    /// Say this agent exists, under this name. On loopback a file only this user's programs read; otherwise a DNS-SD record
    /// everyone on the link can see, with no secret in it. Connecting still needs the proof.
    #[arg(long, value_name = "LABEL")]
    advertise: Option<String>,
    /// A host name to publish in that record, for a certificate a browser accepts. Without it, clients get an address.
    #[arg(long, requires = "advertise", value_name = "NAME")]
    advertise_host: Option<String>,
    /// The agent program, then its arguments.
    #[arg(last = true, required = true)]
    agent: Vec<OsString>,
}

#[derive(clap::Args, Debug)]
struct Forward {
    /// The URL to connect to: wss://, or ws:// on loopback.
    url: Url,
    /// This side's identity. See `keygen`.
    #[arg(long, required_unless_present = "paired")]
    identity: Option<PathBuf>,
    /// Use this user's identity, and prove the remote against whoever this user has paired with.
    #[arg(long, conflicts_with = "identity")]
    paired: bool,
    /// A peer id the remote must prove with `--identity`, and who `--allow-target` is for. Repeat for more.
    #[arg(long = "peer", required_unless_present = "paired")]
    peers: Vec<String>,
    /// A PEM bundle of certificate authorities the remote certificate must chain to, with wss://.
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// Listen here and reach a target from the remote, as LOCAL=TARGET. LOCAL has to be a loopback address. Repeat for more. The
    /// remote decides from its own grants.
    #[arg(long = "local", value_name = "LOCAL=TARGET")]
    locals: Vec<String>,
    /// Let the remote reach this target from here, once it listens for it with `serve --reverse`. Repeat for more. With none, it
    /// reaches nothing.
    #[arg(long = "allow-target", value_name = "HOST:PORT")]
    allow_targets: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct Dial {
    /// The URL to connect to: wss://, or ws:// on loopback for the in-band proof.
    url: Url,
    /// Another endpoint to try when the one before cannot be reached, such as a second gateway. Repeat for more. Each has to
    /// prove the same thing, since nothing else about it is checked differently.
    #[arg(long = "fallback", value_name = "URL")]
    fallbacks: Vec<Url>,
    /// This side's identity, for the in-band proof, which works through a reverse proxy. Created if absent. Everything after
    /// the proof is inside its session.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// Prove the remote against whoever this user has paired with, under this user's own identity: one pairing serves every
    /// command. Takes the place of `--identity` and `--trust-store`.
    #[arg(long, conflicts_with_all = ["identity", "trust_store", "peers", "tls_identity"])]
    paired: bool,
    /// A peer id the remote must prove, with `--identity`. Repeat for more.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// A trust store whose accepted peers the remote may prove, with `--identity`, as `pair-dial` records them.
    #[arg(long)]
    trust_store: Option<PathBuf>,
    /// A TLS identity (certificate and key, PEM) to present over mutual TLS, in place of the in-band proof. Created if absent.
    /// A reverse proxy must vouch for the certificate. With neither this nor `--identity`, TLS checks only the server, by
    /// `--expect` or `--ca-file`.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
    /// Check nothing about the remote, for a ws:// on loopback.
    #[arg(long)]
    no_peer_proof: bool,
    /// A fingerprint the remote certificate may have, with `--tls-identity`. Repeat for more. Needs no certificate authority.
    /// With `--ca-file`, either a listed fingerprint or a certificate from that authority is enough.
    #[arg(long = "expect")]
    expect: Vec<String>,
    /// A PEM bundle of certificate authorities the remote certificate must chain to, with its name checked. With the in-band
    /// proof TLS authenticates nothing, so without this any certificate is taken.
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// A file holding a token to send as `Authorization: Bearer`. It needs a checked certificate, and it is never an argument,
    /// since arguments are visible to other users.
    #[arg(long)]
    bearer_file: Option<PathBuf>,
    /// The longest wait between attempts to reconnect, in seconds.
    #[arg(long, default_value_t = 60)]
    max_backoff_seconds: u64,
    /// The agent program, then its arguments.
    #[arg(last = true, required = true)]
    agent: Vec<OsString>,
}

fn peers(list: &[String]) -> Result<BTreeSet<PeerId>, String> {
    list.iter()
        .map(|text| {
            text.parse()
                .map_err(|_| "a peer id is malformed".to_owned())
        })
        .collect()
}

fn split_agent(agent: &[OsString]) -> Result<(&OsString, &[OsString]), String> {
    agent
        .split_first()
        .ok_or_else(|| "no agent program".to_owned())
}

fn read_tls_identity(path: &std::path::Path) -> Result<tls::Identity, String> {
    tls::Identity::read(path)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the TLS identity file does not exist".to_owned())
}

fn server_tls(path: &std::path::Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let config = tls::server_config_without_client_auth(&read_tls_identity(path)?, &[])
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

/// TLS for a connection whose peer the in-band proof authenticates.
fn in_band_tls(ca_file: Option<&PathBuf>) -> Result<Arc<rustls::ClientConfig>, String> {
    let trust = match ca_file {
        Some(file) => tls::Trust::Roots(tls::Roots::bundle_file(file)),
        None => tls::Trust::AnyWithKeyProof,
    };
    let config =
        tls::client_config_without_identity(&trust, &[]).map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

fn remote_trust(args: &Dial) -> Result<tls::Trust, String> {
    let pins = tls::Pins::fixed(peers(&args.expect)?);
    Ok(match &args.ca_file {
        Some(file) if !args.expect.is_empty() => tls::Trust::PinnedOrRoots {
            pins,
            roots: tls::Roots::bundle_file(file),
        },
        Some(file) => tls::Trust::Roots(tls::Roots::bundle_file(file)),
        None => tls::Trust::Pinned(pins),
    })
}

fn mtls_client(path: &std::path::Path, args: &Dial) -> Result<Arc<rustls::ClientConfig>, String> {
    let identity = tls::Identity::open_or_create(path).map_err(|error| error.to_string())?;
    let trust = remote_trust(args)?;
    // This builder turns resumption off, so a peer that lost trust cannot return on a session.
    let config = tls::client_config(&identity, &trust, &[]).map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

/// A certificate browsers will pin, starting a little early for a client whose clock runs behind.
fn short_lived_identity() -> Result<tls::Identity, String> {
    let not_before = SystemTime::now() - Duration::from_secs(3600);
    tls::Identity::generate_with(not_before, not_before + SHORT_LIVED_VALIDITY)
        .map_err(|error| error.to_string())
}

/// New connections could not open after the certificate ends, and that must not look like a bridge still working. By the clock
/// on the wall: a machine that sleeps does not count that towards a sleep. Agents read their end of the pipe closing.
fn end_with_the_certificate(advertised: Option<Advertised>) {
    let until = SystemTime::now() + SHORT_LIVED_RUN;
    thread::spawn(move || {
        while SystemTime::now() < until {
            thread::sleep(Duration::from_secs(60));
        }
        drop(advertised);
        eprintln!(
            "coqui-acp-bridge: the short-lived certificate is ending: restart to make another"
        );
        std::process::exit(EXIT_CERTIFICATE_ENDING);
    });
}

fn limits() -> Limits {
    Limits::default().with_max_message(MAX_LINE)
}

/// What a channel starts with while its peer has proved nothing; the proof raises it.
fn unproven_limits() -> Limits {
    Limits::default()
        .with_max_message(auth::MAX_PROOF_MESSAGE)
        .with_pending(UNPROVEN_PENDING)
}

/// A count of what is running, with a ceiling.
#[derive(Clone)]
struct Gate(Arc<AtomicUsize>, usize);

struct Entered(Arc<AtomicUsize>);

impl Drop for Entered {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Gate {
    fn new(ceiling: usize) -> Self {
        Self(Arc::new(AtomicUsize::new(0)), ceiling)
    }

    fn enter(&self) -> Option<Entered> {
        let mut running = self.0.load(Ordering::SeqCst);
        while running < self.1 {
            match self
                .0
                .compare_exchange(running, running + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(Entered(Arc::clone(&self.0))),
                Err(now) => running = now,
            }
        }
        None
    }
}

/// What every connection of a `serve` shares.
/// `serve --reverse`: a listener here, for as long as this peer's link is up.
struct Reverse {
    local: SocketAddr,
    peer: String,
    target: Target,
}

fn local_address(text: &str) -> Result<SocketAddr, String> {
    let address: SocketAddr = text
        .parse()
        .map_err(|_| format!("{text} is not an address and port"))?;
    if !forward::is_loopback(address.ip()) {
        return Err(format!(
            "{text} is not loopback: anyone who can reach it gets what this peer was granted"
        ));
    }
    Ok(address)
}

fn parse_reverse(text: &str) -> Result<Reverse, String> {
    let mut parts = text.splitn(3, '=');
    let (Some(local), Some(peer), Some(target)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("--reverse is LOCAL=PEER=TARGET".to_owned());
    };
    Ok(Reverse {
        local: local_address(local)?,
        peer: peer.to_owned(),
        target: target
            .parse()
            .map_err(|error: forward::BadTarget| error.to_string())?,
    })
}

fn parse_local(text: &str) -> Result<(SocketAddr, Target), String> {
    let (local, target) = text.split_once('=').ok_or("--local is LOCAL=TARGET")?;
    Ok((
        local_address(local)?,
        target
            .parse()
            .map_err(|error: forward::BadTarget| error.to_string())?,
    ))
}

/// One listener for the life of a link. A port someone else holds is said once and tried again: the rest of the link is still good,
/// and whoever holds it would otherwise be given this forward's clients for good.
fn listen_for<C: Channel>(mux: &mux::Mux<'_, C>, local: SocketAddr, target: &Target) {
    let mut said = false;
    let listener = loop {
        match std::net::TcpListener::bind(local) {
            Ok(listener) => break listener,
            Err(error) => {
                if !said {
                    eprintln!("could not listen on {local}, trying again: {error}");
                    said = true;
                }
                for _ in 0..50 {
                    if mux.link_state() != mux::LinkState::Up {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    };
    eprintln!("{local} forwards to {target}");
    if let Err(error) = forward::listen(mux, &listener, target) {
        eprintln!("stopped listening on {local}: {error}");
    }
}

struct Service {
    method: Option<WebCrypto>,
    store: Option<Arc<TrustStore>>,
    proof: Duration,
    agent: Vec<OsString>,
    unproven: Gate,
    peers: Gate,
    links: Option<Links>,
    reverses: Vec<Reverse>,
}

/// What a link can run beside the one agent, and who may open what.
struct Links {
    services: Services,
    grants: Box<dyn PeerStore>,
    open: LinkCounts,
}

/// How many links are up. A stream's limits are per link, so without this one key could open as many of everything as it liked.
#[derive(Default)]
struct LinkCounts(Mutex<BTreeMap<String, usize>>);

/// Links from one peer. Each can hold its own streams, so this also bounds what forwarding a peer can have open.
const MAX_LINKS_PER_PEER: usize = 8;
/// Links from everyone together.
const MAX_LINKS: usize = 256;
/// How long a proven peer may take to say whether it is a link or a plain client.
const FIRST_MESSAGE: Duration = Duration::from_secs(60);

impl LinkCounts {
    fn enter<'a>(&'a self, peer: &str) -> Option<LinkEntered<'a>> {
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let total: usize = open.values().sum();
        let mine = open.entry(peer.to_owned()).or_default();
        if *mine >= MAX_LINKS_PER_PEER || total >= MAX_LINKS {
            return None;
        }
        *mine += 1;
        Some(LinkEntered {
            counts: self,
            peer: peer.to_owned(),
        })
    }
}

struct LinkEntered<'a> {
    counts: &'a LinkCounts,
    peer: String,
}

impl Drop for LinkEntered<'_> {
    fn drop(&mut self) {
        let mut open = self.counts.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mine) = open.get_mut(&self.peer) {
            *mine -= 1;
            if *mine == 0 {
                open.remove(&self.peer);
            }
        }
    }
}

/// The token of the agent named after `--`.
const ACP: &str = "acp/1";

/// `--grant`, or everything for whoever is allowed in: the proof already decided who that is.
struct FixedGrants {
    every: Vec<String>,
    named: Option<BTreeMap<String, BTreeSet<String>>>,
}

impl PeerStore for FixedGrants {
    fn get(&self, peer: &str) -> Result<Option<PeerRecord>, Error> {
        let mut record = PeerRecord::new(peer);
        match &self.named {
            Some(named) => record.grants = named.get(peer).cloned().unwrap_or_default(),
            None => record.grants = self.every.iter().cloned().collect(),
        }
        Ok(Some(record))
    }

    fn put(&self, _record: &PeerRecord) -> Result<(), Error> {
        Err(Error::Identity(
            "these grants are fixed by the command line".to_owned(),
        ))
    }

    fn remove(&self, _peer: &str) -> Result<(), Error> {
        Err(Error::Identity(
            "these grants are fixed by the command line".to_owned(),
        ))
    }
}

/// This user's grants, and the agent a peer was paired for: pairing gave it that, and anything else takes a `grant`.
struct PairedGrants(FilePeers);

impl PeerStore for PairedGrants {
    fn get(&self, peer: &str) -> Result<Option<PeerRecord>, Error> {
        let record = self.0.get(peer)?.unwrap_or_else(|| PeerRecord::new(peer));
        Ok(Some(record.granting(ACP)))
    }

    fn put(&self, record: &PeerRecord) -> Result<(), Error> {
        self.0.put(record)
    }

    fn remove(&self, peer: &str) -> Result<(), Error> {
        self.0.remove(peer)
    }
}

fn agent_handler(program: Vec<OsString>, gate: Gate) -> link::Handler {
    Box::new(move |stream: &dyn LinkStream, accepted: &Accepted| {
        let Some(_running) = gate.enter() else {
            return eprintln!("{} was turned away: --max-peers are running", accepted.peer);
        };
        let started = split_agent(&program).and_then(|(program, args)| {
            Agent::spawn(program, args, MAX_LINE)
                .map_err(|error| format!("the agent could not start: {error}"))
        });
        match started {
            Ok(running) => {
                let end = relay(stream, &running);
                eprintln!("{} left {}: {end:?}", accepted.peer, accepted.protocol);
                let _ = running.stop();
            }
            Err(why) => eprintln!("{why}"),
        }
    })
}

fn parse_service(text: &str) -> Result<(String, OsString), String> {
    let (token, program) = text.split_once('=').ok_or("a service is TOKEN=PROGRAM")?;
    if !mux::valid_protocol(token) {
        return Err(
            "a protocol token is printable, with no spaces, commas or equals signs".to_owned(),
        );
    }
    if token.starts_with(forward::TCP_PROTOCOL) || token == link::LINK_PROTOCOL || token == ACP {
        return Err(format!(
            "{token} is not for --service: it means something on every link"
        ));
    }
    if program.is_empty() {
        return Err("a service is TOKEN=PROGRAM".to_owned());
    }
    Ok((token.to_owned(), OsString::from(program)))
}

/// What `--service` and `--grant` ask for, if anything.
fn links(args: &Serve, gate: &Gate) -> Result<Option<Links>, String> {
    if args.services.is_empty() && !args.forward && args.reverses.is_empty() {
        return Ok(None);
    }
    let mut services =
        Services::default().with(ACP, agent_handler(args.agent.clone(), gate.clone()));
    let mut every = vec![ACP.to_owned()];
    for text in &args.services {
        let (token, program) = parse_service(text)?;
        services = services.with(&token, agent_handler(vec![program], gate.clone()));
        every.push(token);
    }
    if args.forward {
        services = forward::serving(services);
    }
    let grants: Box<dyn PeerStore> = if args.paired {
        if !args.grants.is_empty() {
            return Err("--paired keeps its grants with the grant command, not --grant".to_owned());
        }
        Box::new(PairedGrants(FilePeers::new(
            home::grants_path().map_err(|error| error.to_string())?,
        )))
    } else {
        let mut named: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for text in &args.grants {
            let (peer, token) = text.split_once('=').ok_or("a grant is PEER=TOKEN")?;
            let entry = named.entry(peer.to_owned()).or_default();
            if let Some(target) = token.strip_prefix("tcp/1:") {
                if !args.forward {
                    return Err("a target needs --forward".to_owned());
                }
                let target: Target = target
                    .parse()
                    .map_err(|error: forward::BadTarget| error.to_string())?;
                entry.insert(forward::TCP_PROTOCOL.to_owned());
                entry.insert(target.grant_token());
            } else if every.iter().any(|offered| offered == token) {
                entry.insert(token.to_owned());
            } else {
                return Err("a grant names a protocol nothing here serves".to_owned());
            }
        }
        Box::new(FixedGrants {
            every,
            named: (!named.is_empty()).then_some(named),
        })
    };
    Ok(Some(Links {
        services,
        grants,
        open: LinkCounts::default(),
    }))
}

impl Service {
    /// Proves the peer unless TLS already did, gives it a fresh agent, and relays until either ends.
    fn run<C: Channel>(&self, channel: &C, unproven: Entered) {
        let Some(method) = &self.method else {
            drop(unproven);
            self.give_agent(channel, "a certificate peer");
            return channel.close();
        };
        match auth::prove(
            method,
            channel,
            self.proof,
            MAX_LINE + auth::RECORD_OVERHEAD,
        ) {
            Ok(established) => {
                drop(unproven);
                let peer = established.peer;
                let label = peer.to_string();
                let sealed = Sealed::new(channel, established.session);
                let sealed = match &self.store {
                    Some(store) => {
                        let store = Arc::clone(store);
                        sealed.with_check(move || store.is_accepted(&peer))
                    }
                    None => sealed,
                };
                self.serve_peer(&sealed, &label);
                sealed.close();
            }
            Err(error) => eprintln!("a connection was refused: {error}"),
        }
        channel.close();
    }

    /// One protocol, or a link of several: a first message of streams says which, so nothing has to be agreed first. A plain ACP
    /// client speaks first, and with no services the program may be the one to, so this waits only where there is a choice.
    fn serve_peer<C: Channel>(&self, channel: &C, peer: &str) {
        let Some(links) = &self.links else {
            return self.give_agent(channel, peer);
        };
        eprintln!("{peer} proven");
        let mut options = link::Options::default();
        options.limits.max_message = MAX_LINE;
        let Some(_entered) = links.open.enter(peer) else {
            return eprintln!("{peer} was turned away: it has as many links as it may");
        };
        match channel.receive(Some(FIRST_MESSAGE)) {
            Ok(Some(first)) if mux::starts_mux(&first) => {
                let host = link::Host {
                    store: &*links.grants,
                    services: &links.services,
                    events: &NoEvents,
                };
                let reverses: Vec<&Reverse> = self
                    .reverses
                    .iter()
                    .filter(|reverse| reverse.peer == peer)
                    .collect();
                link::serve_with(
                    channel,
                    Side::Answered,
                    options,
                    Some(first),
                    peer,
                    &host,
                    |mux| {
                        thread::scope(|scope| {
                            for reverse in reverses {
                                scope
                                    .spawn(move || listen_for(mux, reverse.local, &reverse.target));
                            }
                        });
                    },
                );
            }
            Ok(Some(first)) => {
                let granted = links
                    .grants
                    .get(peer)
                    .is_ok_and(|record| record.is_some_and(|record| record.grants.contains(ACP)));
                if granted {
                    self.give_agent(&Prefilled::new(channel, first), peer);
                } else {
                    eprintln!("{peer} may not open {ACP}");
                }
            }
            Ok(None) | Err(_) => {}
        }
    }

    fn give_agent<C: Channel>(&self, channel: &C, peer: &str) {
        eprintln!("{peer} proven");
        let Some(_running) = self.peers.enter() else {
            return eprintln!("{peer} was turned away: --max-peers are running");
        };
        let started = split_agent(&self.agent).and_then(|(program, args)| {
            Agent::spawn(program, args, MAX_LINE)
                .map_err(|error| format!("the agent could not start: {error}"))
        });
        match started {
            Ok(running) => {
                let end = relay(channel, &running);
                eprintln!("{peer} left: {end:?}");
                let _ = running.stop();
            }
            Err(why) => eprintln!("{why}"),
        }
    }
}

/// Takes connections for ever, each handshake and proof on a thread of its own, so one slow peer holds up nobody. Past the
/// ceiling of unproven connections a new one is dropped: that many at once is an attack, or the ceiling needs raising.
fn accept_loop<P, C>(
    next: impl Fn() -> Result<P, Failure>,
    handshake: impl Fn(P) -> Result<C, Failure> + Clone + Send + 'static,
    service: &Arc<Service>,
) -> !
where
    P: Send + 'static,
    C: Channel + 'static,
{
    loop {
        let pending = match next() {
            Ok(pending) => pending,
            Err(failure) => {
                eprintln!("a connection failed: {failure}");
                continue;
            }
        };
        let Some(entered) = service.unproven.enter() else {
            eprintln!("a connection was dropped: too many are unproven");
            continue;
        };
        let service = Arc::clone(service);
        let handshake = handshake.clone();
        let spawned = thread::Builder::new()
            .name("coqui-acp-bridge-peer".to_owned())
            .spawn(move || match handshake(pending) {
                Ok(channel) => service.run(&channel, entered),
                Err(failure) => eprintln!("a connection failed: {failure}"),
            });
        if let Err(error) = spawned {
            eprintln!("a connection was dropped: {error}");
        }
    }
}

/// What may start a handshake: `--deny` first, then `--permit` if any are listed. A first-match ACL.
fn admission(
    permit: &[String],
    deny: &[String],
) -> Result<impl Fn(SocketAddr) -> bool + Clone + Send + Sync + 'static, String> {
    let parse = |text: &String| {
        text.parse::<IpNet>()
            .map_err(|_| "a network is malformed".to_owned())
    };
    let acl = if permit.is_empty() && deny.is_empty() {
        None
    } else {
        let default = if permit.is_empty() {
            Action::Permit
        } else {
            Action::Deny
        };
        let mut acl = Acl {
            rules: Vec::new(),
            default,
        };
        for network in deny {
            acl.push(Rule::new(Action::Deny, parse(network)?));
        }
        for network in permit {
            acl.push(Rule::new(Action::Permit, parse(network)?));
        }
        Some(acl)
    };
    let acl = Arc::new(acl);
    Ok(move |source: SocketAddr| {
        acl.as_ref()
            .as_ref()
            .is_none_or(|acl| acl.decide(source, source).proceeds())
    })
}

/// Publishes what `--advertise` asked for. The result has to live as long as the listener.
fn advertise(
    args: &Serve,
    bound: SocketAddr,
    peer: Option<String>,
    webtransport: Option<u16>,
    webtransport_hash: Option<String>,
) -> Result<Option<Advertised>, String> {
    let Some(label) = &args.advertise else {
        return Ok(None);
    };
    let text = |error: Error| error.to_string();
    if !bound.ip().is_loopback() && peer.is_none() {
        return Err(
            "--advertise on the network needs the in-band proof: a client has to be able to check the key a record names"
                .to_owned(),
        );
    }
    if bound.ip().is_loopback() && args.advertise_host.is_some() {
        return Err(
            "--advertise-host names a host on the network, and this listens on loopback".to_owned(),
        );
    }
    let offers: Vec<String> = if args.services.is_empty() && !args.forward {
        Vec::new()
    } else {
        core::iter::once(ACP.to_owned())
            .chain(
                args.services
                    .iter()
                    .filter_map(|text| parse_service(text).ok().map(|(token, _)| token)),
            )
            .chain(args.forward.then(|| forward::TCP_PROTOCOL.to_owned()))
            .collect()
    };
    let advertisement =
        Advertisement::new(label, args.tls_identity.is_some() || args.tls_short_lived)
            .map_err(text)?
            .with_peer(peer)
            .with_webtransport(webtransport)
            .with_webtransport_hash(webtransport_hash)
            .with_offers(&offers)
            .with_host(args.advertise_host.clone())
            .map_err(text)?;
    discovery::advertise(&advertisement, bound)
        .map(Some)
        .map_err(text)
}

fn serve(args: &Serve) -> Result<ExitCode, String> {
    let in_band = !args.allow.is_empty() || args.allow_store.is_some() || args.paired;
    let certificates =
        !args.allow_cert.is_empty() || args.allow_cert_store.is_some() || args.client_ca.is_some();
    let modes = [in_band, certificates, args.no_peer_proof];
    if modes.iter().filter(|mode| **mode).count() != 1
        || (!args.allow.is_empty() && args.allow_store.is_some())
    {
        return Err(
            "give one proof: --allow or --allow-store; client certificates; or --no-peer-proof"
                .to_owned(),
        );
    }
    let admit = admission(&args.permit, &args.deny)?;
    let unproven = Gate::new(MAX_UNPROVEN);
    let running = Gate::new(args.max_peers);
    if !args.grants.is_empty() && args.services.is_empty() && !args.forward {
        return Err(
            "--grant needs --service or --forward: otherwise there is nothing to grant".to_owned(),
        );
    }
    if (!args.services.is_empty() || args.forward || !args.reverses.is_empty())
        && (args.no_peer_proof || certificates)
    {
        return Err("--service, --forward and --reverse need the in-band proof: streams are for a link a proof named a peer on".to_owned());
    }
    if args.tls_short_lived && (args.no_peer_proof || certificates) {
        return Err("--tls-short-lived needs the in-band proof: a client cannot pin this certificate for long".to_owned());
    }
    if args.no_peer_proof {
        return serve_unproven(args, admit, unproven, running);
    }
    if certificates {
        // Silently dropping these would leave an operator believing they apply.
        if args.identity.is_some() || args.proof_seconds.is_some() {
            return Err("--identity and --proof-seconds belong to the in-band proof".to_owned());
        }
        if args.webtransport_port.is_some() {
            return Err(
                "WebTransport has no client certificate here: it needs the in-band proof"
                    .to_owned(),
            );
        }
        let path = args
            .tls_identity
            .as_deref()
            .ok_or("mutual TLS needs --tls-identity")?;
        let pins = match &args.allow_cert_store {
            Some(_) if !args.allow_cert.is_empty() => {
                return Err("give --allow-cert or --allow-cert-store, not both".to_owned());
            }
            Some(store) => {
                tls::Pins::store(TrustStore::open(store).map_err(|error| error.to_string())?)
            }
            None => tls::Pins::fixed(peers(&args.allow_cert)?),
        };
        let pinned = !args.allow_cert.is_empty() || args.allow_cert_store.is_some();
        let trust = match &args.client_ca {
            Some(file) if pinned => tls::Trust::PinnedOrCa {
                pins,
                ca: tls::CaPolicy::new(file),
            },
            Some(file) => tls::Trust::Ca(tls::CaPolicy::new(file)),
            None => tls::Trust::Pinned(pins),
        };
        // This builder turns resumption off, so a peer that lost trust cannot return on a session.
        let config = tls::server_config(&read_tls_identity(path)?, &trust, &[])
            .map_err(|error| error.to_string())?;
        let listener = websocket::Listener::bind(args.listen, Some(Arc::new(config)))
            .map_err(|failure| failure.to_string())?
            .with_limits(limits())
            .with_origins(args.origins.clone())
            .with_admission(admit);
        let bound = listener
            .local_addr()
            .map_err(|failure| failure.to_string())?;
        let _advertised = advertise(args, bound, None, None, None)?;
        eprintln!("listening on {bound}");
        let service = Arc::new(Service {
            method: None,
            store: None,
            proof: Duration::ZERO,
            agent: args.agent.clone(),
            unproven,
            peers: running,
            links: None,
            reverses: Vec::new(),
        });
        accept_loop(
            || listener.next(None),
            websocket::Pending::handshake,
            &service,
        );
    }
    let identity = if args.paired {
        home::identity(None).map_err(|error| error.to_string())?
    } else {
        let identity = args
            .identity
            .as_deref()
            .ok_or("the in-band proof needs --identity (see keygen), or --paired")?;
        auth::read_identity(identity).map_err(|error| error.to_string())?
    };
    let peer_id = identity.peer_id();
    let store = if args.paired {
        Some(Arc::new(
            home::store(None).map_err(|error| error.to_string())?,
        ))
    } else {
        args.allow_store
            .as_deref()
            .map(TrustStore::open)
            .transpose()
            .map_err(|error| error.to_string())?
            .map(Arc::new)
    };
    let trust = match &store {
        Some(store) => Trust::Store(TrustStore::clone(store)),
        None => Trust::pinned(peers(&args.allow)?),
    };
    let method = WebCrypto::new(identity).with_initiators(trust);
    println!("{peer_id}");
    let _ = std::io::stdout().flush();
    let proof = Duration::from_secs(args.proof_seconds.unwrap_or(15));
    let short_lived = args
        .tls_short_lived
        .then(short_lived_identity)
        .transpose()?;
    let tls_config = match &short_lived {
        Some(identity) => {
            let config = tls::server_config_without_client_auth(identity, &[])
                .map_err(|error| error.to_string())?;
            Some(Arc::new(config))
        }
        None => args.tls_identity.as_deref().map(server_tls).transpose()?,
    };
    let plain = websocket::Listener::bind(args.listen, tls_config.clone())
        .map_err(|failure| failure.to_string())?
        .with_limits(unproven_limits())
        .with_frames(Frames::Binary)
        .with_origins(args.origins.clone())
        .with_admission(admit.clone());
    // Every listener is bound before anything says this agent exists, so what is announced is there.
    let quic = args
        .webtransport_port
        .map(|port| {
            let tls_config = tls_config.ok_or("WebTransport needs --tls-identity")?;
            let address = SocketAddr::new(args.listen.ip(), port);
            let listener = webtransport::Listener::bind(address, tls_config)
                .map_err(|failure| failure.to_string())?
                .with_limits(unproven_limits())
                .with_origins(args.origins.clone())
                .with_admission(admit);
            Ok::<_, String>(listener)
        })
        .transpose()?;
    let bound = plain.local_addr().map_err(|failure| failure.to_string())?;
    let hash = short_lived
        .as_ref()
        .map(|identity| identity.fingerprint().to_string());
    if let Some(hash) = &hash {
        eprintln!("certificate {hash}, for 13 days");
    }
    let short_lived_run = hash.is_some();
    let advertised = advertise(
        args,
        bound,
        Some(peer_id.to_string()),
        args.webtransport_port,
        hash,
    )?;
    let _advertised = if short_lived_run {
        end_with_the_certificate(advertised);
        None
    } else {
        advertised
    };
    eprintln!("listening on {bound}");
    let links = links(args, &running)?;
    let reverses = args
        .reverses
        .iter()
        .map(|text| parse_reverse(text))
        .collect::<Result<Vec<_>, _>>()?;
    let service = Arc::new(Service {
        method: Some(method),
        store,
        proof,
        agent: args.agent.clone(),
        unproven,
        peers: running,
        links,
        reverses,
    });
    if let Some(quic) = quic {
        let service = Arc::clone(&service);
        thread::spawn(move || {
            accept_loop(
                || quic.next(None),
                webtransport::Pending::handshake,
                &service,
            )
        });
    }
    accept_loop(|| plain.next(None), websocket::Pending::handshake, &service)
}

/// `serve --no-peer-proof`: the agent for whoever can connect, so who can connect has to be limited.
fn serve_unproven(
    args: &Serve,
    admit: impl Fn(SocketAddr) -> bool + Clone + Send + Sync + 'static,
    unproven: Gate,
    running: Gate,
) -> Result<ExitCode, String> {
    if args.identity.is_some() || args.proof_seconds.is_some() || args.webtransport_port.is_some() {
        return Err("--identity, --proof-seconds and --webtransport-port need a proof".to_owned());
    }
    if !args.listen.ip().is_loopback() && args.permit.is_empty() {
        return Err(
            "--no-peer-proof beyond loopback needs --permit, to say who can reach it".to_owned(),
        );
    }
    let tls_config = args.tls_identity.as_deref().map(server_tls).transpose()?;
    // An empty list checks nothing, and a page on any site could then drive an agent on this machine.
    let origins = if args.origins.is_empty() {
        vec!["none".to_owned()]
    } else {
        args.origins.clone()
    };
    let listener = websocket::Listener::bind(args.listen, tls_config)
        .map_err(|failure| failure.to_string())?
        .with_limits(limits())
        .with_origins(origins)
        .with_admission(admit);
    let bound = listener
        .local_addr()
        .map_err(|failure| failure.to_string())?;
    let _advertised = advertise(args, bound, None, None, None)?;
    eprintln!(
        "listening on {bound}, with no proof of the peer: whoever can connect gets the agent"
    );
    let service = Arc::new(Service {
        method: None,
        store: None,
        proof: Duration::ZERO,
        agent: args.agent.clone(),
        unproven,
        peers: running,
        links: None,
        reverses: Vec::new(),
    });
    accept_loop(
        || listener.next(None),
        websocket::Pending::handshake,
        &service,
    )
}

/// How `dial` knows its peer. Without an in-band proof, whatever TLS checked is all there is.
enum DialProof {
    InBand(WebCrypto, Option<Arc<TrustStore>>),
    Transport,
}

fn in_band_dialer(args: &Dial) -> Result<websocket::Dialer, String> {
    let mut dialer = websocket::Dialer::new()
        .with_limits(unproven_limits())
        .with_frames(Frames::Binary);
    if args.url.scheme() == "wss" {
        dialer = dialer.with_tls(in_band_tls(args.ca_file.as_ref())?);
    }
    Ok(dialer)
}

fn dial_setup(args: &Dial) -> Result<(DialProof, websocket::Dialer), String> {
    if args.paired {
        if !args.expect.is_empty() {
            return Err("--expect belongs to --tls-identity".to_owned());
        }
        if args.bearer_file.is_some() && args.ca_file.is_none() {
            return Err(
                "--bearer-file needs --ca-file, so the token goes only to a checked server"
                    .to_owned(),
            );
        }
        let identity = home::identity(None).map_err(|error| error.to_string())?;
        let store = Arc::new(home::store(None).map_err(|error| error.to_string())?);
        let method =
            WebCrypto::new(identity).with_responders(Trust::Store(TrustStore::clone(&store)));
        return Ok((
            DialProof::InBand(method, Some(store)),
            in_band_dialer(args)?,
        ));
    }
    match (&args.identity, &args.tls_identity) {
        (Some(path), None) => {
            if !args.expect.is_empty() {
                return Err("--expect belongs to --tls-identity; use --peer".to_owned());
            }
            if args.bearer_file.is_some() && args.ca_file.is_none() {
                return Err(
                    "--bearer-file needs --ca-file, so the token goes only to a checked server"
                        .to_owned(),
                );
            }
            let identity = Identity::open_or_create(path).map_err(|error| error.to_string())?;
            let store = args
                .trust_store
                .as_deref()
                .map(TrustStore::open)
                .transpose()
                .map_err(|error| error.to_string())?
                .map(Arc::new);
            let trust = match (&store, args.peers.is_empty()) {
                (Some(_), false) | (None, true) => {
                    return Err("give exactly one of --peer and --trust-store".to_owned());
                }
                (Some(store), true) => Trust::Store(TrustStore::clone(store)),
                (None, false) => Trust::pinned(peers(&args.peers)?),
            };
            let method = WebCrypto::new(identity).with_responders(trust);
            Ok((DialProof::InBand(method, store), in_band_dialer(args)?))
        }
        (None, Some(path)) => {
            if !args.peers.is_empty() || args.trust_store.is_some() {
                return Err("--peer and --trust-store belong to --identity".to_owned());
            }
            if args.expect.is_empty() && args.ca_file.is_none() {
                return Err("--tls-identity needs --expect or --ca-file".to_owned());
            }
            if args.url.scheme() != "wss" {
                return Err("mutual TLS needs a wss:// URL".to_owned());
            }
            let dialer = websocket::Dialer::new()
                .with_tls(mtls_client(path, args)?)
                .with_limits(limits());
            Ok((DialProof::Transport, dialer))
        }
        (None, None) if args.no_peer_proof => {
            if args.url.scheme() != "ws" || !args.expect.is_empty() || args.ca_file.is_some() {
                return Err("--no-peer-proof is for a ws:// on loopback".to_owned());
            }
            let dialer = websocket::Dialer::new().with_limits(limits());
            Ok((DialProof::Transport, dialer))
        }
        (None, None) => {
            if args.url.scheme() != "wss" || (args.expect.is_empty() && args.ca_file.is_none()) {
                return Err(
                    "give --identity, --tls-identity, or a wss:// URL with --expect or --ca-file"
                        .to_owned(),
                );
            }
            if !args.peers.is_empty() || args.trust_store.is_some() {
                return Err("--peer and --trust-store belong to --identity".to_owned());
            }
            let config = tls::client_config_without_identity(&remote_trust(args)?, &[])
                .map_err(|error| error.to_string())?;
            let dialer = websocket::Dialer::new()
                .with_tls(Arc::new(config))
                .with_limits(limits());
            Ok((DialProof::Transport, dialer))
        }
        _ => Err("give --identity or --tls-identity, not both".to_owned()),
    }
}

/// One link's relay. An error here ends `dial`: a remote that proved the wrong key will not fix itself.
fn dial_link(
    proof: &DialProof,
    channel: &websocket::Channel,
    agent: &Agent,
) -> Result<Option<LinkEnd>, String> {
    let DialProof::InBand(method, store) = proof else {
        return Ok(Some(relay(channel, agent)));
    };
    match auth::prove_out(
        method,
        channel,
        DIAL_PROOF,
        MAX_LINE + auth::RECORD_OVERHEAD,
    ) {
        Ok(established) => {
            let peer = established.peer;
            let sealed = Sealed::new(channel, established.session);
            let sealed = match store {
                Some(store) => {
                    let store = Arc::clone(store);
                    sealed.with_check(move || store.is_accepted(&peer))
                }
                None => sealed,
            };
            let end = relay(&sealed, agent);
            sealed.close();
            Ok(Some(end))
        }
        Err(Error::Proof(why)) => Err(format!("the remote did not prove itself: {why}")),
        Err(error) => {
            eprintln!("the proof did not finish: {error}");
            Ok(None)
        }
    }
}

fn dial(args: &Dial) -> Result<ExitCode, String> {
    let (proof, mut dialer) = dial_setup(args)?;
    let (program, rest) = split_agent(&args.agent)?;
    let agent = Agent::spawn(program, rest, MAX_LINE)
        .map_err(|error| format!("the agent could not start: {error}"))?;
    if let Some(file) = &args.bearer_file {
        let token = std::fs::read_to_string(file)
            .map_err(|error| format!("the bearer file could not be read: {error}"))?;
        dialer = dialer.with_header("authorization", format!("Bearer {}", token.trim()));
    }
    let mut wait = Duration::from_secs(1);
    let longest = Duration::from_secs(args.max_backoff_seconds);
    if args
        .fallbacks
        .iter()
        .any(|url| url.scheme() != args.url.scheme())
    {
        return Err(
            "a fallback uses the same scheme as the URL: the same checks have to apply".to_owned(),
        );
    }
    let endpoints: Vec<&Url> = core::iter::once(&args.url).chain(&args.fallbacks).collect();
    let mut endpoint = 0;
    loop {
        if let Some(status) = agent.try_status().map_err(|error| error.to_string())? {
            return Ok(status_code(status));
        }
        match dialer.dial(
            Kind::WebSocket,
            endpoints[endpoint],
            Duration::from_secs(10),
        ) {
            Ok(channel) => {
                eprintln!("connected");
                let end = dial_link(&proof, &channel, &agent);
                channel.close();
                let end = end?;
                if let Some(end) = end {
                    wait = Duration::from_secs(1);
                    eprintln!("the link ended: {end:?}");
                    if matches!(end, LinkEnd::AgentEnded | LinkEnd::AgentStuck) {
                        let status = agent.stop().map_err(|error| error.to_string())?;
                        return Ok(status_code(status));
                    }
                    if end == LinkEnd::Failed(Failure::Permission) {
                        return Err("the remote peer is no longer accepted".to_owned());
                    }
                }
            }
            // A rejected certificate will not fix itself: stop rather than knock forever.
            Err(Failure::Certificate) => {
                return Err("the remote certificate is not the expected one".to_owned());
            }
            Err(failure) => {
                eprintln!("could not connect: {failure}");
                endpoint = (endpoint + 1) % endpoints.len();
                // There is another to try, so nothing is gained by waiting first.
                if endpoint != 0 {
                    continue;
                }
            }
        }
        thread::sleep(wait);
        wait = (wait * 2).min(longest);
    }
}

fn pair_accept_mode(args: &PairAccept) -> Result<ExitCode, String> {
    let identity =
        Arc::new(home::identity(args.identity.as_deref()).map_err(|error| error.to_string())?);
    let store = Arc::new(home::store(args.store.as_deref()).map_err(|error| error.to_string())?);
    let tls_config = args.tls_identity.as_deref().map(server_tls).transpose()?;
    let listener = websocket::Listener::bind(args.listen, tls_config)
        .map_err(|failure| failure.to_string())?
        .with_frames(Frames::Binary)
        .with_limits(
            Limits::default()
                .with_max_message(auth::MAX_PROOF_MESSAGE)
                .with_pending(UNPROVEN_PENDING),
        )
        .with_admission(admission(&args.permit, &args.deny)?);
    let desk = pairing::desk().map_err(|error| error.to_string())?;
    let shown = desk.show_pin().map_err(|error| error.to_string())?;
    println!("{}", shown.pin.as_str());
    println!("{}", identity.peer_id());
    let _ = std::io::stdout().flush();
    let until = Instant::now() + Duration::from_secs(args.timeout_seconds);
    let (report, reports) = mpsc::channel();
    let gate = Gate::new(MAX_PAIRING_RUNS);
    // Each connection runs apart, so a scanner that connects and says nothing costs the person neither the PIN nor the wait.
    // A proxy's address says nothing about the peer, so the desk is not given one.
    loop {
        while let Ok(result) = reports.try_recv() {
            match result {
                Ok(peer) => {
                    eprintln!("paired with {peer}");
                    return Ok(ExitCode::SUCCESS);
                }
                Err(why) => {
                    eprintln!("a pairing attempt failed: {why}");
                    if desk.shown_pin().is_none() {
                        return Err("the PIN is used up or expired: start again".to_owned());
                    }
                }
            }
        }
        if Instant::now() >= until {
            return Err("nobody paired in time".to_owned());
        }
        let pending = match listener.next(Some(Duration::from_millis(100))) {
            Ok(pending) => pending,
            Err(Failure::Timeout) => continue,
            Err(failure) => return Err(failure.to_string()),
        };
        let Some(entered) = gate.enter() else {
            continue;
        };
        let (identity, store, desk, report) = (
            Arc::clone(&identity),
            Arc::clone(&store),
            desk.clone(),
            report.clone(),
        );
        thread::spawn(move || {
            let _entered = entered;
            let result = pending
                .handshake()
                .map_err(|failure| failure.to_string())
                .and_then(|channel| {
                    let paired =
                        pairing::respond(&channel, &identity, &store, &desk, None, PAIRING_RUN);
                    channel.close();
                    paired.map_err(|error| error.to_string())
                })
                .map(|paired| paired.peer.id().to_string());
            let _ = report.send(result);
        });
    }
}

fn pair_dial_mode(args: &PairDial) -> Result<ExitCode, String> {
    let text = match &args.pin_file {
        Some(file) => std::fs::read_to_string(file)
            .map_err(|error| format!("the PIN file could not be read: {error}"))?,
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            line
        }
    };
    let pin = Pin::parse(&CodeAlphabet::pin(), text.trim())
        .map_err(|_| "that is not a PIN".to_owned())?;
    let identity = home::identity(args.identity.as_deref()).map_err(|error| error.to_string())?;
    let store = home::store(args.store.as_deref()).map_err(|error| error.to_string())?;
    let mut dialer = websocket::Dialer::new()
        .with_frames(Frames::Binary)
        .with_limits(unproven_limits());
    if args.url.scheme() == "wss" {
        dialer = dialer.with_tls(in_band_tls(args.ca_file.as_ref())?);
    }
    let limit = Duration::from_secs(args.timeout_seconds);
    let channel = dialer
        .dial(
            Kind::WebSocket,
            &args.url,
            limit.min(Duration::from_secs(10)),
        )
        .map_err(|failure| failure.to_string())?;
    let paired = pairing::join(&channel, &identity, pin, &store, limit)
        .map_err(|error| error.to_string())?;
    channel.close();
    println!("{}", paired.peer.id());
    Ok(ExitCode::SUCCESS)
}

fn status_code(status: std::process::ExitStatus) -> ExitCode {
    ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1),
    )
}

fn discover_mode(seconds: u64) -> Result<ExitCode, String> {
    for candidate in discovery::discover(Duration::from_secs(seconds)) {
        println!("{}", candidate.to_json());
    }
    Ok(ExitCode::SUCCESS)
}

/// What the remote may reach from here, for the peers named: another key this user paired with answering at the same address gets
/// nothing.
struct AllowedTargets {
    peers: BTreeSet<String>,
    grants: BTreeSet<String>,
}

impl PeerStore for AllowedTargets {
    fn get(&self, peer: &str) -> Result<Option<PeerRecord>, Error> {
        let mut record = PeerRecord::new(peer);
        if self.peers.contains(peer) {
            record.grants.clone_from(&self.grants);
        }
        Ok(Some(record))
    }

    fn put(&self, _record: &PeerRecord) -> Result<(), Error> {
        Err(Error::Identity(
            "these grants are fixed by the command line".to_owned(),
        ))
    }

    fn remove(&self, _peer: &str) -> Result<(), Error> {
        Err(Error::Identity(
            "these grants are fixed by the command line".to_owned(),
        ))
    }
}

fn forward_mode(args: &Forward) -> Result<ExitCode, String> {
    let locals = args
        .locals
        .iter()
        .map(|text| parse_local(text))
        .collect::<Result<Vec<_>, _>>()?;
    let mut grants = BTreeSet::new();
    for text in &args.allow_targets {
        let target: Target = text
            .parse()
            .map_err(|error: forward::BadTarget| error.to_string())?;
        grants.insert(forward::TCP_PROTOCOL.to_owned());
        grants.insert(target.grant_token());
    }
    if locals.is_empty() && grants.is_empty() {
        return Err("give --local or --allow-target: otherwise this forwards nothing".to_owned());
    }
    if !grants.is_empty() && args.peers.is_empty() {
        return Err("--allow-target needs --peer: it says who may reach it".to_owned());
    }
    let (method, store) = if args.paired {
        let identity = home::identity(None).map_err(|error| error.to_string())?;
        let store = Arc::new(home::store(None).map_err(|error| error.to_string())?);
        (
            WebCrypto::new(identity).with_responders(Trust::Store(TrustStore::clone(&store))),
            Some(store),
        )
    } else {
        let path = args
            .identity
            .as_deref()
            .ok_or("give --identity or --paired")?;
        let identity = Identity::open_or_create(path).map_err(|error| error.to_string())?;
        (
            WebCrypto::new(identity).with_responders(Trust::pinned(peers(&args.peers)?)),
            None,
        )
    };
    let mut dialer = websocket::Dialer::new()
        .with_limits(unproven_limits())
        .with_frames(Frames::Binary);
    if args.url.scheme() == "wss" {
        dialer = dialer.with_tls(in_band_tls(args.ca_file.as_ref())?);
    }
    let allowed = AllowedTargets {
        peers: args.peers.iter().cloned().collect(),
        grants,
    };
    let services = forward::serving(Services::default());
    let mut options = link::Options::default();
    options.limits.max_message = MAX_LINE;
    let longest = Duration::from_secs(60);
    let mut wait = Duration::from_secs(1);
    loop {
        match dialer.dial(Kind::WebSocket, &args.url, Duration::from_secs(10)) {
            Ok(channel) => match auth::prove_out(
                &method,
                &channel,
                DIAL_PROOF,
                MAX_LINE + auth::RECORD_OVERHEAD,
            ) {
                Ok(established) => {
                    wait = Duration::from_secs(1);
                    let peer = established.peer;
                    eprintln!("connected to {peer}");
                    let sealed = Sealed::new(&channel, established.session);
                    let sealed = match &store {
                        Some(store) => {
                            let store = Arc::clone(store);
                            let proven = peer;
                            sealed.with_check(move || store.is_accepted(&proven))
                        }
                        None => sealed,
                    };
                    let host = link::Host {
                        store: &allowed,
                        services: &services,
                        events: &NoEvents,
                    };
                    link::serve_with(
                        &sealed,
                        Side::Dialed,
                        options,
                        None,
                        &peer.to_string(),
                        &host,
                        |mux| {
                            // Says this is a link, which the far side otherwise waits to hear before it opens anything.
                            if let Err(failure) = link::capabilities(mux, Duration::from_secs(10)) {
                                eprintln!(
                                    "the remote said nothing about what it offers: {failure}"
                                );
                            }
                            thread::scope(|scope| {
                                for (local, target) in &locals {
                                    scope.spawn(move || listen_for(mux, *local, target));
                                }
                            });
                        },
                    );
                    sealed.close();
                }
                Err(Error::Proof(why)) => {
                    return Err(format!("the remote did not prove itself: {why}"));
                }
                Err(error) => eprintln!("the proof did not finish: {error}"),
            },
            Err(failure) => eprintln!("could not connect: {failure}"),
        }
        thread::sleep(wait);
        wait = (wait * 2).min(longest);
    }
}

fn grant_mode(peer: &str, protocol: &str, revoke: bool) -> Result<ExitCode, String> {
    let target = match protocol.strip_prefix("tcp/1:") {
        Some(target) => Some(
            target
                .parse::<Target>()
                .map_err(|error| error.to_string())?,
        ),
        None if mux::valid_protocol(protocol) => None,
        None => {
            return Err(
                "a protocol token is printable, with no spaces, commas or equals signs".to_owned(),
            );
        }
    };
    let store = FilePeers::new(home::grants_path().map_err(|error| error.to_string())?);
    let mut record = store
        .get(peer)
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| PeerRecord::new(peer));
    match (&target, revoke) {
        (Some(target), false) => record = forward::grant(record, target),
        (Some(target), true) => {
            record.grants.remove(&target.grant_token());
        }
        (None, false) => {
            record.grants.insert(protocol.to_owned());
        }
        (None, true) => {
            record.grants.remove(protocol);
            // Otherwise a later grant of one target would bring back all the old ones.
            if protocol == forward::TCP_PROTOCOL {
                record
                    .grants
                    .retain(|grant| Target::from_grant_token(grant).is_none());
            }
        }
    }
    store.put(&record).map_err(|error| error.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn companion_mode(action: &CompanionAction) -> Result<ExitCode, String> {
    match action {
        CompanionAction::Install { extensions } => {
            for written in companion::install(extensions).map_err(|error| error.to_string())? {
                println!("{written}");
            }
        }
        CompanionAction::Uninstall => companion::uninstall().map_err(|error| error.to_string())?,
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    // A browser starts a native host with its caller's origin as the first argument, and no way to name a subcommand.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|first| companion::started_by_browser(&first.to_string_lossy()))
    {
        let result = companion::run(&mut std::io::stdin().lock(), &mut std::io::stdout().lock());
        return result.map_or_else(
            |error| {
                eprintln!("coqui-acp-bridge: {error}");
                ExitCode::from(2)
            },
            |()| ExitCode::SUCCESS,
        );
    }
    let cli = Cli::parse();
    let result = match &cli.command {
        Mode::Keygen { path } => auth::create_identity(path)
            .map(|peer| {
                println!("{peer}");
                ExitCode::SUCCESS
            })
            .map_err(|error| error.to_string()),
        Mode::PairAccept(args) => pair_accept_mode(args),
        Mode::PairDial(args) => pair_dial_mode(args),
        Mode::Serve(args) => serve(args),
        Mode::Dial(args) => dial(args),
        Mode::Forward(args) => forward_mode(args),
        Mode::Grant {
            peer,
            protocol,
            revoke,
        } => grant_mode(peer, protocol, *revoke),
        Mode::Discover { seconds } => discover_mode(*seconds),
        Mode::Companion { action } => companion_mode(action),
    };
    match result {
        Ok(code) => code,
        Err(why) => {
            eprintln!("coqui-acp-bridge: {why}");
            ExitCode::from(2)
        }
    }
}
