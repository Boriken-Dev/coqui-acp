//! What the tests share: the recordings, the replaying stand-in, and a JSON-RPC client by hand.

#![allow(dead_code, reason = "each test binary uses part of this")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_codex::harness::event::Event;
use coqui_acp_codex::{CodexLaunch, Parser};
use serde_json::{Value, json};

/// The recordings of Codex 0.164.0.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codex-0.164.0")
}

/// The lines the app server wrote in recording `name`.
pub fn recorded(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures().join(format!("{name}.stdout"))).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The thread recording `name` opened.
pub fn recorded_thread(name: &str) -> String {
    recorded(name)
        .iter()
        .find_map(|line| line.pointer("/result/thread/id")?.as_str().map(str::to_string))
        .unwrap()
}

/// A parser whose answers are kept.
pub fn parser() -> (Parser, Arc<Mutex<Vec<Value>>>) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    );
    (parser, written)
}

/// Every event recording `name` becomes, and what the parser wrote back.
pub fn parsed(name: &str) -> (Vec<Event>, Arc<Mutex<Vec<Value>>>) {
    let (mut parser, written) = parser();
    let events = recorded(name)
        .iter()
        .flat_map(|value| parser.line(value))
        .collect();
    (events, written)
}

/// The replaying stand-in, built beside the tests.
pub fn replay_program() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    let profile = deps.parent().unwrap().parent().unwrap();
    profile
        .join("examples")
        .join(format!("replay_codex{}", std::env::consts::EXE_SUFFIX))
}

/// A launch of the stand-in replaying `recording` in `cwd`.
pub fn replaying(recording: Option<&str>, cwd: &Path) -> CodexLaunch {
    let mut launch = CodexLaunch::new(cwd);
    launch.executable = replay_program().into();
    if let Some(recording) = recording {
        launch.env.insert(
            "COQUI_ACP_CODEX_REPLAY".into(),
            fixtures().join(recording).into(),
        );
    }
    launch
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
            "coqui-acp-codex-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A client written by hand, for version 2.
pub struct Peer {
    to_agent: Box<dyn Write + Send>,
    from_agent: std::sync::mpsc::Receiver<Value>,
    next: u64,
    pub updates: Vec<Value>,
    /// The answers to permission requests, in order: an option kind to choose, or `cancel`.
    pub answers: VecDeque<String>,
    pub asked: Vec<Value>,
}

impl Peer {
    pub fn serve(
        agent: coqui_acp_codex::CodexAgent,
        settings: coqui_acp_codex::harness::Settings,
    ) -> Self {
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let _ = coqui_acp_codex::harness::serve(
                Arc::new(agent),
                settings,
                agent_reads,
                agent_writes,
            );
        });
        let (sender, receiver) = std::sync::mpsc::channel();
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
