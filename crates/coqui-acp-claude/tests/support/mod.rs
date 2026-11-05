//! What the tests share: the recordings, the replaying stand-in, and a JSON-RPC client by hand.

#![allow(dead_code, reason = "each test binary uses part of this")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coqui_acp_claude::harness::event::Event;
use coqui_acp_claude::{ClaudeLaunch, Parser};
use serde_json::{Value, json};

/// The recordings of Claude Code 2.1.318.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("2.1.318")
}

/// The lines Claude Code wrote in recording `name`.
pub fn recorded(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixtures().join(format!("{name}.stdout"))).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Every event recording `name` becomes, and what the parser wrote back to Claude Code.
pub fn parsed(name: &str) -> (Vec<Event>, Arc<Mutex<Vec<Value>>>) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let mut parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    );
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
        .join(format!("replay_claude{}", std::env::consts::EXE_SUFFIX))
}

/// A launch of the stand-in replaying `recording` in a fresh directory, with `session` as the session id.
pub fn replaying(recording: Option<&str>, cwd: &Path) -> ClaudeLaunch {
    let mut launch = ClaudeLaunch::new(cwd);
    launch.executable = replay_program().into();
    if let Some(recording) = recording {
        launch.env.insert(
            "COQUI_ACP_CLAUDE_REPLAY".into(),
            fixtures().join(recording).into(),
        );
    }
    launch
}

/// The directory under `projects` where Claude Code keeps the sessions of `cwd`: every character but an ASCII letter or
/// digit as `-`, as the recorded `C:\coqui-fx\w` became `C--coqui-fx-w`.
pub fn project_directory(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A fresh directory under the system's temporary one, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("coqui-acp-claude-{name}-{}", uuid_like()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

/// A client written by hand, for version 2 and for what the version 1 client does not show.
pub struct Peer {
    to_agent: Box<dyn Write + Send>,
    from_agent: Arc<Mutex<std::sync::mpsc::Receiver<Value>>>,
    next: u64,
    /// Every `session/update` seen, in order.
    pub updates: Vec<Value>,
    /// The answers to permission requests, in order: an option kind to choose, or `cancel`.
    pub answers: VecDeque<String>,
    /// Every permission request seen.
    pub asked: Vec<Value>,
}

impl Peer {
    /// Serves `agent` on a thread, connected to this peer by pipes.
    pub fn serve(
        agent: coqui_acp_claude::ClaudeAgent,
        settings: coqui_acp_claude::harness::Settings,
    ) -> Self {
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let _ = coqui_acp_claude::harness::serve(
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
            from_agent: Arc::new(Mutex::new(receiver)),
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
                    .find(|option| option["kind"] == wanted.as_str())
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

    fn next_message(&mut self, until: Instant) -> Option<Value> {
        let left = until.saturating_duration_since(Instant::now());
        let message = self.from_agent.lock().unwrap().recv_timeout(left).ok()?;
        Some(message)
    }

    /// Sends a request and waits for its answer, taking updates and answering permission requests meanwhile.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let message = self
                .next_message(until)
                .unwrap_or_else(|| panic!("no answer to {method}"));
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

    /// Takes messages until an update from the `from`th on satisfies `done`, and answers that update.
    pub fn until(&mut self, from: usize, done: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(30);
        if let Some(found) = self.updates[from.min(self.updates.len())..]
            .iter()
            .find(|update| done(update))
        {
            return found.clone();
        }
        loop {
            let message = self.next_message(until).expect("the update did not come");
            self.take(message);
            if let Some(found) = self.updates.last().filter(|update| done(update)) {
                return found.clone();
            }
        }
    }

    /// The kinds of the updates seen.
    pub fn kinds(&self) -> Vec<String> {
        self.updates
            .iter()
            .map(|update| update["sessionUpdate"].as_str().unwrap_or("").to_string())
            .collect()
    }
}
