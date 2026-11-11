//! A stand-in for `grok` and `agy` that behaves as the `acp_harness` contract agent does, for
//! `acp-harness-check --program-agent` and the stdio provider's tests. It never calls a model.
//!
//! Which program it plays is told by its arguments, as the provider starts them: `agent stdio` is Grok, `acp` is
//! Antigravity; `login [--oauth]` and `models` are Grok's. As an ACP agent it answers `initialize`,
//! `session/new` and `session/prompt`, whose text decides the turn: `wait` ends only on `session/cancel`; `ask` asks for
//! a permission and says `echo: allowed` or `echo: refused`, or ends cancelled when the answer was a cancel; any other
//! text is answered `echo: TEXT`.
//!
//! Grok's `login` prints the recorded prompts of the device and loopback flows, waits `COQUI_ACP_FAKE_LOGIN_MS`
//! (1200 by default) and writes `auth.json` in `GROK_HOME`. `models` with `XAI_API_KEY` set accepts any key but
//! `refused-key`; with a login it rotates `auth.json`, or deletes it (exit 0) when `COQUI_ACP_FAKE_REFUSE` is set, or
//! leaves it when `COQUI_ACP_FAKE_ROTATE` is `0`.
//!
//! A `swap_identity` file in `GROK_HOME` makes `models` write the login of another account (`account_id` `other`); a login
//! keeps the `account_id` it had.
//!
//! `COQUI_ACP_FAKE_STDIO_LOG` names a file each start appends to: one line of JSON with the arguments and the names
//! (never the values) of the environment, and the values of the two directory variables and of the endpoint variables.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a stand-in for tests names what it uses from std"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use serde_json::{Value, json};

const DIRECTORY_VARIABLES: [&str; 2] = ["GROK_HOME", "PI_CODING_AGENT_DIR"];

const ENDPOINT_VARIABLES: [&str; 6] = [
    "GROK_CLI_CHAT_PROXY_BASE_URL",
    "GROK_XAI_API_BASE_URL",
    "GROK_MODELS_BASE_URL",
    "GROK_MODELS_LIST_URL",
    "ANTHROPIC_BASE_URL",
    "OPENAI_BASE_URL",
];

fn emit(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn chunk(session: &str, text: &str) {
    emit(
        &json!({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
        }}),
    );
}

fn log_start(args: &[String]) {
    let Some(path) = std::env::var_os("COQUI_ACP_FAKE_STDIO_LOG") else {
        return;
    };
    let names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    let directories: BTreeMap<&str, String> = DIRECTORY_VARIABLES
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (*name, value)))
        .collect();
    let endpoints: BTreeMap<&str, String> = ENDPOINT_VARIABLES
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (*name, value)))
        .collect();
    let line = json!({"args": args, "env_names": names, "directories": directories, "endpoints": endpoints});
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn directory(name: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(name)?);
    if !dir.is_dir() {
        eprintln!("{name} points to {}, which does not exist", dir.display());
        exit(3);
    }
    Some(dir)
}

fn fresh() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn pause(variable: &str, default_ms: u64) {
    let ms = std::env::var(variable)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default_ms);
    let mut left = ms;
    while left > 0 {
        let step = left.min(20);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
}

fn login(rest: &[&str]) -> ! {
    let Some(home) = directory("GROK_HOME") else {
        eprintln!("GROK_HOME is not set");
        exit(3);
    };
    if rest.contains(&"--oauth") {
        eprintln!("Signing in with Grok...");
        eprintln!("Open this URL to sign in:");
        eprintln!(
            "  https://auth.x.ai/oauth2/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A40000%2Fcallback&state=fake"
        );
    } else {
        eprintln!("To sign in, open this URL in your browser:");
        eprintln!("  https://accounts.x.ai/oauth2/device?user_code=FAKE-12345");
        eprintln!("  (Could not open browser automatically \u{2014} open the URL above manually.)");
        eprintln!("Confirm this code in your browser:");
        eprintln!("  FAKE-12345");
        eprintln!(
            "\u{1b}[90mOnly continue with a code you requested. Don't share it with anyone.\u{1b}[0m"
        );
        eprintln!("Waiting for authorization...");
    }
    pause("COQUI_ACP_FAKE_LOGIN_MS", 1200);
    let text = json!({"fake_login": fresh()}).to_string();
    if std::fs::write(home.join("auth.json"), text).is_err() {
        exit(1);
    }
    exit(0)
}

fn models() -> ! {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if key == "refused-key" || key.trim().is_empty() {
            eprintln!("the API key was refused");
            exit(1);
        }
        println!("grok-fake-1");
        exit(0);
    }
    let Some(home) = directory("GROK_HOME") else {
        eprintln!("not signed in");
        exit(1);
    };
    let file = home.join("auth.json");
    let signed_in = std::fs::read_to_string(&file).is_ok_and(|text| !text.trim().is_empty());
    if !signed_in {
        eprintln!("not signed in");
        exit(1);
    }
    if std::env::var_os("COQUI_ACP_FAKE_REFUSE").is_some() {
        let _ = std::fs::remove_file(&file);
        exit(0);
    }
    if std::env::var("COQUI_ACP_FAKE_ROTATE").as_deref() != Ok("0") {
        let old: Value = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        let mut login = json!({"fake_login": format!("rotated-{}", fresh())});
        if let Some(account) = old["account_id"].as_str() {
            login["account_id"] = json!(account);
        }
        if home.join("swap_identity").exists() {
            login["account_id"] = json!("other");
        }
        let text = login.to_string();
        if std::fs::write(&file, text).is_err() {
            exit(1);
        }
    }
    println!("grok-fake-1");
    exit(0)
}

fn text_of(prompt: &Value) -> String {
    prompt
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or("")
        .to_owned()
}

/// A turn that asked for a permission and waits for the answer.
struct Asked {
    prompt: Value,
    session: String,
}

fn agent() -> ! {
    // The directory variables name where the harness keeps its state: it must be there.
    for name in DIRECTORY_VARIABLES {
        let _ = directory(name);
    }
    let mut waiting: Vec<(Value, String)> = Vec::new();
    let mut asked: BTreeMap<String, Asked> = BTreeMap::new();
    let mut permissions = 0u64;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        let Some(method) = message["method"].as_str() else {
            // An answer to a permission.
            let Some(entry) = message["id"].as_str().and_then(|id| asked.remove(id)) else {
                continue;
            };
            let outcome = &message["result"]["outcome"];
            let (text, stop) = match (outcome["outcome"].as_str(), outcome["optionId"].as_str()) {
                (Some("selected"), Some("allow")) => ("echo: allowed", "end_turn"),
                (Some("cancelled"), _) => ("", "cancelled"),
                _ => ("echo: refused", "end_turn"),
            };
            if !text.is_empty() {
                chunk(&entry.session, text);
            }
            emit(&json!({"jsonrpc": "2.0", "id": entry.prompt, "result": {"stopReason": stop}}));
            continue;
        };
        let params = &message["params"];
        let reply = |result: Value| {
            if let Some(id) = &id {
                emit(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
        };
        let fail = |code: i64, why: &str| {
            if let Some(id) = &id {
                emit(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": why}}));
            }
        };
        match method {
            "initialize" => reply(json!({
                "protocolVersion": params["protocolVersion"].as_u64().unwrap_or(1),
                "agentCapabilities": {},
                "authMethods": [],
            })),
            "session/new" => {
                if params["cwd"].is_string() {
                    reply(json!({"sessionId": fresh()}));
                } else {
                    fail(-32602, "cwd is not text");
                }
            }
            "session/prompt" => {
                let (Some(session), true) =
                    (params["sessionId"].as_str(), params["prompt"].is_array())
                else {
                    fail(-32602, "the prompt is malformed");
                    continue;
                };
                let Some(id) = id.clone() else { continue };
                let text = text_of(&params["prompt"]);
                match text.as_str() {
                    "wait" => waiting.push((id, session.to_owned())),
                    "ask" => {
                        permissions += 1;
                        let ask = format!("perm-{permissions}");
                        emit(
                            &json!({"jsonrpc": "2.0", "id": ask, "method": "session/request_permission",
                            "params": {"sessionId": session, "toolCall": {"toolCallId": "t1"},
                                "options": [
                                    {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                                ]}}),
                        );
                        asked.insert(
                            ask,
                            Asked {
                                prompt: id,
                                session: session.to_owned(),
                            },
                        );
                    }
                    _ => {
                        chunk(session, &format!("echo: {text}"));
                        emit(
                            &json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}),
                        );
                    }
                }
            }
            "session/cancel" => {
                let session = params["sessionId"].as_str().unwrap_or_default();
                waiting.retain(|(prompt, waiting_session)| {
                    if waiting_session == session {
                        emit(&json!({"jsonrpc": "2.0", "id": prompt, "result": {"stopReason": "cancelled"}}));
                        false
                    } else {
                        true
                    }
                });
            }
            _ => fail(-32601, "no such method"),
        }
    }
    exit(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log_start(&args);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["login", rest @ ..] => login(rest),
        ["models"] => models(),
        ["agent", "stdio"] | ["acp"] => agent(),
        _ => {
            eprintln!("unknown arguments");
            exit(2);
        }
    }
}
