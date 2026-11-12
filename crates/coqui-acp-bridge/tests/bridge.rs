//! The real binary, a real agent process, a real socket, and for the browser method a real JavaScript initiator.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

mod method;

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
            .args(["--method", method::name()])
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

/// The JavaScript module of this run's method, named by `COQUI_ACP_TEST_WEBCRYPTO` or `COQUI_ACP_TEST_NOISE`.
fn module_variable() -> &'static str {
    match method::method() {
        coqui_agentic_peer::auth::Method::Noise => "COQUI_ACP_TEST_NOISE",
        _ => "COQUI_ACP_TEST_WEBCRYPTO",
    }
}

fn method_module() -> Option<PathBuf> {
    let path = std::env::var_os(module_variable()).map(PathBuf::from)?;
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
            .arg(method::name())
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
        .args(["--method", method::name()])
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
    let (Some(node), Some(module)) = (node(), method_module()) else {
        assert!(
            std::env::var_os(module_variable()).is_none(),
            "a module was named and could not be used"
        );
        eprintln!(
            "SKIPPED: needs Node 22 or later and {} naming that method's bindings/js/coqui-peer-<method>/src/index.mjs",
            module_variable()
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

    use coqui_peer_tls::{Identity, PeerClientVerifier, PeerServerVerifier, Pins, Policy, Trust};

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    pub fn identity(path: &Path) -> Identity {
        Identity::open_or_create(path).unwrap()
    }

    pub fn server_requiring(server: &Identity, client: &Identity) -> Arc<rustls::ServerConfig> {
        let verifier =
            PeerClientVerifier::builder(&Trust::Pinned(Pins::fixed([client.fingerprint()])))
                .build()
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
        let verifier = PeerServerVerifier::new(
            &Trust::Pinned(Pins::fixed([server.fingerprint()])),
            &Policy::default(),
        )
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

use coqui_transport::Channel as _;

fn initialize<C: coqui_transport::Channel>(channel: &C) {
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
        coqui_transport_web::websocket::Listener::builder("127.0.0.1:0".parse().unwrap(), tls)
            .build()
            .unwrap();
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
    coqui_transport::Channel::close(&first);
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
        coqui_transport_web::websocket::Listener::builder("127.0.0.1:0".parse().unwrap(), tls)
            .build()
            .unwrap();
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
    use coqui_transport_web::Dialer;
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
    let refused = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&stranger, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait);
    if let Ok(channel) = refused {
        // TLS 1.3 reports a refused client certificate only on the first read.
        assert!(!matches!(
            coqui_transport::Channel::receive(&channel, Some(wait)),
            Ok(Some(_))
        ));
    }
    assert!(!log.exists(), "no byte from a stranger may reach an agent");
    let channel = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
    assert!(log.exists());
}

#[test]
fn an_acl_drops_a_denied_source_before_its_handshake() {
    use coqui_transport_web::Dialer;
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
    let dialed = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(
            coqui_transport_web::Kind::WebSocket,
            &url,
            Duration::from_secs(5),
        );
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
    let server = coqui_transport_web::websocket::Listener::builder(
        "127.0.0.1:0".parse().unwrap(),
        ca.server,
    )
    .build()
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

/// Serves with a store and returns the bridge's peer id.
fn start_store_bridge(fixture: &mut Fixture, store: &Path, port: u16, log: &Path) -> String {
    let (identity, _) = fixture.keygen("bridge.pem");
    let mut child = Command::new(BRIDGE)
        .arg("serve")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--identity")
        .arg(identity)
        .arg("--allow-store")
        .arg(store)
        .arg("--")
        .arg(echo_agent())
        .env("ECHO_AGENT_LOG", log)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    fixture.bridge = Some(child);
    wait_for_port(port);
    line.trim().to_owned()
}

fn binary_dial(url: &url::Url) -> coqui_transport_web::websocket::Channel {
    use coqui_transport_web::Dialer;
    coqui_transport_web::websocket::Dialer::builder()
        .frames(coqui_transport_web::websocket::Frames::Binary)
        .build()
        .unwrap()
        .dial(
            coqui_transport_web::Kind::WebSocket,
            url,
            Duration::from_secs(10),
        )
        .unwrap()
}

#[test]
fn a_peer_removed_from_the_store_loses_its_live_link_and_cannot_return() {
    use coqui_agentic_peer::auth::{Proof, Trust};
    use coqui_agentic_peer::auth::{Sealed, prove_out};
    use coqui_peer_webcrypto::TrustStore;
    let mut fixture = Fixture::new("removed");
    let store = TrustStore::open(&fixture.dir.join("peers")).unwrap();
    let client = method::identity(&fixture.dir.join("client.pem"));
    let peer = store.accept_key(&client.key_material()).unwrap();
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let store_path = fixture.dir.join("peers");
    let server = start_store_bridge(&mut fixture, &store_path, port, &log);
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/agent")).unwrap();
    let wait = Duration::from_secs(10);
    let method = Proof::builder(client)
        .responders(Trust::pinned([server.parse().unwrap()]))
        .build()
        .unwrap();

    let channel = binary_dial(&url);
    let established = prove_out(&method, &channel, wait, 1 << 20).unwrap();
    let sealed = Sealed::new(&channel, established.session);
    initialize(&sealed);

    store.remove(&peer).unwrap();
    let until = Instant::now() + wait;
    while coqui_transport::Channel::receive(&sealed, Some(Duration::from_millis(100)))
        == Err(coqui_transport::Failure::Timeout)
    {
        assert!(Instant::now() < until, "the live link was not closed");
    }

    std::fs::remove_file(&log).unwrap();
    let again = binary_dial(&url);
    // WebCrypto's initiator hears the refusal in the proof. Noise's finishes on its last message and hears it as the
    // connection ending before anything is answered.
    let proved = prove_out(&method, &again, wait, 1 << 20);
    if method::method() == coqui_agentic_peer::auth::Method::WebCrypto {
        assert!(proved.is_err());
    }
    if let Ok(established) = proved {
        let sealed = Sealed::new(&again, established.session);
        let _ = coqui_transport::Channel::send(
            &sealed,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#,
        );
        assert!(!matches!(
            coqui_transport::Channel::receive(&sealed, Some(wait)),
            Ok(Some(_))
        ));
    }
    assert!(!log.exists(), "a removed peer must not reach an agent");
}

#[test]
fn a_stranger_with_a_good_proof_and_a_plain_message_after_it_reach_nothing() {
    use coqui_agentic_peer::auth::{Proof, Trust};
    use coqui_agentic_peer::auth::{Sealed, prove_out};
    use coqui_peer_webcrypto::TrustStore;
    let mut fixture = Fixture::new("plainafter");
    let store = TrustStore::open(&fixture.dir.join("peers")).unwrap();
    let client = method::identity(&fixture.dir.join("client.pem"));
    store.accept_key(&client.key_material()).unwrap();
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let store_path = fixture.dir.join("peers");
    let server = start_store_bridge(&mut fixture, &store_path, port, &log);
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/agent")).unwrap();
    let wait = Duration::from_secs(10);
    let method = Proof::builder(client)
        .responders(Trust::pinned([server.parse().unwrap()]))
        .build()
        .unwrap();

    let channel = binary_dial(&url);
    prove_out(&method, &channel, wait, 1 << 20).unwrap();
    coqui_transport::Channel::send(
        &channel,
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )
    .unwrap();
    assert!(!matches!(
        coqui_transport::Channel::receive(&channel, Some(wait)),
        Ok(Some(_))
    ));
    assert!(!log.exists(), "what is not sealed must not reach an agent");

    // And a real peer on the same bridge is unaffected.
    let good = binary_dial(&url);
    let established = prove_out(&method, &good, wait, 1 << 20).unwrap();
    initialize(&Sealed::new(&good, established.session));
}

#[test]
fn a_link_cut_before_the_close_record_is_a_failure_and_a_close_record_is_an_orderly_end() {
    use coqui_agentic_peer::auth::{Proof, Trust};
    use coqui_agentic_peer::auth::{Sealed, prove, prove_out};
    let fixture = Fixture::new("closerecord");
    let near = method::identity(&fixture.dir.join("near.pem"));
    let far = method::identity(&fixture.dir.join("far.pem"));
    let near_method = Proof::builder(near.clone())
        .responders(Trust::pinned([far.peer_id()]))
        .build()
        .unwrap();
    let far_method = Proof::builder(far)
        .initiators(Trust::pinned([near.peer_id()]))
        .build()
        .unwrap();
    let wait = Duration::from_secs(10);
    for cut in [false, true] {
        let listener = coqui_transport_web::websocket::Listener::builder(
            "127.0.0.1:0".parse().unwrap(),
            coqui_transport_web::websocket::ServerTls::Plain,
        )
        .frames(coqui_transport_web::websocket::Frames::Binary)
        .build()
        .unwrap();
        let url = url::Url::parse(&format!("ws://{}/", listener.local_addr().unwrap())).unwrap();
        let method = far_method.clone();
        let serving = std::thread::spawn(move || {
            let channel = listener.accept(Some(wait)).unwrap();
            let established = prove(&method, &channel, wait, 1 << 20).unwrap();
            let sealed = Sealed::new(&channel, established.session);
            coqui_transport::Channel::send(&sealed, b"held back or not").unwrap();
            if cut {
                coqui_transport::Channel::close(&channel);
            } else {
                coqui_transport::Channel::close(&sealed);
            }
        });
        let channel = binary_dial(&url);
        let established = prove_out(&near_method, &channel, wait, 1 << 20).unwrap();
        let sealed = Sealed::new(&channel, established.session);
        assert_eq!(
            coqui_transport::Channel::receive(&sealed, Some(wait))
                .unwrap()
                .unwrap(),
            b"held back or not"
        );
        let end = coqui_transport::Channel::receive(&sealed, Some(wait));
        if cut {
            assert_eq!(end, Err(coqui_transport::Failure::PathLost));
        } else {
            assert_eq!(end, Ok(None));
        }
        serving.join().unwrap();
    }
}

/// A proxy that ends TLS, forwards to `back`, and optionally sends the dialer a plain message once the proof is over.
fn dial_proxy(tls: std::sync::Arc<rustls::ServerConfig>, back: u16, inject: bool) -> u16 {
    use coqui_transport_web::Dialer;
    use coqui_transport_web::websocket::Frames;
    let front =
        coqui_transport_web::websocket::Listener::builder("127.0.0.1:0".parse().unwrap(), tls)
            .frames(Frames::Binary)
            .build()
            .unwrap();
    let port = front.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok(outer) = front.accept(Some(Duration::from_secs(30))) {
            let url = url::Url::parse(&format!("ws://127.0.0.1:{back}/")).unwrap();
            let inner = coqui_transport_web::websocket::Dialer::builder()
                .frames(Frames::Binary)
                .build()
                .unwrap()
                .dial(
                    coqui_transport_web::Kind::WebSocket,
                    &url,
                    Duration::from_secs(10),
                )
                .unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    while let Ok(Some(message)) =
                        coqui_transport::Channel::receive(&outer, Some(Duration::from_secs(30)))
                    {
                        if coqui_transport::Channel::send(&inner, &message).is_err() {
                            break;
                        }
                    }
                    coqui_transport::Channel::close(&inner);
                });
                let mut from_back = 0;
                while let Ok(Some(message)) =
                    coqui_transport::Channel::receive(&inner, Some(Duration::from_secs(30)))
                {
                    from_back += 1;
                    if coqui_transport::Channel::send(&outer, &message).is_err() {
                        break;
                    }
                    // The responder's challenge and its acceptance are the proof; this comes right after.
                    if inject && from_back == 2 {
                        let _ = coqui_transport::Channel::send(
                            &outer,
                            br#"{"jsonrpc":"2.0","id":1,"method":"exit"}"#,
                        );
                    }
                }
                coqui_transport::Channel::close(&outer);
            });
        }
    });
    port
}

/// A bridge that ends with its test, however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_in_band_dial(
    fixture: &Fixture,
    port: u16,
    peer: &str,
    ca: &Path,
    log: &Path,
    agent: &Path,
) -> Running {
    let child = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("wss://localhost:{port}/v1/agent"))
        .arg("--identity")
        .arg(fixture.dir.join("dialer.pem"))
        .args(["--method", method::name()])
        .arg("--peer")
        .arg(peer)
        .arg("--ca-file")
        .arg(ca)
        .args(["--max-backoff-seconds", "2", "--"])
        .arg(agent)
        .env("ECHO_AGENT_LOG", log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Running(child)
}

#[test]
fn dial_proves_itself_through_a_proxy_that_ends_tls_and_the_proxy_cannot_inject() {
    use coqui_agentic_peer::auth::{Proof, Trust};
    use coqui_agentic_peer::auth::{Sealed, prove};
    let fixture = Fixture::new("dialproxy");
    // Built first: another test's build can hold the lock for longer than the listeners would wait.
    let agent = echo_agent();
    let responder = method::identity(&fixture.dir.join("responder.pem"));
    let dialer = method::identity(&fixture.dir.join("dialer.pem"));
    let peer = responder.peer_id().to_string();
    let method = Proof::builder(responder)
        .initiators(Trust::pinned([dialer.peer_id()]))
        .build()
        .unwrap();
    let wait = Duration::from_secs(30);
    let backend = coqui_transport_web::websocket::Listener::builder(
        "127.0.0.1:0".parse().unwrap(),
        coqui_transport_web::websocket::ServerTls::Plain,
    )
    .frames(coqui_transport_web::websocket::Frames::Binary)
    .build()
    .unwrap();
    let back = backend.local_addr().unwrap().port();
    let ca = rcgen_issue(&fixture.dir);

    let log = fixture.dir.join("agent.log");
    let honest = dial_proxy(std::sync::Arc::clone(&ca.server), back, false);
    let bridge = start_in_band_dial(&fixture, honest, &peer, &ca.bundle, &log, &agent);
    let channel = backend.accept(Some(wait)).unwrap();
    let established = prove(&method, &channel, wait, 1 << 20).unwrap();
    initialize(&Sealed::new(&channel, established.session));
    assert!(log.exists());
    drop(bridge);
    drop(channel);

    std::fs::remove_file(&log).unwrap();
    let hostile = dial_proxy(ca.server, back, true);
    let mut bridge = start_in_band_dial(&fixture, hostile, &peer, &ca.bundle, &log, &agent);
    let channel = backend.accept(Some(wait)).unwrap();
    prove(&method, &channel, wait, 1 << 20).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        !log.exists(),
        "the proxy's message must not reach the agent"
    );
    assert!(
        bridge.0.try_wait().unwrap().is_none(),
        "a link lost is not the agent ending"
    );
}

#[test]
fn dial_stops_when_the_remote_proves_the_wrong_key() {
    let fixture = Fixture::new("dialwrong");
    let stranger = method::identity(&fixture.dir.join("stranger.pem"));
    let mut serving = {
        let (identity, _) = fixture.keygen("bridge.pem");
        Command::new(BRIDGE)
            .args(["serve", "--listen"])
            .arg("127.0.0.1:0")
            .arg("--identity")
            .arg(identity)
            .arg("--allow")
            .arg(stranger.peer_id().to_string())
            .args(["--", "x"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut banner = String::new();
    BufReader::new(serving.stderr.take().unwrap())
        .read_line(&mut banner)
        .unwrap();
    let address = banner.trim().trim_start_matches("listening on ").to_owned();
    let mut child = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("ws://{address}/"))
        .arg("--identity")
        .arg(fixture.dir.join("dialer.pem"))
        .args(["--method", method::name()])
        .arg("--peer")
        .arg(stranger.peer_id().to_string())
        .arg("--")
        .arg(echo_agent())
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
            let _ = serving.kill();
            panic!("the bridge kept knocking");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = serving.kill();
    let _ = serving.wait();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn silent_connections_hold_up_nobody_and_two_peers_each_get_an_agent() {
    use coqui_transport_web::Dialer;
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
    let first = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&first);
    let second = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
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
    for extra in [
        ["--webtransport-port", "7000"],
        ["--proof-seconds", "5"],
        ["--identity", "unused.pem"],
    ] {
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

impl coqui_transport::Channel for Flood {
    fn send(&self, _: &[u8]) -> Result<(), coqui_transport::Failure> {
        Ok(())
    }

    fn receive(&self, _: Option<Duration>) -> Result<Option<Vec<u8>>, coqui_transport::Failure> {
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

fn spawn_serve(fixture: &mut Fixture, port: u16, flags: &[&str], extra: &[&Path], log: &Path) {
    let mut command = Command::new(BRIDGE);
    command
        .arg("serve")
        .args(["--listen", &format!("127.0.0.1:{port}")])
        .args(flags);
    for path in extra {
        command.arg(path);
    }
    let child = command
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
fn no_peer_proof_serves_a_program_on_loopback_and_refuses_a_browser_unless_listed() {
    use coqui_transport_web::Dialer;
    let mut fixture = Fixture::new("noproof");
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    spawn_serve(&mut fixture, port, &["--no-peer-proof"], &[], &log);
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/")).unwrap();
    let wait = Duration::from_secs(10);
    let page = coqui_transport_web::websocket::Dialer::builder()
        .headers(vec![(
            "origin".to_owned(),
            "https://any-site.example".to_owned(),
        )])
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait);
    assert!(page.is_err(), "a page must not reach an unproven agent");
    assert!(!log.exists());
    let channel = coqui_transport_web::websocket::Dialer::builder()
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
}

#[test]
fn no_peer_proof_is_refused_where_anyone_could_reach_it() {
    let fixture = Fixture::new("noproofwide");
    native::identity(&fixture.dir.join("server.pem"));
    let output = Command::new(BRIDGE)
        .args([
            "serve",
            "--listen",
            "0.0.0.0:0",
            "--no-peer-proof",
            "--tls-identity",
        ])
        .arg(fixture.dir.join("server.pem"))
        .args(["--", "x"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--permit"));
    let proofs = Command::new(BRIDGE)
        .args(["serve", "--no-peer-proof", "--allow"])
        .arg(format!("sha256:{}", "0".repeat(64)))
        .args(["--", "x"])
        .output()
        .unwrap();
    assert!(!proofs.status.success(), "one proof, not two");
}

#[test]
fn a_client_can_pin_a_server_that_asks_nothing_of_it() {
    use coqui_transport_web::Dialer;
    let mut fixture = Fixture::new("serverpin");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let other = native::identity(&fixture.dir.join("other.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let path = fixture.dir.join("server.pem");
    spawn_serve(
        &mut fixture,
        port,
        &["--no-peer-proof", "--tls-identity"],
        &[&path],
        &log,
    );
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/")).unwrap();
    let wait = Duration::from_secs(10);
    let wrong = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &other))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait);
    assert!(wrong.is_err(), "the pin decides");
    let channel = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
}

#[test]
fn dial_can_pin_only_the_server_or_with_a_flag_nothing_on_loopback() {
    let fixture = Fixture::new("dialmodes");
    let agent = echo_agent();
    let server = native::identity(&fixture.dir.join("server.pem"));
    let tls = coqui_peer_tls::ServerBuilder::new()
        .identity(&server)
        .client_auth(coqui_peer_tls::ClientAuth::None)
        .build()
        .unwrap()
        .config;
    let wait = Some(Duration::from_secs(30));
    let pinned = coqui_transport_web::websocket::Listener::builder(
        "127.0.0.1:0".parse().unwrap(),
        std::sync::Arc::new(tls),
    )
    .build()
    .unwrap();
    let port = pinned.local_addr().unwrap().port();
    let bridge = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("wss://127.0.0.1:{port}/"))
        .arg("--expect")
        .arg(server.fingerprint().to_string())
        .arg("--")
        .arg(&agent)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _running = Running(bridge);
    initialize(&pinned.accept(wait).unwrap());

    let plain = coqui_transport_web::websocket::Listener::builder(
        "127.0.0.1:0".parse().unwrap(),
        coqui_transport_web::websocket::ServerTls::Plain,
    )
    .build()
    .unwrap();
    let port = plain.local_addr().unwrap().port();
    let refused = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("ws://127.0.0.1:{port}/"))
        .args(["--", "x"])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "nothing checked needs asking for"
    );
    let bridge = Command::new(BRIDGE)
        .arg("dial")
        .arg(format!("ws://127.0.0.1:{port}/"))
        .arg("--no-peer-proof")
        .arg("--")
        .arg(&agent)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _also = Running(bridge);
    initialize(&plain.accept(wait).unwrap());
}

#[test]
fn a_client_certificate_can_come_from_a_store_or_a_ca() {
    use coqui_peer_tls::TrustStoreExt as _;
    use coqui_transport_web::Dialer;
    let mut fixture = Fixture::new("certsources");
    let server = native::identity(&fixture.dir.join("server.pem"));
    let client = native::identity(&fixture.dir.join("client.pem"));
    let stranger = native::identity(&fixture.dir.join("stranger.pem"));
    let store_dir = fixture.dir.join("certs");
    let store = coqui_peer::TrustStore::open(&store_dir).unwrap();
    let peer = store.accept_certificate(client.certificate_der()).unwrap();
    let log = fixture.dir.join("agent.log");
    let port = free_port();
    let path = fixture.dir.join("server.pem");
    spawn_serve(
        &mut fixture,
        port,
        &["--tls-identity"],
        &[&path, Path::new("--allow-cert-store"), &store_dir],
        &log,
    );
    let url = url::Url::parse(&format!("wss://127.0.0.1:{port}/")).unwrap();
    let wait = Duration::from_secs(10);
    let channel = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
        .unwrap();
    initialize(&channel);
    if let Ok(channel) = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&stranger, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
    {
        assert!(!matches!(
            coqui_transport::Channel::receive(&channel, Some(wait)),
            Ok(Some(_))
        ));
    }
    store.remove(&peer).unwrap();
    if let Ok(channel) = coqui_transport_web::websocket::Dialer::builder()
        .tls(native::client_of(&client, &server))
        .build()
        .unwrap()
        .dial(coqui_transport_web::Kind::WebSocket, &url, wait)
    {
        assert!(!matches!(
            coqui_transport::Channel::receive(&channel, Some(wait)),
            Ok(Some(_))
        ));
    }
}

/// `serve` with `flags`, its first line of standard output, and what it says on standard error as lines arrive.
fn serve_saying(
    flags: &[&str],
    home: Option<&Path>,
) -> (Child, String, std::sync::mpsc::Receiver<String>) {
    let mut command = Command::new(BRIDGE);
    command
        .args(["serve", "--listen", "127.0.0.1:0"])
        .args(flags)
        .arg("--")
        .arg(echo_agent())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = home {
        command.env(coqui_agentic_peer::home::HOME_ENV, home);
    }
    let mut child = command.spawn().unwrap();
    let said = child.stderr.take().unwrap();
    let (lines, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(said).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut peer = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut peer)
        .unwrap();
    (child, peer.trim().to_owned(), heard)
}

/// The next line said that starts with `start`, within 20 seconds.
fn heard_line(heard: &std::sync::mpsc::Receiver<String>, start: &str) -> String {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match heard.recv_timeout(left) {
            Ok(line) if line.starts_with(start) => return line,
            Ok(_) => {}
            Err(_) => panic!("nothing said starting {start:?}"),
        }
    }
}

#[test]
fn a_method_is_the_identitys_and_naming_another_is_refused() {
    use coqui_agentic_peer::auth::{Identity, Method};
    const ANYONE: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let fixture = Fixture::new("methods");
    for (method, other) in [
        (Method::WebCrypto, Method::Noise),
        (Method::Noise, Method::WebCrypto),
    ] {
        let path = fixture.dir.join(format!("{method}.pem"));
        let made = Command::new(BRIDGE)
            .arg("keygen")
            .arg(&path)
            .args(["--method", method.name()])
            .output()
            .unwrap();
        assert!(made.status.success());
        assert_eq!(Identity::read(&path).unwrap().unwrap().method(), method);
        let refused = Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0", "--identity"])
            .arg(&path)
            .args(["--allow", ANYONE, "--method", other.name(), "--", "x"])
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2));
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(
            said.contains(&format!("proves by {method}, not {other}")),
            "{said}"
        );
    }
    let path = fixture.dir.join("default.pem");
    assert!(
        Command::new(BRIDGE)
            .arg("keygen")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        Identity::read(&path).unwrap().unwrap().method(),
        Method::DEFAULT
    );
    let unknown = Command::new(BRIDGE)
        .arg("keygen")
        .arg(fixture.dir.join("unknown.pem"))
        .args(["--method", "p256-spki"])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    assert!(!fixture.dir.join("unknown.pem").exists());

    // A setting looser than the default is said.
    let noise = fixture.dir.join("noise.pem");
    let (mut serving, _, heard) = serve_saying(
        &[
            "--identity",
            noise.to_str().unwrap(),
            "--allow",
            ANYONE,
            "--key-limit",
            "18446744073709551615",
            "--no-rekey",
        ],
        None,
    );
    assert_eq!(heard_line(&heard, "proving by"), "proving by noise");
    let non_defaults = [
        heard_line(&heard, "non-default"),
        heard_line(&heard, "non-default"),
    ];
    assert!(
        non_defaults.iter().any(|line| line.contains("rekey")),
        "{non_defaults:?}"
    );
    let _ = serving.kill();
    let _ = serving.wait();
}

#[test]
fn a_fresh_home_proves_by_the_default_and_a_webcrypto_home_keeps_serving_its_peers() {
    use coqui_agentic_peer::auth::{Identity, Method, Proof, Sealed, Trust, prove_out};
    use coqui_agentic_peer::home;
    use coqui_peer_webcrypto::TrustStore;
    let fixture = Fixture::new("homes");

    let fresh = fixture.dir.join("fresh");
    let (mut serving, peer, heard) = serve_saying(&["--paired"], Some(&fresh));
    assert_eq!(
        heard_line(&heard, "proving by"),
        format!("proving by {}", Method::DEFAULT)
    );
    let made = Identity::read(&fresh.join(home::identity_file(Method::DEFAULT)))
        .unwrap()
        .unwrap();
    assert_eq!(made.peer_id().to_string(), peer);
    let _ = serving.kill();
    let _ = serving.wait();

    // A home paired before Noise holds a WebCrypto identity, which keeps its method and its peers.
    let old = fixture.dir.join("old");
    let server = Identity::open_or_create(
        &old.join(home::identity_file(Method::WebCrypto)),
        Method::WebCrypto,
    )
    .unwrap();
    let client = Identity::generate(Method::WebCrypto).unwrap();
    TrustStore::open(&old.join("peers"))
        .unwrap()
        .accept_key(&client.key_material())
        .unwrap();
    let (mut serving, peer, heard) = serve_saying(&["--paired"], Some(&old));
    assert_eq!(peer, server.peer_id().to_string());
    let address = heard_line(&heard, "listening on")
        .trim_start_matches("listening on ")
        .to_owned();
    assert_eq!(heard_line(&heard, "proving by"), "proving by webcrypto");
    let url = url::Url::parse(&format!("ws://{address}/agent")).unwrap();
    let channel = binary_dial(&url);
    let method = Proof::builder(client)
        .responders(Trust::pinned([server.peer_id()]))
        .build()
        .unwrap();
    let established = prove_out(&method, &channel, Duration::from_secs(10), 1 << 20).unwrap();
    initialize(&Sealed::new(&channel, established.session));

    // A Noise client of the same home is refused, and the bridge says why.
    let stranger = Identity::generate(Method::Noise).unwrap();
    let method = Proof::builder(stranger)
        .responders(Trust::pinned([server.peer_id()]))
        .build()
        .unwrap();
    assert!(
        prove_out(
            &method,
            &binary_dial(&url),
            Duration::from_secs(10),
            1 << 20
        )
        .is_err()
    );
    let refused = heard_line(&heard, "a connection was refused");
    assert!(
        refused.contains("the peer proves by noise, and this endpoint only by webcrypto"),
        "{refused}"
    );
    let _ = serving.kill();
    let _ = serving.wait();
}

#[test]
fn after_moving_to_noise_a_paired_serve_naming_no_method_keeps_working() {
    use coqui_agentic_peer::auth::{Identity, Method};
    use coqui_agentic_peer::home;
    let fixture = Fixture::new("moved");
    let moved = fixture.dir.join("home");
    // A home paired before Noise.
    Identity::open_or_create(
        &moved.join(home::identity_file(Method::WebCrypto)),
        Method::WebCrypto,
    )
    .unwrap();
    // The move: Noise named once, as the migration says.
    let (mut serving, noise_peer, heard) =
        serve_saying(&["--paired", "--method", "noise"], Some(&moved));
    assert_eq!(heard_line(&heard, "proving by"), "proving by noise");
    let _ = serving.kill();
    let _ = serving.wait();
    // Then a program naming no method, as Canoa's app runs its host, gets the method named last.
    let (mut serving, peer, heard) = serve_saying(&["--paired"], Some(&moved));
    assert_eq!(heard_line(&heard, "proving by"), "proving by noise");
    assert_eq!(peer, noise_peer);
    let _ = serving.kill();
    let _ = serving.wait();
}
