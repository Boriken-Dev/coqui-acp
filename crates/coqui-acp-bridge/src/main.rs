//! `coqui-acp-bridge`: argument parsing and process concerns; the logic is in the library.
#![forbid(unsafe_code)]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs
)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use coqui_acp_bridge::agent::Agent;
use coqui_acp_bridge::auth;
use coqui_acp_bridge::relay::{LinkEnd, relay};
use coqui_net_acl::{Acl, Action, IpNet, Rule};
use coqui_peer::PeerId;
use coqui_peer_tls::{Identity, Pins, Roots, Trust};
use coqui_web::{Channel, Dialer, Failure, Kind, Limits, websocket, webtransport};
use url::Url;

const MAX_LINE: usize = 16 * 1024 * 1024;

/// Put a stdio Agent Client Protocol agent on the network, under a proven peer identity.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Create an identity file and print its peer id.
    Keygen {
        /// Where to write the key. An existing file is never replaced.
        path: PathBuf,
    },
    /// Wait for a peer, prove it, and give it the agent. One peer at a time, one agent per connection.
    Serve(Serve),
    /// Connect out to a peer, for an agent behind NAT. Reconnects and keeps the agent running.
    Dial(Dial),
}

#[derive(clap::Args, Debug)]
struct Serve {
    /// The address to listen on. Plain WebSocket is refused beyond loopback.
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: SocketAddr,
    /// This bridge's P-256 identity (see `keygen`).
    #[arg(long)]
    identity: PathBuf,
    /// A peer id allowed in. Repeat for more. With no peer and no certificate allowed, the bridge refuses to start: nothing
    /// is trusted on first use.
    #[arg(long = "allow", required_unless_present = "allow_cert")]
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
    /// A source network allowed to connect at all, like 10.6.0.0/16. Repeat for more. With any listed, everything else is
    /// dropped before its handshake. Checked first, in the order given, against `--deny`.
    #[arg(long = "permit")]
    permit: Vec<String>,
    /// A source network dropped before its handshake. Repeat for more. Takes precedence over `--permit`.
    #[arg(long = "deny")]
    deny: Vec<String>,
    /// How long a peer may take to prove itself, in seconds.
    #[arg(long, default_value_t = 15)]
    proof_seconds: u64,
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
    /// The fingerprint the remote certificate must have. Repeat for more. Needs no certificate authority.
    #[arg(long = "expect", required_unless_present = "ca_file")]
    expect: Vec<String>,
    /// A PEM bundle of certificate authorities the remote certificate may chain to, with its name checked, for a server
    /// behind a normal front door. May be combined with `--expect`.
    #[arg(long)]
    ca_file: Option<PathBuf>,
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

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_tls(path: &std::path::Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let identity = Identity::read(path)
        .map_err(|error| error.to_string())?
        .ok_or("the TLS identity file does not exist")?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(identity.certificate_chain(), identity.private_key_der())
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

fn dial_tls(
    path: &std::path::Path,
    expect: BTreeSet<PeerId>,
    ca_file: Option<&std::path::Path>,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let identity = Identity::open_or_create(path).map_err(|error| error.to_string())?;
    let trust = match ca_file {
        Some(file) if expect.is_empty() => Trust::Roots(Roots::bundle_file(file)),
        Some(file) => Trust::PinnedOrRoots {
            pins: Pins::fixed(expect),
            roots: Roots::bundle_file(file),
        },
        None => Trust::Pinned(Pins::fixed(expect)),
    };
    let verifier = trust
        .server_cert_verifier()
        .map_err(|error| error.to_string())?;
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(identity.certificate_chain(), identity.private_key_der())
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

fn limits() -> Limits {
    Limits::default().with_max_message(MAX_LINE)
}

/// Proves the peer on an accepted channel, gives it a fresh agent, and relays until either ends.
fn serve_one<C: Channel>(
    channel: &C,
    method: &coqui_peer_webcrypto::WebCryptoMethod,
    proof: Duration,
    agent: &[OsString],
) {
    match auth::prove(method, channel, proof) {
        Ok(peer) => {
            eprintln!("peer {peer} proven");
            let (program, args) = match split_agent(agent) {
                Ok(split) => split,
                Err(why) => return eprintln!("{why}"),
            };
            match Agent::spawn(program, args, MAX_LINE) {
                Ok(running) => {
                    let end = relay(channel, &running);
                    eprintln!("peer {peer} left: {end:?}");
                    let _ = running.stop();
                }
                Err(error) => eprintln!("the agent could not start: {error}"),
            }
        }
        Err(error) => eprintln!("a connection was refused: {error}"),
    }
    channel.close();
}

/// What may start a handshake: `--deny` first, then `--permit` if any are listed. A first-match ACL.
fn admission(args: &Serve) -> Result<Option<Acl>, String> {
    if args.permit.is_empty() && args.deny.is_empty() {
        return Ok(None);
    }
    let parse = |text: &String| {
        text.parse::<IpNet>()
            .map_err(|_| "a network is malformed".to_owned())
    };
    let default = if args.permit.is_empty() {
        Action::Permit
    } else {
        Action::Deny
    };
    let mut acl = Acl {
        rules: Vec::new(),
        default,
    };
    for network in &args.deny {
        acl.push(Rule::new(Action::Deny, parse(network)?));
    }
    for network in &args.permit {
        acl.push(Rule::new(Action::Permit, parse(network)?));
    }
    Ok(Some(acl))
}

fn mtls_server(
    path: &std::path::Path,
    certificates: BTreeSet<PeerId>,
) -> Result<Arc<rustls::ServerConfig>, String> {
    let identity = Identity::read(path)
        .map_err(|error| error.to_string())?
        .ok_or("the TLS identity file does not exist")?;
    let verifier = Trust::Pinned(Pins::fixed(certificates))
        .client_cert_verifier()
        .map_err(|error| error.to_string())?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.certificate_chain(), identity.private_key_der())
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

/// Mutual TLS proved the peer in the handshake: give it a fresh agent directly.
fn serve_mtls<C: Channel>(channel: &C, agent: &[OsString]) {
    let (program, args) = match split_agent(agent) {
        Ok(split) => split,
        Err(why) => return eprintln!("{why}"),
    };
    match Agent::spawn(program, args, MAX_LINE) {
        Ok(running) => {
            let end = relay(channel, &running);
            eprintln!("a certificate peer left: {end:?}");
            let _ = running.stop();
        }
        Err(error) => eprintln!("the agent could not start: {error}"),
    }
    channel.close();
}

fn serve(args: &Serve) -> Result<ExitCode, String> {
    if args.allow.is_empty() == args.allow_cert.is_empty() {
        return Err(
            "give peers with --allow or certificates with --allow-cert, not both".to_owned(),
        );
    }
    let allowed = peers(&args.allow)?;
    let acl = admission(args)?;
    let acl = Arc::new(acl);
    let admit = move |source: SocketAddr| {
        acl.as_ref()
            .as_ref()
            .is_none_or(|acl| acl.decide(source, source).proceeds())
    };
    if !args.allow_cert.is_empty() {
        let path = args
            .tls_identity
            .as_deref()
            .ok_or("--allow-cert needs --tls-identity")?;
        let tls = mtls_server(path, peers(&args.allow_cert)?)?;
        let listener = websocket::Listener::bind(args.listen, Some(tls))
            .map_err(|failure| failure.to_string())?
            .with_limits(limits())
            .with_admission(admit.clone());
        eprintln!(
            "listening on {}",
            listener
                .local_addr()
                .map_err(|failure| failure.to_string())?
        );
        loop {
            match listener.accept(None) {
                Ok(channel) => serve_mtls(&channel, &args.agent),
                Err(failure) => eprintln!("a connection failed: {failure}"),
            }
        }
    }
    let key = auth::read_identity(&args.identity).map_err(|error| error.to_string())?;
    let method = auth::method(key, allowed).map_err(|error| error.to_string())?;
    println!("{}", method.peer_id());
    let proof = Duration::from_secs(args.proof_seconds);
    let tls = args.tls_identity.as_deref().map(server_tls).transpose()?;
    let plain = websocket::Listener::bind(args.listen, tls.clone())
        .map_err(|failure| failure.to_string())?
        .with_limits(limits())
        .with_origins(args.origins.clone())
        .with_admission(admit.clone());
    eprintln!(
        "listening on {}",
        plain.local_addr().map_err(|failure| failure.to_string())?
    );
    if let Some(port) = args.webtransport_port {
        let tls = tls.ok_or("WebTransport needs --tls-identity")?;
        let address = SocketAddr::new(args.listen.ip(), port);
        let quic = webtransport::Listener::bind(address, tls)
            .map_err(|failure| failure.to_string())?
            .with_limits(limits())
            .with_origins(args.origins.clone())
            .with_admission(admit.clone());
        let method = method.clone();
        let agent = args.agent.clone();
        thread::spawn(move || {
            loop {
                match quic.accept(None) {
                    Ok(channel) => serve_one(&channel, &method, proof, &agent),
                    Err(failure) => eprintln!("a WebTransport connection failed: {failure}"),
                }
            }
        });
    }
    loop {
        match plain.accept(None) {
            Ok(channel) => serve_one(&channel, &method, proof, &args.agent),
            Err(failure) => eprintln!("a connection failed: {failure}"),
        }
    }
}

fn dial(args: &Dial) -> Result<ExitCode, String> {
    if args.url.scheme() != "wss" {
        return Err("dial needs a wss:// URL".to_owned());
    }
    let expect = peers(&args.expect)?;
    let tls = dial_tls(&args.tls_identity, expect, args.ca_file.as_deref())?;
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
                if end == LinkEnd::AgentEnded {
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
