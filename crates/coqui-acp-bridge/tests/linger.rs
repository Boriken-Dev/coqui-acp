//! `serve --linger`: the real binary, a scripted agent process and a real socket, with a link cut as a page reload cuts it.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_bridge::linger::OUTPUT_LOST;
use coqui_agentic_peer::auth::{Sealed, prove_out};
use coqui_agentic_peer::mux::{Limits, Mux, Side};
use coqui_peer_webcrypto::{Identity, Trust, TrustStore, WebCrypto};
use coqui_web::{Channel, Dialer, Failure, Kind, websocket};
use serde_json::{Value, json};

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");
const WAIT: Duration = Duration::from_secs(30);

struct Fixture {
    dir: PathBuf,
    bridge: Option<Child>,
    said: Arc<Mutex<String>>,
    /// Starts the bridge as a terminal starts a foreground program, the leader of a process group of its own.
    #[cfg(unix)]
    own_group: bool,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-linger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            bridge: None,
            said: Arc::default(),
            #[cfg(unix)]
            own_group: false,
        }
    }

    fn identity(&self, name: &str) -> (PathBuf, Identity) {
        let path = self.dir.join(name);
        let identity = Identity::open_or_create(&path).unwrap();
        (path, identity)
    }

    /// `serve` on a port it chose, with `flags` and the scripted agent given `agent_args`, returning the port and its peer id. What
    /// it says on standard error is kept, for a failure to report.
    fn serve(&mut self, flags: &[&str], agent_args: &[&str]) -> (u16, String) {
        let mut command = Command::new(BRIDGE);
        command
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(flags)
            .arg("--")
            .arg(turn_agent())
            .args(agent_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        if self.own_group {
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        let mut child = command.spawn().unwrap();
        let mut id = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut id)
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        self.bridge = Some(child);
        let (listening, port) = mpsc::channel();
        let said = Arc::clone(&self.said);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(address) = line.strip_prefix("listening on ") {
                    let _ = listening.send(address.rsplit(':').next().unwrap().to_owned());
                }
                said.lock().unwrap().push_str(&format!("{line}\n"));
            }
        });
        let port = port.recv_timeout(WAIT).unwrap().parse().unwrap();
        (port, id.trim().to_owned())
    }

    fn said(&self) -> String {
        self.said.lock().unwrap().clone()
    }

    /// Waits for the process to be gone, and says what the bridge said if it is not.
    fn expect_gone(&self, pid: u64, what: &str) {
        let until = Instant::now() + WAIT;
        while alive(pid) {
            assert!(
                Instant::now() < until,
                "{what} is still running. The bridge said:\n{}",
                self.said()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// The scripted agent built beside the test binary.
fn turn_agent() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "coqui-acp-bridge", "--example", "turn_agent"])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mut path = PathBuf::from(BRIDGE);
    path.pop();
    path.push("examples");
    path.push(format!("turn_agent{}", std::env::consts::EXE_SUFFIX));
    path
}

/// A killed process stays listed as a zombie until something reaps it, and has ended by then, so that does not count.
fn alive(pid: u64) -> bool {
    let pid = pid.to_string();
    if cfg!(windows) {
        let listed = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&pid)
    } else {
        let listed = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&listed.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }
}

/// JSON-RPC over one channel, keeping every message it passes on the way to the one wanted.
struct Client<'a, C: Channel + ?Sized> {
    channel: &'a C,
    seen: Vec<Value>,
}

impl<'a, C: Channel + ?Sized> Client<'a, C> {
    fn new(channel: &'a C) -> Self {
        Self {
            channel,
            seen: Vec::new(),
        }
    }

    fn send(&self, id: u64, method: &str, params: &Value) {
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.channel.send(message.to_string().as_bytes()).unwrap();
    }

    /// Reads until `wanted` matches, and returns that message.
    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + WAIT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no {what}, after {:?}", self.seen);
            match self.channel.receive(Some(left)) {
                Ok(Some(message)) => {
                    let message: Value = serde_json::from_slice(&message).unwrap();
                    self.seen.push(message.clone());
                    if wanted(&message) {
                        return message;
                    }
                }
                Err(Failure::Timeout) => {}
                other => panic!("no {what}: {other:?}, after {:?}", self.seen),
            }
        }
    }

    fn call(&mut self, id: u64, method: &str, params: &Value) -> Value {
        self.send(id, method, params);
        self.until(method, |message| {
            message["id"] == id && message.get("method").is_none()
        })
    }

    /// `initialize`, which answers with the agent's process id.
    fn pid(&mut self) -> u64 {
        let answer = self.call(1, "initialize", &json!({"protocolVersion": 1}));
        answer["result"]["_meta"]["pid"].as_u64().unwrap()
    }

    fn new_session(&mut self) -> String {
        let answer = self.call(2, "session/new", &json!({"cwd": "/w", "mcpServers": []}));
        answer["result"]["sessionId"].as_str().unwrap().to_owned()
    }

    fn prompt(&self, id: u64, session: &str, script: &str) {
        self.send(
            id,
            "session/prompt",
            &json!({"sessionId": session, "prompt": [{"type": "text", "text": script}]}),
        );
    }

    fn resume(&mut self, id: u64, session: &str) -> Value {
        self.call(
            id,
            "session/resume",
            &json!({"sessionId": session, "cwd": "/w", "mcpServers": []}),
        )
    }

    fn chunk_arrives(&mut self, number: u64) {
        self.until("chunk", |message| chunk_number(message) == Some(number));
    }

    /// The numbers of the chunks seen, in the order they came.
    fn chunks(&self) -> Vec<u64> {
        self.seen.iter().filter_map(chunk_number).collect()
    }
}

fn chunk_number(message: &Value) -> Option<u64> {
    if message["method"] != "session/update" {
        return None;
    }
    message["params"]["update"]["content"]["text"]
        .as_str()?
        .split(' ')
        .next()?
        .parse()
        .ok()
}

fn is_answer(message: &Value, id: u64) -> bool {
    message["id"] == id && message["result"]["stopReason"] == "end_turn"
}

/// A key that can reach one `serve`.
struct Peer {
    method: WebCrypto,
    port: u16,
}

impl Peer {
    fn new(identity: Identity, port: u16, server: &str) -> Self {
        Self {
            method: WebCrypto::new(identity)
                .with_responders(Trust::pinned([server.parse().unwrap()])),
            port,
        }
    }

    fn dial(&self) -> websocket::Channel {
        let url = url::Url::parse(&format!("ws://127.0.0.1:{}/agent", self.port)).unwrap();
        websocket::Dialer::new()
            .with_frames(websocket::Frames::Binary)
            .dial(Kind::WebSocket, &url, Duration::from_secs(10))
            .unwrap()
    }

    /// Connects, proves this key, and runs `body` as a plain ACP client. The link is left for `body` to end.
    fn on_link<T>(
        &self,
        body: impl FnOnce(&mut Client<'_, Sealed<'_, websocket::Channel>>, &websocket::Channel) -> T,
    ) -> T {
        let channel = self.dial();
        let established = prove_out(&self.method, &channel, WAIT, 1 << 25).unwrap();
        let sealed = Sealed::new(&channel, established.session);
        let mut client = Client::new(&sealed);
        body(&mut client, &channel)
    }
}

/// Ends a link as a reload does: gone, with no close record.
fn cut(channel: &websocket::Channel) {
    Channel::close(channel);
}

/// `serve` with `extra`, allowing `allowed`, and the one peer returned.
fn serving(fixture: &mut Fixture, extra: &[&str]) -> Peer {
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let allowed = client.peer_id().to_string();
    let mut flags = vec!["--identity", key.to_str().unwrap(), "--allow", &allowed];
    flags.extend_from_slice(extra);
    let (port, server) = fixture.serve(&flags, &[]);
    Peer::new(client, port, &server)
}

#[test]
fn a_reload_mid_turn_reconnects_resumes_and_sees_the_turn_finish() {
    let mut fixture = Fixture::new("reload");
    let peer = serving(&mut fixture, &["--linger", "60", "--max-peers", "1"]);
    let (pid, session) = peer.on_link(|client, channel| {
        let pid = client.pid();
        let session = client.new_session();
        client.prompt(3, &session, "chunks 40 50");
        client.chunk_arrives(3);
        cut(channel);
        (pid, session)
    });
    // The turn goes on while nobody is connected.
    std::thread::sleep(Duration::from_millis(300));
    peer.on_link(|client, _| {
        assert_eq!(client.pid(), pid, "the same agent. {}", fixture.said());
        let resumed = client.resume(2, &session);
        assert!(resumed.get("result").is_some(), "{resumed}");
        client.until("the turn's answer", |message| is_answer(message, 3));
        let chunks = client.chunks();
        assert_eq!(chunks.last(), Some(&39), "{chunks:?}");
        assert!(chunks.first().is_some_and(|first| *first > 3), "{chunks:?}");
        let unbroken: Vec<u64> = (chunks[0]..40).collect();
        assert_eq!(
            chunks, unbroken,
            "what was kept comes unbroken and in order"
        );
    });
    assert!(alive(pid), "the agent stays while its time runs");
}

#[test]
fn a_reconnect_faster_than_the_old_link_ends_still_finds_the_agent() {
    let mut fixture = Fixture::new("fast");
    let peer = serving(
        &mut fixture,
        &[
            "--linger",
            "60",
            "--max-peers",
            "1",
            "--linger-settle-ms",
            "5000",
        ],
    );
    let old = peer.dial();
    let established = prove_out(&peer.method, &old, WAIT, 1 << 25).unwrap();
    let old_sealed = Sealed::new(&old, established.session);
    let pid = Client::new(&old_sealed).pid();
    // The new connection is proven while the old link is still up, as a reload can be.
    peer.on_link(|client, _| {
        cut(&old);
        assert_eq!(client.pid(), pid, "{}", fixture.said());
    });
}

#[test]
fn an_orderly_close_keeps_the_agent_too() {
    let mut fixture = Fixture::new("orderly");
    let peer = serving(&mut fixture, &["--linger", "60"]);
    let pid = peer.on_link(|client, _| {
        let pid = client.pid();
        Channel::close(client.channel);
        pid
    });
    peer.on_link(|client, _| {
        assert_eq!(client.pid(), pid, "{}", fixture.said());
    });
}

#[test]
fn another_peer_gets_a_fresh_agent_and_cannot_resume_the_kept_one() {
    let mut fixture = Fixture::new("other");
    let (key, _) = fixture.identity("server");
    let (_, one) = fixture.identity("one");
    let (_, two) = fixture.identity("two");
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &one.peer_id().to_string(),
        "--allow",
        &two.peer_id().to_string(),
        "--linger",
        "60",
        "--linger-total",
        "4",
    ]
    .map(str::to_owned);
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let (port, server) = fixture.serve(&flags, &[]);
    let first = Peer::new(one, port, &server);
    let second = Peer::new(two, port, &server);

    let (pid, session) = first.on_link(|client, channel| {
        let pid = client.pid();
        let session = client.new_session();
        client.prompt(3, &session, "chunks 2 0");
        client.until("the answer", |message| is_answer(message, 3));
        cut(channel);
        (pid, session)
    });
    // Time for the first link's end to be seen, so a mix-up would have the agent to hand out.
    std::thread::sleep(Duration::from_millis(300));
    second.on_link(|client, _| {
        let other = client.pid();
        assert_ne!(other, pid, "{}", fixture.said());
        let resumed = client.resume(2, &session);
        assert_eq!(
            resumed["error"]["message"],
            format!("turn_agent has no session {session}"),
            "the agent's own error: {resumed}"
        );
        assert!(
            client.chunks().is_empty(),
            "nothing of the first peer's: {:?}",
            client.seen
        );
    });
    first.on_link(|client, _| {
        assert_eq!(client.pid(), pid, "its own is still there");
    });
}

#[test]
fn past_the_time_the_agent_is_gone_and_resume_fails_with_the_agents_own_error() {
    let mut fixture = Fixture::new("expired");
    let peer = serving(&mut fixture, &["--linger", "1"]);
    let (pid, session) = peer.on_link(|client, channel| {
        let pid = client.pid();
        let session = client.new_session();
        cut(channel);
        (pid, session)
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(pid), "still within its time");
    fixture.expect_gone(pid, "an agent past its time");
    assert!(
        fixture.said().contains("its time ran out"),
        "{}",
        fixture.said()
    );
    peer.on_link(|client, _| {
        let other = client.pid();
        assert_ne!(other, pid);
        let resumed = client.resume(2, &session);
        assert_eq!(
            resumed["error"]["message"],
            format!("turn_agent has no session {session}"),
            "the agent's own error: {resumed}"
        );
    });
}

#[test]
fn without_linger_a_link_ending_ends_the_agent() {
    let mut fixture = Fixture::new("default");
    let peer = serving(&mut fixture, &[]);
    let pid = peer.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    fixture.expect_gone(pid, "an agent with no --linger");
}

#[test]
fn an_agent_that_exits_while_kept_is_not_given_back() {
    let mut fixture = Fixture::new("exits");
    let peer = serving(&mut fixture, &["--linger", "60"]);
    let pid = peer.on_link(|client, channel| {
        let pid = client.pid();
        let session = client.new_session();
        client.prompt(3, &session, "exit 200");
        client.chunk_arrives(0);
        cut(channel);
        pid
    });
    fixture.expect_gone(pid, "an agent that exited");
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("it ended") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    peer.on_link(|client, _| {
        assert_ne!(client.pid(), pid, "{}", fixture.said());
    });
}

#[test]
fn output_past_the_buffer_loses_the_oldest_and_the_next_link_is_told_first() {
    let mut fixture = Fixture::new("overflow");
    let peer = serving(
        &mut fixture,
        &["--linger", "60", "--linger-buffer-bytes", "4096"],
    );
    const LINES: u64 = 2000;
    let session = peer.on_link(|client, channel| {
        client.pid();
        let session = client.new_session();
        client.prompt(3, &session, &format!("flood {LINES} 64 500"));
        client.chunk_arrives(0);
        cut(channel);
        session
    });
    // The flood starts after the cut, and is over well before this.
    std::thread::sleep(Duration::from_millis(2000));
    peer.on_link(|client, _| {
        client.until("the turn's answer", |message| is_answer(message, 3));
        let notice = &client.seen[0];
        assert_eq!(notice["method"], OUTPUT_LOST, "first on the link: {notice}");
        assert!(notice.get("id").is_none(), "a notification: {notice}");
        let chunks = client.chunks();
        let lost = notice["params"]["lines"].as_u64().unwrap();
        assert!(
            lost > 0 && notice["params"]["bytes"].as_u64().unwrap() > 0,
            "{notice}"
        );
        // Lines 1 to LINES-1 and the answer: each is either lost and counted, or here, and what is here is what came last.
        let here = u64::try_from(chunks.len()).unwrap() + 1;
        assert_eq!(lost + here, LINES, "{notice}, then {} chunks", chunks.len());
        let unbroken: Vec<u64> = (LINES - (here - 1)..LINES).collect();
        assert_eq!(chunks, unbroken);
        let kept: usize = client.seen[1..]
            .iter()
            .map(|message| message.to_string().len())
            .sum();
        assert!(kept < 2 * 4096, "bounded by the setting: {kept}");
        let resumed = client.resume(9, &session);
        assert!(resumed.get("result").is_some(), "{resumed}");
    });
}

#[test]
fn a_peer_keeps_one_agent_and_its_older_one_ends() {
    let mut fixture = Fixture::new("perpeer");
    let peer = serving(
        &mut fixture,
        &["--linger", "60", "--linger-settle-ms", "100"],
    );
    let older = peer.dial();
    let established = prove_out(&peer.method, &older, WAIT, 1 << 25).unwrap();
    let older_sealed = Sealed::new(&older, established.session);
    let older_pid = Client::new(&older_sealed).pid();
    let newer = peer.dial();
    let established = prove_out(&peer.method, &newer, WAIT, 1 << 25).unwrap();
    let newer_sealed = Sealed::new(&newer, established.session);
    let newer_pid = Client::new(&newer_sealed).pid();
    assert_ne!(older_pid, newer_pid);

    cut(&older);
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(alive(older_pid), "one is within the bound");
    cut(&newer);
    fixture.expect_gone(older_pid, "the older agent of a peer past its bound");
    assert!(alive(newer_pid));
    peer.on_link(|client, _| {
        assert_eq!(client.pid(), newer_pid, "{}", fixture.said());
    });
}

#[test]
fn a_higher_bound_keeps_both_of_a_peers_agents_and_gives_back_the_oldest_first() {
    let mut fixture = Fixture::new("perpeer2");
    let peer = serving(
        &mut fixture,
        &[
            "--linger",
            "60",
            "--linger-settle-ms",
            "100",
            "--linger-per-peer",
            "2",
        ],
    );
    let older = peer.dial();
    let established = prove_out(&peer.method, &older, WAIT, 1 << 25).unwrap();
    let older_sealed = Sealed::new(&older, established.session);
    let older_pid = Client::new(&older_sealed).pid();
    let newer = peer.dial();
    let established = prove_out(&peer.method, &newer, WAIT, 1 << 25).unwrap();
    let newer_sealed = Sealed::new(&newer, established.session);
    let newer_pid = Client::new(&newer_sealed).pid();

    cut(&older);
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    cut(&newer);
    let until = Instant::now() + WAIT;
    while fixture.said().matches("left:").count() < 2 {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(alive(older_pid) && alive(newer_pid));
    peer.on_link(|client, channel| {
        assert_eq!(client.pid(), older_pid, "{}", fixture.said());
        cut(channel);
    });
}

#[test]
fn the_total_bound_ends_the_oldest_kept_agent_whoever_it_is_for() {
    let mut fixture = Fixture::new("total");
    let (key, _) = fixture.identity("server");
    let (_, one) = fixture.identity("one");
    let (_, two) = fixture.identity("two");
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &one.peer_id().to_string(),
        "--allow",
        &two.peer_id().to_string(),
        "--linger",
        "60",
        "--linger-total",
        "1",
    ]
    .map(str::to_owned);
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let (port, server) = fixture.serve(&flags, &[]);
    let first = Peer::new(one, port, &server);
    let second = Peer::new(two, port, &server);
    let older = first.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(alive(older));
    let newer = second.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    fixture.expect_gone(older, "the oldest kept agent past the total");
    assert!(alive(newer));
}

#[test]
fn a_second_connection_while_the_first_is_up_gets_an_agent_of_its_own() {
    let mut fixture = Fixture::new("second");
    let peer = serving(
        &mut fixture,
        &["--linger", "60", "--linger-settle-ms", "100"],
    );
    let first = peer.dial();
    let established = prove_out(&peer.method, &first, WAIT, 1 << 25).unwrap();
    let first_sealed = Sealed::new(&first, established.session);
    let mut first_client = Client::new(&first_sealed);
    let first_pid = first_client.pid();
    peer.on_link(|client, _| {
        let second_pid = client.pid();
        assert_ne!(second_pid, first_pid, "{}", fixture.said());
        let session = client.new_session();
        assert!(session.starts_with(&second_pid.to_string()), "{session}");
    });
    assert_eq!(
        first_client.pid(),
        first_pid,
        "the first link is unaffected"
    );
}

#[test]
fn a_kept_agent_takes_no_place_among_max_peers() {
    let mut fixture = Fixture::new("slots");
    let (key, _) = fixture.identity("server");
    let (_, one) = fixture.identity("one");
    let (_, two) = fixture.identity("two");
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &one.peer_id().to_string(),
        "--allow",
        &two.peer_id().to_string(),
        "--linger",
        "60",
        "--max-peers",
        "1",
    ]
    .map(str::to_owned);
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let (port, server) = fixture.serve(&flags, &[]);
    let first = Peer::new(one, port, &server);
    let second = Peer::new(two, port, &server);
    let kept = first.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    second.on_link(|client, _| {
        assert_ne!(client.pid(), kept, "{}", fixture.said());
    });
    assert!(alive(kept));
}

#[test]
fn a_kept_agent_goes_back_over_a_stream_of_a_link_too() {
    let mut fixture = Fixture::new("stream");
    let peer = serving(&mut fixture, &["--linger", "60", "--forward"]);
    let on_stream = |body: &mut dyn FnMut(u64)| {
        let channel = peer.dial();
        let established = prove_out(&peer.method, &channel, WAIT, 1 << 25).unwrap();
        let sealed = Sealed::new(&channel, established.session);
        Mux::run(&sealed, Side::Dialed, Limits::default(), None, |mux| {
            let stream = mux.open("acp/1").unwrap();
            body(Client::new(&stream).pid());
        });
    };
    let mut first = 0;
    on_stream(&mut |pid| first = pid);
    on_stream(&mut |pid| assert_eq!(pid, first, "{}", fixture.said()));
}

#[test]
fn a_peer_removed_from_the_store_does_not_leave_an_agent_behind() {
    let mut fixture = Fixture::new("removed");
    let store = TrustStore::open(&fixture.dir.join("peers")).unwrap();
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let listed = store.accept_key(&client.key_material()).unwrap();
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow-store",
        fixture.dir.join("peers").to_str().unwrap(),
        "--linger",
        "300",
    ]
    .map(str::to_owned);
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let (port, server) = fixture.serve(&flags, &[]);
    let peer = Peer::new(client, port, &server);
    peer.on_link(|client, _| {
        let pid = client.pid();
        let session = client.new_session();
        // Busy, so sending fails too once the refusal closes the channel: the refusal is still what ended the link. Read, because
        // closing a channel waits for a peer that has stopped reading.
        client.prompt(3, &session, "flood 1000000 64 0");
        client.chunk_arrives(0);
        store.remove(&listed).unwrap();
        while matches!(
            client.channel.receive(Some(Duration::from_millis(100))),
            Ok(Some(_)) | Err(Failure::Timeout)
        ) {}
        fixture.expect_gone(pid, "the agent of a peer no longer accepted");
    });
}

#[test]
fn killing_the_bridge_ends_a_kept_agent_that_ignores_its_input_closing() {
    let mut fixture = Fixture::new("killedkept");
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let allowed = client.peer_id().to_string();
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &allowed,
        "--linger",
        "300",
    ];
    let (port, server) = fixture.serve(&flags, &["--ignore-eof"]);
    let peer = Peer::new(client, port, &server);
    let pid = peer.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    let bridge = fixture.bridge.as_mut().unwrap();
    bridge.kill().unwrap();
    bridge.wait().unwrap();
    fixture.expect_gone(pid, "a kept agent after its bridge was killed");
}

#[test]
fn killing_the_bridge_ends_an_attached_agent_that_ignores_its_input_closing() {
    let mut fixture = Fixture::new("killedlive");
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let allowed = client.peer_id().to_string();
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &allowed,
        "--linger",
        "300",
    ];
    let (port, server) = fixture.serve(&flags, &["--ignore-eof"]);
    let peer = Peer::new(client, port, &server);
    peer.on_link(|client, _| {
        let pid = client.pid();
        let bridge = fixture.bridge.as_mut().unwrap();
        bridge.kill().unwrap();
        bridge.wait().unwrap();
        fixture.expect_gone(pid, "an attached agent after its bridge was killed");
    });
}

/// A terminal's Ctrl-C goes to the whole foreground group, which has to leave the helper standing.
#[cfg(unix)]
#[test]
fn a_terminals_ctrl_c_to_the_bridge_ends_a_kept_agent_too() {
    let mut fixture = Fixture::new("ctrlc");
    fixture.own_group = true;
    let (key, _) = fixture.identity("server");
    let (_, client) = fixture.identity("client");
    let allowed = client.peer_id().to_string();
    let flags = [
        "--identity",
        key.to_str().unwrap(),
        "--allow",
        &allowed,
        "--linger",
        "300",
    ];
    let (port, server) = fixture.serve(&flags, &["--ignore-eof"]);
    let peer = Peer::new(client, port, &server);
    let pid = peer.on_link(|client, channel| {
        let pid = client.pid();
        cut(channel);
        pid
    });
    let until = Instant::now() + WAIT;
    while !fixture.said().contains("left:") {
        assert!(Instant::now() < until, "{}", fixture.said());
        std::thread::sleep(Duration::from_millis(20));
    }
    let bridge = fixture.bridge.as_mut().unwrap();
    let group = format!("-{}", bridge.id());
    let sent = Command::new("kill")
        .args(["-s", "INT", "--", &group])
        .status()
        .unwrap();
    assert!(sent.success());
    assert!(!bridge.wait().unwrap().success(), "Ctrl-C ends the bridge");
    fixture.expect_gone(pid, "a kept agent after Ctrl-C at the bridge's terminal");
}

#[test]
fn a_helper_told_the_agent_was_dealt_with_leaves_it_alone_and_one_not_told_ends_it() {
    use coqui_acp_bridge::agent::{Agent, watch};
    let program = turn_agent().into_os_string();
    let started = Agent::spawn(&program, &[], 1 << 20).unwrap();
    let pid = started.pid();
    watch(pid, &b"x"[..]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(u64::from(pid)), "it was told not to look");
    watch(pid, &b""[..]);
    let until = Instant::now() + WAIT;
    while alive(u64::from(pid)) {
        assert!(Instant::now() < until, "the pipe ended with nothing said");
        std::thread::sleep(Duration::from_millis(50));
    }
    started.stop().unwrap();
}

#[test]
fn stopping_a_guarded_agent_dismisses_its_helper() {
    use coqui_acp_bridge::agent::Agent;
    let program = turn_agent().into_os_string();
    let started = Agent::spawn(&program, &[], 1 << 20)
        .unwrap()
        .guarded(BRIDGE.as_ref(), &["agent-guard"])
        .unwrap();
    let pid = started.pid();
    let helper = started.guard_pid().unwrap();
    assert!(
        alive(u64::from(helper)),
        "the check has to be able to see it"
    );
    started.stop().unwrap();
    assert!(!alive(u64::from(pid)));
    assert!(!alive(u64::from(helper)), "stop waits for it to go");
    assert_eq!(started.guard_pid(), None);
}

/// A link taking `sent_ok` messages and then failing, or with none given, one that takes all and closes after `until_id`'s answer.
struct Scripted {
    incoming: Mutex<Vec<&'static str>>,
    sent_ok: Option<usize>,
    until_id: Option<u64>,
    sent: Mutex<Vec<Value>>,
}

impl Channel for Scripted {
    fn send(&self, message: &[u8]) -> Result<(), Failure> {
        let mut sent = self.sent.lock().unwrap();
        if self.sent_ok.is_some_and(|limit| sent.len() >= limit) {
            return Err(Failure::PathLost);
        }
        sent.push(serde_json::from_slice(message).unwrap());
        Ok(())
    }

    fn receive(&self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, Failure> {
        if let Some(next) = self.incoming.lock().unwrap().pop() {
            return Ok(Some(next.as_bytes().to_vec()));
        }
        let over = self.until_id.is_some_and(|id| {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .any(|message| is_answer(message, id))
        });
        if over {
            return Ok(None);
        }
        std::thread::sleep(timeout.unwrap_or(WAIT));
        Err(Failure::Timeout)
    }

    fn close(&self) {}
}

#[test]
fn a_line_the_link_could_not_take_goes_first_on_the_next() {
    use coqui_acp_bridge::agent::Agent;
    use coqui_acp_bridge::linger::{Keeper, Settings};
    use coqui_acp_bridge::relay::LinkEnd;
    let program = turn_agent().into_os_string();
    let keeper = Keeper::new(Settings::new(Duration::from_secs(5)), |_| {});
    let dying = Scripted {
        incoming: Mutex::new(vec![
            r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"s","prompt":[{"type":"text","text":"chunks 5 0"}]}}"#,
        ]),
        sent_ok: Some(0),
        until_id: None,
        sent: Mutex::default(),
    };
    let arrival = keeper.arrive("peer");
    let end = arrival
        .serve(&dying, || Agent::spawn(&program, &[], 1 << 20))
        .unwrap();
    drop(arrival);
    assert_eq!(end, LinkEnd::Failed(Failure::PathLost));
    let next = Scripted {
        incoming: Mutex::default(),
        sent_ok: None,
        until_id: Some(3),
        sent: Mutex::default(),
    };
    let arrival = keeper.arrive("peer");
    let end = arrival
        .serve(&next, || panic!("the kept agent should come back"))
        .unwrap();
    assert_eq!(end, LinkEnd::PeerClosed);
    let sent = next.sent.lock().unwrap();
    let chunks: Vec<u64> = sent.iter().filter_map(chunk_number).collect();
    assert_eq!(chunks, [0, 1, 2, 3, 4], "{sent:?}");
}

#[test]
fn linger_is_refused_where_no_key_was_proved_and_its_settings_need_it() {
    let fixture = Fixture::new("flags");
    let refused = |flags: &[&str], expect: &str| {
        let mut child = Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(flags)
            .args(["--", "x"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // One that is not refused serves for ever: that is the failure, and it must not hold the test up.
        let until = Instant::now() + WAIT;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{flags:?} was not refused");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{flags:?}");
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains(expect), "{flags:?}: {text}");
    };
    refused(&["--no-peer-proof", "--linger", "5"], "in-band proof");
    let (certificate, _) = fixture.identity("certificate");
    let tls = coqui_peer_tls::Identity::open_or_create(&fixture.dir.join("tls")).unwrap();
    refused(
        &[
            "--tls-identity",
            fixture.dir.join("tls").to_str().unwrap(),
            "--allow-cert",
            &tls.fingerprint().to_string(),
            "--linger",
            "5",
        ],
        "in-band proof",
    );
    let (_, client) = fixture.identity("client");
    let allowed = client.peer_id().to_string();
    let base = [
        "--identity",
        certificate.to_str().unwrap(),
        "--allow",
        &allowed,
    ];
    for flag in [
        ["--linger-per-peer", "2"],
        ["--linger-total", "2"],
        ["--linger-buffer-bytes", "2"],
        ["--linger-settle-ms", "2"],
    ] {
        let mut flags = base.to_vec();
        flags.extend_from_slice(&flag);
        refused(&flags, "--linger");
    }
    let mut zero = base.to_vec();
    zero.extend_from_slice(&["--linger", "5", "--linger-per-peer", "0"]);
    refused(&zero, "--linger-per-peer");
    let mut endless = base.to_vec();
    endless.extend_from_slice(&["--linger", "18446744073709551615"]);
    refused(&endless, "--linger");
}
