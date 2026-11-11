//! The ACP agent, version 1 through coqui-acp's own client and version 2 by hand, against the stand-in.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use coqui_acp::{Decider, Decision, Event as ClientEvent, PermissionRequest};
use coqui_acp_pi::harness::{Mode, Settings};
use coqui_acp_pi::{PiAgent, PiLaunch};
use serde_json::{Value, json};
use support::{Peer, Scratch, launching};

fn agent(cwd: &Path, home: &Path, tweak: impl FnOnce(&mut PiLaunch)) -> PiAgent {
    let mut defaults = launching(cwd, home);
    tweak(&mut defaults);
    PiAgent::new().defaults(defaults)
}

struct Answers(std::sync::Mutex<Vec<Decision>>);

impl Decider for Answers {
    fn decide(&self, _request: &PermissionRequest) -> Decision {
        self.0.lock().unwrap().remove(0)
    }
}

#[test]
fn version_1_through_coqui_acps_client_runs_a_turn_with_a_question() {
    let scratch = Scratch::new("v1");
    let home = scratch.dir("home");
    let work = scratch.dir("work");
    let (agent_reads, client_writes) = std::io::pipe().unwrap();
    let (client_reads, agent_writes) = std::io::pipe().unwrap();
    let served = agent(&work, &home, |_| {});
    std::thread::spawn(move || {
        let _ = coqui_acp_pi::harness::serve(
            Arc::new(served),
            Settings::new(),
            agent_reads,
            agent_writes,
        );
    });
    let (transport, _) = coqui_acp::lines::over_io(
        client_reads,
        client_writes,
        8,
        coqui_acp::lines::DEFAULT_MAX_LINE,
    )
    .unwrap();
    let session = coqui_acp::Client::builder(work.to_string_lossy())
        .decider(Arc::new(Answers(std::sync::Mutex::new(vec![
            Decision::Allow,
        ]))))
        .prompt("ask")
        .connect(transport)
        .unwrap();
    let mut events = Vec::new();
    loop {
        let event = session
            .next_event(Duration::from_secs(30))
            .expect("the turn did not end");
        let done = matches!(
            event,
            ClientEvent::Completed { .. } | ClientEvent::Failed(_)
        );
        events.push(event);
        if done {
            break;
        }
    }
    assert!(
        matches!(events.last(), Some(ClientEvent::Completed { stop }) if stop == "end_turn"),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ClientEvent::Permission { .. }))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        events.iter().any(
            |event| matches!(event, ClientEvent::Text(text) if text.contains("echo: allowed"))
        )
    );
    assert!(work.join("checked.txt").exists());
    session.close();
}

fn v2_session(peer: &mut Peer, cwd: &Path, params: Value) -> Value {
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], 2);
    assert_eq!(answer["result"]["info"]["title"], "Pi 1.5.2");
    let mut params = params;
    params["cwd"] = json!(cwd.to_string_lossy());
    peer.request("session/new", params)
}

fn idle(update: &Value) -> bool {
    update["sessionUpdate"] == "state_update" && update["state"] == "idle"
}

fn prompt(peer: &mut Peer, session: &str, text: &str) -> usize {
    let from = peer.updates.len();
    let answer = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
    );
    assert!(answer["result"]["messageId"].is_string(), "{answer}");
    peer.until(from, idle);
    from
}

fn said_since(peer: &Peer, from: usize) -> String {
    peer.updates[from..]
        .iter()
        .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|update| update["content"]["text"].as_str())
        .collect()
}

#[test]
fn version_2_offers_the_model_the_thinking_level_and_the_modes_pi_has() {
    let scratch = Scratch::new("v2");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut peer = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    let opened = v2_session(
        &mut peer,
        &work,
        json!({"systemPrompt": {"mode": "append", "content": [{"type": "text", "text": "Be brief."}]}}),
    );
    let session = opened["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_string();
    let options = opened["result"]["configOptions"].as_array().unwrap();
    let ids: Vec<_> = options
        .iter()
        .map(|option| option["configId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["model", "effort", "mode"]);
    assert_eq!(options[0]["category"], "model");
    assert_eq!(options[0]["currentValue"], "stub/stub-think");
    assert_eq!(options[1]["category"], "thought_level");
    assert_eq!(options[1]["currentValue"], "medium");
    let modes: Vec<_> = options[2]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mode| mode["value"].as_str().unwrap())
        .collect();
    assert_eq!(
        modes,
        ["manual", "plan"],
        "Auto is never offered, Bypass not allowed, and nothing is looser than the start"
    );
    assert_eq!(options[2]["currentValue"], "manual");

    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "model", "value": "stub/stub-fast"}),
    );
    let after = changed["result"]["configOptions"].as_array().unwrap();
    assert_eq!(after[0]["currentValue"], "stub/stub-fast");
    assert_eq!(after[1]["currentValue"], "off");
    assert_eq!(
        after[1]["options"].as_array().unwrap().len(),
        1,
        "a model without reasoning takes the one level"
    );
    let refused = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "effort", "value": "high"}),
    );
    assert!(refused["error"].is_object(), "{refused}");
    let refused = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass"}),
    );
    assert!(refused["error"].is_object(), "{refused}");

    let from = prompt(&mut peer, &session, "hello");
    assert_eq!(said_since(&peer, from), "echo: hello");
    assert_eq!(peer.updates.last().unwrap()["stopReason"], "end_turn");
    let kinds = peer.kinds();
    for kind in [
        "agent_message_chunk",
        "agent_message",
        "usage_update",
        "session_info_update",
        "available_commands_update",
    ] {
        assert!(kinds.iter().any(|seen| seen == kind), "{kind} in {kinds:?}");
    }
}

#[test]
fn a_question_is_a_permission_request_and_the_mode_chosen_decides_whether_one_comes() {
    let scratch = Scratch::new("permission");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut peer = Peer::serve(
        agent(&work, &home, |launch| {
            launch.allow_bypass = true;
            launch.modes = Some(vec![Mode::AcceptEdits, Mode::Bypass]);
        }),
        Settings::new(),
    );
    let opened = v2_session(&mut peer, &work, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let modes: Vec<_> = opened["result"]["configOptions"][2]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mode| mode["value"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["manual", "accept_edits", "plan", "bypass"]);

    peer.answers.push_back("allow_once".into());
    let from = prompt(&mut peer, &session, "ask");
    assert_eq!(peer.asked.len(), 1);
    assert_eq!(peer.asked[0]["subject"]["type"], "tool_call");
    assert_eq!(peer.asked[0]["title"], "Run echo checked > checked.txt");
    assert_eq!(said_since(&peer, from), "echo: allowed");

    peer.answers.push_back("reject_once".into());
    let from = prompt(&mut peer, &session, "ask");
    assert_eq!(said_since(&peer, from), "echo: refused");

    // Plan refuses without asking, and Bypass allows without asking.
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "plan"}),
    );
    let from = prompt(&mut peer, &session, "ask");
    assert_eq!(said_since(&peer, from), "echo: refused");
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass"}),
    );
    let from = prompt(&mut peer, &session, "ask");
    assert_eq!(said_since(&peer, from), "echo: allowed");
    assert_eq!(peer.asked.len(), 2, "only Manual asked");
}

#[test]
fn a_cancel_stops_the_run_and_it_ends_cancelled() {
    let scratch = Scratch::new("cancel");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut peer = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    let opened = v2_session(&mut peer, &work, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let from = peer.updates.len();
    peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "wait"}]}),
    );
    peer.notify("session/cancel", json!({"sessionId": session}));
    peer.until(from, idle);
    assert_eq!(peer.updates.last().unwrap()["stopReason"], "cancelled");
}

#[test]
fn a_limit_is_a_refusal_with_its_meta_and_another_error_fails_the_prompt() {
    let scratch = Scratch::new("limit");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut peer = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    assert_eq!(answer["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        answer["result"]["agentCapabilities"]["promptCapabilities"]["image"],
        true
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": work.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let ended = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "limit"}]}),
    );
    assert_eq!(ended["result"]["stopReason"], "refusal", "{ended}");
    assert_eq!(
        ended["result"]["_meta"]["dev.boriken.coqui.acp/limit"]["text"],
        "Error 429"
    );
    let failed = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "fail"}]}),
    );
    assert_eq!(failed["error"]["code"], -32603, "{failed}");
    assert!(
        failed["error"]["data"]["message"]
            .as_str()
            .unwrap()
            .starts_with("500: "),
        "{failed}"
    );
}

#[test]
fn a_session_loads_with_its_history_resumes_without_and_an_id_that_is_none_is_refused() {
    let scratch = Scratch::new("load");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut first = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    let opened = v2_session(&mut first, &work, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    prompt(&mut first, &session, "first");
    first.request("session/close", json!({"sessionId": session}));

    let mut peer = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let loaded = peer.request(
        "session/load",
        json!({"sessionId": session, "cwd": work.to_string_lossy(), "mcpServers": []}),
    );
    assert!(loaded["error"].is_null(), "{loaded}");
    let kinds = peer.kinds();
    assert!(
        kinds.iter().any(|kind| kind == "user_message_chunk"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "agent_message_chunk"),
        "{kinds:?}"
    );
    let from = peer.updates.len();
    let ended = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "again"}]}),
    );
    assert_eq!(ended["result"]["stopReason"], "end_turn");
    assert_eq!(said_since(&peer, from), "echo: again");

    let mut resumed = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    resumed.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let answer = resumed.request(
        "session/resume",
        json!({"sessionId": session, "cwd": work.to_string_lossy(), "mcpServers": []}),
    );
    assert!(answer["error"].is_null(), "{answer}");
    assert!(
        !resumed
            .kinds()
            .iter()
            .any(|kind| kind == "user_message_chunk"),
        "a resume replays nothing"
    );

    for bad in ["../x", "00000000-0000-0000-0000-000000000000"] {
        let refused = peer.request(
            "session/load",
            json!({"sessionId": bad, "cwd": work.to_string_lossy(), "mcpServers": []}),
        );
        assert!(refused.get("error").is_some(), "{bad}: {refused}");
    }
}

#[test]
fn what_pi_cannot_take_is_refused_before_pi_is_started() {
    let scratch = Scratch::new("refused");
    let (home, work) = (scratch.dir("home"), scratch.dir("work"));
    let mut peer = Peer::serve(agent(&work, &home, |_| {}), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    for server in [
        json!({"type": "sse", "name": "s", "url": "https://example.invalid/sse", "headers": []}),
        json!({"type": "http", "name": "h", "url": "https://example.invalid/mcp", "headers": []}),
        json!({"name": "c", "command": work.join("c").to_string_lossy(), "args": [], "env": []}),
    ] {
        let refused = peer.request(
            "session/new",
            json!({"cwd": work.to_string_lossy(), "mcpServers": [server]}),
        );
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
    }
    assert!(support::logged(&home).is_empty(), "Pi was never started");
}
