//! The provider driven end to end through the real `codex`, against a stand-in for the model on loopback: a host
//! seeds a session at a UUID with no history, resumes it, prompts it, and reads its history back under that UUID.
//!
//! Not run by default: it needs Codex installed and a stand-in model answering on `COQUI_ACP_STANDIN_URL`
//! (`tools/record_harnesses/responses_model.py`). Nothing is spent and nothing of the person's is read: the session
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
use coqui_acp_codex::{CodexProvider, ProviderSettings};
use coqui_acp_harness::LocalSpawner;
use serde_json::{Value, json};
use support::Scratch;

const ID: &str = "6e2d3b88-4c5f-4071-8b92-1d3c8e0f2a46";

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

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn history(provider: &CodexProvider, work: &str, environment: &Environment) -> String {
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
#[ignore = "needs Codex installed and a stand-in model: set COQUI_ACP_STANDIN_URL"]
fn a_seeded_empty_session_is_resumed_prompted_and_exported_under_its_uuid() {
    let url = std::env::var("COQUI_ACP_STANDIN_URL").expect("COQUI_ACP_STANDIN_URL");
    let scratch = Scratch::new("live-provider");
    let work = scratch.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work_text = work.to_string_lossy().into_owned();
    let home = scratch.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Codex's own configuration names the stand-in as the model provider.
    std::fs::write(
        home.join("config.toml"),
        format!(
            "model_provider = \"standin\"\n\n[model_providers.standin]\nname = \"standin\"\nbase_url = \"{url}/v1\"\nenv_key = \"STANDIN_API_KEY\"\nwire_api = \"responses\"\n"
        ),
    )
    .unwrap();
    let mut environment = Environment::new();
    environment.insert("CODEX_HOME", home.to_string_lossy().into_owned());
    environment.insert("STANDIN_API_KEY", "dummy-stand-in-key");
    let provider = CodexProvider::new(ProviderSettings::default());

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
    // One rollout, named for the thread Codex assigned.
    let found = coqui_acp_codex::history::rollout(&home, ID);
    assert!(
        found.is_err(),
        "no rollout is named for the host's UUID: Codex gave the thread an id of its own"
    );
    let sessions = home.join("sessions");
    let rollouts: Vec<_> = walk(&sessions);
    assert_eq!(
        rollouts.len(),
        1,
        "both turns are in one thread's rollout: {rollouts:?}"
    );
    let text = std::fs::read_to_string(&rollouts[0]).unwrap();
    assert!(text.contains("hello") && text.contains("and again"));
}
