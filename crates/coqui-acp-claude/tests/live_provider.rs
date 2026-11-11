//! The provider driven end to end through the real `claude`, against a stand-in for the model on loopback: a host
//! seeds a session at a UUID with no history, resumes it, prompts it, and reads its history back under that UUID.
//!
//! Not run by default: it needs Claude Code installed and a stand-in model answering on `COQUI_ACP_STANDIN_URL`
//! (`tools/record_harnesses/anthropic_model.py`). Nothing is spent and nothing of the person's is read: the session
//! lives in a configuration directory made for the test.
#![cfg(feature = "history")]
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, AcpHarness, ChannelSink, Environment,
    ProcessSpawner, SessionExport, SessionImport, StartRequest,
};
use coqui_acp_claude::{ClaudeProvider, ProviderSettings};
use coqui_acp_harness::LocalSpawner;
use serde_json::{Value, json};
use support::Scratch;

const ID: &str = "5d1c2a77-3b4e-4f60-9a81-0c2b7d9e1f35";

struct Keeper;

impl AccountKeeper for Keeper {
    fn renewed(&self, _renewal: AccountRenewal) -> Result<(), ProviderError> {
        Ok(())
    }

    fn lost(&self, _loss: AccountLoss) -> Result<(), ProviderError> {
        Ok(())
    }
}

struct Sink(mpsc::Sender<Value>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

fn spawner() -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().into_object()
}

/// Every message up to the answer to request `id`, and that answer.
fn until(received: &mpsc::Receiver<Value>, id: u64) -> (Vec<Value>, Value) {
    let mut told = Vec::new();
    loop {
        let message = received
            .recv_timeout(Duration::from_secs(120))
            .expect("an answer in time");
        if message["id"] == id && message.get("method").is_none() {
            return (told, message);
        }
        told.push(message);
    }
}

fn history(provider: &ClaudeProvider, work: &str, environment: &Environment) -> String {
    let mut joined = String::new();
    let mut after: Option<String> = None;
    loop {
        let mut ask = SessionExport::new(ID, work).environment(environment.clone());
        if let Some(after) = &after {
            ask = ask.after(after.clone());
        }
        let page = provider
            .export_session(ask, spawner(), Keeper.into_object())
            .expect("a page");
        joined.push_str(&page.text);
        match page.next {
            Some(next) => after = Some(next),
            None => return joined,
        }
    }
}

#[test]
#[ignore = "needs Claude Code installed and a stand-in model: set COQUI_ACP_STANDIN_URL"]
fn a_seeded_empty_session_is_resumed_prompted_and_exported_under_its_uuid() {
    let url = std::env::var("COQUI_ACP_STANDIN_URL").expect("COQUI_ACP_STANDIN_URL");
    let scratch = Scratch::new("live-provider");
    let work = scratch.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work_text = work.to_string_lossy().into_owned();
    let config = scratch.0.join("config").to_string_lossy().into_owned();
    let mut environment = Environment::new();
    for (name, value) in [
        ("ANTHROPIC_BASE_URL", url.as_str()),
        ("ANTHROPIC_API_KEY", "dummy-stand-in-key"),
        ("CLAUDE_CONFIG_DIR", config.as_str()),
        ("DISABLE_TELEMETRY", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
        ("DISABLE_ERROR_REPORTING", "1"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ] {
        environment.insert(name, value);
    }
    if cfg!(windows) && std::path::Path::new("C:/Program Files/Git/bin/bash.exe").exists() {
        environment.insert(
            "CLAUDE_CODE_GIT_BASH_PATH",
            "C:/Program Files/Git/bin/bash.exe",
        );
    }
    let mut settings = ProviderSettings::default();
    settings.setting_sources = Some(Vec::new());
    let provider = ClaudeProvider::new(settings);

    // Seeded with nothing, at the host's UUID.
    let report = provider
        .import_session(
            SessionImport::new(ID, &work_text, String::new()).environment(environment.clone()),
            spawner(),
            Keeper.into_object(),
        )
        .expect("an empty seed");
    assert!(report.seeded_from.is_none());

    // Resumed twice, each time on a connection of its own, and prompted once each time.
    for (round, text) in ["hello", "and again"].into_iter().enumerate() {
        let (sender, received) = mpsc::channel();
        let request = StartRequest::builder()
            .environment(environment.clone())
            .build();
        let channel = provider
            .start(
                request,
                spawner(),
                Sink(sender).into_object(),
                Keeper.into_object(),
            )
            .expect("it starts");
        let send = |message: Value| channel.send(message.to_string().as_bytes()).expect("taken");
        send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}}),
        );
        until(&received, 1);
        send(
            json!({"jsonrpc": "2.0", "id": 2, "method": "session/resume",
            "params": {"sessionId": ID, "cwd": work_text, "mcpServers": []}}),
        );
        let (_, resumed) = until(&received, 2);
        assert!(resumed.get("error").is_none(), "round {round}: {resumed}");
        send(
            json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt",
            "params": {"sessionId": ID, "prompt": [{"type": "text", "text": text}]}}),
        );
        let (told, answer) = until(&received, 3);
        assert_eq!(
            answer["result"]["stopReason"], "end_turn",
            "round {round}: {answer}"
        );
        let updates: Vec<&Value> = told
            .iter()
            .filter(|m| m["method"] == "session/update")
            .collect();
        assert!(!updates.is_empty());
        assert!(
            updates.iter().all(|m| m["params"]["sessionId"] == ID),
            "every update names the host's UUID"
        );
        channel.close(0, "");
        drop(channel);
    }

    // The history, under that UUID, holds both turns.
    let text = history(&provider, &work_text, &environment);
    let doc: coqui_acp_history::Document = text.parse().expect("a history");
    assert_eq!(doc.header.id, ID);
    let prompts: Vec<String> = doc
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            coqui_acp_history::Body::Prompt { prompt } => {
                prompt[0]["text"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["hello", "and again"]);
    let answers = doc
        .lines
        .iter()
        .filter(|line| matches!(&line.body, coqui_acp_history::Body::Update { update } if update["sessionUpdate"] == "agent_message_chunk"))
        .count();
    assert_eq!(answers, 2);
    // One transcript, named for the UUID.
    let found =
        coqui_acp_claude::transcript(&scratch.0.join("config"), &work, ID).expect("its transcript");
    assert!(found.ends_with(format!("{ID}.jsonl")));
}
