//! A scripted stdio ACP agent for the bridge's tests: answers with one chunk echoing the prompt. Not an AI agent.
//! With `ECHO_AGENT_LOG=path` it appends a line to the file for each message, so a test can prove none reached it.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let log = std::env::var_os("ECHO_AGENT_LOG");
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        if let Some(path) = &log
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "message");
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = &message["id"];
        let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let out = match message["method"].as_str() {
            Some("initialize") => reply(json!({"protocolVersion": 1, "agentCapabilities": {}})),
            Some("session/new") => reply(json!({"sessionId": "echo"})),
            Some("session/prompt") => {
                let text = message["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default();
                let update = json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "echo",
                    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}});
                let _ = writeln!(stdout, "{update}");
                reply(json!({"stopReason": "end_turn"}))
            }
            Some("exit") => std::process::exit(7),
            _ if id.is_null() => continue,
            _ => reply(json!({})),
        };
        let _ = writeln!(stdout, "{out}");
        let _ = stdout.flush();
    }
}
