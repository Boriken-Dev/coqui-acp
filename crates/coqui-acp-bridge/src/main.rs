//! `coqui-acp-bridge`: argument parsing and process concerns; the logic is in the library.
#![forbid(unsafe_code)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs
)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use coqui_acp_bridge::agent::Agent;
use coqui_acp_bridge::auth::Sealed;
use coqui_acp_bridge::relay::{LinkEnd, relay};
use coqui_acp_bridge::{Error, auth, pairing};
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
}

#[derive(clap::Args, Debug)]
struct PairAccept {
    /// The address to wait on, for a WebSocket. On loopback it is plain, for a reverse proxy in front; any other address
    /// needs `--tls-identity`. The PIN authenticates either way.
    #[arg(long)]
    listen: SocketAddr,
    /// This side's identity, whose key is the one paired. Created if absent.
    #[arg(long)]
    identity: PathBuf,
    /// A TLS identity for wss://, when this address is not loopback.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
    /// The directory of the trust store that records the peer. Use it with `serve --allow-store`.
    #[arg(long)]
    store: PathBuf,
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
    /// This side's identity, whose key is the one paired. Created if absent.
    #[arg(long)]
    identity: PathBuf,
    /// The directory of the trust store that records the peer. Use it with `dial --trust-store`.
    #[arg(long)]
    store: PathBuf,
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
    #[arg(long = "allow", required_unless_present_any = ["allow_cert", "allow_store"])]
    allow: Vec<String>,
    /// A trust store whose accepted peers are allowed in, proved in-band, read on each connection and while it lasts, so a
    /// peer paired or removed later takes effect at once.
    #[arg(long)]
    allow_store: Option<PathBuf>,
    /// A TLS identity (certificate and key, PEM) for wss:// and WebTransport. Without it, plain ws:// on loopback only, where
    /// a reverse proxy may stand in front: the in-band proof and its session do not depend on TLS.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
    /// A client certificate fingerprint allowed in over mutual TLS, in place of the in-band proof. Repeat for more. Needs
    /// `--tls-identity`. The stream is then plain ACP protected only by that TLS, so it ends where TLS ends.
    #[arg(long = "allow-cert")]
    allow_cert: Vec<String>,
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
    /// The agent program, then its arguments.
    #[arg(last = true, required = true)]
    agent: Vec<OsString>,
}

#[derive(clap::Args, Debug)]
struct Dial {
    /// The URL to connect to: wss://, or ws:// on loopback for the in-band proof.
    url: Url,
    /// This side's identity, for the in-band proof, which works through a reverse proxy. Created if absent. Everything after
    /// the proof is inside its session.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// A peer id the remote must prove, with `--identity`. Repeat for more.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// A trust store whose accepted peers the remote may prove, with `--identity`, as `pair-dial` records them.
    #[arg(long)]
    trust_store: Option<PathBuf>,
    /// A TLS identity (certificate and key, PEM) to present over mutual TLS, in place of the in-band proof. Created if absent.
    /// A reverse proxy must vouch for the certificate.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
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

fn mtls_client(path: &std::path::Path, args: &Dial) -> Result<Arc<rustls::ClientConfig>, String> {
    let identity = tls::Identity::open_or_create(path).map_err(|error| error.to_string())?;
    let pins = tls::Pins::fixed(peers(&args.expect)?);
    let trust = match &args.ca_file {
        Some(file) if !args.expect.is_empty() => tls::Trust::PinnedOrRoots {
            pins,
            roots: tls::Roots::bundle_file(file),
        },
        Some(file) => tls::Trust::Roots(tls::Roots::bundle_file(file)),
        None => tls::Trust::Pinned(pins),
    };
    // This builder turns resumption off, so a peer that lost trust cannot return on a session.
    let config = tls::client_config(&identity, &trust, &[]).map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
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
struct Service {
    method: Option<WebCrypto>,
    store: Option<Arc<TrustStore>>,
    proof: Duration,
    agent: Vec<OsString>,
    unproven: Gate,
    peers: Gate,
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
                self.give_agent(&sealed, &label);
                sealed.close();
            }
            Err(error) => eprintln!("a connection was refused: {error}"),
        }
        channel.close();
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

fn serve(args: &Serve) -> Result<ExitCode, String> {
    let modes = [
        !args.allow.is_empty(),
        !args.allow_cert.is_empty(),
        args.allow_store.is_some(),
    ];
    if modes.iter().filter(|mode| **mode).count() != 1 {
        return Err("give exactly one of --allow, --allow-cert and --allow-store".to_owned());
    }
    let admit = admission(&args.permit, &args.deny)?;
    let unproven = Gate::new(MAX_UNPROVEN);
    let running = Gate::new(args.max_peers);
    if !args.allow_cert.is_empty() {
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
        let pins = tls::Pins::fixed(peers(&args.allow_cert)?);
        // This builder turns resumption off, so a peer that lost trust cannot return on a session.
        let config = tls::server_config(&read_tls_identity(path)?, &tls::Trust::Pinned(pins), &[])
            .map_err(|error| error.to_string())?;
        let listener = websocket::Listener::bind(args.listen, Some(Arc::new(config)))
            .map_err(|failure| failure.to_string())?
            .with_limits(limits())
            .with_origins(args.origins.clone())
            .with_admission(admit);
        eprintln!(
            "listening on {}",
            listener
                .local_addr()
                .map_err(|failure| failure.to_string())?
        );
        let service = Arc::new(Service {
            method: None,
            store: None,
            proof: Duration::ZERO,
            agent: args.agent.clone(),
            unproven,
            peers: running,
        });
        accept_loop(
            || listener.next(None),
            websocket::Pending::handshake,
            &service,
        );
    }
    let identity = args
        .identity
        .as_deref()
        .ok_or("the in-band proof needs --identity (see keygen)")?;
    let identity = auth::read_identity(identity).map_err(|error| error.to_string())?;
    let peer_id = identity.peer_id();
    let store = args
        .allow_store
        .as_deref()
        .map(TrustStore::open)
        .transpose()
        .map_err(|error| error.to_string())?
        .map(Arc::new);
    let trust = match &store {
        Some(store) => Trust::Store(TrustStore::clone(store)),
        None => Trust::pinned(peers(&args.allow)?),
    };
    let method = WebCrypto::new(identity).with_initiators(trust);
    println!("{peer_id}");
    let _ = std::io::stdout().flush();
    let proof = Duration::from_secs(args.proof_seconds.unwrap_or(15));
    let tls_config = args.tls_identity.as_deref().map(server_tls).transpose()?;
    let plain = websocket::Listener::bind(args.listen, tls_config.clone())
        .map_err(|failure| failure.to_string())?
        .with_limits(unproven_limits())
        .with_frames(Frames::Binary)
        .with_origins(args.origins.clone())
        .with_admission(admit.clone());
    eprintln!(
        "listening on {}",
        plain.local_addr().map_err(|failure| failure.to_string())?
    );
    let service = Arc::new(Service {
        method: Some(method),
        store,
        proof,
        agent: args.agent.clone(),
        unproven,
        peers: running,
    });
    if let Some(port) = args.webtransport_port {
        let tls_config = tls_config.ok_or("WebTransport needs --tls-identity")?;
        let address = SocketAddr::new(args.listen.ip(), port);
        let quic = webtransport::Listener::bind(address, tls_config)
            .map_err(|failure| failure.to_string())?
            .with_limits(unproven_limits())
            .with_origins(args.origins.clone())
            .with_admission(admit);
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

/// How `dial` knows its peer.
enum DialProof {
    InBand(WebCrypto, Option<Arc<TrustStore>>),
    Certificate,
}

fn dial_setup(args: &Dial) -> Result<(DialProof, websocket::Dialer), String> {
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
            let mut dialer = websocket::Dialer::new()
                .with_limits(unproven_limits())
                .with_frames(Frames::Binary);
            if args.url.scheme() == "wss" {
                dialer = dialer.with_tls(in_band_tls(args.ca_file.as_ref())?);
            }
            Ok((DialProof::InBand(method, store), dialer))
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
            Ok((DialProof::Certificate, dialer))
        }
        _ => Err("give exactly one of --identity and --tls-identity".to_owned()),
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
    loop {
        if let Some(status) = agent.try_status().map_err(|error| error.to_string())? {
            return Ok(status_code(status));
        }
        match dialer.dial(Kind::WebSocket, &args.url, Duration::from_secs(10)) {
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
            Err(failure) => eprintln!("could not connect: {failure}"),
        }
        thread::sleep(wait);
        wait = (wait * 2).min(longest);
    }
}

fn pair_accept_mode(args: &PairAccept) -> Result<ExitCode, String> {
    let identity =
        Arc::new(Identity::open_or_create(&args.identity).map_err(|error| error.to_string())?);
    let store = Arc::new(TrustStore::open(&args.store).map_err(|error| error.to_string())?);
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
    let identity = Identity::open_or_create(&args.identity).map_err(|error| error.to_string())?;
    let store = TrustStore::open(&args.store).map_err(|error| error.to_string())?;
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

fn main() -> ExitCode {
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
    };
    match result {
        Ok(code) => code,
        Err(why) => {
            eprintln!("coqui-acp-bridge: {why}");
            ExitCode::from(2)
        }
    }
}
