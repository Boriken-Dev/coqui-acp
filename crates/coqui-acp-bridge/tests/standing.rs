//! A program nobody declared anything to, reaching a peer's tools from what `mcp-config` prints, through the real binary.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

use coqui_agentic_peer::auth::{Sealed, prove_out};
use coqui_agentic_peer::mux::{Limits, Mux, Side};
use coqui_peer_webcrypto::{Identity, Trust, WebCrypto};
use coqui_web::Channel as _;
use serde_json::{Value, json};

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");
const WAIT: Duration = Duration::from_secs(10);
const STANDING: &[u8] = b"standing/1";
const READY: &[u8] = br#"{"standing":1}"#;
const CLAIMED: &[u8] = br#"{"claimed":true}"#;
const INITIALIZE: &[u8] = br#"{"jsonrpc":"2.0","id":7,"method":"initialize"}"#;
const ANSWERED: &[u8] = br#"{"jsonrpc":"2.0","id":7,"result":{}}"#;

struct Fixture {
    dir: PathBuf,
    home: PathBuf,
    children: Vec<Child>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-standing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            dir,
            home,
            children: Vec::new(),
        }
    }

    fn identity(&self, name: &str) -> (PathBuf, Identity) {
        let path = self.dir.join(name);
        let identity = Identity::open_or_create(&path).unwrap();
        (path, identity)
    }

    /// `serve --mcp-offers`, returning the peer id it prints, once it is listening.
    fn serve(&mut self, flags: &[&str], port: u16) -> String {
        let log = std::fs::File::create(self.dir.join(format!("serve-{port}.log"))).unwrap();
        let mut child = Command::new(BRIDGE)
            .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
            .args(flags)
            .args(["--", "no-agent-is-ever-started"])
            .env(coqui_agentic_peer::home::HOME_ENV, &self.home)
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        self.children.push(child);
        let until = Instant::now() + WAIT;
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < until, "nothing listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        line.trim().to_owned()
    }

    /// The door's file for a peer, as the bridge wrote it.
    fn door(&self, peer: &str) -> PathBuf {
        let hex = peer.strip_prefix("sha256:").unwrap();
        self.home
            .join("offers")
            .join(format!("standing-{hex}.json"))
    }

    fn config(&self, peer: &str) -> Value {
        let output = Command::new(BRIDGE)
            .args(["mcp-config", "--peer", peer])
            .env(coqui_agentic_peer::home::HOME_ENV, &self.home)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Starts what `mcp-config` printed, with nothing else in its environment to go on.
    fn start_client(&self, peer: &str) -> Client {
        let config = self.config(peer);
        let entry = &config["mcpServers"]["canoa-browser"];
        let mut command = Command::new(entry["command"].as_str().unwrap());
        for argument in entry["args"].as_array().unwrap() {
            command.arg(argument.as_str().unwrap());
        }
        for (name, value) in entry["env"].as_object().unwrap() {
            command.env(name, value.as_str().unwrap());
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Client {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
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

struct Client {
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    child: Child,
}

impl Client {
    fn say(&mut self, line: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input.write_all(line).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        line
    }

    /// Its status and what it said, once it has ended, however long that takes up to a limit.
    fn ended(mut self) -> (Output, String) {
        drop(self.input.take());
        let until = Instant::now() + WAIT * 2;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "the shim went on");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut stdout = String::new();
        let _ = self.output.read_to_string(&mut stdout);
        (self.child.wait_with_output().unwrap(), stdout)
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn dial(port: u16) -> coqui_web::websocket::Channel {
    use coqui_web::Dialer as _;
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

/// A bridge, with a peer of one identity on a link to it, handed to `body`.
fn with_peer(
    name: &str,
    body: impl FnOnce(&Fixture, &str, &Mux<'_, Sealed<'_, coqui_web::websocket::Channel>>),
) {
    let mut fixture = Fixture::new(name);
    let (key, _) = fixture.identity("server");
    let (_, browser) = fixture.identity("browser");
    let peer = browser.peer_id().to_string();
    let port = free_port();
    let server = fixture.serve(
        &[
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--mcp-offers",
        ],
        port,
    );
    over_link(port, browser, &server, |mux| body(&fixture, &peer, mux));
}

fn wait_until(what: &str, test: impl Fn() -> bool) {
    let until = Instant::now() + WAIT;
    while !test() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn message<C: coqui_web::Channel>(stream: &C) -> Vec<u8> {
    stream.receive(Some(WAIT)).unwrap().unwrap()
}

#[test]
fn the_entry_names_the_peer_and_holds_nothing_secret() {
    let fixture = Fixture::new("config");
    let peer = format!("sha256:{}", "a".repeat(64));
    let config = fixture.config(&peer);
    let entry = &config["mcpServers"]["canoa-browser"];
    assert_eq!(entry["type"], "stdio");
    assert!(Path::new(entry["command"].as_str().unwrap()).is_absolute());
    assert_eq!(entry["args"], json!(["mcp-shim", "--peer", peer]));
    // A client may start it with little of its own environment, so the entry says where to look.
    let home = entry["env"][coqui_agentic_peer::home::HOME_ENV]
        .as_str()
        .unwrap();
    assert_eq!(Path::new(home), fixture.home);
    let bare = Command::new(BRIDGE)
        .args(["mcp-config", "--peer", &peer])
        .env_remove(coqui_agentic_peer::home::HOME_ENV)
        .env("LOCALAPPDATA", &fixture.dir)
        .env("XDG_CONFIG_HOME", &fixture.dir)
        .output()
        .unwrap();
    let bare: Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(
        Path::new(
            bare["mcpServers"]["canoa-browser"]["env"][coqui_agentic_peer::home::HOME_ENV]
                .as_str()
                .unwrap()
        ),
        fixture.dir.join("coqui-agentic-peer")
    );
    for wrong in [
        "sha256:peer".to_owned(),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:../../{}", "a".repeat(58)),
        "a".repeat(71),
    ] {
        let output = Command::new(BRIDGE)
            .args(["mcp-config", "--peer", &wrong])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{wrong}");
    }
}

#[test]
fn with_nothing_connected_the_client_is_answered_and_the_shim_exits() {
    let fixture = Fixture::new("nobody");
    let peer = format!("sha256:{}", "b".repeat(64));
    let mut client = fixture.start_client(&peer);
    client.say(INITIALIZE);
    let answer: Value = serde_json::from_str(&client.line()).unwrap();
    assert_eq!(answer["id"], 7);
    assert_eq!(answer["error"]["code"], -32000);
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no browser is connected"),
        "{answer}"
    );
    let (output, _) = client.ended();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no browser is connected"),
        "{output:?}"
    );
}

#[test]
fn a_client_gets_a_stream_of_its_own_and_the_peer_hears_it_was_taken() {
    with_peer("one", |fixture, peer, mux| {
        let first = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&first), READY);
        let second = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&second), READY);

        let mut one = fixture.start_client(peer);
        one.say(INITIALIZE);
        assert_eq!(message(&first), CLAIMED);
        assert_eq!(message(&first), INITIALIZE);
        first.send(ANSWERED).unwrap();
        assert_eq!(one.line().trim().as_bytes(), ANSWERED);

        // Two at once, each on its own stream, under the one peer.
        let mut two = fixture.start_client(peer);
        two.say(INITIALIZE);
        assert_eq!(message(&second), CLAIMED);
        assert_eq!(message(&second), INITIALIZE);
        second.send(ANSWERED).unwrap();
        assert_eq!(two.line().trim().as_bytes(), ANSWERED);
        first.send(b"{\"id\":1}").unwrap();
        assert_eq!(one.line(), "{\"id\":1}\n");

        // Every stream taken: the third hears so, and nothing of it reaches anyone.
        let mut three = fixture.start_client(peer);
        three.say(INITIALIZE);
        let answer: Value = serde_json::from_str(&three.line()).unwrap();
        assert!(
            answer["error"]["message"]
                .as_str()
                .unwrap()
                .contains("in use"),
            "{answer}"
        );
        assert_eq!(three.ended().0.status.code(), Some(1));
    });
}

#[test]
fn a_client_leaving_ends_its_stream_and_the_door_goes_with_the_last_stream() {
    with_peer("leaves", |fixture, peer, mux| {
        let door = fixture.door(peer);
        assert!(!door.exists());
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        let spare = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&spare), READY);
        assert!(door.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&door).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
            let parent = std::fs::metadata(door.parent().unwrap()).unwrap();
            assert_eq!(parent.permissions().mode() & 0o077, 0);
        }

        let mut client = fixture.start_client(peer);
        client.say(INITIALIZE);
        assert_eq!(message(&stream), CLAIMED);
        assert_eq!(message(&stream), INITIALIZE);
        let (output, _) = client.ended();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(stream.receive(Some(WAIT)), Ok(None));

        // The spare is still there for the next, and is the only thing keeping the door.
        assert!(door.is_file());
        let mut next = fixture.start_client(peer);
        next.say(INITIALIZE);
        assert_eq!(message(&spare), CLAIMED);
        assert_eq!(message(&spare), INITIALIZE);
        spare.close();
        assert_eq!(next.line(), "");
        let (output, _) = next.ended();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("the browser ended"),
            "{output:?}"
        );
        wait_until("the door's file stayed", || !door.exists());
    });
}

#[test]
fn a_shim_reaches_only_the_peer_it_names() {
    let mut fixture = Fixture::new("peers");
    let (key, _) = fixture.identity("server");
    let (_, browser) = fixture.identity("browser");
    let (_, other) = fixture.identity("other");
    let browser_peer = browser.peer_id().to_string();
    let other_peer = other.peer_id().to_string();
    let port = free_port();
    let server = fixture.serve(
        &[
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &browser_peer,
            "--allow",
            &other_peer,
            "--mcp-offers",
        ],
        port,
    );
    over_link(port, other, &server, |mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        // Another allowed peer stands by, and a program asking for the browser does not get it.
        let mut client = fixture.start_client(&browser_peer);
        client.say(INITIALIZE);
        let answer: Value = serde_json::from_str(&client.line()).unwrap();
        assert!(
            answer["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no browser is connected"),
            "{answer}"
        );
        assert!(matches!(
            stream.receive(Some(Duration::from_millis(300))),
            Err(coqui_web::Failure::Timeout)
        ));
        assert!(!fixture.door(&browser_peer).exists());
        // The one that names it gets it.
        let mut own = fixture.start_client(&other_peer);
        own.say(INITIALIZE);
        assert_eq!(message(&stream), CLAIMED);
        assert_eq!(message(&stream), INITIALIZE);
        drop(own);
    });
}

#[test]
fn a_peer_allowed_something_else_cannot_stand_by() {
    let mut fixture = Fixture::new("not-granted");
    let (key, _) = fixture.identity("server");
    let (_, browser) = fixture.identity("browser");
    let peer = browser.peer_id().to_string();
    let port = free_port();
    let grant = format!("{peer}=acp/1");
    let server = fixture.serve(
        &[
            "--identity",
            key.to_str().unwrap(),
            "--allow",
            &peer,
            "--mcp-offers",
            "--grant",
            &grant,
        ],
        port,
    );
    over_link(port, browser, &server, |mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert!(stream.receive(Some(WAIT)).is_err());
    });
    assert!(!fixture.door(&peer).exists());
}

#[test]
fn a_wrong_token_or_a_web_request_reaches_nothing() {
    with_peer("wrong", |fixture, peer, mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        let door: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.door(peer)).unwrap()).unwrap();
        let address = door["address"].as_str().unwrap();
        assert!(address.starts_with("127.0.0.1:"), "{address}");
        let gets_nothing = |mut connection: std::net::TcpStream| {
            connection.set_read_timeout(Some(WAIT)).unwrap();
            let mut heard = Vec::new();
            let _ = connection.read_to_end(&mut heard);
            assert!(heard.is_empty(), "{heard:?}");
        };
        let mut wrong = std::net::TcpStream::connect(address).unwrap();
        writeln!(wrong, "{}", "0".repeat(64)).unwrap();
        wrong.write_all(b"{\"id\":\"wrong\"}\n").unwrap();
        gets_nothing(wrong);
        let mut web = std::net::TcpStream::connect(address).unwrap();
        write!(
            web,
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://a.example\r\nContent-Length: 2\r\n\r\n{{}}{}",
            " ".repeat(64)
        )
        .unwrap();
        gets_nothing(web);
        assert!(matches!(
            stream.receive(Some(Duration::from_millis(300))),
            Err(coqui_web::Failure::Timeout)
        ));
        // The stream is still free for the real thing.
        let mut client = fixture.start_client(peer);
        client.say(INITIALIZE);
        assert_eq!(message(&stream), CLAIMED);
        assert_eq!(message(&stream), INITIALIZE);
    });
}

#[test]
fn something_else_on_a_dead_bridges_port_hears_only_an_old_token_and_gets_no_message() {
    let fixture = Fixture::new("impostor");
    let peer = format!("sha256:{}", "c".repeat(64));
    let impostor = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = impostor.local_addr().unwrap().to_string();
    std::fs::create_dir_all(fixture.door(&peer).parent().unwrap()).unwrap();
    std::fs::write(
        fixture.door(&peer),
        json!({ "address": address, "token": "t".repeat(64), "answer": "a".repeat(64) })
            .to_string(),
    )
    .unwrap();
    let there = std::thread::spawn(move || {
        let (mut connection, _) = impostor.accept().unwrap();
        // Whatever it answers it cannot know the answer, and it is told nothing more.
        connection
            .write_all(format!("{} ok", "0".repeat(64)).as_bytes())
            .unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut heard = Vec::new();
        let _ = connection.read_to_end(&mut heard);
        heard
    });
    let mut client = fixture.start_client(&peer);
    client.say(INITIALIZE);
    let answer: Value = serde_json::from_str(&client.line()).unwrap();
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not that bridge"),
        "{answer}"
    );
    assert_eq!(client.ended().0.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(there.join().unwrap()).unwrap(),
        format!("{}\n", "t".repeat(64))
    );
}

#[test]
fn an_answer_that_is_not_text_is_no_answer_and_no_panic() {
    let fixture = Fixture::new("not-text");
    let peer = format!("sha256:{}", "e".repeat(64));
    let impostor = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = impostor.local_addr().unwrap().to_string();
    std::fs::create_dir_all(fixture.door(&peer).parent().unwrap()).unwrap();
    std::fs::write(
        fixture.door(&peer),
        json!({ "address": address, "token": "t".repeat(64), "answer": "a".repeat(64) })
            .to_string(),
    )
    .unwrap();
    let there = std::thread::spawn(move || {
        let (mut connection, _) = impostor.accept().unwrap();
        // A character of several bytes across where the answer ends.
        let mut answer = vec![b'a'; 63];
        answer.extend_from_slice("\u{20ac}\u{20ac}".as_bytes());
        answer.truncate(67);
        connection.write_all(&answer).unwrap();
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut client = fixture.start_client(&peer);
    client.say(INITIALIZE);
    let answer: Value = serde_json::from_str(&client.line()).unwrap();
    assert_eq!(answer["error"]["code"], -32000, "{answer}");
    assert_eq!(client.ended().0.status.code(), Some(1));
    there.join().unwrap();
}

#[test]
fn a_flood_of_connections_keeps_no_shim_out() {
    with_peer("flood", |fixture, peer, mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        let door: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture.door(peer)).unwrap()).unwrap();
        let address = door["address"].as_str().unwrap().to_owned();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let mut held = std::collections::VecDeque::new();
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        if let Ok(connection) = std::net::TcpStream::connect(&address) {
                            held.push_back(connection);
                        }
                        if held.len() > 64 {
                            held.pop_front();
                        }
                    }
                });
            }
            std::thread::sleep(Duration::from_millis(200));
            let mut client = fixture.start_client(peer);
            client.say(INITIALIZE);
            let claimed = stream.receive(Some(WAIT));
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(claimed.unwrap().unwrap(), CLAIMED);
            assert_eq!(message(&stream), INITIALIZE);
            drop(client);
        });
    });
}

#[cfg(windows)]
#[test]
fn on_windows_the_door_answers_only_its_owner() {
    with_peer("acl", |fixture, peer, mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        // In SDDL, whichever language the system speaks.
        let saved = fixture.dir.join("acl");
        let status = Command::new("icacls")
            .arg(fixture.door(peer))
            .arg("/save")
            .arg(&saved)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&saved).unwrap();
        let wide: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let sddl = String::from_utf16_lossy(&wide);
        assert!(sddl.contains("D:P"), "inherits from its directory: {sddl}");
        let dacl = sddl.split("D:").nth(1).unwrap();
        assert_eq!(dacl.matches("(A;").count(), 1, "{sddl}");
        assert!(dacl.contains(";;;OW)"), "{sddl}");
        // The owner has lost nothing by it, so the real thing still works.
        let mut client = fixture.start_client(peer);
        client.say(INITIALIZE);
        assert_eq!(message(&stream), CLAIMED);
        drop(client);
    });
}

#[test]
fn a_shim_will_not_show_its_token_beyond_this_machine() {
    let fixture = Fixture::new("beyond");
    let peer = format!("sha256:{}", "d".repeat(64));
    std::fs::create_dir_all(fixture.door(&peer).parent().unwrap()).unwrap();
    std::fs::write(
        fixture.door(&peer),
        json!({ "address": "10.0.0.1:80", "token": "t", "answer": "a" }).to_string(),
    )
    .unwrap();
    let mut client = fixture.start_client(&peer);
    client.say(INITIALIZE);
    let answer: Value = serde_json::from_str(&client.line()).unwrap();
    assert_eq!(answer["error"]["code"], -32000, "{answer}");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not on this machine"),
        "{answer}"
    );
    assert_eq!(client.ended().0.status.code(), Some(1));
}

#[test]
fn an_ordinary_offer_is_what_it_was_beside_a_standing_one() {
    with_peer("beside", |_, _, mux| {
        let standing = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&standing), READY);
        let session = mux.open_with("mcp-offer/1", b"canoa-browser").unwrap();
        let entry: Value = serde_json::from_slice(&message(&session)).unwrap();
        assert_eq!(entry["name"], "canoa-browser");
        assert_eq!(entry["args"], json!(["mcp-shim"]));
    });
}

#[test]
fn a_line_with_a_break_in_it_ends_the_connection_and_cannot_forge_another() {
    with_peer("forge", |fixture, peer, mux| {
        let stream = mux.open_with("mcp-offer/1", STANDING).unwrap();
        assert_eq!(message(&stream), READY);
        let mut client = fixture.start_client(peer);
        client.say(INITIALIZE);
        assert_eq!(message(&stream), CLAIMED);
        assert_eq!(message(&stream), INITIALIZE);
        stream.send(b"{\"a\":1}\n{\"forged\":true}").unwrap();
        assert_eq!(client.line(), "");
        let (output, stdout) = client.ended();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(stdout, "");
    });
}
