//! The real binary, a real agent process, a real socket, and for the browser method a real JavaScript initiator.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");

struct Fixture {
    dir: PathBuf,
    bridge: Option<Child>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, bridge: None }
    }

    fn keygen(&self, name: &str) -> (PathBuf, String) {
        let path = self.dir.join(name);
        let output = Command::new(BRIDGE)
            .args(["keygen"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            path,
            String::from_utf8(output.stdout).unwrap().trim().to_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(bridge) = &mut self.bridge {
            let _ = bridge.kill();
            let _ = bridge.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The echo agent built beside the test binary: `target/debug/examples/echo_agent`.
fn echo_agent() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "coqui-acp-bridge", "--example", "echo_agent"])
        .status()
        .unwrap();
    assert!(status.success());
    let mut path = PathBuf::from(BRIDGE);
    path.pop();
    path.join("examples")
        .join(format!("echo_agent{}", std::env::consts::EXE_SUFFIX))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn node() -> Option<PathBuf> {
    let found = std::env::var_os("COQUI_ACP_TEST_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("node"));
    let version = Command::new(&found).arg("--version").output().ok()?;
    let text = String::from_utf8(version.stdout).ok()?;
    let major: u32 = text
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()?;
    (major >= 22).then_some(found)
}

fn webcrypto_module() -> Option<PathBuf> {
    let path = std::env::var_os("COQUI_ACP_TEST_WEBCRYPTO").map(PathBuf::from)?;
    path.exists().then_some(path)
}

/// A JavaScript initiator waiting for its target: its peer id, then a call to `go`.
struct Initiator {
    child: Child,
    peer: String,
}

impl Initiator {
    fn start(node: &Path, module: &Path) -> Self {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("initiator.mjs");
        let mut child = Command::new(node)
            .arg(script)
            .arg(module)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut peer = String::new();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        reader.read_line(&mut peer).unwrap();
        child.stdout = Some(reader.into_inner());
        Self {
            child,
            peer: peer.trim().to_owned(),
        }
    }

    fn go(mut self, port: u16, expected: &str) -> std::process::Output {
        let mut stdin = self.child.stdin.take().unwrap();
        writeln!(stdin, "{port} {expected}").unwrap();
        drop(stdin);
        self.child.wait_with_output().unwrap()
    }
}

fn start_serve(
    fixture: &mut Fixture,
    identity: &Path,
    allow: &str,
    port: u16,
    log: Option<&Path>,
) -> String {
    let mut command = Command::new(BRIDGE);
    command
        .arg("serve")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--identity")
        .arg(identity)
        .arg("--allow")
        .arg(allow)
        .arg("--proof-seconds")
        .arg("5")
        .arg("--")
        .arg(echo_agent())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(log) = log {
        command.env("ECHO_AGENT_LOG", log);
    }
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    fixture.bridge = Some(child);
    wait_for_port(port);
    line.trim().to_owned()
}

fn wait_for_port(port: u16) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the bridge did not listen");
}

#[test]
fn it_will_not_start_without_an_allowed_peer() {
    let fixture = Fixture::new("noallow");
    let (identity, _) = fixture.keygen("id.pem");
    let output = Command::new(BRIDGE)
        .arg("serve")
        .arg("--identity")
        .arg(&identity)
        .arg("--")
        .arg("x")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow"));
}

#[test]
fn keygen_never_replaces_a_file_and_help_takes_no_secret_value() {
    let fixture = Fixture::new("keygen");
    let (path, peer) = fixture.keygen("id.pem");
    assert!(peer.starts_with("sha256:"), "{peer}");
    let again = Command::new(BRIDGE)
        .arg("keygen")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!again.status.success());
    for mode in ["serve", "dial"] {
        let help = Command::new(BRIDGE)
            .args([mode, "--help"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&help.stdout).to_lowercase();
        assert!(
            !text.contains("--token") && !text.contains("--password") && !text.contains("--secret"),
            "{text}"
        );
    }
}

#[test]
fn a_plain_listener_beyond_loopback_is_refused() {
    let fixture = Fixture::new("plain");
    let (identity, peer) = fixture.keygen("id.pem");
    let output = Command::new(BRIDGE)
        .args([
            "serve",
            "--listen",
            "0.0.0.0:0",
            "--allow",
            &peer,
            "--identity",
        ])
        .arg(&identity)
        .args(["--", "x"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn a_proven_browser_peer_gets_the_agent_and_a_stranger_never_reaches_it() {
    let (Some(node), Some(module)) = (node(), webcrypto_module()) else {
        eprintln!(
            "SKIPPED: needs Node 22 or later and COQUI_ACP_TEST_WEBCRYPTO naming coqui-peer's methods/webcrypto/src/index.mjs"
        );
        return;
    };
    let mut fixture = Fixture::new("proof");
    let (identity, _) = fixture.keygen("id.pem");
    let allowed = Initiator::start(&node, &module);
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let server_id = start_serve(&mut fixture, &identity, &allowed.peer, port, Some(&log));

    let stranger = Initiator::start(&node, &module).go(port, &server_id);
    assert!(
        !stranger.status.success(),
        "an unlisted peer must be refused"
    );
    assert!(!log.exists(), "no byte from a stranger may reach an agent");

    let output = allowed.go(port, &server_id);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("echoed: hello through the bridge"), "{text}");
    assert!(log.exists());
}

/// A native peer: a wss listener that demands this client certificate, or a client presenting one.
mod native {
    use std::path::Path;
    use std::sync::Arc;

    use coqui_peer_tls::{Identity, Pins, Trust};

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    pub fn identity(path: &Path) -> Identity {
        Identity::open_or_create(path).unwrap()
    }

    pub fn server_requiring(server: &Identity, client: &Identity) -> Arc<rustls::ServerConfig> {
        let verifier = Trust::Pinned(Pins::fixed([client.fingerprint()]))
            .client_cert_verifier()
            .unwrap();
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_client_cert_verifier(verifier)
                .with_single_cert(server.certificate_chain(), server.private_key_der())
                .unwrap(),
        )
    }

    pub fn client_of(client: &Identity, server: &Identity) -> Arc<rustls::ClientConfig> {
        let verifier = Trust::Pinned(Pins::fixed([server.fingerprint()]))
            .server_cert_verifier()
            .unwrap();
        Arc::new(
            rustls::ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_client_auth_cert(client.certificate_chain(), client.private_key_der())
                .unwrap(),
        )
    }
}

use coqui_web::Channel as _;

fn initialize<C: coqui_web::Channel>(channel: &C) {
    channel
        .send(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#)
        .unwrap();
    let answer = channel
        .receive(Some(Duration::from_secs(20)))
        .unwrap()
        .unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["result"]["protocolVersion"], 1, "{answer}");
}

#[test]
fn dial_reaches_out_reconnects_with_the_agent_alive_and_ends_with_its_status() {
    let fixture = Fixture::new("dial");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let tls = native::server_requiring(&server, &client);
    let listener =
        coqui_web::websocket::Listener::bind("127.0.0.1:0".parse().unwrap(), Some(tls)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut bridge = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("wss://127.0.0.1:{port}/agent"))
        .arg("--tls-identity")
        .arg(fixture.dir.join("client.pem"))
        .arg("--expect")
        .arg(server.fingerprint().to_string())
        .arg("--max-backoff-seconds")
        .arg("2")
        .arg("--")
        .arg(echo_agent())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let wait = Some(Duration::from_secs(30));
    let first = listener.accept(wait).unwrap();
    initialize(&first);
    // A dropped link: the bridge connects again, to the same agent.
    coqui_web::Channel::close(&first);
    let second = listener.accept(wait).unwrap();
    initialize(&second);
    second
        .send(br#"{"jsonrpc":"2.0","id":2,"method":"exit"}"#)
        .unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = bridge.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < until,
            "the bridge should end with its agent"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(7), "the agent's own exit status");
}

#[test]
fn dial_stops_on_the_wrong_certificate_rather_than_knocking_forever() {
    let fixture = Fixture::new("wrongcert");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let stranger = native::identity(&fixture.dir.join("stranger.pem"));
    let tls = native::server_requiring(&server, &client);
    // Built first: another test's build can hold the lock for longer than the listener would wait.
    let agent = echo_agent();
    let listener =
        coqui_web::websocket::Listener::bind("127.0.0.1:0".parse().unwrap(), Some(tls)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepting = std::thread::spawn(move || {
        let _ = listener.accept(Some(Duration::from_secs(25)));
    });
    let log = fixture.dir.join("agent.log");
    let mut child = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("wss://127.0.0.1:{port}/agent"))
        .arg("--tls-identity")
        .arg(fixture.dir.join("client.pem"))
        .arg("--expect")
        .arg(stranger.fingerprint().to_string())
        .arg("--")
        .arg(agent)
        .env("ECHO_AGENT_LOG", &log)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > until {
            let _ = child.kill();
            panic!("the bridge kept knocking");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(status.code(), Some(2));
    assert!(!log.exists());
    accepting.join().unwrap();
}

fn start_mtls(
    fixture: &mut Fixture,
    server: &Path,
    allow: &str,
    port: u16,
    extra: &[&str],
    log: &Path,
) {
    let child = Command::new(BRIDGE)
        .arg("serve")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--tls-identity")
        .arg(server)
        .arg("--allow-cert")
        .arg(allow)
        .args(extra)
        .arg("--")
        .arg(echo_agent())
        .env("ECHO_AGENT_LOG", log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.bridge = Some(child);
    wait_for_port(port);
}

#[test]
fn a_listed_certificate_gets_plain_acp_and_another_never_reaches_the_agent() {
    use coqui_web::Dialer;
    let mut fixture = Fixture::new("mtls");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let stranger = native::identity(&fixture.dir.join("stranger.pem"));
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let server_path = fixture.dir.join("server.pem");
    start_mtls(
        &mut fixture,
        &server_path,
        &client.fingerprint().to_string(),
        port,
        &[],
        &log,
    );
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/agent")).unwrap();
    let wait = Duration::from_secs(10);
    let refused = coqui_web::websocket::Dialer::new()
        .with_tls(native::client_of(&stranger, &server))
        .dial(coqui_web::Kind::WebSocket, &url, wait);
    if let Ok(channel) = refused {
        // TLS 1.3 reports a refused client certificate only on the first read.
        assert!(!matches!(
            coqui_web::Channel::receive(&channel, Some(wait)),
            Ok(Some(_))
        ));
    }
    assert!(!log.exists(), "no byte from a stranger may reach an agent");
    let channel = coqui_web::websocket::Dialer::new()
        .with_tls(native::client_of(&client, &server))
        .dial(coqui_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
    assert!(log.exists());
}

#[test]
fn an_acl_drops_a_denied_source_before_its_handshake() {
    use coqui_web::Dialer;
    let mut fixture = Fixture::new("acl");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let server_path = fixture.dir.join("server.pem");
    start_mtls(
        &mut fixture,
        &server_path,
        &client.fingerprint().to_string(),
        port,
        &["--deny", "127.0.0.0/8"],
        &log,
    );
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/agent")).unwrap();
    let dialed = coqui_web::websocket::Dialer::new()
        .with_tls(native::client_of(&client, &server))
        .dial(coqui_web::Kind::WebSocket, &url, Duration::from_secs(5));
    assert!(
        dialed.is_err(),
        "even an allowed certificate must not get past a denied source"
    );
    assert!(!log.exists());
}

#[test]
fn dial_trusts_a_private_ca_by_name_on_a_versioned_path() {
    let fixture = Fixture::new("privateca");
    let ca = rcgen_issue(&fixture.dir);
    let server =
        coqui_web::websocket::Listener::bind("127.0.0.1:0".parse().unwrap(), Some(ca.server))
            .unwrap();
    let port = server.local_addr().unwrap().port();
    let bearer = fixture.dir.join("token");
    std::fs::write(&bearer, "secret-token\n").unwrap();
    let mut bridge = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("wss://localhost:{port}/v1/assistant/agent-link"))
        .arg("--tls-identity")
        .arg(fixture.dir.join("client.pem"))
        .arg("--ca-file")
        .arg(&ca.bundle)
        .arg("--bearer-file")
        .arg(&bearer)
        .arg("--")
        .arg(echo_agent())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let channel = server.accept(Some(Duration::from_secs(30))).unwrap();
    initialize(&channel);
    let _ = bridge.kill();
    let _ = bridge.wait();
}

struct PrivateCa {
    bundle: PathBuf,
    server: std::sync::Arc<rustls::ServerConfig>,
}

fn rcgen_issue(dir: &Path) -> PrivateCa {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.clone().self_signed(&ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf = params.signed_by(&key, &issuer).unwrap();
    let bundle = dir.join("ca.pem");
    std::fs::write(&bundle, ca.pem()).unwrap();
    let server = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    PrivateCa {
        bundle,
        server: std::sync::Arc::new(server),
    }
}

fn start_store_bridge(fixture: &mut Fixture, server: &Path, store: &Path, port: u16, log: &Path) {
    let child = Command::new(BRIDGE)
        .arg("serve")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--tls-identity")
        .arg(server)
        .arg("--allow-store")
        .arg(store)
        .arg("--")
        .arg(echo_agent())
        .env("ECHO_AGENT_LOG", log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fixture.bridge = Some(child);
    wait_for_port(port);
}

#[test]
fn a_peer_removed_from_the_store_cannot_return_on_a_session_it_holds() {
    use coqui_peer_tls::TrustStoreExt as _;
    use coqui_web::Dialer;
    let mut fixture = Fixture::new("removed");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let store = coqui_peer::TrustStore::open(&fixture.dir.join("peers")).unwrap();
    let peer = store.accept_certificate(client.certificate_der()).unwrap();
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let (server_path, store_path) = (fixture.dir.join("server.pem"), fixture.dir.join("peers"));
    start_store_bridge(&mut fixture, &server_path, &store_path, port, &log);
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/agent")).unwrap();
    let wait = Duration::from_secs(10);
    // One configuration throughout: it keeps the session and tickets a resuming client would use.
    let tls = native::client_of(&client, &server);
    let channel = coqui_web::websocket::Dialer::new()
        .with_tls(std::sync::Arc::clone(&tls))
        .dial(coqui_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
    coqui_web::Channel::close(&channel);
    store.remove(&peer).unwrap();
    std::fs::remove_file(&log).unwrap();
    let dialed = coqui_web::websocket::Dialer::new().with_tls(tls).dial(
        coqui_web::Kind::WebSocket,
        &url,
        wait,
    );
    if let Ok(channel) = dialed {
        let _ = coqui_web::Channel::send(
            &channel,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        );
        assert!(!matches!(
            coqui_web::Channel::receive(&channel, Some(wait)),
            Ok(Some(_))
        ));
    }
    assert!(!log.exists(), "a removed peer must not reach an agent");
}

#[test]
fn silent_connections_hold_up_nobody_and_two_peers_each_get_an_agent() {
    use coqui_web::Dialer;
    let mut fixture = Fixture::new("slow");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let server_path = fixture.dir.join("server.pem");
    start_mtls(
        &mut fixture,
        &server_path,
        &client.fingerprint().to_string(),
        port,
        &[],
        &log,
    );
    let silent: Vec<_> = (0..8)
        .map(|_| std::net::TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/agent")).unwrap();
    let wait = Duration::from_secs(5);
    let started = Instant::now();
    let first = coqui_web::websocket::Dialer::new()
        .with_tls(native::client_of(&client, &server))
        .dial(coqui_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&first);
    let second = coqui_web::websocket::Dialer::new()
        .with_tls(native::client_of(&client, &server))
        .dial(coqui_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&second);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    drop(silent);
}

#[test]
fn flags_that_cannot_apply_are_refused_rather_than_dropped() {
    let fixture = Fixture::new("flags");
    let server = fixture.dir.join("server.pem");
    native::identity(&server);
    for extra in [["--webtransport-port", "7000"], ["--proof-seconds", "5"]] {
        let output = Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0", "--tls-identity"])
            .arg(&server)
            .args([
                "--allow-cert",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            ])
            .args(extra)
            .args(["--", "x"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{extra:?}");
    }
}

/// A channel with an endless supply of messages for the agent.
struct Flood;

impl coqui_web::Channel for Flood {
    fn send(&self, _: &[u8]) -> Result<(), coqui_web::Failure> {
        Ok(())
    }

    fn receive(&self, _: Option<Duration>) -> Result<Option<Vec<u8>>, coqui_web::Failure> {
        Ok(Some(vec![b'x'; 1024 * 1024]))
    }

    fn close(&self) {}
}

#[test]
fn an_agent_that_stops_reading_ends_the_link_rather_than_wedging_it() {
    let agent = coqui_acp_bridge::agent::Agent::spawn(
        &echo_agent().into_os_string(),
        &["--deaf".into()],
        1024 * 1024,
    )
    .unwrap()
    .with_write_limit(Duration::from_millis(300));
    let started = Instant::now();
    let end = coqui_acp_bridge::relay::relay(&Flood, &agent);
    assert_eq!(end, coqui_acp_bridge::relay::LinkEnd::AgentStuck);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    agent.stop().unwrap();
}
