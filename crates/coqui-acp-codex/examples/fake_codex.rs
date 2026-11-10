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
//! Threads are kept in `fake_threads/<id>.json` of `CODEX_HOME`, which it requires so that it never writes in a real
//! home: `thread/resume` of an id with no file there is refused as Codex refuses it. The window the reads report is
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

    fn load(&self, id: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.thread_file(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, id: &str, thread: &Value) {
        let _ = std::fs::create_dir_all(self.home.join("fake_threads"));
        let _ = std::fs::write(self.thread_file(id), thread.to_string());
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
    if let Some(path) = std::env::var_os("COQUI_ACP_FAKE_CODEX_LOG") {
        let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", json!({"args": args, "env_names": names}));
        }
    }
    let Some(home) = std::env::var_os("CODEX_HOME") else {
        eprintln!("fake_codex: set CODEX_HOME, so that it never writes in a real home");
        std::process::exit(2);
    };
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
