//! The ACP agent, version 1 through coqui-acp's own client and version 2 by hand, against the replaying stand-in.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use coqui_acp::{Decider, Decision, Event as ClientEvent, PermissionRequest};
use coqui_acp_codex::harness::Settings;
use coqui_acp_codex::{CodexAgent, CodexLaunch};
use serde_json::{Value, json};
use support::{Peer, Scratch, fixtures, replay_program};

fn agent(recording: Option<&str>, inputs: Option<&std::path::Path>) -> CodexAgent {
    let mut defaults = CodexLaunch::new(".");
    defaults.executable = replay_program().into();
    if let Some(recording) = recording {
        defaults.env.insert(
            "COQUI_ACP_CODEX_REPLAY".into(),
            fixtures().join(recording).into(),
        );
    }
    if let Some(inputs) = inputs {
        defaults
            .env
            .insert("COQUI_ACP_CODEX_INPUTS".into(), inputs.into());
    }
    CodexAgent::new().defaults(defaults)
}

struct Answers(std::sync::Mutex<Vec<Decision>>);

impl Decider for Answers {
    fn decide(&self, _request: &PermissionRequest) -> Decision {
        self.0.lock().unwrap().remove(0)
    }
}

#[test]
fn version_1_through_coqui_acps_client_runs_a_turn_with_approvals() {
    let scratch = Scratch::new("v1");
    let (agent_reads, client_writes) = std::io::pipe().unwrap();
    let (client_reads, agent_writes) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        let _ = coqui_acp_codex::harness::serve(
            Arc::new(agent(Some("turn"), None)),
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
    let answers = Answers(std::sync::Mutex::new(vec![
        Decision::Allow,
        Decision::Deny("no".into()),
        Decision::Allow,
        Decision::Allow,
    ]));
    let session = coqui_acp::Client::builder(scratch.0.to_string_lossy())
        .decider(Arc::new(answers))
        .prompt("Change two to three in notes.txt.")
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
        4,
        "{events:?}"
    );
    assert!(events.iter().any(
        |event| matches!(event, ClientEvent::Text(text) if text.contains("Changed two to three"))
    ));
    session.close();
}

fn v2_session(peer: &mut Peer, cwd: &std::path::Path, params: Value) -> Value {
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], 2);
    assert_eq!(answer["result"]["info"]["title"], "Codex 0.164.0");
    let mut params = params;
    params["cwd"] = json!(cwd.to_string_lossy());
    peer.request("session/new", params)
}

#[test]
fn version_2_runs_a_turn_with_its_own_shapes_and_every_part_applied() {
    let scratch = Scratch::new("v2");
    let written = scratch.0.join("inputs.jsonl");
    let mut peer = Peer::serve(agent(Some("turn"), Some(&written)), Settings::new());
    let skills = scratch.0.join("profile");
    let opened = v2_session(
        &mut peer,
        &scratch.0,
        json!({
            "mcpServers": [{"type": "stdio", "name": "echo", "command": scratch.0.join("echo").to_string_lossy(), "args": [], "env": [{"name": "TOKEN", "value": "t"}]},
                           {"type": "http", "name": "docs", "url": "https://docs.example.invalid/mcp", "headers": [{"name": "Authorization", "value": "Bearer x"}]}],
            "systemPrompt": {"mode": "append", "content": [{"type": "text", "text": "Answer briefly."}]},
            "additionalDirectories": [skills.to_string_lossy()]
        }),
    );
    let session = opened["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_string();
    assert_eq!(session, support::recorded_thread("turn"));
    let options = opened["result"]["configOptions"].as_array().unwrap();
    assert_eq!(options[0]["configId"], "model");
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "model", "value": "gpt-6-luna"}),
    );
    assert_eq!(
        changed["result"]["configOptions"][0]["currentValue"],
        "gpt-6-luna"
    );
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "effort", "value": "low"}),
    );
    assert_eq!(changed["result"]["configOptions"][1]["currentValue"], "low");
    peer.answers.extend(
        [
            "allow_once",
            "reject_once",
            "accept_for_session",
            "allow_once",
        ]
        .map(String::from),
    );
    let from = peer.updates.len();
    let answer = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Change two to three."}]}),
    );
    assert!(answer["result"]["messageId"].is_string(), "{answer}");
    peer.until(from, |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "idle"
    });
    assert_eq!(peer.updates.last().unwrap()["stopReason"], "end_turn");
    assert_eq!(peer.asked.len(), 4);
    assert_eq!(peer.asked[0]["subject"]["type"], "tool_call");
    let kinds = peer.kinds();
    for kind in [
        "terminal_update",
        "tool_call_update",
        "agent_message_chunk",
        "agent_message",
        "agent_thought_chunk",
        "usage_update",
        "session_info_update",
    ] {
        assert!(kinds.iter().any(|seen| seen == kind), "{kind} in {kinds:?}");
    }
    assert!(!kinds.iter().any(|kind| kind == "tool_call"));
    let sent: Vec<Value> = std::fs::read_to_string(&written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skills_root = sent
        .iter()
        .find(|input| input["method"] == "skills/extraRoots/set")
        .expect("the skill root is set");
    assert!(
        skills_root["params"]["extraRoots"][0]
            .as_str()
            .unwrap()
            .ends_with("skills")
    );
    let start = sent
        .iter()
        .find(|input| input["method"] == "thread/start")
        .unwrap();
    assert_eq!(start["params"]["developerInstructions"], "Answer briefly.");
    assert_eq!(
        start["params"]["config"]["mcp_servers"]["echo"]["env"]["TOKEN"],
        "t"
    );
    assert_eq!(
        start["params"]["config"]["mcp_servers"]["docs"]["http_headers"]["Authorization"],
        "Bearer x"
    );
    assert_eq!(
        start["params"]["config"]["sandbox_workspace_write"]["writable_roots"][0],
        skills.to_string_lossy().as_ref()
    );
    let turn = sent
        .iter()
        .find(|input| input["method"] == "turn/start")
        .unwrap();
    assert_eq!(
        (
            turn["params"]["model"].as_str(),
            turn["params"]["effort"].as_str()
        ),
        (Some("gpt-6-luna"), Some("low"))
    );
}

#[test]
fn a_cancel_interrupts_the_turn_and_it_ends_cancelled() {
    let scratch = Scratch::new("cancel");
    let mut peer = Peer::serve(agent(Some("interrupt"), None), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let from = peer.updates.len();
    peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Take your time."}]}),
    );
    peer.notify("session/cancel", json!({"sessionId": session}));
    peer.until(from, |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "idle"
    });
    assert_eq!(peer.updates.last().unwrap()["stopReason"], "cancelled");
}

#[test]
fn a_limit_ends_the_turn_as_a_refusal_with_its_meta() {
    let scratch = Scratch::new("limit");
    let mut peer = Peer::serve(agent(Some("errors"), None), Settings::new());
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    assert_eq!(
        answer["result"]["agentCapabilities"]["mcpCapabilities"]["sse"],
        false
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let ended = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Hit the limit."}]}),
    );
    assert_eq!(ended["result"]["stopReason"], "refusal", "{ended}");
    assert!(
        ended["result"]["_meta"]["dev.boriken.coqui.acp/limit"]["text"]
            .as_str()
            .unwrap()
            .contains("429")
    );
    let failed = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Fail."}]}),
    );
    assert_eq!(failed["error"]["code"], -32603, "{failed}");
}

#[test]
fn a_version_1_load_replays_the_threads_history_and_a_bad_id_is_refused() {
    let scratch = Scratch::new("load");
    let mut peer = Peer::serve(agent(Some("resume"), None), Settings::new());
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    assert_eq!(answer["result"]["agentCapabilities"]["loadSession"], true);
    let refused = peer.request(
        "session/load",
        json!({"sessionId": "../x", "cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    assert_eq!(refused["error"]["code"], -32602);
    let sse = peer.request("session/new", json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": [{"type": "sse", "name": "s", "url": "u", "headers": []}]}));
    assert_eq!(sse["error"]["code"], -32602, "Codex has no SSE transport");
}

/// A thread of another project is not replayed to a client held to its roots, whatever id it names.
#[test]
fn a_thread_outside_the_working_roots_is_not_opened() {
    let scratch = Scratch::new("roots");
    let mut settings = Settings::new();
    settings.working_roots = vec![scratch.0.clone()];
    let mut peer = Peer::serve(agent(Some("resume"), None), settings);
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let thread = support::recorded("resume")
        .iter()
        .find_map(|line| line.pointer("/result/thread/id").cloned())
        .expect("a resumed thread");
    let refused = peer.request(
        "session/load",
        json!({"sessionId": thread, "cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    assert!(refused.get("error").is_some(), "{refused}");
    assert!(
        !peer.kinds().iter().any(|kind| kind == "user_message_chunk"),
        "nothing of it was told"
    );
}

/// `session/set_config_option` names a value's type in version 2.
#[test]
fn a_config_value_may_name_its_type() {
    let scratch = Scratch::new("type");
    let mut peer = Peer::serve(agent(Some("turn"), None), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}}),
    );
    let opened = peer.request("session/new", json!({"cwd": scratch.0.to_string_lossy()}));
    let session = opened["result"]["sessionId"].clone();
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "effort", "type": "id", "value": "low"}),
    );
    assert_eq!(changed["result"]["configOptions"][1]["currentValue"], "low");
}

/// The `mode` option of a version 2 `configOptions`: its current value and the ids it offers.
fn mode_of(options: &Value) -> Option<(String, Vec<String>)> {
    let option = options
        .as_array()?
        .iter()
        .find(|option| option["configId"] == "mode")?;
    assert_eq!(option["category"], "mode");
    let offered = option["options"]
        .as_array()?
        .iter()
        .map(|value| value["value"].as_str().unwrap().to_string())
        .collect();
    Some((option["currentValue"].as_str()?.to_string(), offered))
}

fn mode_agent(inputs: &std::path::Path, allow_bypass: bool) -> CodexAgent {
    let mut defaults = CodexLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.allow_bypass = allow_bypass;
    defaults.env.insert(
        "COQUI_ACP_CODEX_REPLAY".into(),
        fixtures().join("turn").into(),
    );
    defaults
        .env
        .insert("COQUI_ACP_CODEX_INPUTS".into(), inputs.into());
    CodexAgent::new().defaults(defaults)
}

/// What was written to the stand-in, one message a line.
fn written_to(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn run_a_turn(peer: &mut Peer, session: &str) {
    peer.answers
        .extend(["allow_once"; 4].into_iter().map(String::from));
    let from = peer.updates.len();
    peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Change two to three."}]}),
    );
    peer.until(from, |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "idle"
    });
}

fn the_turn_start(path: &std::path::Path) -> Value {
    written_to(path)
        .into_iter()
        .find(|input| input["method"] == "turn/start")
        .unwrap()["params"]
        .clone()
}

/// Codex 0.166.0 takes its approval policy, sandbox, reviewer and collaboration mode with each `turn/start`, so a
/// mode changes at the next turn; before one is chosen, a turn names none of them and the configuration holds.
#[test]
fn the_mode_is_an_option_whose_choice_each_later_turn_carries() {
    let scratch = Scratch::new("mode");
    let written = scratch.0.join("inputs.jsonl");
    let mut peer = Peer::serve(mode_agent(&written, false), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    // The recording's thread asks for everything: `untrusted`, read-only.
    let (current, offered) = mode_of(&opened["result"]["configOptions"]).expect("a mode option");
    assert_eq!(current, "manual");
    assert_eq!(offered, ["manual", "accept_edits", "auto", "plan"]);
    let refused = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass", "type": "id"}),
    );
    assert!(refused["error"].is_object(), "{refused}");
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "plan", "type": "id"}),
    );
    assert_eq!(
        mode_of(&changed["result"]["configOptions"]).unwrap().0,
        "plan",
        "{changed}"
    );
    run_a_turn(&mut peer, &session);
    let params = the_turn_start(&written);
    assert_eq!(params["collaborationMode"]["mode"], "plan", "{params}");
    assert_eq!(
        params["collaborationMode"]["settings"]["model"],
        "gpt-6.1-sol"
    );
    assert_eq!(params["approvalPolicy"], "on-request");
    assert_eq!(params["sandboxPolicy"]["type"], "readOnly");
    assert_eq!(params["approvalsReviewer"], "user");
}

#[test]
fn a_turn_before_any_mode_is_chosen_names_none_of_the_mode_settings() {
    let scratch = Scratch::new("nomode");
    let written = scratch.0.join("inputs.jsonl");
    let mut peer = Peer::serve(mode_agent(&written, false), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    run_a_turn(&mut peer, &session);
    let params = the_turn_start(&written);
    for key in [
        "collaborationMode",
        "approvalPolicy",
        "sandboxPolicy",
        "approvalsReviewer",
    ] {
        assert!(params.get(key).is_none(), "{key} in {params}");
    }
}

#[test]
fn bypass_is_offered_once_the_host_allows_it_and_neither_asks_nor_sandboxes() {
    let scratch = Scratch::new("bypass");
    let written = scratch.0.join("inputs.jsonl");
    let mut peer = Peer::serve(mode_agent(&written, true), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let (_, offered) = mode_of(&opened["result"]["configOptions"]).unwrap();
    assert_eq!(
        offered,
        ["manual", "accept_edits", "auto", "plan", "bypass"]
    );
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass", "type": "id"}),
    );
    run_a_turn(&mut peer, &session);
    let params = the_turn_start(&written);
    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandboxPolicy"]["type"], "dangerFullAccess");
    assert_eq!(params["collaborationMode"]["mode"], "default");
}

#[test]
fn auto_names_codexs_automatic_review_in_a_sandbox_that_may_write_the_workspace() {
    let scratch = Scratch::new("auto");
    let written = scratch.0.join("inputs.jsonl");
    let mut peer = Peer::serve(mode_agent(&written, false), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "auto", "type": "id"}),
    );
    run_a_turn(&mut peer, &session);
    let params = the_turn_start(&written);
    assert_eq!(params["approvalPolicy"], "on-request");
    assert_eq!(params["approvalsReviewer"], "auto_review");
    assert_eq!(params["sandboxPolicy"]["type"], "workspaceWrite");
}
