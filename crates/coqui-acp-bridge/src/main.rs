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
use coqui_acp_bridge::relay::{LinkEnd, relay};
use coqui_acp_bridge::{auth, pairing};
use coqui_net_acl::{Acl, Action, IpNet, Rule};
use coqui_peer::PeerId;
use coqui_peer::TrustStore;
use coqui_peer_tls::pair::{CodeAlphabet, Pin};
use coqui_peer_tls::{
    Identity, Pins, Roots, Trust, client_config, client_config_without_identity, server_config,
    server_config_without_client_auth,
};
use coqui_web::websocket::Frames;
use coqui_web::{Channel, Dialer, Endpoints, Failure, Kind, Limits, websocket, webtransport};
use url::Url;

const MAX_LINE: usize = 16 * 1024 * 1024;
/// Connections that have not proved themselves yet, each a thread for at most the handshake and proof limits.
const MAX_UNPROVEN: usize = 64;
/// Connections to one `pair-accept` at once.
const MAX_PAIRING_RUNS: usize = 16;
/// How long one connection may take to pair. `pair-dial` asks for the PIN before it connects, so no person is waited for.
const PAIRING_RUN: Duration = Duration::from_secs(60);
/// The largest pairing message: certificates and PAKE messages, nothing a peer may grow.
const PAIR_MESSAGE: usize = 64 * 1024;
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
    /// serves wss with `--tls-identity`'s certificate. The PIN authenticates either way.
    #[arg(long)]
    listen: SocketAddr,
    /// This side's TLS identity, whose key is the one paired. Created if absent.
    #[arg(long)]
    tls_identity: PathBuf,
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
    /// This side's TLS identity, whose key is the one paired. Created if absent.
    #[arg(long)]
    tls_identity: PathBuf,
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
    /// This bridge's P-256 identity (see `keygen`), for `--allow`.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// A peer id allowed in. Repeat for more. With no peer and no certificate allowed, the bridge refuses to start: nothing
    /// is trusted on first use.
    #[arg(long = "allow", required_unless_present_any = ["allow_cert", "allow_store"])]
    allow: Vec<String>,
    /// A TLS identity (certificate and key, PEM) for wss:// and WebTransport. Without it, plain ws:// on loopback only.
    #[arg(long)]
    tls_identity: Option<PathBuf>,
    /// Serve WebTransport on this UDP port too; needs `--tls-identity`.
    #[arg(long)]
    webtransport_port: Option<u16>,
    /// A browser origin allowed in. Repeat for more. A non-browser client sends none. With none listed, no origin is checked.
    #[arg(long = "origin")]
    origins: Vec<String>,
    /// A client certificate fingerprint allowed in over mutual TLS, in place of the in-band proof. Repeat for more. Needs
    /// `--tls-identity`. The stream is then plain ACP, for a native client that cannot run the browser method.
    #[arg(long = "allow-cert")]
    allow_cert: Vec<String>,
    /// A trust store whose accepted peers are allowed in over mutual TLS, read on each connection, so a peer paired or removed
    /// later takes effect at once. Needs `--tls-identity`.
    #[arg(long)]
    allow_store: Option<PathBuf>,
    /// A source network allowed to connect at all, like 10.6.0.0/16. Repeat for more. With any listed, everything else is
    /// dropped before its handshake. Checked first, in the order given, against `--deny`.
    #[arg(long = "permit")]
    permit: Vec<String>,
    /// A source network dropped before its handshake. Repeat for more. Takes precedence over `--permit`.
    #[arg(long = "deny")]
    deny: Vec<String>,
    /// How long a peer may take to prove itself, in seconds, with `--allow`. Default 15.
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
    /// The `wss://` URL to connect to.
    url: Url,
    /// A TLS identity (certificate and key, PEM) this bridge presents. Created if absent.
    #[arg(long)]
    tls_identity: PathBuf,
    /// A fingerprint the remote certificate may have. Repeat for more. Needs no certificate authority. With `--ca-file`,
    /// either a listed fingerprint or a certificate from that authority is enough.
    #[arg(long = "expect", required_unless_present_any = ["ca_file", "trust_store"])]
    expect: Vec<String>,
    /// A PEM bundle of certificate authorities the remote certificate may chain to, with its name checked, for a server
    /// behind a normal front door. May be combined with `--expect`.
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// A trust store whose accepted peers the remote certificate may be, as `pair-dial` records them.
    #[arg(long)]
    trust_store: Option<PathBuf>,
    /// A file holding a token to send as `Authorization: Bearer`, in addition to the client certificate. A token is
    /// never an argument, since arguments are visible to other users.
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

fn read_tls_identity(path: &std::path::Path) -> Result<Identity, String> {
    Identity::read(path)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the TLS identity file does not exist".to_owned())
}

fn server_tls(path: &std::path::Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let config = server_config_without_client_auth(&read_tls_identity(path)?, &[])
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

fn dial_tls(args: &Dial) -> Result<Arc<rustls::ClientConfig>, String> {
    let identity =
        Identity::open_or_create(&args.tls_identity).map_err(|error| error.to_string())?;
    let expect = peers(&args.expect)?;
    let pins = match &args.trust_store {
        Some(path) if expect.is_empty() => {
            Pins::store(TrustStore::open(path).map_err(|error| error.to_string())?)
        }
        Some(_) => return Err("give --expect or --trust-store, not both".to_owned()),
        None => Pins::fixed(expect),
    };
    let pinned = !args.expect.is_empty() || args.trust_store.is_some();
    let trust = match &args.ca_file {
        Some(file) if pinned => Trust::PinnedOrRoots {
            pins,
            roots: Roots::bundle_file(file),
        },
        Some(file) => Trust::Roots(Roots::bundle_file(file)),
        None => Trust::Pinned(pins),
    };
    // This builder turns resumption off, so a peer removed from a store cannot return on a session.
    let config = client_config(&identity, &trust, &[]).map_err(|error| error.to_string())?;
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
        self.0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |running| {
                (running < self.1).then_some(running + 1)
            })
            .ok()
            .map(|_| Entered(Arc::clone(&self.0)))
    }
}

/// What every connection of a `serve` shares.
struct Service {
    method: Option<coqui_peer_webcrypto::WebCryptoMethod>,
    proof: Duration,
    agent: Vec<OsString>,
    unproven: Gate,
    peers: Gate,
}

impl Service {
    /// Proves the peer unless TLS already did, gives it a fresh agent, and relays until either ends.
    fn run<C: Channel>(&self, channel: &C, unproven: Entered) {
        let peer = match &self.method {
            Some(method) => match auth::prove(method, channel, self.proof, MAX_LINE) {
                Ok(peer) => peer.to_string(),
                Err(error) => {
                    eprintln!("a connection was refused: {error}");
                    return channel.close();
                }
            },
            None => "a certificate peer".to_owned(),
        };
        drop(unproven);
        eprintln!("{peer} proven");
        let Some(_running) = self.peers.enter() else {
            eprintln!("{peer} was turned away: --max-peers are running");
            return channel.close();
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
        channel.close();
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
    let peers_running = Gate::new(args.max_peers);
    if !args.allow_cert.is_empty() || args.allow_store.is_some() {
        // Silently dropping these would leave an operator believing they apply.
        if args.identity.is_some() || args.proof_seconds.is_some() {
            return Err("--identity and --proof-seconds belong to --allow".to_owned());
        }
        if args.webtransport_port.is_some() {
            return Err("WebTransport has no client certificate here: it needs --allow".to_owned());
        }
        let path = args
            .tls_identity
            .as_deref()
            .ok_or("mutual TLS needs --tls-identity")?;
        let pins = match &args.allow_store {
            Some(store) => Pins::store(TrustStore::open(store).map_err(|error| error.to_string())?),
            None => Pins::fixed(peers(&args.allow_cert)?),
        };
        // This builder turns resumption off, so a peer removed from a store cannot return on a session.
        let tls = server_config(&read_tls_identity(path)?, &Trust::Pinned(pins), &[])
            .map_err(|error| error.to_string())?;
        let listener = websocket::Listener::bind(args.listen, Some(Arc::new(tls)))
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
            proof: Duration::ZERO,
            agent: args.agent.clone(),
            unproven: Gate::new(MAX_UNPROVEN),
            peers: peers_running,
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
        .ok_or("--allow needs --identity (see keygen)")?;
    let key = auth::read_identity(identity).map_err(|error| error.to_string())?;
    let method = auth::method(key, peers(&args.allow)?).map_err(|error| error.to_string())?;
    println!("{}", method.peer_id());
    let _ = std::io::stdout().flush();
    let proof = Duration::from_secs(args.proof_seconds.unwrap_or(15));
    let tls = args.tls_identity.as_deref().map(server_tls).transpose()?;
    let plain = websocket::Listener::bind(args.listen, tls.clone())
        .map_err(|failure| failure.to_string())?
        .with_limits(unproven_limits())
        .with_origins(args.origins.clone())
        .with_admission(admit.clone());
    eprintln!(
        "listening on {}",
        plain.local_addr().map_err(|failure| failure.to_string())?
    );
    let service = Arc::new(Service {
        method: Some(method),
        proof,
        agent: args.agent.clone(),
        unproven: Gate::new(MAX_UNPROVEN),
        peers: peers_running,
    });
    if let Some(port) = args.webtransport_port {
        let tls = tls.ok_or("WebTransport needs --tls-identity")?;
        let address = SocketAddr::new(args.listen.ip(), port);
        let quic = webtransport::Listener::bind(address, tls)
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

fn dial(args: &Dial) -> Result<ExitCode, String> {
    if args.url.scheme() != "wss" {
        return Err("dial needs a wss:// URL".to_owned());
    }
    let tls = dial_tls(args)?;
    let (program, rest) = split_agent(&args.agent)?;
    let agent = Agent::spawn(program, rest, MAX_LINE)
        .map_err(|error| format!("the agent could not start: {error}"))?;
    let mut dialer = websocket::Dialer::new().with_tls(tls).with_limits(limits());
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
                wait = Duration::from_secs(1);
                let end = relay(&channel, &agent);
                channel.close();
                eprintln!("the link ended: {end:?}");
                if matches!(end, LinkEnd::AgentEnded | LinkEnd::AgentStuck) {
                    let status = agent.stop().map_err(|error| error.to_string())?;
                    return Ok(status_code(status));
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
        Arc::new(Identity::open_or_create(&args.tls_identity).map_err(|error| error.to_string())?);
    let store = Arc::new(TrustStore::open(&args.store).map_err(|error| error.to_string())?);
    let tls = if args.listen.ip().is_loopback() {
        None
    } else {
        Some(server_tls(&args.tls_identity)?)
    };
    let listener = websocket::Listener::bind(args.listen, tls)
        .map_err(|failure| failure.to_string())?
        .with_frames(Frames::Binary)
        .with_limits(
            Limits::default()
                .with_max_message(PAIR_MESSAGE)
                .with_pending(8),
        )
        .with_admission(admission(&args.permit, &args.deny)?);
    let desk = pairing::desk().map_err(|error| error.to_string())?;
    let shown = desk.show_pin().map_err(|error| error.to_string())?;
    println!("{}", shown.pin.as_str());
    println!("{}", identity.fingerprint());
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
    let identity =
        Identity::open_or_create(&args.tls_identity).map_err(|error| error.to_string())?;
    let store = TrustStore::open(&args.store).map_err(|error| error.to_string())?;
    Endpoints::builder()
        .websocket(args.url.as_str())
        .allow_plain_loopback(true)
        .build()
        .map_err(|error| error.to_string())?;
    let mut dialer = websocket::Dialer::new().with_frames(Frames::Binary);
    if args.url.scheme() == "wss" {
        let trust = match &args.ca_file {
            Some(file) => Trust::Roots(Roots::bundle_file(file)),
            None => Trust::AnyWithKeyProof,
        };
        let config =
            client_config_without_identity(&trust, &[]).map_err(|error| error.to_string())?;
        dialer = dialer.with_tls(Arc::new(config));
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
