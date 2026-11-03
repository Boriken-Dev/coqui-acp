//! A scripted stdio ACP agent whose turns can outlast a connection, for the tests of `serve --linger`. Not an AI agent. It says its
//! process id in `initialize`'s `_meta`, knows only the sessions it made, and takes what a prompt's text says to do:
//!
//! - `chunks N MS`: one chunk at once, the others MS apart, then the prompt's answer. A chunk's text starts with its number.
//! - `flood N BYTES MS`: one chunk at once, then after MS the others, each BYTES longer, as fast as they go, then the answer.
//! - `exit MS`: one chunk at once, then after MS it exits.
//!
//! It ends when its standard input does, unless started with `--ignore-eof`, which is for what that costs a bridge going away.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

/// How long a bridge has to end an agent that ignores its input closing, before it stops by itself.
const IGNORING_EOF: Duration = Duration::from_secs(90);

type Out = Arc<Mutex<std::io::Stdout>>;

fn say(out: &Out, message: &Value) {
    let mut out = out.lock().unwrap();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn chunk(out: &Out, session: &str, text: &str) {
    say(
        out,
        &json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}}),
    );
}

fn turn(out: &Out, id: &Value, session: &str, script: &str) {
    let words: Vec<&str> = script.split_whitespace().collect();
    let number = |at: usize| {
        words
            .get(at)
            .and_then(|word| word.parse::<u64>().ok())
            .unwrap_or(0)
    };
    match words.first().copied() {
        Some("chunks") => {
            for n in 0..number(1) {
                if n > 0 {
                    thread::sleep(Duration::from_millis(number(2)));
                }
                chunk(out, session, &n.to_string());
            }
        }
        Some("flood") => {
            let padding = "x".repeat(usize::try_from(number(2)).unwrap_or(0));
            chunk(out, session, "0");
            thread::sleep(Duration::from_millis(number(3)));
            for n in 1..number(1) {
                chunk(out, session, &format!("{n} {padding}"));
            }
        }
        Some("exit") => {
            chunk(out, session, "0");
            thread::sleep(Duration::from_millis(number(1)));
            std::process::exit(9);
        }
        _ => {}
    }
    say(
        out,
        &json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}),
    );
}

fn main() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut sessions = BTreeSet::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str() {
            Some("initialize") => Ok(json!({
                "protocolVersion": 1,
                "agentCapabilities": {"sessionCapabilities": {"resume": {}}},
                "_meta": {"pid": std::process::id()},
            })),
            Some("session/new") => {
                let session = format!("{}-{}", std::process::id(), sessions.len());
                sessions.insert(session.clone());
                Ok(json!({"sessionId": session}))
            }
            Some("session/resume") => {
                let session = params["sessionId"].as_str().unwrap_or_default();
                if sessions.contains(session) {
                    Ok(json!({}))
                } else {
                    Err(format!("turn_agent has no session {session}"))
                }
            }
            Some("session/prompt") => {
                let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
                let script = params["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let (out, id) = (Arc::clone(&out), id.clone());
                thread::spawn(move || turn(&out, &id, &session, &script));
                continue;
            }
            _ if id.is_null() => continue,
            _ => Err("turn_agent does not do that".to_owned()),
        };
        say(
            &out,
            &match result {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(error) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32002, "message": error}})
                }
            },
        );
    }
    if std::env::args().any(|arg| arg == "--ignore-eof") {
        thread::sleep(IGNORING_EOF);
    }
}
