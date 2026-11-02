//! Several protocols on one proven link, through the real binary, with real agent processes.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use coqui_agentic_peer::auth::{Sealed, prove_out};
use coqui_agentic_peer::link::capabilities;
use coqui_agentic_peer::mux::{Limits, Mux, Side};
use coqui_peer_webcrypto::{Identity, Trust, WebCrypto};
use coqui_web::Channel as _;

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");
const WAIT: Duration = Duration::from_secs(10);
const INITIALIZE: &[u8] =
    br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#;

struct Fixture {
    dir: PathBuf,
    children: Vec<Child>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-services-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            children: Vec::new(),
        }
    }

    fn identity(&self, name: &str) -> (PathBuf, Identity) {
        let path = self.dir.join(name);
        let identity = Identity::open_or_create(&path).unwrap();
        (path, identity)
    }

    /// `serve`, returning the peer id it prints, once it is listening.
    fn serve(&mut self, home: Option<&Path>, flags: &[&str], port: u16) -> String {
        let mut command = Command::new(BRIDGE);
        command
            .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
            .args(flags)
            .arg("--")
            .arg(echo_agent());
        if let Some(home) = home {
            command.env(coqui_agentic_peer::home::HOME_ENV, home);
        }
        let log = std::fs::File::create(self.dir.join(format!("serve-{port}.log"))).unwrap();
        let mut child = command.stdout(Stdio::piped()).stderr(log).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        self.children.push(child);
        wait_for_port(port);
        line.trim().to_owned()
    }
}

impl Fixture {
    /// What every process here wrote to its log, for a failure to report.
    fn said(&self) -> String {
        let mut all = String::new();
        for entry in std::fs::read_dir(&self.dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "log") {
                all.push_str(&format!("--- {}\n", path.display()));
                all.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
        all
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn echo_agent() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "coqui-acp-bridge", "--example", "echo_agent"])
        .status()
        .unwrap();
    assert!(status.success());
    let mut path = PathBuf::from(BRIDGE);
    path.pop();
    path.push("examples");
    path.push(format!("echo_agent{}", std::env::consts::EXE_SUFFIX));
    path
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_port(port: u16) {
    let until = Instant::now() + WAIT;
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < until, "nothing listened");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn dial(port: u16) -> coqui_web::websocket::Channel {
    use coqui_web::Dialer;
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/link")).unwrap();
    coqui_web::websocket::Dialer::new()
        .with_frames(coqui_web::websocket::Frames::Binary)
        .dial(coqui_web::Kind::WebSocket, &url, WAIT)
        .unwrap()
}

/// Proves itself, then runs `body` over a mux on that link.
fn over_link(
    port: u16,
    client: Identity,
    server: &str,
    body: impl FnOnce(&Mux<'_, Sealed<'_, coqui_web::websocket::Channel>>),
) {
    let method = WebCrypto::new(client).with_responders(Trust::pinned([server.parse().unwrap()]));
    let channel = dial(port);
    let established = prove_out(&method, &channel, WAIT, 1 << 25).unwrap();
    let sealed = Sealed::new(&channel, established.session);
    Mux::run(&sealed, Side::Dialed, Limits::default(), None, body);
}

fn initialized<C: coqui_web::Channel>(stream: &C) {
    stream.send(INITIALIZE).unwrap();
    let answer: serde_json::Value =
        serde_json::from_slice(&stream.receive(Some(WAIT)).unwrap().unwrap()).unwrap();
    assert_eq!(answer["result"]["protocolVersion"], 1, "{answer}");
}

#[test]
fn two_protocols_share_one_proof_and_a_plain_client_still_works() {
    let mut fixture = Fixture::new("two");
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let peer = client.peer_id().to_string();
    let agent = echo_agent();
    let port = free_port();
    let service = format!("mcp={}", agent.display());
    let server = fixture.serve(
        None,
        &[
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--service",
            &service,
        ],
        port,
    );

    over_link(port, client, &server, |mux| {
        assert_eq!(capabilities(mux, WAIT).unwrap().offers, ["acp/1", "mcp"]);
        let acp = mux.open("acp/1").unwrap();
        let mcp = mux.open("mcp").unwrap();
        initialized(&acp);
        initialized(&mcp);
    });

    // A client that knows nothing of streams gets the one protocol it speaks.
    let (_, again) = fixture.identity("client");
    let method = WebCrypto::new(again).with_responders(Trust::pinned([server.parse().unwrap()]));
    let channel = dial(port);
    let established = prove_out(&method, &channel, WAIT, 1 << 25).unwrap();
    initialized(&Sealed::new(&channel, established.session));
}

#[test]
fn a_grant_names_what_a_peer_may_open_and_keeps_the_rest_refused() {
    let mut fixture = Fixture::new("grant");
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let peer = client.peer_id().to_string();
    let agent = echo_agent();
    let port = free_port();
    let service = format!("mcp={}", agent.display());
    let grant = format!("{peer}=mcp");
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &peer,
        "--service",
        &service,
        "--grant",
        &grant,
    ];
    let server = fixture.serve(None, &flags, port);
    over_link(port, client, &server, |mux| {
        assert_eq!(capabilities(mux, WAIT).unwrap().offers, ["mcp"]);
        assert_eq!(
            mux.open("acp/1").unwrap().receive(Some(WAIT)),
            Err(coqui_web::Failure::Permission)
        );
        initialized(&mux.open("mcp").unwrap());
    });
}

/// Pairs two homes once, as a person would.
fn pair(server_home: &Path, client_home: &Path) {
    let port = free_port();
    let home = coqui_agentic_peer::home::HOME_ENV;
    let mut accept = Command::new(BRIDGE)
        .args(["pair-accept", "--listen", &format!("127.0.0.1:{port}")])
        .env(home, server_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Held open: `pair-accept` prints its peer id next, and a closed pipe would end it.
    let mut printed = BufReader::new(accept.stdout.take().unwrap());
    let mut pin = String::new();
    printed.read_line(&mut pin).unwrap();
    let mut joining = Command::new(BRIDGE)
        .args(["pair-dial", &format!("ws://127.0.0.1:{port}/pair")])
        .env(home, client_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    joining
        .stdin
        .as_mut()
        .unwrap()
        .write_all(pin.as_bytes())
        .unwrap();
    drop(joining.stdin.take());
    assert!(joining.wait().unwrap().success());
    assert!(accept.wait().unwrap().success());
}

#[test]
fn one_pairing_and_a_new_capability_is_a_grant_counted_at_once() {
    let mut fixture = Fixture::new("paired");
    let (server_home, client_home) = (fixture.dir.join("server"), fixture.dir.join("client"));
    pair(&server_home, &client_home);
    let client = Identity::open_or_create(&client_home.join("identity")).unwrap();
    let peer = client.peer_id().to_string();
    let agent = echo_agent();
    let port = free_port();
    let service = format!("mcp={}", agent.display());
    let server = fixture.serve(
        Some(&server_home),
        &["--paired", "--service", &service],
        port,
    );

    let granting = |action: &str| {
        let status = Command::new(BRIDGE)
            .args(["grant", &peer, "mcp"])
            .args(if action == "revoke" {
                &["--revoke"][..]
            } else {
                &[][..]
            })
            .env(coqui_agentic_peer::home::HOME_ENV, &server_home)
            .status()
            .unwrap();
        assert!(status.success());
    };
    over_link(port, client, &server, |mux| {
        // Being paired gives the agent it was paired for, and nothing else.
        assert_eq!(capabilities(mux, WAIT).unwrap().offers, ["acp/1"]);
        assert_eq!(
            mux.open("mcp").unwrap().receive(Some(WAIT)),
            Err(coqui_web::Failure::Permission)
        );
        granting("grant");
        assert_eq!(capabilities(mux, WAIT).unwrap().offers, ["acp/1", "mcp"]);
        let mcp = mux.open("mcp").unwrap();
        initialized(&mcp);
        // Taken away, the running stream ends within the recheck, saying why.
        granting("revoke");
        assert_eq!(mcp.receive(Some(WAIT)), Err(coqui_web::Failure::Permission));
        assert!(matches!(
            mcp.end(),
            Some(coqui_agentic_peer::mux::StreamEnd::Revoked(_))
        ));
        initialized(&mux.open("acp/1").unwrap());
    });
}

#[test]
fn a_service_needs_the_in_band_proof_and_a_grant_needs_a_service() {
    let fixture = Fixture::new("flags");
    let (key, identity) = fixture.identity("server");
    let peer = identity.peer_id().to_string();
    let agent = echo_agent();
    let service = format!("mcp={}", agent.display());
    let bad_token = format!("a b={}", agent.display());
    let grant = format!("{peer}=mcp");
    for flags in [
        vec!["--no-peer-proof", "--service", service.as_str()],
        vec![
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--service",
            "no-program",
        ],
        vec![
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--service",
            bad_token.as_str(),
        ],
        vec![
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--grant",
            grant.as_str(),
        ],
    ] {
        let output = Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(&flags)
            .arg("--")
            .arg(&agent)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{flags:?}");
    }
}

#[test]
fn dial_tries_its_fallback_when_the_first_endpoint_is_not_there() {
    let mut fixture = Fixture::new("fallback");
    let (key, _) = fixture.identity("server");
    let (client_key, client) = fixture.identity("client");
    let peer = client.peer_id().to_string();
    let port = free_port();
    let dead = free_port();
    let server = fixture.serve(
        None,
        &["--identity", key.to_str().unwrap(), "--allow", &peer],
        port,
    );
    let mut dialing = Command::new(BRIDGE)
        .args([
            "dial",
            &format!("ws://127.0.0.1:{dead}/acp"),
            "--fallback",
            &format!("ws://127.0.0.1:{port}/acp"),
        ])
        .args([
            "--identity",
            client_key.to_str().unwrap(),
            "--peer",
            &server,
            "--",
        ])
        .arg(echo_agent())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let said = dialing.stderr.take().unwrap();
    let (lines, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(said).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut connected = false;
    while let Ok(line) = heard.recv_timeout(Duration::from_secs(20)) {
        if line.contains("connected") {
            connected = true;
            break;
        }
    }
    let _ = dialing.kill();
    let _ = dialing.wait();
    assert!(connected);
}

/// A web server that names itself, which only a real HTTP exchange gets an answer from.
fn web_server(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let serving = std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut connection) = connection else {
                return;
            };
            std::thread::spawn(move || {
                let mut asked = BufReader::new(connection.try_clone().unwrap());
                let mut line = String::new();
                while asked.read_line(&mut line).is_ok_and(|read| read > 0) && line != "\r\n" {
                    line.clear();
                }
                let _ = write!(
                    connection,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (address, serving)
}

/// An unmodified HTTP client, so nothing here knows it went over a link.
fn curl(url: &str) -> Option<String> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--max-time", "5", url])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn curl_until(url: &str, fixture: &Fixture) -> String {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(answer) = curl(url) {
            return answer;
        }
        assert!(
            Instant::now() < until,
            "nothing answered at {url}
{}",
            fixture.said()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn curl_reaches_a_server_through_a_link_in_both_directions_and_only_as_granted() {
    let mut fixture = Fixture::new("forward");
    let (server_key, _) = fixture.identity("server");
    let (client_key, client) = fixture.identity("client");
    let client_id = client.peer_id().to_string();
    let (behind_server, _server_web) = web_server("behind the server");
    let (not_granted, _other_web) = web_server("not granted");
    let (behind_client, _client_web) = web_server("behind the client");
    let port = free_port();
    let (forward_port, refused_port, reverse_port) = (free_port(), free_port(), free_port());
    let grant = format!("{client_id}=tcp/1:{behind_server}");
    let reverse = format!("127.0.0.1:{reverse_port}={client_id}={behind_client}");
    let flags = [
        "--identity",
        server_key.to_str().unwrap(),
        "--allow",
        &client_id,
        "--forward",
        "--grant",
        &grant,
        "--reverse",
        &reverse,
    ];
    let server = fixture.serve(None, &flags, port);
    let forward_log = std::fs::File::create(fixture.dir.join("forward.log")).unwrap();
    let forwarding = Command::new(BRIDGE)
        .args([
            "forward",
            &format!("ws://127.0.0.1:{port}/link"),
            "--identity",
            client_key.to_str().unwrap(),
            "--peer",
            &server,
        ])
        .args([
            "--local",
            &format!("127.0.0.1:{forward_port}={behind_server}"),
        ])
        .args([
            "--local",
            &format!("127.0.0.1:{refused_port}={not_granted}"),
        ])
        .args(["--allow-target", &behind_client])
        .stdout(Stdio::null())
        .stderr(forward_log)
        .spawn()
        .unwrap();
    fixture.children.push(forwarding);

    assert_eq!(
        curl_until(&format!("http://127.0.0.1:{forward_port}/"), &fixture),
        "behind the server"
    );
    assert_eq!(
        curl_until(&format!("http://127.0.0.1:{reverse_port}/"), &fixture),
        "behind the client"
    );
    wait_for_port(refused_port);
    assert_eq!(
        curl(&format!("http://127.0.0.1:{refused_port}/")),
        None,
        "a target with no grant reaches nothing"
    );
}

#[test]
fn forwarding_is_refused_by_default_and_a_target_takes_a_grant_of_its_own() {
    let mut fixture = Fixture::new("noforward");
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let peer = client.peer_id().to_string();
    let (web, _serving) = web_server("reached");
    let port = free_port();
    let server = fixture.serve(
        None,
        &[
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--forward",
            "--service",
            "mcp=nothing",
        ],
        port,
    );
    over_link(port, client, &server, |mux| {
        // Allowed in with no --grant gets every protocol that runs a program, and never this.
        assert_eq!(capabilities(mux, WAIT).unwrap().offers, ["acp/1", "mcp"]);
        let target: coqui_agentic_peer::forward::Target = web.parse().unwrap();
        let stream = coqui_agentic_peer::forward::connect(mux, &target).unwrap();
        assert_eq!(
            stream.receive(Some(WAIT)),
            Err(coqui_web::Failure::Permission)
        );
    });
}

#[test]
fn a_listener_beyond_loopback_is_refused() {
    let fixture = Fixture::new("beyond");
    let (key, identity) = fixture.identity("server");
    let peer = identity.peer_id().to_string();
    let output = Command::new(BRIDGE)
        .args([
            "forward",
            "ws://127.0.0.1:1/link",
            "--identity",
            key.to_str().unwrap(),
            "--peer",
            &peer,
            "--local",
            "0.0.0.0:9000=db:5432",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
    let reverse = format!("0.0.0.0:9000={peer}=db:5432");
    let output = Command::new(BRIDGE)
        .args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--reverse",
            &reverse,
            "--",
            "x",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
