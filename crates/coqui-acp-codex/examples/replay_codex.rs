//! A stand-in for `codex app-server` that replays a recorded session, for tests.
//!
//! `COQUI_ACP_CODEX_REPLAY` names a recording without its extension: `<name>.stdout` holds the lines the app server
//! wrote, as it wrote them, and `<name>.steps.json` the lines written to it, each with how many output lines came
//! before it. Before each such point this waits for a line of the same kind (the same method, or an answer), and the
//! id of a request it reads takes the place of the recorded one in the recorded answer. A request the recording did
//! not have is answered here: `model/list` with no models, anything else with `{}`. A recorded `thread/list`, which
//! a client of this crate never sends, is not waited for, and its answer is left out.
//!
//! `COQUI_ACP_CODEX_REFUSE` names a method such a request of is refused, and `COQUI_ACP_CODEX_THREAD` the thread an
//! unrecorded `thread/start` opens.
//!
//! `COQUI_ACP_CODEX_RATE_LIMITS` names a file holding the JSON an unrecorded `account/rateLimits/read` answers with, read
//! afresh at each request so a test may change it between reads.
//!
//! `COQUI_ACP_CODEX_THREAD_STATE` is the approval policy, sandbox and reviewer, comma-separated, a recorded `thread/start`
//! answers with in place of the recorded ones.
//!
//! `COQUI_ACP_CODEX_ARGS` names a file its arguments are written to, as JSON, and `COQUI_ACP_CODEX_INPUTS` one each
//! line it reads is added to. `COQUI_ACP_CODEX_EXIT` makes it write
//! one line of diagnostics after its first input line and exit with that code, and `COQUI_ACP_CODEX_END` exit once
//! the recording is told. `--version` prints the recorded version.

#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::mpsc;

use serde_json::{Value, json};

const SKIPPED: [&str; 1] = ["thread/list"];

fn kind(value: &Value) -> String {
    value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("answer")
        .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("codex-cli 0.164.0");
        return;
    }
    if let Some(path) = std::env::var_os("COQUI_ACP_CODEX_ARGS") {
        std::fs::write(path, serde_json::to_vec(&args).unwrap()).unwrap();
    }
    let (sender, inputs) = mpsc::channel::<Value>();
    let kept = std::env::var_os("COQUI_ACP_CODEX_INPUTS");
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { return };
            if let Some(path) = &kept {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap();
                writeln!(file, "{line}").unwrap();
            }
            if let Ok(value) = serde_json::from_str(&line)
                && sender.send(value).is_err()
            {
                return;
            }
        }
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if let Ok(code) = std::env::var("COQUI_ACP_CODEX_EXIT") {
        let _ = inputs.recv();
        writeln!(out, "replay-codex: Error: failed to load configuration").unwrap();
        out.flush().unwrap();
        std::process::exit(code.parse().unwrap());
    }
    let base = std::env::var("COQUI_ACP_CODEX_REPLAY").unwrap_or_default();
    let (lines, steps) = if base.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let bytes = std::fs::read(format!("{base}.stdout")).unwrap();
        let lines: Vec<Vec<u8>> = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect();
        let steps: Vec<Value> =
            serde_json::from_slice(&std::fs::read(format!("{base}.steps.json")).unwrap()).unwrap();
        (lines, steps)
    };
    let mut ids: HashMap<String, Value> = HashMap::new();
    let mut skipped: HashSet<String> = HashSet::new();
    let mut step = 0;
    // The next input of the kind `expected` names; a request of another kind is answered here.
    let next_input = |out: &mut std::io::StdoutLock<'_>,
                      expected: Option<&Value>|
     -> Option<Value> {
        loop {
            let input = inputs.recv().ok()?;
            if expected.is_some_and(|expected| kind(expected) == kind(&input)) {
                return Some(input);
            }
            let Some(id) = input.get("id").filter(|_| input.get("method").is_some()) else {
                if expected.is_none() {
                    return Some(input);
                }
                continue;
            };
            let method = input.get("method").and_then(Value::as_str);
            if method.is_some() && std::env::var("COQUI_ACP_CODEX_REFUSE").ok().as_deref() == method
            {
                writeln!(
                    out,
                    "{}",
                    json!({"id": id, "error": {"code": -32601, "message": "not here"}})
                )
                .unwrap();
                out.flush().unwrap();
                continue;
            }
            let result = match method {
                Some("model/list") => json!({"data": [], "nextCursor": null}),
                Some("config/read") => json!({"config": {"model": "from-config"}, "origins": {}}),
                Some("account/rateLimits/read")
                    if std::env::var_os("COQUI_ACP_CODEX_RATE_LIMITS").is_some() =>
                {
                    let path = std::env::var_os("COQUI_ACP_CODEX_RATE_LIMITS").unwrap();
                    std::fs::read(path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                        .unwrap_or_else(|| json!({}))
                }
                Some("thread/start") if std::env::var_os("COQUI_ACP_CODEX_THREAD").is_some() => {
                    json!({"thread": {"id": std::env::var("COQUI_ACP_CODEX_THREAD").unwrap()}})
                }
                _ => json!({}),
            };
            writeln!(out, "{}", json!({"id": id, "result": result})).unwrap();
            out.flush().unwrap();
        }
    };
    for (index, line) in lines.iter().enumerate() {
        while step < steps.len() && steps[step]["after"].as_u64() == Some(index as u64) {
            let expected = &steps[step]["input"];
            if SKIPPED.contains(&kind(expected).as_str()) {
                if let Some(id) = expected.get("id") {
                    skipped.insert(id.to_string());
                }
                step += 1;
                continue;
            }
            let Some(input) = next_input(&mut out, Some(expected)) else {
                return;
            };
            if let (Some(recorded), Some(actual)) = (expected.get("id"), input.get("id"))
                && expected.get("method").is_some()
            {
                ids.insert(recorded.to_string(), actual.clone());
            }
            step += 1;
        }
        let mut value: Option<Value> = serde_json::from_slice(line).ok();
        let answer_id = value
            .as_ref()
            .filter(|value| value.get("method").is_none())
            .and_then(|value| value.get("id"))
            .map(Value::to_string);
        if let Some(id) = &answer_id {
            if skipped.contains(id) {
                continue;
            }
            if let (Some(actual), Some(value)) = (ids.get(id), value.as_mut()) {
                value["id"] = actual.clone();
                // The state the thread opens in, as `approval,sandbox,reviewer`: the recording's own is `untrusted`,
                // `readOnly`, `user`.
                if let Ok(state) = std::env::var("COQUI_ACP_CODEX_THREAD_STATE")
                    && value.pointer("/result/thread").is_some()
                    && value.pointer("/result/approvalPolicy").is_some()
                {
                    let parts: Vec<&str> = state.split(',').collect();
                    value["result"]["approvalPolicy"] = json!(parts[0]);
                    value["result"]["sandbox"] = json!({"type": parts[1]});
                    value["result"]["approvalsReviewer"] = json!(parts[2]);
                }
                writeln!(out, "{value}").unwrap();
                out.flush().unwrap();
                continue;
            }
        }
        out.write_all(line).unwrap();
        out.flush().unwrap();
    }
    if std::env::var_os("COQUI_ACP_CODEX_END").is_some() {
        return;
    }
    while next_input(&mut out, None).is_some() {}
}
