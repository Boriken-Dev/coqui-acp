//! What the tests share: the recordings, the stand-in, and a JSON-RPC client by hand.

#![allow(dead_code, reason = "each test binary uses part of this")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_pi::harness::event::Event;
use coqui_acp_pi::{Parser, PiLaunch};
use serde_json::{Value, json};

/// The recordings of Pi 1.5.2.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pi-1.5.2")
}

/// The lines Pi wrote in recording `name`.
pub fn recorded(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures().join(format!("{name}.stdout"))).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The directory relative paths are against in the parser tests.
pub fn work() -> PathBuf {
    std::env::temp_dir().join("coqui-acp-pi-fixture-work")
}

/// A parser whose writes are kept.
pub fn parser() -> (Parser, Arc<Mutex<Vec<Value>>>) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    )
    .with_roots(&work(), Vec::new());
    (parser, written)
}

/// Every event recording `name` becomes with a run begun at the start and after each `agent_settled`, and what the parser
/// wrote back.
pub fn parsed(name: &str) -> (Vec<Event>, Arc<Mutex<Vec<Value>>>) {
    let (mut parser, written) = parser();
    let mut events = Vec::new();
    parser.prompting();
    for line in recorded(name) {
        events.extend(parser.line(&line));
        if line["type"] == "agent_settled" {
            parser.prompting();
        }
    }
    (events, written)
}

/// The stand-in, built beside the tests.
pub fn fake_program() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    let profile = deps.parent().unwrap().parent().unwrap();
    profile
        .join("examples")
        .join(format!("fake_pi{}", std::env::consts::EXE_SUFFIX))
}

/// A launch of the stand-in in `cwd`, with its sessions and its log in `home`.
pub fn launching(cwd: &Path, home: &Path) -> PiLaunch {
    let mut launch = PiLaunch::new(cwd);
    launch.executable = fake_program().into();
    launch.env.insert("PI_CODING_AGENT_DIR".into(), home.into());
    launch
}

/// What the stand-in logged for each start: its arguments, environment names, gate and system prompt.
pub fn logged(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("fake-pi.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A fresh directory under the system's temporary one, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "coqui-acp-pi-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// A directory inside, made.
    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Events received from a session, waited for.
pub struct Events {
    pub received: Receiver<Event>,
    pub seen: Vec<Event>,
}

impl Events {
    /// A sink for `PiSession::start` and the events it will collect.
    pub fn new() -> (impl Fn(Event) -> bool + Send + Sync + 'static, Self) {
        let (sender, received) = mpsc::channel();
        let sender = Mutex::new(sender);
        (
            move |event| sender.lock().unwrap().send(event).is_ok(),
            Self {
                received,
                seen: Vec::new(),
            },
        )
    }

    /// Takes events until one satisfies `done`, which is the last taken.
    pub fn until(&mut self, done: impl Fn(&Event) -> bool) {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let event = self
                .received
                .recv_timeout(left)
                .expect("the event did not come");
            let finished = done(&event);
            self.seen.push(event);
            if finished {
                return;
            }
        }
    }

    /// Takes events up to and including the turn's end.
    pub fn turn(&mut self) {
        self.until(|event| matches!(event, Event::TurnEnd(_)));
    }

    /// Everything already received, without waiting.
    pub fn drain(&mut self) {
        while let Ok(event) = self.received.try_recv() {
            self.seen.push(event);
        }
    }

    /// The text of the agent's message chunks so far.
    pub fn said(&self) -> String {
        self.seen
            .iter()
            .filter_map(|event| match event {
                Event::MessageChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A client written by hand, for version 2 or 1.
pub struct Peer {
    to_agent: Box<dyn Write + Send>,
    from_agent: Receiver<Value>,
    next: u64,
    pub updates: Vec<Value>,
    /// The answers to permission requests, in order: an option kind to choose, or `cancel`.
    pub answers: VecDeque<String>,
    pub asked: Vec<Value>,
}

impl Peer {
    pub fn serve(agent: coqui_acp_pi::PiAgent, settings: coqui_acp_pi::harness::Settings) -> Self {
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let _ =
                coqui_acp_pi::harness::serve(Arc::new(agent), settings, agent_reads, agent_writes);
        });
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(client_reads).lines() {
                let Ok(line) = line else { return };
                if sender.send(serde_json::from_str(&line).unwrap()).is_err() {
                    return;
                }
            }
        });
        Self {
            to_agent: Box::new(client_writes),
            from_agent: receiver,
            next: 0,
            updates: Vec::new(),
            answers: VecDeque::new(),
            asked: Vec::new(),
        }
    }

    pub fn send(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.to_agent.write_all(&line).unwrap();
        self.to_agent.flush().unwrap();
    }

    fn take(&mut self, message: Value) -> Option<Value> {
        match message.get("method").and_then(Value::as_str) {
            Some("session/update") => {
                self.updates.push(message["params"]["update"].clone());
                None
            }
            Some("session/request_permission") => {
                self.asked.push(message["params"].clone());
                let wanted = self.answers.pop_front().unwrap_or_else(|| "cancel".into());
                let chosen = message["params"]["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|option| {
                        option["kind"] == wanted.as_str() || option["optionId"] == wanted.as_str()
                    })
                    .map(|option| option["optionId"].clone());
                let outcome = match chosen {
                    Some(id) => json!({"outcome": "selected", "optionId": id}),
                    None => json!({"outcome": "cancelled"}),
                };
                self.send(
                    json!({"jsonrpc": "2.0", "id": message["id"], "result": {"outcome": outcome}}),
                );
                None
            }
            _ => Some(message),
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let message = self
                .from_agent
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            if let Some(message) = self.take(message)
                && message["id"] == id
            {
                return message;
            }
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Takes messages until an update from the `from`th on satisfies `done`.
    pub fn until(&mut self, from: usize, done: impl Fn(&Value) -> bool) {
        if self.updates.iter().skip(from).any(&done) {
            return;
        }
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let message = self
                .from_agent
                .recv_timeout(left)
                .expect("the update did not come");
            self.take(message);
            if self.updates.last().is_some_and(&done) {
                return;
            }
        }
    }

    pub fn kinds(&self) -> Vec<String> {
        self.updates
            .iter()
            .map(|update| update["sessionUpdate"].as_str().unwrap_or("").to_string())
            .collect()
    }
}

/// An npm-shaped install of Pi on Windows: the shim `pi.cmd` with its script, and in a `bin` directory a `node.exe` (the
/// stand-in, which reads Pi's arguments and ignores the script's). The directories of the shim and of `bin`.
#[cfg(windows)]
pub fn npm_install(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let npm = scratch.dir("npm");
    let package = npm.join("node_modules").join("pkg").join("dist");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("cli.js"), "// the script\n").unwrap();
    std::fs::write(
        npm.join("pi.cmd"),
        "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\n\
         IF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n)\r\n\r\n\
         endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\pkg\\dist\\cli.js\" %*\r\n",
    )
    .unwrap();
    let bin = scratch.dir("bin");
    std::fs::copy(fake_program(), bin.join("node.exe")).unwrap();
    (npm, bin)
}

/// A `PATH` of these directories, then the system's.
#[cfg(windows)]
pub fn path_with(dirs: &[&Path]) -> std::ffi::OsString {
    let mut all: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
    all.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(all).unwrap()
}
