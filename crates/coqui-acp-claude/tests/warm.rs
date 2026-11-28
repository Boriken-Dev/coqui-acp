//! Claude Code processes kept ahead of `session/new`, against the stand-in program, counting what the spawner is asked to start.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_claude::harness::{Exit, Process, Settings, WarmPolicy};
use coqui_acp_claude::{
    ClaudeAgent, ClaudeLaunch, ClaudeSettings, Launch, PlainSpawn, Spawn, Spawned,
};
use serde_json::{Value, json};
use support::{Peer, Scratch, replay_program};

/// Starts programs as [`PlainSpawn`] does and keeps what it was asked to start and a way to end each process from outside.
#[derive(Default)]
struct Counting {
    sessions: Mutex<Vec<String>>,
    versions: Mutex<usize>,
    processes: Mutex<Vec<Arc<dyn Process>>>,
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

impl Spawn for Counting {
    fn spawn(&self, launch: &Launch) -> io::Result<Spawned> {
        let id = launch.args.iter().find_map(|arg| {
            arg.to_string_lossy()
                .strip_prefix("--session-id=")
                .map(str::to_string)
        });
        match id {
            Some(id) => self.sessions.lock().unwrap().push(id),
            None => *self.versions.lock().unwrap() += 1,
        }
        let spawned = PlainSpawn.spawn(launch)?;
        let process: Arc<dyn Process> = Arc::from(spawned.process);
        self.processes.lock().unwrap().push(Arc::clone(&process));
        Ok(Spawned::new(
            spawned.stdin,
            spawned.stdout,
            spawned.stderr,
            Box::new(Shared(process)),
        ))
    }
}

impl Counting {
    fn sessions(&self) -> Vec<String> {
        self.sessions.lock().unwrap().clone()
    }
}

fn agent(spawn: &Arc<Counting>, program: &Path, config: &Path, policy: WarmPolicy) -> ClaudeAgent {
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = OsString::from(program);
    defaults
        .env
        .insert("CLAUDE_CONFIG_DIR".into(), config.into());
    let mut settings = ClaudeSettings::new();
    settings.warm = policy;
    let spawn: Arc<dyn Spawn> = Arc::clone(spawn) as Arc<dyn Spawn>;
    ClaudeAgent::new()
        .defaults(defaults)
        .spawn(spawn)
        .settings(settings)
}

fn warm(ahead: usize, idle: Duration) -> WarmPolicy {
    WarmPolicy::new(ahead, idle)
}

fn until(what: &str, done: impl Fn() -> bool) {
    let limit = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < limit, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn new_session(peer: &mut Peer, cwd: &Path, servers: Value) -> String {
    let answer = peer.request(
        "session/new",
        json!({"cwd": cwd.to_string_lossy(), "mcpServers": servers}),
    );
    answer["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no session: {answer}"))
        .to_string()
}

fn start(agent: ClaudeAgent) -> Peer {
    let mut peer = Peer::serve(agent, Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    peer
}

/// A new session takes the process started ahead (its session id is the one that process was started with), another
/// agent of the process takes it as well, and a replacement is started behind it.
#[test]
fn a_new_session_takes_the_warm_process_and_a_replacement_follows() {
    let scratch = Scratch::new("hit");
    let spawn = Arc::new(Counting::default());
    let policy = warm(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &scratch.0, policy.clone());
    preparing.prewarm(&scratch.0);
    until("a process is warm", || {
        preparing.warm_ready(&scratch.0) == 1
    });
    assert_eq!(spawn.sessions().len(), 1);
    // Another agent, as another connection's is.
    let mut peer = start(agent(&spawn, &replay_program(), &scratch.0, policy));
    let session = new_session(&mut peer, &scratch.0, json!([]));
    assert_eq!(session, spawn.sessions()[0], "the warm process served it");
    until("a replacement is warm", || {
        preparing.warm_ready(&scratch.0) == 1
    });
    assert_eq!(
        spawn.sessions().len(),
        2,
        "the warm one and its replacement"
    );
    // And the next takes the replacement.
    let next = new_session(&mut peer, &scratch.0, json!([]));
    assert_eq!(next, spawn.sessions()[1]);
}

/// A key that differs (another directory, another MCP server) starts its own process, and the warm one waits for its own key.
#[test]
fn a_key_that_does_not_match_starts_its_own_process() {
    let scratch = Scratch::new("miss");
    let other = Scratch::new("miss-other");
    let spawn = Arc::new(Counting::default());
    let policy = warm(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &scratch.0, policy.clone());
    preparing.prewarm(&scratch.0);
    until("a process is warm", || {
        preparing.warm_ready(&scratch.0) == 1
    });
    let warmed = spawn.sessions()[0].clone();
    let mut peer = start(agent(&spawn, &replay_program(), &scratch.0, policy));
    let elsewhere = new_session(&mut peer, &other.0, json!([]));
    assert_ne!(elsewhere, warmed, "another directory");
    let command = replay_program();
    let server =
        json!([{"name": "x", "command": command.to_string_lossy(), "args": [], "env": []}]);
    let with_server = new_session(&mut peer, &scratch.0, server);
    assert_ne!(with_server, warmed, "another MCP server");
    assert_eq!(
        preparing.warm_ready(&scratch.0),
        1,
        "the warm one is still there"
    );
    let plain = new_session(&mut peer, &scratch.0, json!([]));
    assert_eq!(plain, warmed);
}

/// With the default policy nothing is started ahead and a session starts its own, as before.
#[test]
fn with_warming_off_nothing_is_started_ahead() {
    let scratch = Scratch::new("off");
    let spawn = Arc::new(Counting::default());
    let policy = WarmPolicy::default();
    let one = agent(&spawn, &replay_program(), &scratch.0, policy);
    one.prewarm(&scratch.0);
    let mut peer = start(one);
    new_session(&mut peer, &scratch.0, json!([]));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(spawn.sessions().len(), 1, "only the session's own");
}

/// A process that waits past the idle time is stopped, and one that exited is not handed out.
#[test]
fn an_idle_process_is_stopped_and_an_exited_one_is_not_handed_out() {
    let scratch = Scratch::new("idle");
    let spawn = Arc::new(Counting::default());
    let idle = agent(
        &spawn,
        &replay_program(),
        &scratch.0,
        warm(1, Duration::from_millis(300)),
    );
    idle.prewarm(&scratch.0);
    until("it starts", || spawn.sessions().len() == 1);
    until("it is stopped for idling", || {
        idle.warm_ready(&scratch.0) == 0
            && spawn
                .processes
                .lock()
                .unwrap()
                .first()
                .is_some_and(|process| process.try_wait().is_ok_and(|exit| exit.is_some()))
    });

    let other = Scratch::new("idle-exited");
    let spawn = Arc::new(Counting::default());
    let policy = warm(1, Duration::from_secs(60));
    let preparing = agent(&spawn, &replay_program(), &other.0, policy.clone());
    preparing.prewarm(&other.0);
    until("a process is warm", || preparing.warm_ready(&other.0) == 1);
    let ended = spawn.sessions()[0].clone();
    spawn.processes.lock().unwrap()[0].kill().unwrap();
    until("it is no longer counted", || {
        preparing.warm_ready(&other.0) == 0
    });
    let mut peer = start(agent(&spawn, &replay_program(), &other.0, policy));
    let session = new_session(&mut peer, &other.0, json!([]));
    assert_ne!(session, ended, "a process that exited is not handed out");
}

/// `--version` is asked once for a version of the program, whichever agent asks.
#[test]
fn the_version_probe_is_shared_by_every_agent_of_the_process() {
    let scratch = Scratch::new("probe");
    let program = scratch
        .0
        .join(format!("claude{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(replay_program(), &program).unwrap();
    let spawn = Arc::new(Counting::default());
    let first = agent(&spawn, &program, &scratch.0, WarmPolicy::default());
    let second = agent(&spawn, &program, &scratch.0, WarmPolicy::default());
    assert_eq!(first.cli_version().as_deref(), Some("2.1.318"));
    assert_eq!(second.cli_version().as_deref(), Some("2.1.318"));
    assert_eq!(*spawn.versions.lock().unwrap(), 1);
}
