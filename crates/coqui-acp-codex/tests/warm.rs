//! Codex app servers kept ahead of `session/new`, against the stand-in program, recording what each process is asked.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_codex::harness::{Exit, Process, Settings, WarmPolicy};
use coqui_acp_codex::{CodexAgent, CodexLaunch, CodexSettings, Launch, PlainSpawn, Spawn, Spawned};
use serde_json::{Value, json};
use support::{Peer, Scratch, replay_program};

/// What one app server was started with and has been written: each line it was sent, as JSON.
#[derive(Default)]
struct Started {
    sent: Mutex<Vec<Value>>,
    process: Mutex<Option<Arc<dyn Process>>>,
}

impl Started {
    fn methods(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|line| line["method"].as_str().map(str::to_string))
            .collect()
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line["method"] == method)
            .cloned()
            .collect()
    }
}

/// Starts programs as [`PlainSpawn`] does and records every app server, and every version probe.
#[derive(Default)]
struct Recording {
    servers: Mutex<Vec<Arc<Started>>>,
    versions: Mutex<usize>,
}

struct Shared(Arc<dyn Process>);

impl Process for Shared {
    fn kill(&self) -> io::Result<()> {
        self.0.kill()
    }
    fn try_wait(&self) -> io::Result<Option<Exit>> {
        self.0.try_wait()
    }
}

/// A standard input that keeps the lines written to it.
struct Tee {
    to: Box<dyn Write + Send>,
    started: Arc<Started>,
    line: Vec<u8>,
}

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.to.write(bytes)?;
        for &byte in &bytes[..written] {
            if byte == b'\n' {
                if let Ok(value) = serde_json::from_slice(&self.line) {
                    self.started.sent.lock().unwrap().push(value);
                }
                self.line.clear();
            } else {
                self.line.push(byte);
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}

impl Spawn for Recording {
    fn spawn(&self, launch: &Launch) -> io::Result<Spawned> {
        let spawned = PlainSpawn.spawn(launch)?;
        if launch.args.iter().any(|arg| arg == "--version") {
            *self.versions.lock().unwrap() += 1;
            return Ok(spawned);
        }
        let started = Arc::new(Started::default());
        let process: Arc<dyn Process> = Arc::from(spawned.process);
        *started.process.lock().unwrap() = Some(Arc::clone(&process));
        self.servers.lock().unwrap().push(Arc::clone(&started));
        Ok(Spawned::new(
            Box::new(Tee {
                to: spawned.stdin,
                started,
                line: Vec::new(),
            }),
            spawned.stdout,
            spawned.stderr,
            Box::new(Shared(process)),
        ))
    }
}

impl Recording {
    fn server(&self, index: usize) -> Arc<Started> {
        Arc::clone(&self.servers.lock().unwrap()[index])
    }

    fn count(&self) -> usize {
        self.servers.lock().unwrap().len()
    }
}

fn agent(spawn: &Arc<Recording>, program: &Path, home: &Path, policy: WarmPolicy) -> CodexAgent {
    let mut defaults = CodexLaunch::new(home);
    defaults.executable = OsString::from(program);
    defaults.env.insert("CODEX_HOME".into(), home.into());
    defaults
        .env
        .insert("COQUI_ACP_CODEX_THREAD".into(), "thread-1".into());
    let mut settings = CodexSettings::new();
    settings.warm = policy;
    let spawn: Arc<dyn Spawn> = Arc::clone(spawn) as Arc<dyn Spawn>;
    CodexAgent::new()
        .defaults(defaults)
        .spawn(spawn)
        .settings(settings)
}

fn until(what: &str, done: impl Fn() -> bool) {
    let limit = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < limit, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start(agent: CodexAgent) -> Peer {
    let mut peer = Peer::serve(agent, Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    peer
}

fn new_session(peer: &mut Peer, cwd: &Path) {
    let answer = peer.request("session/new", json!({"cwd": cwd.to_string_lossy()}));
    assert!(answer["result"]["sessionId"].is_string(), "{answer}");
}

/// A new session takes an app server that has done `initialize` and listed the models, and only starts its thread, in its own
/// directory, which the app server was not started in; a replacement is started behind it.
#[test]
fn a_new_session_takes_the_warm_app_server_and_starts_its_thread_there() {
    let home = Scratch::new("hit");
    let elsewhere = Scratch::new("hit-cwd");
    let spawn = Arc::new(Recording::default());
    let policy = WarmPolicy::new(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &home.0, policy.clone());
    preparing.prewarm();
    until("an app server is warm", || preparing.warm_ready() == 1);
    assert_eq!(
        spawn.server(0).methods(),
        ["initialize", "initialized", "model/list"],
        "no thread yet"
    );
    // Another agent, as another connection's is.
    let mut peer = start(agent(&spawn, &replay_program(), &home.0, policy.clone()));
    new_session(&mut peer, &elsewhere.0);
    let threads = spawn.server(0).requests("thread/start");
    assert_eq!(threads.len(), 1, "the warm app server served the session");
    assert_eq!(
        threads[0]["params"]["cwd"],
        json!(elsewhere.0.to_string_lossy()),
        "in the session's own directory"
    );
    until("a replacement is warm", || preparing.warm_ready() == 1);
    assert_eq!(spawn.count(), 2);
    assert_eq!(spawn.server(1).requests("thread/start").len(), 0);
    // The stand-in names one thread, so the next session is another connection's.
    let mut next = start(agent(&spawn, &replay_program(), &home.0, policy));
    new_session(&mut next, &home.0);
    assert_eq!(spawn.server(1).requests("thread/start").len(), 1);
}

/// An app server started for another environment is not taken, and starts its own.
#[test]
fn another_environment_starts_its_own_app_server() {
    let home = Scratch::new("miss");
    let other = Scratch::new("miss-other");
    let spawn = Arc::new(Recording::default());
    let policy = WarmPolicy::new(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &home.0, policy.clone());
    preparing.prewarm();
    until("an app server is warm", || preparing.warm_ready() == 1);
    let mut peer = start(agent(&spawn, &replay_program(), &other.0, policy));
    new_session(&mut peer, &other.0);
    assert_eq!(spawn.server(0).requests("thread/start").len(), 0);
    assert!(spawn.count() >= 2);
    assert_eq!(
        preparing.warm_ready(),
        1,
        "the warm one waits for its own key"
    );
}

/// With the default policy nothing is started ahead.
#[test]
fn with_warming_off_nothing_is_started_ahead() {
    let home = Scratch::new("off");
    let spawn = Arc::new(Recording::default());
    let one = agent(&spawn, &replay_program(), &home.0, WarmPolicy::default());
    one.prewarm();
    let mut peer = start(one);
    new_session(&mut peer, &home.0);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(spawn.count(), 1, "only the session's own");
    assert_eq!(spawn.server(0).requests("thread/start").len(), 1);
}

/// An app server that waits past the idle time is stopped, and one that exited is not handed out.
#[test]
fn an_idle_app_server_is_stopped_and_an_exited_one_is_not_handed_out() {
    let home = Scratch::new("idle");
    let spawn = Arc::new(Recording::default());
    let idle = agent(
        &spawn,
        &replay_program(),
        &home.0,
        WarmPolicy::new(1, Duration::from_millis(300)),
    );
    idle.prewarm();
    until("it starts", || spawn.count() == 1);
    until("it is stopped for idling", || {
        let process = spawn.server(0).process.lock().unwrap().clone().unwrap();
        idle.warm_ready() == 0 && process.try_wait().is_ok_and(|exit| exit.is_some())
    });

    let other = Scratch::new("idle-exited");
    let spawn = Arc::new(Recording::default());
    let policy = WarmPolicy::new(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &other.0, policy.clone());
    preparing.prewarm();
    until("an app server is warm", || preparing.warm_ready() == 1);
    let ended = spawn.server(0);
    let process = ended.process.lock().unwrap().clone().unwrap();
    process.kill().unwrap();
    until("it is no longer counted", || preparing.warm_ready() == 0);
    let mut peer = start(agent(&spawn, &replay_program(), &other.0, policy));
    new_session(&mut peer, &other.0);
    assert_eq!(ended.requests("thread/start").len(), 0, "never handed out");
}

/// `--version` is asked once for a version of the program, whichever agent asks.
#[test]
fn the_version_probe_is_shared_by_every_agent_of_the_process() {
    let home = Scratch::new("probe");
    let program = home
        .0
        .join(format!("codex{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(replay_program(), &program).unwrap();
    let spawn = Arc::new(Recording::default());
    let first = agent(&spawn, &program, &home.0, WarmPolicy::default());
    let second = agent(&spawn, &program, &home.0, WarmPolicy::default());
    assert_eq!(first.cli_version().as_deref(), Some("0.164.0"));
    assert_eq!(second.cli_version().as_deref(), Some("0.164.0"));
    assert_eq!(*spawn.versions.lock().unwrap(), 1);
}
