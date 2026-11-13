//! A stand-in for Claude Code that behaves as the `acp_harness` contract agent does, in Claude Code's own stream-json
//! dialect, for `acp-harness-check --program-agent` and the provider's tests. It never calls a model.
//!
//! Its lines have the shapes of the recordings in `tests/fixtures/2.1.318` (`init`, `assistant`, `result`,
//! `rate_limit_event`, `control_request` `can_use_tool`, the control answers). A prompt's text decides the turn:
//! `wait` ends only when it is interrupted; `die` exits with status 3; `ask` asks to use a tool, and says `echo: allowed`, `echo: refused` or, when
//! the answer asks to interrupt, ends cancelled; `quota P` reports the five-hour window used P of 1000 beside its
//! answer; `limit` hits the five-hour limit; any other text is answered `echo: TEXT`.
//!
//! Sessions are kept as Claude Code keeps them, in `projects/<directory>/<id>.jsonl` of `CLAUDE_CONFIG_DIR` or of
//! `.claude` in the home `ACP_HARNESS_CHECK_HOME` names, one of which it requires so that it never writes in a real home: `--resume` of an id with no file there fails as Claude Code does.
//!
//! `COQUI_ACP_FAKE_CLAUDE_LOG` names a file each start appends to: one line of JSON with the arguments, the names (never
//! the values) of the environment, and the two prompt-cache variables' values. `--version` prints the recorded version.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a stand-in for tests names what it uses from std"
)]

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const VERSION: &str = "2.1.318";

/// The version it reports: `COQUI_ACP_FAKE_CLAUDE_VERSION` when set, so a test can run a version that was not tested.
fn version() -> String {
    std::env::var("COQUI_ACP_FAKE_CLAUDE_VERSION").unwrap_or_else(|_| VERSION.to_owned())
}
const MODEL: &str = "claude-standin";
const TTL_VARIABLES: [&str; 2] = [
    "CLAUDE_CODE_PROMPT_CACHE_TTL",
    "CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL",
];

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn emit(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// What a session keeps between turns.
struct Fake {
    session: String,
    cwd: PathBuf,
    transcript: PathBuf,
    mode: String,
    started: bool,
    turns: u64,
}

impl Fake {
    fn answer(&self, id: &str, response: &Value) {
        emit(
            &json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": response}}),
        );
    }

    fn init(&self) -> Value {
        json!({
            "commands": [],
            "agents": [],
            "models": [{
                "value": "default",
                "resolvedModel": MODEL,
                "displayName": "Default",
                "description": "A stand-in model",
                "supportsEffort": true,
                "supportedEffortLevels": ["low", "medium", "high"],
            }],
            "account": {"tokenSource": "none", "apiKeySource": "none", "apiProvider": "firstParty"},
            "claude_code_version": version(),
            "current_permission_mode": self.mode,
            "session_state": "idle",
        })
    }

    fn keep(&self, line: &Value) {
        if let Some(dir) = self.transcript.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn kept(&self, role: &str, content: Value) {
        self.keep(&json!({
            "parentUuid": null,
            "isSidechain": false,
            "type": role,
            "message": {"role": role, "content": content},
            "uuid": uuid(),
            "timestamp": "2026-11-10T00:00:00.000Z",
            "cwd": self.cwd.to_string_lossy(),
            "sessionId": self.session,
            "version": version(),
        }));
    }

    fn line_extra(&self) -> Value {
        json!({"session_id": self.session, "parent_tool_use_id": null, "uuid": uuid()})
    }

    fn assistant(&self, content: Value) -> Value {
        let mut line = json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_fake_{}", self.turns),
                "type": "message",
                "role": "assistant",
                "model": MODEL,
                "content": content,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 11, "output_tokens": 7, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            },
        });
        merge(&mut line, &self.line_extra());
        line
    }

    fn result(&self, text: &str, error: Option<(&str, &str)>) -> Value {
        let turns = self.turns;
        let (is_error, terminal) = match error {
            Some((_, terminal)) => (true, terminal),
            None => (false, "completed"),
        };
        let mut line = json!({
            "type": "result",
            "subtype": error.map_or("success", |(subtype, _)| subtype),
            "is_error": is_error,
            "result": text,
            "stop_reason": if is_error { Value::Null } else { json!("end_turn") },
            "terminal_reason": terminal,
            "total_cost_usd": 0.0000046 * turns as f64,
            "usage": {"input_tokens": 11, "output_tokens": 7, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            "modelUsage": {MODEL: {
                "inputTokens": 11 * turns,
                "outputTokens": 7 * turns,
                "cacheReadInputTokens": 0,
                "cacheCreationInputTokens": 0,
                "costUSD": 0.0000046 * turns as f64,
                "contextWindow": 200_000,
                "thinkingTokens": 0,
            }},
            "permission_denials": [],
            "num_turns": turns,
        });
        merge(&mut line, &self.line_extra());
        line
    }

    fn rate_limit(&self, utilization: f64, status: &str) -> Value {
        let resets = now_secs() + 3600;
        let mut line = json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": status,
                "resetsAt": resets,
                "rateLimitType": "five_hour",
                "isUsingOverage": false,
                "unifiedWindows": {"five_hour": {"utilization": utilization, "resetsAt": resets, "status": status}},
            },
        });
        merge(&mut line, &self.line_extra());
        line
    }

    /// A turn that ends with `text`, and what it keeps.
    fn say(&self, text: &str) {
        emit(&self.assistant(json!([{"type": "text", "text": text}])));
        self.kept("assistant", json!([{"type": "text", "text": text}]));
        emit(&self.result(text, None));
    }
}

fn merge(into: &mut Value, extra: &Value) {
    if let (Some(into), Some(extra)) = (into.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            into.insert(key.clone(), value.clone());
        }
    }
}

fn project_directory(cwd: &Path) -> String {
    coqui_acp_claude::project_directory(cwd)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("{} (Claude Code)", version());
        return;
    }
    let mut logs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("COQUI_ACP_FAKE_CLAUDE_LOG") {
        logs.push(path.into());
    }
    // Beside the configuration directory too, so a call that sets no variable of its own can still be read back.
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        logs.push(Path::new(&dir).join("fake-claude.log"));
    }
    if !logs.is_empty() {
        let ttl: serde_json::Map<String, Value> = TTL_VARIABLES
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|v| ((*name).to_string(), json!(v)))
            })
            .collect();
        let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
        let line = json!({"args": args, "env_names": names, "ttl": ttl, "config": std::env::var("CLAUDE_CONFIG_DIR").ok()});
        for path in logs {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }
    // Never in a real home: in the directory it is told, or in the home the contract suite gave it.
    let config = std::env::var_os("CLAUDE_CONFIG_DIR").or_else(|| {
        std::env::var_os("ACP_HARNESS_CHECK_HOME").and_then(|_| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| Path::new(&home).join(".claude").into_os_string())
        })
    });
    let Some(config) = config else {
        eprintln!(
            "fake_claude: set CLAUDE_CONFIG_DIR, or run where the contract suite gave a home, so that it never writes in a real home"
        );
        std::process::exit(2);
    };
    if args.first().is_some_and(|first| first == "auth") {
        std::process::exit(auth(&args, Path::new(&config)));
    }
    let value_of = |flag: &str| {
        args.iter()
            .find_map(|arg| arg.strip_prefix(flag).map(str::to_string))
    };
    let resumed = value_of("--resume=");
    let session = resumed
        .clone()
        .or_else(|| value_of("--session-id="))
        .unwrap_or_else(uuid);
    let cwd = std::env::current_dir().unwrap_or_default();
    let transcript = Path::new(&config)
        .join("projects")
        .join(project_directory(&cwd))
        .join(format!("{session}.jsonl"));
    if resumed.is_some() && !transcript.is_file() {
        eprintln!("No conversation found with session ID: {session}");
        std::process::exit(1);
    }
    let mode = match value_of("--permission-mode=").as_deref() {
        None | Some("manual") => "default".to_string(),
        Some(other) => other.to_string(),
    };
    let mut fake = Fake {
        session,
        cwd,
        transcript,
        mode,
        started: false,
        turns: 0,
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines().map_while(Result::ok);
    while let Some(line) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["type"].as_str() {
            Some("control_request") => control(&mut fake, &message),
            Some("user") => turn(&mut fake, &message, &mut lines),
            _ => {}
        }
    }
}

fn control(fake: &mut Fake, message: &Value) {
    let id = message["request_id"].as_str().unwrap_or("");
    match message["request"]["subtype"].as_str() {
        Some("initialize") => fake.answer(id, &fake.init()),
        Some("set_permission_mode") => {
            if let Some(mode) = message["request"]["mode"].as_str() {
                fake.mode = mode.to_string();
            }
            fake.answer(id, &json!({}));
        }
        _ => fake.answer(id, &json!({})),
    }
}

/// The text of the first block of a prompt.
fn text_of(message: &Value) -> String {
    message["message"]["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find_map(|b| b["text"].as_str()))
        .unwrap_or("")
        .to_string()
}

fn turn(fake: &mut Fake, message: &Value, lines: &mut impl Iterator<Item = String>) {
    let text = text_of(message);
    fake.turns += 1;
    fake.kept("user", message["message"]["content"].clone());
    if !fake.started {
        fake.started = true;
        emit(&json!({
            "type": "system",
            "subtype": "init",
            "cwd": fake.cwd.to_string_lossy(),
            "session_id": fake.session,
            "tools": [],
            "mcp_servers": [],
            "model": MODEL,
            "permissionMode": fake.mode,
            "claude_code_version": version(),
            "uuid": uuid(),
        }));
    }
    emit(
        &json!({"type": "system", "subtype": "status", "status": "requesting", "session_id": fake.session, "uuid": uuid()}),
    );
    match text.as_str() {
        "die" => std::process::exit(3),
        "wait" => {
            for line in lines {
                let Ok(next) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if next["type"] == "control_request" && next["request"]["subtype"] == "interrupt" {
                    fake.answer(
                        next["request_id"].as_str().unwrap_or(""),
                        &json!({"still_queued": false}),
                    );
                    emit(&fake.result("", Some(("error_during_execution", "aborted_streaming"))));
                    return;
                }
                if next["type"] == "control_request" {
                    control(fake, &next);
                }
            }
            std::process::exit(0);
        }
        "ask" => ask(fake, lines),
        "limit" => {
            emit(&fake.rate_limit(1.0, "rejected"));
            let said = "You've hit your 5-hour limit \u{b7} resets 5:43pm (UTC)";
            emit(&fake.assistant(json!([{"type": "text", "text": said}])));
            let mut result = fake.result(said, None);
            result["is_error"] = json!(true);
            result["stop_reason"] = json!("stop_sequence");
            result["terminal_reason"] = json!("api_error");
            result["api_error_status"] = json!(429);
            emit(&result);
        }
        other => {
            if let Some(figure) = other.strip_prefix("quota ") {
                let used: f64 = figure.trim().parse().unwrap_or(0.0);
                emit(&fake.rate_limit(used / 1000.0, "allowed"));
            }
            fake.say(&format!("echo: {other}"));
        }
    }
}

fn ask(fake: &Fake, lines: &mut impl Iterator<Item = String>) {
    let tool = format!("toolu_fake_{}", fake.turns);
    let input = json!({"file_path": "notes.txt", "old_string": "two", "new_string": "three", "replace_all": false});
    emit(
        &fake.assistant(json!([{"type": "tool_use", "id": tool, "name": "Edit", "input": input}])),
    );
    let request = format!("ask-{}", fake.turns);
    emit(&json!({
        "type": "control_request",
        "request_id": request,
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "Edit",
            "display_name": "Edit",
            "input": input,
            "description": "notes.txt",
            "permission_suggestions": [],
            "tool_use_id": tool,
        },
    }));
    for line in lines {
        let Ok(next) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if next["type"] != "control_response" || next["response"]["request_id"] != request.as_str()
        {
            continue;
        }
        let answer = &next["response"]["response"];
        let allowed = answer["behavior"] == "allow";
        emit(&json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool,
                "content": if allowed { "ok" } else { "The person refused this." }, "is_error": !allowed}]},
            "session_id": fake.session,
            "uuid": uuid(),
        }));
        if allowed {
            fake.say("echo: allowed");
        } else if answer["interrupt"] == true {
            emit(&fake.result("", Some(("error_during_execution", "aborted_tools"))));
        } else {
            fake.say("echo: refused");
        }
        return;
    }
    std::process::exit(0);
}

/// The login the stand-in keeps, as Claude Code's `.credentials.json` holds one.
fn login_file(access: &str, refresh: &str, expires_ms: u64) -> Value {
    json!({"claudeAiOauth": {
        "accessToken": access,
        "refreshToken": refresh,
        "expiresAt": expires_ms,
        "refreshTokenExpiresAt": expires_ms + 86_400_000,
        "scopes": ["user:inference"],
        "subscriptionType": "max",
        "subscriptionTypeSource": "stand-in",
        "rateLimitTier": "stand-in",
    }})
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

/// `claude auth login` and `claude auth status`, as recorded in `references/account_kinds`: the sign-in prints the page
/// to open and waits for the code it shows; a status refreshes a login in its last five minutes.
///
/// A file named `fake-refresh` in the configuration directory scripts what a refresh does: `refuse-blank` blanks both
/// tokens and `refuse-delete` deletes the file, as a refused refresh does; `fake-login` holding `refuse` refuses a
/// code. Standard input is read for the code and never echoed or logged.
fn auth(args: &[String], config: &Path) -> i32 {
    let credentials = config.join(".credentials.json");
    match args.get(1).map(String::as_str) {
        Some("login") => {
            println!("Opening browser to sign in\u{2026}");
            let page = if args.iter().any(|arg| arg == "--console") {
                "https://platform.claude.com/oauth/authorize?code=true&client_id=stand-in&state=stand-in"
            } else {
                "https://claude.com/cai/oauth/authorize?code=true&client_id=stand-in&state=stand-in"
            };
            println!(
                "If the browser didn't open, visit: \u{1b}]8;;{page}\u{1b}\\{page}\u{1b}]8;;\u{1b}\\"
            );
            print!("Paste code here if prompted > ");
            let _ = std::io::stdout().flush();
            let mut code = String::new();
            let _ = std::io::stdin().read_line(&mut code);
            let refused = std::fs::read_to_string(config.join("fake-login"))
                .is_ok_and(|s| s.trim() == "refuse");
            if code.trim().is_empty() || refused {
                eprintln!("Login failed: the code was not accepted");
                return 1;
            }
            // The exchange with the sign-in service takes a moment.
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = std::fs::create_dir_all(config);
            let file = login_file(
                "stand-in-access-1",
                "stand-in-refresh-1",
                now_ms() + 8 * 3_600_000,
            );
            let _ = std::fs::write(&credentials, file.to_string());
            println!("\nLogin successful.");
            0
        }
        Some("status") => {
            // A status that takes long and leaves a mark when it ends, to see whether it was ended.
            if config.join("fake-status-sleep").exists() {
                std::thread::sleep(std::time::Duration::from_millis(2500));
                let _ = std::fs::write(config.join("status-finished"), "done");
            }
            for (variable, method) in [
                ("ANTHROPIC_API_KEY", "api_key"),
                ("ANTHROPIC_AUTH_TOKEN", "oauth_token"),
                ("CLAUDE_CODE_OAUTH_TOKEN", "oauth_token"),
            ] {
                if std::env::var(variable).is_ok_and(|v| !v.is_empty()) {
                    println!(
                        "{}",
                        json!({"loggedIn": true, "authMethod": method, "apiProvider": "firstParty"})
                    );
                    return 0;
                }
            }
            let signed_out = || {
                println!(
                    "{}",
                    json!({"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"})
                );
                1
            };
            let Ok(text) = std::fs::read_to_string(&credentials) else {
                return signed_out();
            };
            if text.trim().is_empty() {
                return signed_out();
            }
            let Ok(mut file) = serde_json::from_str::<Value>(&text) else {
                // A login the stand-in did not make: signed in.
                println!(
                    "{}",
                    json!({"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty"})
                );
                return 0;
            };
            let oauth = &file["claudeAiOauth"];
            if oauth["accessToken"].as_str().is_none_or(str::is_empty)
                && oauth["refreshToken"].as_str().is_none_or(str::is_empty)
            {
                return signed_out();
            }
            let ends = oauth["expiresAt"].as_u64().unwrap_or(u64::MAX);
            if now_ms() + 300_000 >= ends {
                match std::fs::read_to_string(config.join("fake-refresh"))
                    .unwrap_or_default()
                    .trim()
                {
                    "refuse-blank" => {
                        file["claudeAiOauth"]["accessToken"] = json!("");
                        file["claudeAiOauth"]["refreshToken"] = json!("");
                        let _ = std::fs::write(&credentials, file.to_string());
                        return signed_out();
                    }
                    "refuse-delete" => {
                        let _ = std::fs::remove_file(&credentials);
                        return signed_out();
                    }
                    _ => {
                        // The refresh token is single use: the new login replaces both.
                        let number = oauth["accessToken"]
                            .as_str()
                            .and_then(|t| t.rsplit('-').next())
                            .and_then(|n| n.parse::<u64>().ok())
                            .unwrap_or(0)
                            + 1;
                        let renewed = login_file(
                            &format!("stand-in-access-{number}"),
                            &format!("stand-in-refresh-{number}"),
                            now_ms() + 8 * 3_600_000,
                        );
                        let _ = std::fs::write(&credentials, renewed.to_string());
                    }
                }
            }
            println!(
                "{}",
                json!({"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty"})
            );
            0
        }
        _ => 2,
    }
}
