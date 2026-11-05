//! A stand-in for Claude Code that replays a recorded stream-json transcript, for tests.
//!
//! `COQUI_ACP_CLAUDE_REPLAY` names a recording without its extension: `<name>.stdout` holds the lines Claude Code
//! wrote, as it wrote them, and `<name>.steps.json` the lines written to it, each with how many output lines came
//! before it. Before each such point this waits for a line on its standard input, and the id of a control request it
//! reads takes the place of the recorded one in the answer. An `initialize` the recording did not have is answered
//! with no models or commands. Once the recording is told it answers control requests until its input ends.
//!
//! `COQUI_ACP_CLAUDE_ARGS` names a file its arguments are written to, as JSON, and `COQUI_ACP_CLAUDE_COPY` a file the
//! file `--mcp-config=` names is copied to. `COQUI_ACP_CLAUDE_STDERR` makes it write that many bytes of diagnostics on
//! one line first, then a line of them per line told, failing if standard error is closed. `COQUI_ACP_CLAUDE_EXIT` makes it write
//! one line of diagnostics after its first input line and exit with that code, and `COQUI_ACP_CLAUDE_END` exit once the
//! recording is told. The recorded session id is written as the one `--session-id=` or `--resume=` names, as Claude Code
//! does, unless `COQUI_ACP_CLAUDE_KEEP_SESSION` is set. `--version` prints the recorded
//! version.

#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::mpsc;

use serde_json::{Value, json};

fn answer(id: &str, response: &Value) -> String {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": response}}).to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("2.1.318 (Claude Code)");
        return;
    }
    if let Some(path) = std::env::var_os("COQUI_ACP_CLAUDE_ARGS") {
        std::fs::write(path, serde_json::to_vec(&args).unwrap()).unwrap();
    }
    if let (Some(copy), Some(config)) = (
        std::env::var_os("COQUI_ACP_CLAUDE_COPY"),
        args.iter()
            .find_map(|arg| arg.strip_prefix("--mcp-config=")),
    ) && !config.starts_with('{')
    {
        std::fs::copy(config, copy).unwrap();
    }
    let (sender, inputs) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { return };
            if let Ok(value) = serde_json::from_str(&line)
                && sender.send(value).is_err()
            {
                return;
            }
        }
    });
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if let Ok(code) = std::env::var("COQUI_ACP_CLAUDE_EXIT") {
        let _ = inputs.recv();
        writeln!(out, "replay-claude: Error: the stored login has expired").unwrap();
        out.flush().unwrap();
        std::process::exit(code.parse().unwrap());
    }
    let base = std::env::var("COQUI_ACP_CLAUDE_REPLAY").unwrap_or_default();
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
    // Claude Code writes the session id it was given: the recorded one becomes the one asked for.
    let recorded_session = lines.iter().find_map(|line| {
        let value: Value = serde_json::from_slice(line).ok()?;
        value.get("session_id")?.as_str().map(str::to_string)
    });
    let asked_session = args.iter().find_map(|arg| {
        arg.strip_prefix("--session-id=")
            .or_else(|| arg.strip_prefix("--resume="))
            .map(str::to_string)
    });
    let lines: Vec<Vec<u8>> = match (recorded_session, asked_session) {
        (Some(recorded), Some(asked))
            if recorded != asked && std::env::var_os("COQUI_ACP_CLAUDE_KEEP_SESSION").is_none() =>
        {
            lines
                .into_iter()
                .map(|line| {
                    String::from_utf8_lossy(&line)
                        .replace(&recorded, &asked)
                        .into_bytes()
                })
                .collect()
        }
        _ => lines,
    };
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut step = 0;
    // The next input of the kind `expected` names; one of another kind is answered here, a control request with
    // success, so a client may ask what the recording did not.
    let next_input =
        |out: &mut std::io::StdoutLock<'_>, expected: Option<&Value>| -> Option<Value> {
            let kind = |value: &Value| {
                (
                    value
                        .get("type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    value
                        .pointer("/request/subtype")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                )
            };
            loop {
                let input = inputs.recv().ok()?;
                if expected.is_some_and(|expected| kind(expected) == kind(&input)) {
                    return Some(input);
                }
                if input.get("type").and_then(Value::as_str) != Some("control_request") {
                    if expected.is_none() {
                        return Some(input);
                    }
                    continue;
                }
                let id = input
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let response = match input.pointer("/request/subtype").and_then(Value::as_str) {
                    Some("initialize") => {
                        json!({"commands": [], "models": [], "claude_code_version": "2.1.318"})
                    }
                    Some("interrupt") => json!({"still_queued": []}),
                    Some("set_model") => json!({"model": input.pointer("/request/model")}),
                    _ => json!({}),
                };
                writeln!(out, "{}", answer(id, &response)).unwrap();
                out.flush().unwrap();
            }
        };
    // A diagnostic line longer than any reader keeps, then one diagnostic line per line told.
    let diagnostics: Option<usize> = std::env::var("COQUI_ACP_CLAUDE_STDERR")
        .ok()
        .and_then(|bytes| bytes.parse().ok());
    if let Some(bytes) = diagnostics {
        let mut stderr = std::io::stderr();
        stderr.write_all(&vec![b'x'; bytes]).unwrap();
        stderr.write_all(b"\n").unwrap();
    }
    for (index, line) in lines.iter().enumerate() {
        if diagnostics.is_some() {
            std::io::stderr()
                .write_all(b"replay-claude: a line told\n")
                .unwrap();
        }
        while step < steps.len() && steps[step]["after"].as_u64() == Some(index as u64) {
            let Some(input) = next_input(&mut out, Some(&steps[step]["input"])) else {
                return;
            };
            if let (Some(recorded), Some(actual)) = (
                steps[step]
                    .pointer("/input/request_id")
                    .and_then(Value::as_str),
                input.get("request_id").and_then(Value::as_str),
            ) {
                ids.insert(recorded.to_string(), actual.to_string());
            }
            step += 1;
        }
        let mut value: Option<Value> = serde_json::from_slice(line).ok();
        let renamed = value.as_mut().and_then(|value| {
            let id = value.pointer("/response/request_id")?.as_str()?;
            let actual = ids.get(id)?.clone();
            value["response"]["request_id"] = json!(actual);
            Some(value.to_string())
        });
        match renamed {
            Some(text) => writeln!(out, "{text}").unwrap(),
            None => out.write_all(line).unwrap(),
        }
        out.flush().unwrap();
    }
    if std::env::var_os("COQUI_ACP_CLAUDE_END").is_some() {
        return;
    }
    while next_input(&mut out, None).is_some() {}
}
