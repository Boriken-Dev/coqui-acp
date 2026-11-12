//! PIN pairing through a proxy that ends TLS, as nginx would, and against one that attacks it: the binary end to end, and the
//! library for what a dishonest relay does.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::needless_pass_by_value
)]

mod method;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use coqui_agentic_peer::auth::Identity;
use coqui_agentic_peer::pairing;
use coqui_peer_webcrypto::pair::{CodeAlphabet, Pairing, Pin, PinRole};
use coqui_peer_webcrypto::{TrustStore, random};
use coqui_transport::Channel;
use coqui_transport_web::websocket::{self, Frames};
use coqui_transport_web::{Dialer, Kind};
use rustls::pki_types::PrivatePkcs8KeyDer;
use url::Url;

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");
const WAIT: Duration = Duration::from_secs(20);
const SHORT: Duration = Duration::from_secs(10);

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-pairing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn accepted(store: &Path) -> usize {
    std::fs::read_dir(store.join("peers").join("accepted")).map_or(0, Iterator::count)
}

/// Forwards every message both ways, as a WebSocket reverse proxy does, changing the joiner's message number `tamper`.
fn forward<A: Channel, B: Channel>(front: &A, back: &B, tamper: Option<usize>) {
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut count = 0;
            while let Ok(Some(mut message)) = front.receive(Some(WAIT)) {
                if tamper == Some(count)
                    && let Some(last) = message.last_mut()
                {
                    *last ^= 0x01;
                }
                count += 1;
                if back.send(&message).is_err() {
                    break;
                }
            }
            back.close();
        });
        while let Ok(Some(message)) = back.receive(Some(WAIT)) {
            if front.send(&message).is_err() {
                break;
            }
        }
        front.close();
    });
}

fn pair_accept(dir: &Dir, port: u16) -> (Child, String) {
    let mut child = Command::new(BRIDGE)
        .arg("pair-accept")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--identity")
        .arg(dir.path("server.pem"))
        .args(["--method", method::name()])
        .arg("--store")
        .arg(dir.path("server-store"))
        .args(["--timeout-seconds", "20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut pin = String::new();
    reader.read_line(&mut pin).unwrap();
    let mut peer = String::new();
    reader.read_line(&mut peer).unwrap();
    assert!(peer.trim().starts_with("sha256:"), "{peer}");
    (child, pin.trim().to_owned())
}

fn pair_dial(dir: &Dir, url: &str, pin: &str, ca: Option<&Path>) -> std::process::Output {
    let file = dir.path("pin.txt");
    std::fs::write(&file, format!("{pin}\n")).unwrap();
    let mut command = Command::new(BRIDGE);
    command
        .arg("pair-dial")
        .arg(url)
        .arg("--identity")
        .arg(dir.path("client.pem"))
        .args(["--method", method::name()])
        .arg("--store")
        .arg(dir.path("client-store"))
        .arg("--pin-file")
        .arg(&file)
        .args(["--timeout-seconds", "20"]);
    if let Some(ca) = ca {
        command.arg("--ca-file").arg(ca);
    }
    command.output().unwrap()
}

struct Tls {
    bundle: PathBuf,
    server: Arc<rustls::ServerConfig>,
}

/// A front door's certificate for `localhost`, from a private CA.
fn front_door(dir: &Dir) -> Tls {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.clone().self_signed(&ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf = params.signed_by(&key, &issuer).unwrap();
    let bundle = dir.path("ca.pem");
    std::fs::write(&bundle, ca.pem()).unwrap();
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
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
    Tls {
        bundle,
        server: Arc::new(server),
    }
}

/// A proxy that ends TLS and passes messages to a plain backend, on a path under `/v1`.
fn proxy(tls: Arc<rustls::ServerConfig>, backend: u16) -> (u16, thread::JoinHandle<()>) {
    let front = websocket::Listener::builder(loopback(), tls)
        .frames(Frames::Binary)
        .build()
        .unwrap();
    let port = front.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let Ok(outer) = front.accept(Some(WAIT)) else {
            return;
        };
        let url = Url::parse(&format!("ws://127.0.0.1:{backend}/backend")).unwrap();
        let inner = websocket::Dialer::builder()
            .frames(Frames::Binary)
            .build()
            .unwrap()
            .dial(Kind::WebSocket, &url, WAIT)
            .unwrap();
        forward(&outer, &inner, None);
    });
    (port, handle)
}

fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let until = Instant::now() + Duration::from_secs(40);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < until, "the bridge did not end");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_pin_pairs_through_a_proxy_that_ends_tls_and_the_real_keys_are_stored() {
    let dir = Dir::new("proxy");
    let tls = front_door(&dir);
    let backend = free_port();
    let (mut accepting, pin) = pair_accept(&dir, backend);
    let (port, proxy) = proxy(tls.server, backend);
    let url = format!("wss://localhost:{port}/v1/agent/pair");
    let joined = pair_dial(&dir, &url, &pin, Some(&tls.bundle));
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert!(wait_exit(&mut accepting).success());
    proxy.join().unwrap();

    let server = Identity::read(&dir.path("server.pem")).unwrap().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&joined.stdout).trim(),
        server.peer_id().to_string(),
        "the backend's key, not the proxy's"
    );
    assert_eq!(accepted(&dir.path("server-store")), 1);
    assert_eq!(accepted(&dir.path("client-store")), 1);
}

#[test]
fn a_wrong_pin_through_the_proxy_pairs_nobody() {
    let dir = Dir::new("wrong");
    let tls = front_door(&dir);
    let backend = free_port();
    let (mut accepting, pin) = pair_accept(&dir, backend);
    let (port, proxy) = proxy(tls.server, backend);
    let wrong = if pin == "000000" { "000001" } else { "000000" };
    let joined = pair_dial(
        &dir,
        &format!("wss://localhost:{port}/v1/agent/pair"),
        wrong,
        Some(&tls.bundle),
    );
    assert!(!joined.status.success());
    assert!(!wait_exit(&mut accepting).success());
    proxy.join().unwrap();
    assert_eq!(accepted(&dir.path("server-store")), 0);
    assert_eq!(accepted(&dir.path("client-store")), 0);
}

#[test]
fn a_certificate_outside_the_given_ca_is_refused_before_any_pin_is_used() {
    let dir = Dir::new("ca");
    let tls = front_door(&dir);
    let other = Dir::new("ca-other");
    let elsewhere = front_door(&other);
    let backend = free_port();
    let (mut accepting, pin) = pair_accept(&dir, backend);
    let (port, proxy) = proxy(tls.server, backend);
    let joined = pair_dial(
        &dir,
        &format!("wss://localhost:{port}/v1/agent/pair"),
        &pin,
        Some(&elsewhere.bundle),
    );
    assert!(!joined.status.success());
    let _ = accepting.kill();
    let _ = accepting.wait();
    let _ = proxy.join();
    assert_eq!(accepted(&dir.path("client-store")), 0);
}

#[test]
fn plain_ws_is_refused_beyond_loopback_and_a_pin_is_not_an_argument() {
    let dir = Dir::new("plain");
    let joined = pair_dial(&dir, "ws://example.org/pair", "000000", None);
    assert!(!joined.status.success());
    let help = Command::new(BRIDGE)
        .args(["pair-dial", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(
        text.contains("--pin-file") && !text.contains("--pin "),
        "{text}"
    );
}

#[test]
fn a_silent_scanner_costs_neither_the_pin_nor_the_wait() {
    let dir = Dir::new("scanner");
    let backend = free_port();
    let (mut accepting, pin) = pair_accept(&dir, backend);
    let silent: Vec<_> = (0..4)
        .map(|_| std::net::TcpStream::connect(("127.0.0.1", backend)).unwrap())
        .collect();
    let joined = pair_dial(&dir, &format!("ws://127.0.0.1:{backend}/pair"), &pin, None);
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert!(wait_exit(&mut accepting).success());
    drop(silent);
    assert_eq!(accepted(&dir.path("server-store")), 1);
}

/// The library's two sides on a real socket, with something between them.
struct Sides {
    server: Identity,
    client: Identity,
    server_store: TrustStore,
    client_store: TrustStore,
    dir: Dir,
}

fn sides(name: &str) -> Sides {
    let dir = Dir::new(name);
    Sides {
        server: method::identity(&dir.path("s.pem")),
        client: method::identity(&dir.path("c.pem")),
        server_store: TrustStore::open(&dir.path("server-store")).unwrap(),
        client_store: TrustStore::open(&dir.path("client-store")).unwrap(),
        dir,
    }
}

fn binary_pair() -> (websocket::Channel, websocket::Channel) {
    let listener = websocket::Listener::builder(loopback(), websocket::ServerTls::Plain)
        .frames(Frames::Binary)
        .build()
        .unwrap();
    let url = Url::parse(&format!("ws://{}/", listener.local_addr().unwrap())).unwrap();
    let accepting = thread::spawn(move || listener.accept(Some(WAIT)).unwrap());
    let near = websocket::Dialer::builder()
        .frames(Frames::Binary)
        .build()
        .unwrap()
        .dial(Kind::WebSocket, &url, WAIT)
        .unwrap();
    (near, accepting.join().unwrap())
}

/// A relay changing one of the joiner's messages: 0 and 1 are the handshake, 2 is the first pairing record.
fn tampered_run(name: &str, tamper: usize) {
    let sides = sides(name);
    let (joiner_end, relay_near) = binary_pair();
    let (relay_far, responder_end) = binary_pair();
    let desk = pairing::desk().unwrap();
    let pin = desk.show_pin().unwrap().pin;
    let relay = thread::spawn(move || forward(&relay_near, &relay_far, Some(tamper)));
    thread::scope(|scope| {
        let responding = scope.spawn(|| {
            pairing::respond(
                &responder_end,
                &sides.server,
                &sides.server_store,
                &desk,
                None,
                SHORT,
            )
        });
        let joined = pairing::join(&joiner_end, &sides.client, pin, &sides.client_store, SHORT);
        assert!(joined.is_err(), "the joiner must refuse a changed run");
        assert!(
            responding.join().unwrap().is_err(),
            "the responder must refuse a changed run"
        );
    });
    drop(joiner_end);
    drop(responder_end);
    relay.join().unwrap();
    assert_eq!(accepted(&sides.dir.path("server-store")), 0);
    assert_eq!(accepted(&sides.dir.path("client-store")), 0);
}

#[test]
fn a_relay_that_changes_a_handshake_message_fails_both_sides() {
    tampered_run("tamper-handshake", 0);
}

#[test]
fn a_relay_that_changes_a_pairing_record_fails_both_sides() {
    tampered_run("tamper-record", 2);
}

#[test]
fn a_relay_that_swaps_in_its_own_key_without_the_pin_fails_both_sides() {
    let sides = sides("swap");
    let attacker = method::identity(&sides.dir.path("a.pem"));
    let attacker_store = TrustStore::open(&sides.dir.path("attacker-store")).unwrap();
    let attacker_store_two = TrustStore::open(&sides.dir.path("attacker-store-two")).unwrap();
    let (joiner_end, attacker_near) = binary_pair();
    let (attacker_far, responder_end) = binary_pair();
    let real_desk = pairing::desk().unwrap();
    let pin = real_desk.show_pin().unwrap().pin;
    // The attacker cannot read the PIN, so it shows its own to the joiner and guesses at the responder.
    let attacker_desk = pairing::desk().unwrap();
    let _ = attacker_desk.show_pin().unwrap();
    let guess = Pin::parse(
        &CodeAlphabet::pin(),
        if pin.as_str() == "000000" {
            "000001"
        } else {
            "000000"
        },
    )
    .unwrap();
    thread::scope(|scope| {
        let to_joiner = scope.spawn(|| {
            pairing::respond(
                &attacker_near,
                &attacker,
                &attacker_store,
                &attacker_desk,
                None,
                SHORT,
            )
        });
        let to_responder = scope
            .spawn(|| pairing::join(&attacker_far, &attacker, guess, &attacker_store_two, SHORT));
        let responding = scope.spawn(|| {
            pairing::respond(
                &responder_end,
                &sides.server,
                &sides.server_store,
                &real_desk,
                None,
                SHORT,
            )
        });
        let joined = pairing::join(&joiner_end, &sides.client, pin, &sides.client_store, SHORT);
        assert!(
            joined.is_err(),
            "the joiner must not pair with the attacker"
        );
        assert!(
            responding.join().unwrap().is_err(),
            "the responder must not pair with the attacker"
        );
        assert!(to_joiner.join().unwrap().is_err());
        assert!(to_responder.join().unwrap().is_err());
    });
    assert_eq!(accepted(&sides.dir.path("server-store")), 0);
    assert_eq!(accepted(&sides.dir.path("client-store")), 0);
}

fn run_joiner(
    sides: &Sides,
    asked: Pairing,
    limit: Duration,
) -> Result<(), coqui_agentic_peer::Error> {
    let (near, far) = binary_pair();
    let desk = pairing::desk().unwrap();
    let _ = desk.show_pin().unwrap();
    thread::scope(|scope| {
        let responding = scope.spawn(|| {
            pairing::respond(&far, &sides.server, &sides.server_store, &desk, None, limit)
        });
        let joined = pairing::join_as(&near, &sides.client, asked, &sides.client_store, limit);
        assert!(responding.join().unwrap().is_err());
        joined.map(|_| ())
    })
}

#[test]
fn a_joiner_asking_for_the_device_flow_is_refused() {
    let sides = sides("device");
    let asked = Pairing::device("a laptop").allow_unverified(true);
    assert!(
        run_joiner(&sides, asked, SHORT).is_err(),
        "only PIN runs are offered"
    );
    assert_eq!(accepted(&sides.dir.path("server-store")), 0);
}

#[test]
fn a_joiner_showing_its_own_pin_is_refused_at_once() {
    let sides = sides("shown");
    let pin = Pin::generate(&CodeAlphabet::pin(), &random()).unwrap();
    let started = Instant::now();
    assert!(run_joiner(&sides, Pairing::pin(pin, PinRole::Shown), SHORT).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(accepted(&sides.dir.path("server-store")), 0);
}

/// The status line `pair-accept` answers a WebSocket upgrade with, sent with `origin` if any.
fn upgrade_status(port: u16, origin: Option<&str>) -> String {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let origin = origin.map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
    write!(
        stream,
        "GET /pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{origin}\r\n"
    )
    .unwrap();
    let mut answer = [0; 64];
    let read = stream.read(&mut answer).unwrap_or(0);
    String::from_utf8_lossy(&answer[..read])
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `pair-accept` with `origins` allowed, and its PIN.
fn pair_accept_allowing(dir: &Dir, port: u16, origins: &[&str]) -> (Child, String) {
    let mut command = Command::new(BRIDGE);
    command
        .arg("pair-accept")
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--identity")
        .arg(dir.path("server.pem"))
        .args(["--method", method::name()])
        .arg("--store")
        .arg(dir.path("server-store"))
        .args(["--timeout-seconds", "20"]);
    for origin in origins {
        command.args(["--origin", origin]);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut pin = String::new();
    reader.read_line(&mut pin).unwrap();
    let mut peer = String::new();
    reader.read_line(&mut peer).unwrap();
    (child, pin.trim().to_owned())
}

#[test]
fn a_page_cannot_spend_the_pin_and_only_a_listed_origin_reaches_the_exchange() {
    let dir = Dir::new("origins");
    let port = free_port();
    let (mut accepting, pin) = pair_accept(&dir, port);
    // Any browser sends an Origin; with none listed, every one is refused before the handshake.
    assert!(upgrade_status(port, Some("https://evil.example")).contains(" 403"));
    assert!(
        upgrade_status(
            port,
            Some("chrome-extension://abcdefghijklmnopabcdefghijklmnop")
        )
        .contains(" 403")
    );
    // A native client sends none, and the PIN still pairs it.
    assert!(upgrade_status(port, None).contains(" 101"));
    let joined = pair_dial(&dir, &format!("ws://127.0.0.1:{port}/pair"), &pin, None);
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert!(wait_exit(&mut accepting).success());

    let dir = Dir::new("origins-listed");
    let port = free_port();
    let extension = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";
    let (mut accepting, _) = pair_accept_allowing(&dir, port, &[extension]);
    assert!(upgrade_status(port, Some(extension)).contains(" 101"));
    assert!(upgrade_status(port, Some("https://evil.example")).contains(" 403"));
    let _ = accepting.kill();
    let _ = accepting.wait();
}

#[test]
fn a_pin_waits_as_long_as_the_desk_keeps_it_and_a_run_is_a_setting() {
    let help = Command::new(BRIDGE)
        .args(["pair-accept", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    // The desk's PIN lives ten minutes: waiting longer would only wait for a dead PIN.
    assert!(text.contains("[default: 600]"), "{text}");
    assert!(
        text.contains("--run-seconds") && text.contains("[default: 120]"),
        "{text}"
    );
    assert!(text.contains("--origin"), "{text}");
}
