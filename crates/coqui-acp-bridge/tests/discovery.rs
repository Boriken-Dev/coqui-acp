//! The real binary as a person and a browser start it: `serve --advertise`, `discover`, and the native host's framing.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BRIDGE: &str = env!("CARGO_BIN_EXE_coqui-acp-bridge");
const ENV: &str = coqui_acp_bridge::discovery::DESCRIPTORS_ENV;

struct Fixture {
    dir: PathBuf,
    children: Vec<Child>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("coqui-acp-discovery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        Self {
            dir,
            children: Vec::new(),
        }
    }

    fn descriptors(&self) -> PathBuf {
        self.dir.join("agents")
    }

    fn keygen(&self) -> (PathBuf, String) {
        let path = self.dir.join("identity");
        let output = Command::new(BRIDGE)
            .arg("keygen")
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        (
            path,
            String::from_utf8(output.stdout).unwrap().trim().to_owned(),
        )
    }

    /// Starts `serve --advertise` on a port of its choosing, and returns when it listens.
    fn serve(&mut self, identity: &Path, allowed: &str, label: &str) -> usize {
        let mut child = Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0", "--identity"])
            .arg(identity)
            .args([
                "--allow",
                allowed,
                "--advertise",
                label,
                "--",
                "unused-agent",
            ])
            .env(ENV, self.descriptors())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stderr.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.starts_with("listening on"), "{line}");
        self.children.push(child);
        self.children.len() - 1
    }

    fn discover(&self) -> Vec<Value> {
        let output = Command::new(BRIDGE)
            .args(["discover", "--seconds", "0"])
            .env(ENV, self.descriptors())
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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

fn waited_for(what: &str, mut test: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !test() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_agent_on_loopback_is_found_with_its_key_and_gone_when_it_dies() {
    let mut fixture = Fixture::new("loopback");
    assert_eq!(fixture.discover(), Vec::<Value>::new());
    let (identity, peer) = fixture.keygen();
    let serving = fixture.serve(&identity, &peer, "My agent");
    let found = fixture.discover();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["label"], "My agent");
    assert_eq!(found[0]["peer"], peer.as_str());
    assert_eq!(found[0]["source"], "local");
    assert!(
        found[0]["url"]
            .as_str()
            .unwrap()
            .starts_with("ws://127.0.0.1:"),
        "{found:?}"
    );

    // A kill leaves its file behind, and the next search clears it.
    fixture.children[serving].kill().unwrap();
    fixture.children[serving].wait().unwrap();
    assert_eq!(std::fs::read_dir(fixture.descriptors()).unwrap().count(), 1);
    waited_for("the stale descriptor to clear", || {
        fixture.discover().is_empty()
    });
    assert_eq!(std::fs::read_dir(fixture.descriptors()).unwrap().count(), 0);
}

#[test]
fn nothing_is_advertised_unless_asked_for() {
    let fixture = Fixture::new("off");
    let (identity, peer) = fixture.keygen();
    let mut child = Command::new(BRIDGE)
        .args(["serve", "--listen", "127.0.0.1:0", "--identity"])
        .arg(&identity)
        .args(["--allow", &peer, "--", "unused-agent"])
        .env(ENV, fixture.descriptors())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("listening on"), "{line}");
    assert_eq!(std::fs::read_dir(fixture.descriptors()).unwrap().count(), 0);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn what_a_descriptor_cannot_carry_is_refused_at_the_command_line() {
    let fixture = Fixture::new("flags");
    let (identity, peer) = fixture.keygen();
    let run = |flags: &[&str]| {
        Command::new(BRIDGE)
            .args(["serve", "--listen", "127.0.0.1:0", "--identity"])
            .arg(&identity)
            .args(["--allow", &peer])
            .args(flags)
            .args(["--", "unused-agent"])
            .env(ENV, fixture.descriptors())
            .output()
            .unwrap()
    };
    let long = "x".repeat(64);
    for flags in [
        vec![
            "--advertise",
            "Mine",
            "--advertise-host",
            "agent.example.com",
        ],
        vec!["--advertise", long.as_str()],
        vec!["--advertise-host", "agent.example.com"],
    ] {
        let output = run(&flags);
        assert!(!output.status.success(), "{flags:?}");
    }
}

/// A native host as a browser runs it: its origin as the first argument, and framed JSON on its standard streams.
struct NativeHost(Child);

impl NativeHost {
    fn start(descriptors: &Path) -> Self {
        Self(
            Command::new(BRIDGE)
                .args([
                    "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
                    "--parent-window=0",
                ])
                .env(ENV, descriptors)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn send(&mut self, message: &Value) {
        let text = message.to_string();
        let stdin = self.0.stdin.as_mut().unwrap();
        stdin
            .write_all(&u32::try_from(text.len()).unwrap().to_ne_bytes())
            .unwrap();
        stdin.write_all(text.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let stdout = self.0.stdout.as_mut().unwrap();
        let mut length = [0_u8; 4];
        stdout.read_exact(&mut length).unwrap();
        let mut body = vec![0; usize::try_from(u32::from_ne_bytes(length)).unwrap()];
        stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_companion_answers_a_browser_and_ends_when_it_goes() {
    let mut fixture = Fixture::new("native");
    let (identity, peer) = fixture.keygen();
    fixture.serve(&identity, &peer, "Via companion");
    let mut host = NativeHost::start(&fixture.descriptors());
    host.send(&json!({ "op": "discover", "seconds": 0 }));
    let reply = host.receive();
    assert_eq!(reply["candidates"][0]["label"], "Via companion", "{reply}");
    assert_eq!(reply["candidates"][0]["peer"], peer.as_str());
    host.send(&json!({ "op": "spawn", "program": "calc" }));
    assert_eq!(host.receive()["error"], "unknown op");
    drop(host.0.stdin.take());
    assert!(host.0.wait().unwrap().success());
}

#[test]
fn the_companion_ends_on_a_length_no_browser_would_send() {
    let fixture = Fixture::new("framing");
    let mut host = NativeHost::start(&fixture.descriptors());
    let stdin = host.0.stdin.as_mut().unwrap();
    stdin.write_all(&u32::MAX.to_ne_bytes()).unwrap();
    stdin.flush().unwrap();
    assert_eq!(host.0.wait().unwrap().code(), Some(2));
}

/// Multicast has to work on this machine, so it is not in the default run.
#[test]
#[ignore = "needs multicast on this machine's interfaces"]
fn a_record_on_the_link_is_found_with_what_it_claims() {
    use coqui_acp_bridge::discovery::{Advertisement, Source, advertise, browse};
    let label = format!("coqui-acp-test-{}", std::process::id());
    let peer = "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    let advertisement = Advertisement::new(&label, true)
        .unwrap()
        .with_peer(Some(peer.clone()))
        .with_webtransport(Some(8766));
    let published = advertise(&advertisement, "0.0.0.0:48765".parse().unwrap()).unwrap();
    let found = browse(Duration::from_secs(6)).unwrap();
    let mine = found
        .iter()
        .find(|candidate| candidate.label == label)
        .unwrap_or_else(|| panic!("{found:?}"));
    assert_eq!(mine.source, Source::Network);
    assert_eq!(mine.peer.as_deref(), Some(peer.as_str()));
    assert_eq!(mine.webtransport, Some(8766));
    assert!(
        mine.url.starts_with("wss://") && mine.url.ends_with(":48765/acp"),
        "{}",
        mine.url
    );
    drop(published);
}
