//! A stand-in for `codex app-server` that behaves as the `acp_harness` contract agent does, in the app server's own
//! JSON-RPC dialect, for `acp-harness-check --program-agent` and the provider's tests. It never calls a model.
//!
//! Its messages have the shapes of the recordings in `tests/fixtures/codex-0.166.0` (`initialize`, `model/list`,
//! `thread/start`, `turn/start` and the notifications of a turn, `item/commandExecution/requestApproval`,
//! `account/rateLimits/read`). A prompt's text decides the turn: `wait` ends only when it is interrupted; `ask` asks to
//! run a command, and says `echo: allowed` or `echo: refused`, or ends interrupted when the answer cancels; `quota P`
//! reports the five-hour window used P tenths of a percent beside its answer and keeps it for the reads that follow;
//! `limit` hits the usage limit; any other text is answered `echo: TEXT`.
//!
//! Threads are kept in `fake_threads/<id>.json` of `CODEX_HOME` or of `.codex` in the home `ACP_HARNESS_CHECK_HOME`
//! names, one of which it requires so that it never writes in a real home: `thread/resume` of an id with no file there is refused as Codex refuses it. The window the reads report is
//! kept in `fake_rate_limits.json` there.
//!
//! `COQUI_ACP_FAKE_CODEX_LOG` names a file each start appends to: one line of JSON with the arguments and the names
//! (never the values) of the environment. `--version` prints the recorded version.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a stand-in for tests names what it uses from std"
)]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const VERSION: &str = "0.166.0";
const MODEL: &str = "gpt-standin";

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

fn answer(id: &Value, result: &Value) {
    emit(&json!({"id": id, "result": result}));
}

fn refuse(id: &Value, message: &str) {
    emit(&json!({"id": id, "error": {"code": -32600, "message": message}}));
}

fn notify(method: &str, params: &Value) {
    emit(&json!({"method": method, "params": params}));
}

struct Fake {
    home: PathBuf,
    /// The id of the next request this makes of the client.
    next_request: u64,
}

impl Fake {
    fn thread_file(&self, id: &str) -> PathBuf {
        self.home.join("fake_threads").join(format!("{id}.json"))
    }

    /// Where Codex keeps a thread's rollout, so that a history can be imported and exported through it.
    fn rollout_file(&self, id: &str) -> PathBuf {
        self.home
            .join("sessions")
            .join("2026")
            .join("11")
            .join("10")
            .join(format!("rollout-2026-11-10T00-00-00-{id}.jsonl"))
    }

    fn load(&self, id: &str) -> Option<Value> {
        if let Ok(text) = std::fs::read_to_string(self.thread_file(id)) {
            return serde_json::from_str(&text).ok();
        }
        self.thread_of_rollout(id)
    }

    /// A thread made of a rollout nothing here wrote: the person's and the agent's messages, in turns.
    fn thread_of_rollout(&self, id: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.rollout_file(id)).ok()?;
        let mut cwd = Value::Null;
        let mut turns = Vec::new();
        let mut asked: Option<String> = None;
        for line in text.lines() {
            let Ok(line) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let payload = &line["payload"];
            match line["type"].as_str() {
                Some("session_meta") => cwd = payload["cwd"].clone(),
                Some("response_item") if payload["type"] == "message" => {
                    let said = payload["content"][0]["text"].as_str().unwrap_or_default();
                    if payload["role"] == "user" && !said.starts_with('<') {
                        asked = Some(said.to_string());
                    } else if payload["role"] == "assistant" {
                        let user = json!({"type": "userMessage", "id": uuid(), "clientId": null,
                            "content": [{"type": "text", "text": asked.take().unwrap_or_default(), "text_elements": []}]});
                        let item = json!({"type": "agentMessage", "id": uuid(), "text": said, "phase": null});
                        turns.push(
                            json!({"id": uuid(), "items": [user, item], "status": "completed"}),
                        );
                    }
                }
                _ => {}
            }
        }
        Some(json!({"id": id, "sessionId": id, "cwd": cwd, "turns": turns}))
    }

    fn save(&self, id: &str, thread: &Value) {
        let _ = std::fs::create_dir_all(self.home.join("fake_threads"));
        let _ = std::fs::write(self.thread_file(id), thread.to_string());
        self.save_rollout(id, thread);
    }

    /// The thread as Codex writes one to its rollout, in the older form every version reads.
    fn save_rollout(&self, id: &str, thread: &Value) {
        let at = "2026-11-10T00:00:00.000Z";
        let mut out = String::new();
        let mut push = |kind: &str, payload: Value| {
            out.push_str(&json!({"timestamp": at, "type": kind, "payload": payload}).to_string());
            out.push('\n');
        };
        push(
            "session_meta",
            json!({"id": id, "timestamp": at, "cwd": thread["cwd"], "originator": "fake_codex",
                "cli_version": "0.166.0", "source": "vscode", "model_provider": "openai"}),
        );
        for turn in thread["turns"].as_array().into_iter().flatten() {
            let turn_id = turn["id"].as_str().unwrap_or_default();
            let user = turn["items"][0]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            let agent = turn["items"][1]["text"].as_str().unwrap_or_default();
            push(
                "event_msg",
                json!({"type": "task_started", "turn_id": turn_id, "started_at": 0}),
            );
            push(
                "response_item",
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": user}]}),
            );
            push(
                "event_msg",
                json!({"type": "user_message", "message": user, "images": [], "local_images": [], "text_elements": []}),
            );
            push(
                "response_item",
                json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": agent}]}),
            );
            push(
                "event_msg",
                json!({"type": "agent_message", "message": agent}),
            );
            push(
                "event_msg",
                json!({"type": "task_complete", "turn_id": turn_id, "last_agent_message": agent}),
            );
        }
        let file = self.rollout_file(id);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, out);
    }

    /// The five-hour window's used percent, as the last `quota` or `limit` left it.
    fn used(&self) -> f64 {
        std::fs::read_to_string(self.home.join("fake_rate_limits.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value["used"].as_f64())
            .unwrap_or(0.0)
    }

    fn set_used(&self, used: f64) {
        let _ = std::fs::write(
            self.home.join("fake_rate_limits.json"),
            json!({"used": used}).to_string(),
        );
    }

    fn snapshot(&self, used: f64) -> Value {
        json!({
            "limitId": "codex",
            "limitName": null,
            "normalModelSlug": null,
            "primary": {"usedPercent": used, "windowDurationMins": 300, "resetsAt": now_secs() + 3600},
            "secondary": {"usedPercent": 3, "windowDurationMins": 10080, "resetsAt": now_secs() + 86_400},
            "credits": null,
            "individualLimit": null,
            "spendControlReached": null,
            "planType": "plus",
            "rateLimitReachedType": null,
        })
    }

    fn rate_limits(&self) -> Value {
        let used = self.used();
        let snapshot = self.snapshot(used);
        json!({
            "ordinaryUsageAllowed": used < 100.0,
            "rateLimits": snapshot,
            "rateLimitsByLimitId": {"codex": snapshot},
        })
    }
}

fn sandbox_of(word: Option<&str>) -> &'static str {
    match word {
        Some("read-only") => "readOnly",
        Some("danger-full-access") => "dangerFullAccess",
        _ => "workspaceWrite",
    }
}

fn opened(thread: &Value, params: &Value) -> Value {
    json!({
        "thread": thread,
        "model": MODEL,
        "reasoningEffort": "low",
        "cwd": thread["cwd"],
        "approvalPolicy": params["approvalPolicy"].as_str().unwrap_or("untrusted"),
        "approvalsReviewer": "user",
        "sandbox": {"type": sandbox_of(params["sandbox"].as_str())},
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("codex-cli {VERSION}");
        return;
    }
    let mut logs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("COQUI_ACP_FAKE_CODEX_LOG") {
        logs.push(path.into());
    }
    // Beside the home too, so a call that sets no variable of its own can still be read back.
    if let Some(dir) = std::env::var_os("CODEX_HOME")
        && std::fs::create_dir_all(&dir).is_ok()
    {
        logs.push(PathBuf::from(dir).join("fake-codex.log"));
    }
    if !logs.is_empty() {
        let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
        let line =
            json!({"args": args, "env_names": names, "home": std::env::var("CODEX_HOME").ok()});
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
    let home = std::env::var_os("CODEX_HOME").or_else(|| {
        std::env::var_os("ACP_HARNESS_CHECK_HOME").and_then(|_| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".codex").into_os_string())
        })
    });
    let Some(home) = home else {
        eprintln!(
            "fake_codex: set CODEX_HOME, or run where the contract suite gave a home, so that it never writes in a real home"
        );
        std::process::exit(2);
    };
    let _ = std::fs::create_dir_all(&home);
    if args.first().is_some_and(|first| first == "login") {
        std::process::exit(login(&args, std::path::Path::new(&home)));
    }
    let mut fake = Fake {
        home: PathBuf::from(home),
        next_request: 0,
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines().map_while(Result::ok);
    while let Some(line) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        request(&mut fake, &message, &mut lines);
    }
}

/// Answers a request that needs no turn; `false` when it is not one.
fn plain(fake: &Fake, message: &Value) -> bool {
    let id = &message["id"];
    match message["method"].as_str() {
        Some("initialize") => answer(
            id,
            &json!({
                "userAgent": format!("coqui-acp-codex/{VERSION} (Fake; x86_64) unknown (coqui-acp-codex; 0.0.0)"),
                "codexHome": fake.home.to_string_lossy(),
                "platformFamily": "fake",
                "platformOs": "fake",
            }),
        ),
        Some("model/list") => answer(
            id,
            &json!({"data": [{
                "id": MODEL,
                "model": MODEL,
                "displayName": "Stand-in",
                "description": "A stand-in model",
                "hidden": false,
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": "Fast"},
                    {"reasoningEffort": "high", "description": "Deep"},
                ],
                "defaultReasoningEffort": "low",
                "isDefault": true,
            }], "nextCursor": null}),
        ),
        Some("config/read") => answer(id, &json!({"config": {}})),
        Some("skills/extraRoots/set") => answer(id, &json!({})),
        Some("account/rateLimits/read") => answer(id, &fake.rate_limits()),
        Some("account/read") => answer(id, &account_read(fake, &message["params"])),
        Some("turn/interrupt") => answer(id, &json!({})),
        _ => return false,
    }
    true
}

fn request(fake: &mut Fake, message: &Value, lines: &mut impl Iterator<Item = String>) {
    if message.get("id").is_none() || plain(fake, message) {
        return;
    }
    let id = &message["id"];
    let params = &message["params"];
    match message["method"].as_str() {
        Some("thread/start") => {
            let thread_id = uuid();
            let thread =
                json!({"id": thread_id, "sessionId": thread_id, "cwd": params["cwd"], "turns": []});
            fake.save(&thread_id, &thread);
            answer(id, &opened(&thread, params));
        }
        Some("thread/resume") => {
            let wanted = params["threadId"].as_str().unwrap_or("");
            match fake.load(wanted) {
                Some(thread) => answer(id, &opened(&thread, params)),
                None => refuse(id, &format!("no rollout found for thread id {wanted}")),
            }
        }
        Some("turn/start") => turn(fake, message, lines),
        _ => refuse(id, "not handled"),
    }
}

/// The text of the first block of a turn's input.
fn text_of(params: &Value) -> String {
    params["input"]
        .as_array()
        .and_then(|blocks| blocks.iter().find_map(|b| b["text"].as_str()))
        .unwrap_or("")
        .to_string()
}

fn turn(fake: &mut Fake, message: &Value, lines: &mut impl Iterator<Item = String>) {
    let params = &message["params"];
    let thread_id = params["threadId"].as_str().unwrap_or("").to_string();
    let Some(mut thread) = fake.load(&thread_id) else {
        refuse(&message["id"], "no such thread");
        return;
    };
    let text = text_of(params);
    let turn_id = uuid();
    let running = json!({"id": turn_id, "rootTurnId": turn_id, "items": [], "itemsView": "notLoaded", "status": "inProgress", "error": null});
    answer(&message["id"], &json!({"turn": running}));
    notify(
        "thread/status/changed",
        &json!({"threadId": thread_id, "status": {"type": "active", "activeFlags": []}}),
    );
    notify(
        "turn/started",
        &json!({"threadId": thread_id, "turn": running}),
    );
    let user = json!({"type": "userMessage", "id": uuid(), "clientId": null, "content": [{"type": "text", "text": text, "text_elements": []}]});
    notify(
        "item/started",
        &json!({"item": user, "threadId": thread_id, "turnId": turn_id}),
    );
    notify(
        "item/completed",
        &json!({"item": user, "threadId": thread_id, "turnId": turn_id}),
    );
    let ending = |status: &str, items: &[Value], error: Value| {
        notify(
            "thread/status/changed",
            &json!({"threadId": thread_id, "status": {"type": "idle"}}),
        );
        notify(
            "turn/completed",
            &json!({"threadId": thread_id, "turn": {"id": turn_id, "rootTurnId": turn_id, "items": items,
                "itemsView": "summary", "status": status, "error": error}}),
        );
    };
    let mut said: Option<String> = None;
    match text.as_str() {
        "wait" => {
            for line in lines {
                let Ok(next) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if next["method"] == "turn/interrupt" {
                    answer(&next["id"], &json!({}));
                    ending("interrupted", &[], Value::Null);
                    return;
                }
                if next.get("id").is_some() {
                    plain(fake, &next);
                }
            }
            std::process::exit(0);
        }
        "ask" => {
            let item = format!("exec-{}", uuid());
            let command = json!({"type": "commandExecution", "id": item, "command": "echo checked > checked.txt",
                "cwd": thread["cwd"], "source": "agent", "status": "inProgress", "commandActions": []});
            notify(
                "item/started",
                &json!({"item": command, "threadId": thread_id, "turnId": turn_id}),
            );
            let asked = fake.next_request;
            fake.next_request += 1;
            emit(
                &json!({"method": "item/commandExecution/requestApproval", "id": asked, "params": {
                "kind": "command", "threadId": thread_id, "turnId": turn_id, "itemId": item,
                "command": "echo checked > checked.txt", "cwd": thread["cwd"], "commandActions": [],
                "availableDecisions": ["accept", "cancel"]}}),
            );
            for line in lines {
                let Ok(next) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if next.get("method").is_none() && next["id"] == asked {
                    match next["result"]["decision"].as_str() {
                        Some("accept") => said = Some("echo: allowed".into()),
                        Some("decline") => said = Some("echo: refused".into()),
                        _ => {
                            ending("interrupted", &[], Value::Null);
                            return;
                        }
                    }
                    break;
                }
                if next.get("id").is_some() && next.get("method").is_some() {
                    plain(fake, &next);
                }
            }
            if said.is_none() {
                std::process::exit(0);
            }
        }
        "limit" => {
            fake.set_used(100.0);
            notify(
                "account/rateLimits/updated",
                &json!({"rateLimits": fake.snapshot(100.0)}),
            );
            ending(
                "failed",
                &[],
                json!({"message": "You've hit your usage limit. Try again later.", "codexErrorInfo": "usageLimitExceeded",
                       "resetsAt": now_secs() + 3600}),
            );
            return;
        }
        other => {
            if let Some(figure) = other.strip_prefix("quota ") {
                let used = figure.trim().parse::<f64>().unwrap_or(0.0) / 10.0;
                fake.set_used(used);
                notify(
                    "account/rateLimits/updated",
                    &json!({"rateLimits": fake.snapshot(used)}),
                );
            }
            said = Some(format!("echo: {other}"));
        }
    }
    let said = said.unwrap_or_default();
    let item = json!({"type": "agentMessage", "id": format!("msg_{turn_id}"), "text": said, "phase": null});
    let mut started = item.clone();
    started["text"] = json!("");
    notify(
        "item/started",
        &json!({"item": started, "threadId": thread_id, "turnId": turn_id}),
    );
    notify(
        "item/agentMessage/delta",
        &json!({"threadId": thread_id, "turnId": turn_id, "itemId": item["id"], "delta": said}),
    );
    notify(
        "item/completed",
        &json!({"item": item, "threadId": thread_id, "turnId": turn_id}),
    );
    let turns = thread["turns"].as_array().map_or(0, Vec::len) as u64 + 1;
    let total = |per: u64| per * turns;
    notify(
        "thread/tokenUsage/updated",
        &json!({"threadId": thread_id, "turnId": turn_id, "tokenUsage": {
            "total": {"totalTokens": total(30), "inputTokens": total(21), "cachedInputTokens": total(5),
                      "cacheWriteInputTokens": 0, "outputTokens": total(9), "reasoningOutputTokens": total(2)},
            "last": {"totalTokens": 30, "inputTokens": 21, "cachedInputTokens": 5, "cacheWriteInputTokens": 0,
                     "outputTokens": 9, "reasoningOutputTokens": 2},
            "modelContextWindow": 200_000}}),
    );
    // Kept before the turn ends, since a client may end the process as soon as it hears so.
    if let Some(list) = thread["turns"].as_array_mut() {
        list.push(json!({"id": turn_id, "items": [user, item], "status": "completed"}));
    }
    fake.save(&thread_id, &thread);
    ending("completed", &[item], Value::Null);
}

/// The `auth.json` of a ChatGPT login, as Codex keeps one (names and types of the recording in
/// `references/account_kinds/codex/stored_file_shape.txt`).
fn chatgpt_file(number: u64) -> Value {
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "last_refresh": "2026-11-11T00:00:00Z",
        "tokens": {
            "id_token": "stand-in-id",
            "access_token": format!("stand-in-access-{number}"),
            "refresh_token": format!("stand-in-refresh-{number}"),
            "account_id": "stand-in-account",
            "login_owner_id": "stand-in-owner",
            "oauth_provenance": {"issuer": "https://auth.openai.com", "client_id": "stand-in"},
        },
    })
}

/// The refresh number in a stand-in login file, 0 for any other text.
fn number_of(text: &str) -> u64 {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|file| {
            file["tokens"]["refresh_token"]
                .as_str()
                .and_then(|t| t.rsplit('-').next())
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

/// `codex login ...`, as recorded in `references/account_kinds/codex`. A key or token is read from standard input and
/// never echoed or logged; `fake-login` in the home holding `refuse` refuses what it is given, and a token that starts
/// with `bad` is refused as Codex refuses one that is not a JWT.
fn login(args: &[String], home: &std::path::Path) -> i32 {
    let auth = home.join("auth.json");
    let read_line = || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line.trim().to_string()
    };
    let refused =
        std::fs::read_to_string(home.join("fake-login")).is_ok_and(|s| s.trim() == "refuse");
    match args.get(1).map(String::as_str) {
        Some("--with-api-key") => {
            eprintln!("Reading API key from stdin...");
            let key = read_line();
            let mode = std::fs::read_to_string(home.join("fake-login")).unwrap_or_default();
            if mode.trim() == "echo" {
                // A refusal that quotes the start and end of what it was given.
                eprintln!(
                    "Error: invalid key {}...{}",
                    key.chars().take(6).collect::<String>(),
                    key.chars()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<String>()
                );
                return 1;
            }
            if mode.trim() == "sleep" {
                // Slow to finish, and writes the key late: to see whether it was ended.
                std::thread::sleep(std::time::Duration::from_millis(2500));
                let _ = std::fs::write(
                    &auth,
                    json!({"auth_mode": "apikey", "OPENAI_API_KEY": key}).to_string(),
                );
                return 0;
            }
            if key.is_empty() || refused {
                eprintln!("Error logging in with an API key: the key is empty");
                return 1;
            }
            let _ = std::fs::write(
                &auth,
                json!({"auth_mode": "apikey", "OPENAI_API_KEY": key}).to_string(),
            );
            eprintln!("API key saved. Authentication is checked when you send a request.");
            0
        }
        Some("--with-access-token") => {
            eprintln!("Reading access token from stdin...");
            let token = read_line();
            if token.is_empty() || token.starts_with("bad") || refused {
                eprintln!("Error logging in with access token: invalid agent identity JWT format");
                return 1;
            }
            let _ = std::fs::write(
                &auth,
                json!({"auth_mode": "agentIdentity", "OPENAI_API_KEY": null, "tokens": {"access_token": token}}).to_string(),
            );
            eprintln!("Logged in.");
            0
        }
        Some("status") => match std::fs::read_to_string(&auth) {
            Ok(text) if !text.trim().is_empty() => {
                eprintln!("Logged in");
                0
            }
            _ => {
                eprintln!("Not logged in");
                1
            }
        },
        Some("--device-auth") => {
            println!("\nWelcome to Codex [v{VERSION}]\nOpenAI's command-line coding agent\n");
            println!(
                "Follow these steps to sign in with ChatGPT using device code authorization:\n"
            );
            println!(
                "1. Open this link in your browser and sign in to your account\n   https://auth.openai.com/codex/device\n"
            );
            println!("2. Enter this one-time code (expires in 15 minutes)\n   ABCD-12345\n");
            println!(
                "Continue only if you started this login in Codex. If a website or another person gave you this code, cancel.\n"
            );
            let _ = std::io::stdout().flush();
            finish_web(&auth, refused)
        }
        None => {
            eprintln!("Starting local login server on http://localhost:1455.");
            eprintln!("If your browser did not open, navigate to this URL to authenticate:\n");
            eprintln!(
                "https://auth.openai.com/oauth/authorize?response_type=code&client_id=stand-in&state=stand-in\n"
            );
            eprintln!("On a remote or headless machine? Use `codex login --device-auth` instead.");
            finish_web(&auth, refused)
        }
        _ => 2,
    }
}

/// The browser comes back after a moment, and the login is written.
fn finish_web(auth: &std::path::Path, refused: bool) -> i32 {
    std::thread::sleep(std::time::Duration::from_millis(600));
    if refused {
        eprintln!("Login failed");
        return 1;
    }
    let _ = std::fs::write(auth, chatgpt_file(1).to_string());
    eprintln!("Successfully logged in");
    0
}

/// `account/read`: the signed-in account, refreshing a ChatGPT login when asked to. A file named `fake-refresh` in the
/// home scripts what a refresh does, as in the Claude Code stand-in: `refuse-blank` blanks the tokens and
/// `refuse-delete` deletes the file.
fn account_read(fake: &Fake, params: &Value) -> Value {
    let auth = fake.home.join("auth.json");
    let signed_out = || json!({"account": null, "requiresOpenaiAuth": true});
    let Ok(text) = std::fs::read_to_string(&auth) else {
        return signed_out();
    };
    if text.trim().is_empty() {
        return signed_out();
    }
    let Ok(file) = serde_json::from_str::<Value>(&text) else {
        // A login the stand-in did not make: signed in.
        return json!({"account": {"type": "chatgpt", "email": "stand-in@example.invalid", "planType": "plus"}, "requiresOpenaiAuth": true});
    };
    if file["tokens"]["refresh_token"]
        .as_str()
        .is_none_or(str::is_empty)
        && file["tokens"]["access_token"]
            .as_str()
            .is_none_or(str::is_empty)
        && file["OPENAI_API_KEY"].as_str().is_none_or(str::is_empty)
    {
        return signed_out();
    }
    if params["refreshToken"] == true && file["tokens"]["refresh_token"].is_string() {
        match std::fs::read_to_string(fake.home.join("fake-refresh"))
            .unwrap_or_default()
            .trim()
        {
            "refuse-blank" => {
                let mut blank = file.clone();
                blank["tokens"]["access_token"] = json!("");
                blank["tokens"]["refresh_token"] = json!("");
                let _ = std::fs::write(&auth, blank.to_string());
                return signed_out();
            }
            "refuse-delete" => {
                let _ = std::fs::remove_file(&auth);
                return signed_out();
            }
            _ => {
                // The refresh token is single use: the new login replaces it.
                let renewed = chatgpt_file(number_of(&text) + 1);
                let _ = std::fs::write(&auth, renewed.to_string());
            }
        }
    }
    json!({"account": {"type": "chatgpt", "email": "stand-in@example.invalid", "planType": "plus"}, "requiresOpenaiAuth": true})
}
