//! A stand-in for `grok`, `agy` and `opencode` that behaves as the `acp_harness` contract agent does, for
//! the contract check's `--program-agent` and the stdio provider's tests. It never calls a model.
//!
//! Which program it plays is told by its arguments, as the provider starts them: `agent stdio` is Grok, `acp` is
//! Antigravity or OpenCode; `login [--oauth]` and `models` are Grok's; `auth list`, `auth login [-p PROVIDER] [-m METHOD]`, `export --pure ID` and `import FILE` are OpenCode's. As an ACP agent it answers `initialize`,
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
//! OpenCode's sessions are the files of a fixture folder, `$XDG_DATA_HOME/opencode/fixture/<id>.json`, which the agent
//! lists (`session/list`), opens (`session/load`, `session/resume`) and `export` prints: `{title, directory, updated,
//! turns: [[user text, agent text]], export}`, where `export` is what `opencode export` prints for it. With no such
//! folder the agent advertises no session capability, as the other two do not.
//!
//! `COQUI_ACP_FAKE_STDIO_LOG` names a file each start appends to: one line of JSON with the arguments and the names
//! (never the values) of the environment, and the values of the two directory variables and of the endpoint variables.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a stand-in for tests names what it uses from std"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use serde_json::{Value, json};

const DIRECTORY_VARIABLES: [&str; 3] = ["GROK_HOME", "PI_CODING_AGENT_DIR", "XDG_DATA_HOME"];

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
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let line = json!({"args": args, "cwd": cwd, "env_names": names, "directories": directories, "endpoints": endpoints});
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

/// The fixture folder of OpenCode's sessions, when the data folder has one.
fn store() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_DATA_HOME")?)
        .join("opencode")
        .join("fixture");
    dir.is_dir().then_some(dir)
}

/// The fixture session `id`, when there is one. An id is only letters and digits after `ses_`, so no path is built from
/// anything else.
fn fixture(id: &str) -> Option<Value> {
    let rest = id.strip_prefix("ses_")?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let text = std::fs::read_to_string(store()?.join(format!("{id}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn fixtures() -> Vec<(String, Value)> {
    let Some(dir) = store() else {
        return Vec::new();
    };
    let mut found: Vec<(String, Value)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?.to_owned();
            Some((id.clone(), fixture(&id)?))
        })
        .collect();
    found.sort_by(|a, b| b.1["updated"].as_str().cmp(&a.1["updated"].as_str()));
    found
}

/// What `opencode auth list` prints: each key variable of the environment as OpenCode lists a provider that is signed in
/// by it, never its value.
fn auth_list() -> ! {
    println!("Credentials");
    println!("0 credentials");
    println!("Environment");
    let mut seen = 0;
    for name in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_GENERATIVE_AI_API_KEY",
        "XAI_API_KEY",
        "OPENROUTER_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "DEEPSEEK_API_KEY",
        "OPENCODE_API_KEY",
        "TOGETHER_API_KEY",
        "CEREBRAS_API_KEY",
        "FIREWORKS_API_KEY",
        "HF_TOKEN",
    ] {
        if std::env::var_os(name).is_some() {
            println!("\u{2022}  Provider {name}");
            seen += 1;
        }
    }
    println!("{seen} environment variables");
    exit(0)
}

/// `opencode export --pure ID`: the session's export, or the program's refusal.
fn export(id: &str) -> ! {
    match fixture(id).and_then(|session| session.get("export").cloned()) {
        Some(export) => {
            println!("{export}");
            exit(0)
        }
        None => {
            eprintln!("Exporting session: {id}");
            eprintln!("Error: Session not found: {id}");
            exit(1)
        }
    }
}

/// `opencode import FILE`: the session the file holds becomes a fixture of the data folder, listed and opened by the agent,
/// and `Imported session: ID` is printed. The checks are the ones the real program was seen to make of a hand-written
/// file (ids that start `ses`, `msg` and `prt`, the keys it needs), and the session's directory is the working directory
/// of this process, as the real program's is.
fn import(file: &str) -> ! {
    let Some(base) = std::env::var_os("XDG_DATA_HOME") else {
        eprintln!("no data folder");
        exit(3);
    };
    let Some(file) = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        eprintln!("Error: the file is not a session");
        exit(1);
    };
    let refuse = |why: &str| -> ! {
        eprintln!("Error: Unexpected error\n\n{why}");
        exit(1)
    };
    let prefixed = |value: &Value, prefix: &str| -> bool {
        value.as_str().is_some_and(|text| {
            text.starts_with(prefix)
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    };
    let info = &file["info"];
    if !prefixed(&info["id"], "ses") {
        refuse("Expected a string starting with \"ses\"\n  at [\"id\"]");
    }
    for key in ["slug", "title", "version"] {
        if !info[key].is_string() {
            refuse(&format!("Missing key\n  at [\"{key}\"]"));
        }
    }
    for key in ["created", "updated"] {
        if !info["time"][key].is_number() {
            refuse(&format!("Missing key\n  at [\"time\"][\"{key}\"]"));
        }
    }
    let mut turns: Vec<[String; 2]> = Vec::new();
    for message in file["messages"].as_array().into_iter().flatten() {
        let (role, message_info) = (message["info"]["role"].as_str(), &message["info"]);
        if !prefixed(&message_info["id"], "msg") || !prefixed(&message_info["sessionID"], "ses") {
            refuse("Expected a string starting with \"msg\"\n  at [\"id\"]");
        }
        let mut said = String::new();
        for part in message["parts"].as_array().into_iter().flatten() {
            if !prefixed(&part["id"], "prt") || !prefixed(&part["messageID"], "msg") {
                refuse("Expected a string starting with \"prt\"\n  at [\"id\"]");
            }
            if part["type"] == "text" {
                said.push_str(part["text"].as_str().unwrap_or_default());
            }
        }
        match role {
            Some("user") => turns.push([said, String::new()]),
            Some("assistant") => {
                if let Some(turn) = turns.last_mut() {
                    turn[1].push_str(&said);
                }
            }
            _ => refuse("Expected Message"),
        }
    }
    let id = info["id"].as_str().unwrap_or_default().to_owned();
    let folder = PathBuf::from(base).join("opencode").join("fixture");
    let _ = std::fs::create_dir_all(&folder);
    let directory = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let session = json!({
        "title": info["title"], "directory": directory, "updated": "2026-11-18T12:00:00Z",
        "turns": turns, "export": file,
    });
    if std::fs::write(folder.join(format!("{id}.json")), session.to_string()).is_err() {
        exit(1);
    }
    println!("Imported session: {id}");
    exit(0)
}

/// What the program printed on plain pipes, recorded in `tests/fixtures/opencode_auth_login.json`.
fn recorded() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/opencode_auth_login.json"))
        .expect("the recorded prompts are JSON")
}

fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// What the program's input says up to Enter (a carriage return, as a terminal sends it, or a line feed), or the end of it:
/// the program ends when its input does.
fn typed_line() -> String {
    let mut typed = Vec::new();
    let mut byte = [0_u8; 1];
    let mut input = std::io::stdin().lock();
    loop {
        match input.read(&mut byte) {
            Ok(1) if matches!(byte[0], b'\r' | b'\n') => {
                return String::from_utf8_lossy(&typed).into_owned();
            }
            Ok(1) => typed.push(byte[0]),
            _ => exit(1),
        }
    }
}

/// What a finished sign-in leaves: the provider's entry in `auth.json` of the data folder. The fields of an oauth entry are
/// not recorded, so the stand-in's are made up.
fn write_sign_in(provider: &str, entry: &Value) {
    let Some(base) = std::env::var_os("XDG_DATA_HOME") else {
        eprintln!("no data folder");
        exit(3);
    };
    let folder = PathBuf::from(base).join("opencode");
    let _ = std::fs::create_dir_all(&folder);
    let file = folder.join("auth.json");
    let mut all: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    all[provider] = entry.clone();
    if std::fs::write(
        &file,
        serde_json::to_string_pretty(&all).unwrap_or_default(),
    )
    .is_err()
    {
        exit(1);
    }
}

/// A device code that is confirmed after a moment, as far as the stand-in knows.
fn device_flow(provider: &str, scenario: &Value, key: &str) -> ! {
    say(scenario[key].as_str().unwrap_or_default());
    pause("COQUI_ACP_FAKE_LOGIN_MS", 1200);
    write_sign_in(
        provider,
        &json!({"type": "oauth", "refresh": format!("fake-{}", fresh()), "access": "fake", "expires": 0}),
    );
    say("\n");
    exit(0)
}

/// A key typed on one line, kept as the provider's entry.
fn key_flow(provider: &str, prompt: &str, after: &str) -> ! {
    say(prompt);
    let key = typed_line();
    say(after);
    write_sign_in(provider, &json!({"type": "api", "key": key}));
    exit(0)
}

/// `opencode auth login [-p PROVIDER] [-m METHOD]` as the recorded prompts say it answers.
fn auth_login(args: &[&str]) -> ! {
    let recorded = recorded();
    let scenario = |name: &str| recorded["scenarios"][name].clone();
    let output = |name: &str| {
        scenario(name)["output"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let mut provider = None;
    let mut method = None;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "-p" => provider = rest.next().copied(),
            "-m" => method = rest.next().copied(),
            _ => {}
        }
    }
    let after_key = scenario("openrouter_key")["after_input"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    match (provider, method) {
        (None, _) => {
            say(&output("picker"));
            typed_line();
            exit(1)
        }
        (Some("openrouter"), _) => key_flow(
            "openrouter",
            scenario("openrouter_key")["before_input"]
                .as_str()
                .unwrap_or_default(),
            &after_key,
        ),
        (Some(name @ ("anthropic" | "opencode")), _) => {
            key_flow(name, &output(&format!("{name}_key")), &after_key)
        }
        (Some("openai"), None) => {
            // The selection is answered by Enter, which takes the first, the browser.
            say(&output("openai_methods"));
            typed_line();
            say(&output("openai_browser"));
            std::thread::sleep(Duration::from_secs(30));
            exit(1)
        }
        (Some("openai"), Some("Manually enter API Key")) => {
            key_flow("openai", &output("openai_key"), &after_key)
        }
        (Some("openai"), Some("ChatGPT Pro/Plus (headless)")) => {
            device_flow("openai", &scenario("openai_headless"), "output")
        }
        (Some("openai"), Some("ChatGPT Pro/Plus (browser)")) => {
            say(&output("openai_browser"));
            std::thread::sleep(Duration::from_secs(30));
            exit(1)
        }
        (Some("openai"), Some(_)) => {
            say(&output("openai_bad_method"));
            exit(1)
        }
        (Some("github-copilot"), _) => {
            say(&output("copilot_type"));
            typed_line();
            device_flow("github-copilot", &scenario("copilot_device"), "after_input")
        }
        _ => {
            say(&output("bad_provider"));
            exit(1)
        }
    }
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
                "agentCapabilities": if store().is_some() {
                    json!({"loadSession": true, "sessionCapabilities": {"list": {}, "resume": {}}})
                } else {
                    json!({})
                },
                "authMethods": [],
            })),
            "session/list" => {
                let wanted = params["cwd"].as_str();
                let sessions: Vec<Value> = fixtures()
                    .into_iter()
                    .filter(|(_, session)| wanted.is_none_or(|cwd| session["directory"] == cwd))
                    .map(|(id, session)| {
                        json!({"sessionId": id, "cwd": session["directory"], "title": session["title"], "updatedAt": session["updated"]})
                    })
                    .collect();
                reply(json!({"sessions": sessions}));
            }
            "session/load" | "session/resume" => {
                let id = params["sessionId"].as_str().unwrap_or_default();
                match fixture(id) {
                    Some(session) => {
                        if method == "session/load" {
                            for turn in session["turns"].as_array().into_iter().flatten() {
                                emit(
                                    &json!({"jsonrpc": "2.0", "method": "session/update", "params": {
                                        "sessionId": id,
                                        "update": {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": turn[0]}},
                                    }}),
                                );
                                chunk(id, turn[1].as_str().unwrap_or_default());
                            }
                        }
                        reply(json!({}));
                    }
                    None => fail(-32602, "Session not found"),
                }
            }
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
                    "die" => std::process::exit(3),
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
        ["auth", "list"] => auth_list(),
        ["auth", "login", rest @ ..] => auth_login(rest),
        ["export", "--pure", id] => export(id),
        ["import", file] => import(file),
        _ => {
            eprintln!("unknown arguments");
            exit(2);
        }
    }
}
