//! A stand-in for `pi --mode rpc` that behaves as the `acp_harness` contract agent does, in Pi's own JSONL dialect, for
//! `acp-harness-check --program-agent` and the provider's tests. It never calls a model.
//!
//! Its lines have the shapes of the recordings in `tests/fixtures/pi-1.5.2` (`get_state`, `get_available_models`,
//! `prompt` and the events of a run, the extension dialog of the permission gate, `abort`). A prompt's text decides the
//! turn: `wait` ends only when the run is aborted; `ask` runs `echo checked > checked.txt` through a `bash` call, which
//! the gate may refuse, and says `echo: allowed` or `echo: refused`; `[[bash:CMD]]`, `[[write:PATH]]`, `[[edit:PATH]]`
//! and `[[read:PATH]]` make one call of that tool; `[[think]]` adds a thinking block; `limit` ends on a 429; `fail` on a
//! 500; `/fake` is a command of an extension, handled with no run; any other text is answered `echo: TEXT`.
//!
//! A gate is emulated when `-e` names a file, as the extension does: every tool call is put to the client as a `select`
//! dialog whose answer decides (`allow`, `plan`, anything else blocks).
//!
//! Sessions are kept in `sessions/<id>.json` of `PI_CODING_AGENT_DIR`, or of `.pi/agent` in the home
//! `ACP_HARNESS_CHECK_HOME` names, one of which it requires so that it never writes in a real home; `--session` of an id
//! with no file there fails as Pi does, and a file exists only after the first prompt.
//!
//! `COQUI_ACP_FAKE_PI_LOG` names a file each start appends to: one line of JSON with the arguments, the names (never the
//! values) of the environment, and whether a gate was loaded. `COQUI_ACP_FAKE_PI_VERSION` replaces the version
//! `--version` prints.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a stand-in for tests names what it uses from std"
)]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const VERSION: &str = "1.5.2";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn emit(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn respond(id: &Value, command: &str, data: Option<Value>) {
    let mut line = json!({"type": "response", "command": command, "success": true});
    if !id.is_null() {
        line["id"] = id.clone();
    }
    if let Some(data) = data {
        line["data"] = data;
    }
    emit(&line);
}

fn refuse(id: &Value, command: &str, why: &str) {
    let mut line = json!({"type": "response", "command": command, "success": false, "error": why});
    if !id.is_null() {
        line["id"] = id.clone();
    }
    emit(&line);
}

fn model_json(id: &str) -> Value {
    let think = id == "stub-think";
    json!({
        "id": id,
        "name": if think { "Stub Think" } else { "Stub Fast" },
        "api": "openai-completions",
        "provider": "stub",
        "baseUrl": "http://127.0.0.1:1/v1",
        "reasoning": think,
        "input": if think { json!(["text", "image"]) } else { json!(["text"]) },
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": if think { 32000 } else { 128_000 },
        "maxTokens": 4096,
    })
}

fn levels(model: &str) -> Vec<&'static str> {
    if model == "stub-think" {
        vec!["off", "minimal", "low", "medium", "high"]
    } else {
        vec!["off"]
    }
}

fn usage(input: u64, output: u64) -> Value {
    json!({"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0, "reasoning": 0,
           "totalTokens": input + output,
           "cost": {"input": 0.000_025, "output": 0.000_012, "cacheRead": 0, "cacheWrite": 0, "total": 0.000_037,
                    "totalFromProvider": false}})
}

struct Fake {
    home: PathBuf,
    cwd: PathBuf,
    id: String,
    model: String,
    level: String,
    gated: bool,
    messages: Vec<Value>,
    /// The id of an `abort`, answered once the run it stopped has settled.
    abort_id: Option<Value>,
    lines: Receiver<Value>,
}

impl Fake {
    fn file(&self) -> PathBuf {
        self.home.join("sessions").join(format!("{}.json", self.id))
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(self.home.join("sessions"));
        let _ = std::fs::write(
            self.file(),
            json!({"id": self.id, "cwd": self.cwd, "messages": self.messages}).to_string(),
        );
    }

    fn state(&self) -> Value {
        json!({
            "model": model_json(&self.model),
            "thinkingLevel": self.level,
            "isStreaming": false,
            "isCompacting": false,
            "steeringMode": "one-at-a-time",
            "followUpMode": "one-at-a-time",
            "sessionFile": self.file(),
            "sessionId": self.id,
            "autoCompactionEnabled": true,
            "messageCount": self.messages.len(),
            "pendingMessageCount": 0,
        })
    }

    /// Answers a command that needs no run.
    fn plain(&mut self, line: &Value) -> bool {
        let id = &line["id"];
        let command = line["type"].as_str().unwrap_or("");
        match command {
            "get_state" => respond(id, command, Some(self.state())),
            "get_available_models" => respond(
                id,
                command,
                Some(json!({"models": [model_json("stub-fast"), model_json("stub-think")]})),
            ),
            "get_available_thinking_levels" => {
                respond(id, command, Some(json!({"levels": levels(&self.model)})));
            }
            "get_commands" => respond(
                id,
                command,
                Some(
                    json!({"commands": [{"name": "fake", "description": "A command of an extension",
                                          "source": "extension"}]}),
                ),
            ),
            "get_messages" => respond(id, command, Some(json!({"messages": self.messages}))),
            "set_model" => {
                let wanted = line["modelId"].as_str().unwrap_or("");
                if line["provider"] == "stub" && (wanted == "stub-fast" || wanted == "stub-think") {
                    self.model = wanted.to_string();
                    if !levels(wanted).contains(&self.level.as_str()) {
                        self.level = if wanted == "stub-think" {
                            "medium"
                        } else {
                            "off"
                        }
                        .to_string();
                    }
                    emit(&json!({"type": "thinking_level_changed", "level": self.level}));
                    respond(id, command, Some(model_json(wanted)));
                } else {
                    refuse(
                        id,
                        command,
                        &format!(
                            "Model not found: {}/{wanted}",
                            line["provider"].as_str().unwrap_or("")
                        ),
                    );
                }
            }
            "set_thinking_level" => {
                let wanted = line["level"].as_str().unwrap_or("");
                // Pi clamps a level the model lacks, and says success.
                if levels(&self.model).contains(&wanted) {
                    self.level = wanted.to_string();
                    emit(&json!({"type": "thinking_level_changed", "level": self.level}));
                }
                respond(id, command, None);
            }
            "abort" | "abort_retry" | "clear_queue" => respond(id, command, None),
            "extension_ui_response" => {}
            _ => return false,
        }
        true
    }

    fn assistant(&self, content: &[Value], stop: &str, error: Option<&str>, tokens: u64) -> Value {
        let mut message = json!({
            "role": "assistant", "content": content, "api": "openai-completions", "provider": "stub",
            "model": self.model, "usage": usage(tokens, 6), "stopReason": stop, "timestamp": now_ms(),
        });
        if let Some(error) = error {
            message["errorMessage"] = json!(error);
            message["usage"] = usage(0, 0);
        }
        message
    }

    fn text_message(&mut self, text: &str, thinking: bool) -> Value {
        let mut content = Vec::new();
        emit(
            &json!({"type": "message_start", "message": {"role": "assistant", "content": [],
                      "api": "openai-completions", "provider": "stub", "model": self.model,
                      "usage": usage(0, 0), "stopReason": "pending", "timestamp": now_ms()}}),
        );
        let mut index = 0;
        let update = |event: Value| {
            emit(
                &json!({"type": "message_update", "usage": usage(25, 6), "assistantMessageEvent": event}),
            );
        };
        if thinking {
            update(json!({"type": "thinking_start", "contentIndex": index}));
            update(
                json!({"type": "thinking_delta", "contentIndex": index, "delta": "Plan first."}),
            );
            update(
                json!({"type": "thinking_end", "contentIndex": index, "content": "Plan first."}),
            );
            content.push(
                json!({"type": "thinking", "thinking": "Plan first.", "thinkingSignature": ""}),
            );
            index += 1;
        }
        update(json!({"type": "text_start", "contentIndex": index}));
        update(json!({"type": "text_delta", "contentIndex": index, "delta": text}));
        update(json!({"type": "text_end", "contentIndex": index, "content": text}));
        content.push(json!({"type": "text", "text": text}));
        let message = self.assistant(&content, "stop", None, 25);
        emit(&json!({"type": "message_end", "message": message}));
        message
    }

    /// The run of one prompt, between `agent_start` and `agent_settled`.
    fn run(&mut self, text: &str, images: usize) {
        emit(&json!({"type": "agent_start"}));
        emit(&json!({"type": "turn_start"}));
        let user = json!({"role": "user", "content": [{"type": "text", "text": text}], "timestamp": now_ms()});
        emit(&json!({"type": "message_start", "message": user}));
        emit(&json!({"type": "message_end", "message": user}));
        self.messages.push(user);
        let _ = images;
        let mut aborted = false;
        if text == "wait" {
            emit(
                &json!({"type": "message_start", "message": {"role": "assistant", "content": [],
                          "model": self.model, "stopReason": "pending", "usage": usage(0, 0), "timestamp": now_ms()}}),
            );
            loop {
                match self.lines.recv() {
                    Ok(line) if line["type"] == "abort" => {
                        self.abort_id = Some(line["id"].clone());
                        aborted = true;
                        break;
                    }
                    Ok(line) => {
                        self.plain(&line);
                    }
                    Err(_) => std::process::exit(0),
                }
            }
            let message = self.assistant(&[], "aborted", Some("Request was aborted"), 0);
            emit(&json!({"type": "message_end", "message": message}));
            self.messages.push(message);
        } else if text == "limit" || text == "fail" {
            let why = if text == "limit" {
                "429: {\"message\":\"usage limit reached\",\"type\":\"rate_limit\"}"
            } else {
                "500: {\"message\":\"the model failed\"}"
            };
            let message = self.assistant(&[], "error", Some(why), 0);
            emit(&json!({"type": "message_start", "message": message}));
            emit(&json!({"type": "message_end", "message": message}));
            self.messages.push(message);
        } else if let Some((tool, args, refusal_text)) = self.tool_of(text) {
            aborted = self.tool_turn(&tool, &args, &refusal_text);
        } else {
            let message = self.text_message(&format!("echo: {text}"), text.contains("[[think]]"));
            self.messages.push(message);
        }
        // Kept before the run settles, since a client may end the process as soon as it hears so.
        self.save();
        emit(&json!({"type": "agent_end", "messages": [], "willRetry": false}));
        emit(&json!({"type": "agent_settled", "aborted": aborted}));
        if let Some(id) = self.abort_id.take() {
            respond(&id, "abort", None);
        }
    }

    /// The tool a prompt calls, its arguments, and what is said if the call is refused.
    fn tool_of(&self, text: &str) -> Option<(String, Value, String)> {
        if text == "ask" {
            return Some((
                "bash".into(),
                json!({"command": "echo checked > checked.txt"}),
                "echo: refused".into(),
            ));
        }
        let marker = text.strip_prefix("[[")?.split_once("]]")?.0;
        let (tool, arg) = marker.split_once(':')?;
        let args = match tool {
            "bash" => json!({"command": arg}),
            "write" => json!({"path": arg, "content": "written by stand-in\nline two\n"}),
            "edit" => json!({"path": arg, "edits": [{"oldText": "alpha", "newText": "ALPHA"}]}),
            "read" => json!({"path": arg}),
            _ => return None,
        };
        Some((tool.to_string(), args, "Tool finished.".into()))
    }

    /// One tool call, asked about when a gate is loaded. Whether the run was aborted.
    fn tool_turn(&mut self, tool: &str, args: &Value, refused: &str) -> bool {
        let call = json!({"type": "toolCall", "id": "call_1", "name": tool, "arguments": args});
        let message = self.assistant(std::slice::from_ref(&call), "toolUse", None, 25);
        emit(
            &json!({"type": "message_start", "message": {"role": "assistant", "content": [],
                      "model": self.model, "stopReason": "pending", "usage": usage(0, 0), "timestamp": now_ms()}}),
        );
        let update = |event: Value| {
            emit(
                &json!({"type": "message_update", "usage": usage(25, 30), "assistantMessageEvent": event}),
            );
        };
        update(
            json!({"type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": tool}),
        );
        update(json!({"type": "toolcall_delta", "contentIndex": 0, "delta": args.to_string()}));
        update(json!({"type": "toolcall_end", "contentIndex": 0, "toolCall": call}));
        emit(&json!({"type": "message_end", "message": message}));
        self.messages.push(message);
        emit(
            &json!({"type": "tool_execution_start", "toolCallId": "call_1", "toolName": tool, "args": args}),
        );
        let mut allowed = true;
        let mut blocked = "Blocked by user".to_string();
        let mut aborted = false;
        if self.gated {
            let dialog = uuid();
            emit(
                &json!({"type": "extension_ui_request", "id": dialog, "method": "select",
                         "title": format!("coqui-acp-pi-gate {}", json!({"id": "call_1", "tool": tool})),
                         "options": ["allow", "deny", "plan"]}),
            );
            loop {
                let Ok(line) = self.lines.recv() else {
                    std::process::exit(0)
                };
                if line["type"] == "extension_ui_response" && line["id"] == json!(dialog) {
                    match line["value"].as_str() {
                        Some("allow") => {}
                        Some("plan") => {
                            allowed = false;
                            "Plan mode: nothing may be changed or run. Say what you would do instead."
                                .clone_into(&mut blocked);
                        }
                        _ => allowed = false,
                    }
                    // Pi goes on with the blocked call; a client that cancelled the dialog aborts right after.
                    if line["cancelled"] == true {
                        aborted = matches!(
                            self.lines.recv_timeout(Duration::from_millis(500)),
                            Ok(next) if next["type"] == "abort"
                        );
                    }
                    break;
                }
                self.plain(&line);
            }
        }
        let (result, is_error) = if allowed {
            self.perform(tool, args)
        } else {
            (
                json!({"content": [{"type": "text", "text": blocked}], "details": {}}),
                true,
            )
        };
        emit(
            &json!({"type": "tool_execution_end", "toolCallId": "call_1", "toolName": tool, "result": result,
                     "isError": is_error, "durationMs": 3}),
        );
        let result_message = json!({"role": "toolResult", "toolCallId": "call_1", "toolName": tool,
            "content": result["content"], "details": result["details"], "isError": is_error, "timestamp": now_ms()});
        emit(&json!({"type": "message_start", "message": result_message}));
        emit(&json!({"type": "message_end", "message": result_message}));
        self.messages.push(result_message);
        emit(&json!({"type": "turn_end", "message": {}, "toolResults": []}));
        if aborted {
            let message = self.assistant(&[], "aborted", Some("Request was aborted"), 0);
            emit(&json!({"type": "message_start", "message": message}));
            emit(&json!({"type": "message_end", "message": message}));
            self.messages.push(message);
            return true;
        }
        emit(&json!({"type": "turn_start"}));
        let said = if refused.starts_with("echo:") {
            if allowed { "echo: allowed" } else { refused }.to_string()
        } else {
            "Tool finished.".to_string()
        };
        let message = self.text_message(&said, false);
        self.messages.push(message);
        false
    }

    /// What the tool does, as Pi's results are shaped.
    fn perform(&self, tool: &str, args: &Value) -> (Value, bool) {
        let path = self.cwd.join(args["path"].as_str().unwrap_or(""));
        match tool {
            "bash" => {
                let command = args["command"].as_str().unwrap_or("");
                let output = if cfg!(windows) {
                    std::process::Command::new("cmd")
                        .args(["/C", command])
                        .current_dir(&self.cwd)
                        .output()
                } else {
                    std::process::Command::new("sh")
                        .args(["-c", command])
                        .current_dir(&self.cwd)
                        .output()
                };
                let (text, code) = output.map_or_else(
                    |error| (error.to_string(), 127),
                    |out| {
                        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                        text.push_str(&String::from_utf8_lossy(&out.stderr));
                        (text, out.status.code().unwrap_or(1))
                    },
                );
                emit(
                    &json!({"type": "tool_execution_update", "toolCallId": "call_1", "toolName": "bash", "args": args,
                             "partialResult": {"content": [{"type": "text", "text": text}], "details": {}}}),
                );
                let shown = if code == 0 {
                    text.clone()
                } else {
                    format!("{text}\n\nCommand exited with code {code}")
                };
                (
                    json!({"content": [{"type": "text", "text": shown}],
                           "structuredContent": {"output": text, "truncated": false, "exit_code": code,
                                                 "wall_time_seconds": 0.1}}),
                    code != 0,
                )
            }
            "write" => {
                let content = args["content"].as_str().unwrap_or("");
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, content);
                (
                    json!({"content": [{"type": "text", "text": format!("Successfully wrote to {}", args["path"].as_str().unwrap_or(""))}],
                           "details": {}}),
                    false,
                )
            }
            "edit" => {
                let name = args["path"].as_str().unwrap_or("");
                let Ok(old) = std::fs::read_to_string(&path) else {
                    return (
                        json!({"content": [{"type": "text", "text": format!("ENOENT: no such file or directory, access '{name}'")}], "details": {}}),
                        true,
                    );
                };
                if !old.contains("alpha") {
                    return (
                        json!({"content": [{"type": "text", "text": format!("Could not find the exact text in {name}. The old text must match exactly including all whitespace and newlines.")}], "details": {}}),
                        true,
                    );
                }
                let new = old.replacen("alpha", "ALPHA", 1);
                let _ = std::fs::write(&path, &new);
                let first = new.lines().next().unwrap_or("");
                let before = old.lines().next().unwrap_or("");
                (
                    json!({"content": [{"type": "text", "text": format!("Successfully replaced 1 block(s) in {name}.")}],
                           "details": {"diff": format!("-1 {before}\n+1 {first}"),
                                       "patch": format!("--- {name}\n+++ {name}\n@@ -1,1 +1,1 @@\n-{before}\n+{first}\n"),
                                       "firstChangedLine": 1}}),
                    false,
                )
            }
            "read" => match std::fs::read_to_string(&path) {
                Ok(text) => (
                    json!({"content": [{"type": "text", "text": text}], "details": {}}),
                    false,
                ),
                Err(_) => (
                    json!({"content": [{"type": "text", "text": format!("ENOENT: no such file or directory, access '{}'", path.display())}], "details": {}}),
                    true,
                ),
            },
            _ => (
                json!({"content": [{"type": "text", "text": "unknown tool"}], "details": {}}),
                true,
            ),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-v") {
        println!(
            "{}",
            std::env::var("COQUI_ACP_FAKE_PI_VERSION").unwrap_or_else(|_| VERSION.to_string())
        );
        return;
    }
    let value_of = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    // Never in a real home: in the directory it is told, or in the home the contract suite gave it.
    let home = std::env::var_os("PI_CODING_AGENT_DIR").or_else(|| {
        std::env::var_os("ACP_HARNESS_CHECK_HOME").and_then(|_| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|home| {
                PathBuf::from(home)
                    .join(".pi")
                    .join("agent")
                    .into_os_string()
            })
        })
    });
    let Some(home) = home else {
        eprintln!(
            "fake_pi: set PI_CODING_AGENT_DIR, or run where the contract suite gave a home, so that it never writes in a real home"
        );
        std::process::exit(2);
    };
    let home = PathBuf::from(home);
    let _ = std::fs::create_dir_all(&home);
    let gate = value_of("-e").map(PathBuf::from);
    let mut logs: Vec<PathBuf> = vec![home.join("fake-pi.log")];
    if let Some(path) = std::env::var_os("COQUI_ACP_FAKE_PI_LOG") {
        logs.push(path.into());
    }
    let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    let line = json!({"args": args, "env_names": names, "gate": gate.as_ref().and_then(|g| std::fs::read_to_string(g).ok()),
                      "system_prompt": value_of("--append-system-prompt").and_then(|p| std::fs::read_to_string(p).ok()),
                      "cwd": std::env::current_dir().ok()});
    for path in logs {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut model = "stub-think".to_string();
    if let Some(wanted) = value_of("--model") {
        let bare = wanted.strip_prefix("stub/").unwrap_or(&wanted);
        let bare = bare.split(':').next().unwrap_or(bare);
        if bare != "stub-fast" && bare != "stub-think" {
            eprintln!(
                "Error: Model \"{wanted}\" not found. Use --list-models to see available models."
            );
            std::process::exit(1);
        }
        model = bare.to_string();
    }
    let mut level = if model == "stub-think" {
        "medium"
    } else {
        "off"
    }
    .to_string();
    if let Some(wanted) = value_of("--thinking")
        && levels(&model).contains(&wanted.as_str())
    {
        level = wanted;
    }
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && sender.send(value).is_err()
            {
                return;
            }
        }
    });
    let mut fake = Fake {
        home,
        cwd,
        id: uuid(),
        model,
        level,
        gated: gate.is_some(),
        messages: Vec::new(),
        abort_id: None,
        lines,
    };
    if let Some(wanted) = value_of("--session") {
        let saved =
            std::fs::read_to_string(fake.home.join("sessions").join(format!("{wanted}.json")))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let Some(saved) = saved else {
            eprintln!("No session found matching '{wanted}'");
            std::process::exit(1);
        };
        fake.id = wanted;
        fake.messages = saved["messages"].as_array().cloned().unwrap_or_default();
    }
    loop {
        let line = match fake.lines.recv_timeout(Duration::from_secs(3600)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if fake.plain(&line) {
            continue;
        }
        let id = line["id"].clone();
        match line["type"].as_str() {
            Some("prompt") => {
                let text = line["message"].as_str().unwrap_or("").to_string();
                let images = line["images"].as_array().map_or(0, Vec::len);
                if let Some(path) = std::env::var_os("COQUI_ACP_FAKE_PI_LOG")
                    && let Ok(mut file) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                {
                    let _ = writeln!(file, "{}", json!({"prompt": text, "images": images}));
                }
                if text.starts_with("/fake") {
                    respond(&id, "prompt", Some(json!({"disposition": "handled"})));
                    continue;
                }
                respond(&id, "prompt", Some(json!({"disposition": "started"})));
                fake.run(&text, images);
            }
            Some(other) => refuse(&id, other, &format!("Unknown command: {other}")),
            None => {}
        }
    }
}
