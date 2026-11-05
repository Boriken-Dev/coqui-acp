//! The ACP agent, version 1 through coqui-acp's own client and version 2 by hand, against the replaying stand-in.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use coqui_acp::{Decider, Decision, Event as ClientEvent, PermissionRequest};
use coqui_acp_claude::harness::Settings;
use coqui_acp_claude::{ClaudeAgent, ClaudeLaunch};
use serde_json::{Value, json};
use support::{Peer, Scratch, fixtures, replay_program};

/// The update kinds Canoa's chat renders, from its recorded coding task (`app/test/fixtures/coding_task_updates.jsonl` in Boriken-Dev/canoa).
const CANOA_KINDS: [&str; 17] = [
    "agent_message",
    "agent_message_chunk",
    "agent_thought",
    "agent_thought_chunk",
    "available_commands_update",
    "compaction_summary_chunk",
    "compaction_update",
    "notice",
    "plan_update",
    "session_info_update",
    "state_update",
    "terminal_update",
    "tool_call_content_chunk",
    "tool_call_update",
    "usage_update",
    "user_message",
    "user_message_chunk",
];

fn agent(recording: Option<&str>, config: Option<&std::path::Path>) -> ClaudeAgent {
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    if let Some(recording) = recording {
        defaults.env.insert(
            "COQUI_ACP_CLAUDE_REPLAY".into(),
            fixtures().join(recording).into(),
        );
    }
    if let Some(config) = config {
        defaults
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), config.into());
    }
    ClaudeAgent::new().defaults(defaults)
}

struct Answers(std::sync::Mutex<Vec<Decision>>);

impl Decider for Answers {
    fn decide(&self, _request: &PermissionRequest) -> Decision {
        self.0.lock().unwrap().remove(0)
    }
}

#[test]
fn version_1_through_coqui_acps_client_runs_a_turn_with_permissions() {
    let scratch = Scratch::new("v1");
    let (agent_reads, client_writes) = std::io::pipe().unwrap();
    let (client_reads, agent_writes) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        let _ = coqui_acp_claude::harness::serve(
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
    ]));
    let session = coqui_acp::Client::builder(scratch.0.to_string_lossy())
        .decider(Arc::new(answers))
        .prompt("Change two to three in notes.txt, then write new.txt.")
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
    let permissions: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, ClientEvent::Permission { .. }))
        .collect();
    assert_eq!(permissions.len(), 3, "{events:?}");
    assert!(events.iter().any(
        |event| matches!(event, ClientEvent::Tool { title, .. } if title.starts_with("Edit "))
    ));
    assert!(events.iter().any(
        |event| matches!(event, ClientEvent::Text(text) if text.contains("Changed two to three"))
    ));
    session.close();
}

fn v2_session(peer: &mut Peer, cwd: &std::path::Path, params: Value) -> String {
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], 2);
    assert!(answer["result"]["capabilities"]["session"]["prompt"]["image"].is_object());
    let mut params = params;
    params["cwd"] = json!(cwd.to_string_lossy());
    let opened = peer.request("session/new", params);
    opened["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_string()
}

fn v2_turn(peer: &mut Peer, session: &str, text: &str) -> Value {
    let from = peer.updates.len();
    let answer = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
    );
    assert!(answer["result"]["messageId"].is_string(), "{answer}");
    peer.until(from, |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "idle"
    })
}

#[test]
fn version_2_runs_a_turn_with_its_own_shapes() {
    let scratch = Scratch::new("v2");
    let mut peer = Peer::serve(agent(Some("turn"), None), Settings::new());
    let session = v2_session(&mut peer, &scratch.0, json!({}));
    peer.answers
        .extend(["allow_once", "reject_once", "allow_always"].map(String::from));
    let idle = v2_turn(&mut peer, &session, "Change two to three.");
    assert_eq!(idle["stopReason"], "end_turn");
    assert_eq!(idle["usage"]["inputTokens"], 77);
    assert_eq!(peer.asked.len(), 3);
    assert_eq!(peer.asked[0]["subject"]["type"], "tool_call");
    assert!(peer.asked[0]["title"].as_str().unwrap().starts_with("Edit"));
    let kinds = peer.kinds();
    assert!(
        !kinds.iter().any(|kind| kind == "tool_call"),
        "version 2 upserts with tool_call_update"
    );
    let chunk = peer
        .updates
        .iter()
        .find(|update| update["sessionUpdate"] == "agent_message_chunk")
        .unwrap();
    assert!(chunk["messageId"].is_string());
    let diff = peer
        .updates
        .iter()
        .filter_map(|update| update["content"].as_array())
        .flatten()
        .find(|content| content["type"] == "diff" && content["patch"].is_object())
        .expect("a finished edit's patch");
    assert_eq!(diff["changes"][0]["operation"], "modify");
    let states: Vec<_> = peer
        .updates
        .iter()
        .filter(|update| update["sessionUpdate"] == "state_update")
        .map(|update| update["state"].clone())
        .collect();
    assert_eq!(states.first().unwrap(), "running");
    assert!(states.contains(&json!("requires_action")));
}

#[test]
fn model_and_effort_are_config_options_changed_between_turns() {
    let scratch = Scratch::new("options");
    let mut peer = Peer::serve(agent(Some("turn"), None), Settings::new());
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], 1);
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let options = opened["result"]["configOptions"].as_array().unwrap();
    assert_eq!(options[0]["id"], "model");
    assert_eq!(options[0]["category"], "model");
    let models: Vec<_> = options[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["value"].clone())
        .collect();
    assert!(models.contains(&json!("haiku")));
    assert_eq!(options[1]["category"], "thought_level");
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "model", "value": "haiku"}),
    );
    assert_eq!(
        changed["result"]["configOptions"][0]["currentValue"],
        "haiku"
    );
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "effort", "value": "low"}),
    );
    assert_eq!(changed["result"]["configOptions"][1]["currentValue"], "low");
    let refused = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "model", "value": "no-such-model"}),
    );
    assert_eq!(refused["error"]["code"], -32602);
}

#[test]
fn a_cancel_interrupts_the_turn_and_it_ends_cancelled() {
    let scratch = Scratch::new("cancel");
    let mut peer = Peer::serve(agent(Some("interrupt"), None), Settings::new());
    let session = v2_session(&mut peer, &scratch.0, json!({}));
    let answer = peer.request(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": "Take your time."}]}),
    );
    assert!(answer["result"]["messageId"].is_string());
    peer.notify("session/cancel", json!({"sessionId": session}));
    let idle = peer.until(0, |update| {
        update["sessionUpdate"] == "state_update" && update["state"] == "idle"
    });
    assert_eq!(idle["stopReason"], "cancelled");
}

#[test]
fn every_update_kind_canoas_chat_renders_appears_in_version_2() {
    let mut kinds = BTreeSet::new();
    for (recording, answers, turns) in [
        (
            "turn",
            vec!["allow_once", "reject_once", "allow_always"],
            vec!["Change two to three."],
        ),
        ("tasks", vec![], vec!["Plan the change."]),
        ("compact", vec![], vec!["Say something.", "/compact"]),
        ("errors", vec![], vec!["Hit the limit."]),
    ] {
        let scratch = Scratch::new(recording);
        let mut peer = Peer::serve(agent(Some(recording), None), Settings::new());
        let session = v2_session(&mut peer, &scratch.0, json!({}));
        peer.answers.extend(answers.into_iter().map(String::from));
        for text in turns {
            v2_turn(&mut peer, &session, text);
        }
        kinds.extend(peer.kinds());
    }
    // History replayed: the person's messages as chunks.
    let scratch = Scratch::new("load");
    let config = scratch.0.join("config");
    let project = config
        .join("projects")
        .join(support::project_directory(&scratch.0));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::copy(
        fixtures().join("turn.transcript.jsonl"),
        project.join("6f2c1b0e-0000-4000-8000-00000000c0de.jsonl"),
    )
    .unwrap();
    let mut peer = Peer::serve(agent(None, Some(&config)), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let resumed = peer.request(
        "session/resume",
        json!({"sessionId": "6f2c1b0e-0000-4000-8000-00000000c0de", "cwd": scratch.0.to_string_lossy(), "replayFrom": {"type": "start"}}),
    );
    assert!(resumed["result"]["configOptions"].is_array(), "{resumed}");
    let replayed = peer.kinds();
    assert!(
        replayed.iter().any(|kind| kind == "user_message_chunk"),
        "{replayed:?}"
    );
    assert!(
        replayed.iter().any(|kind| kind == "tool_call_update"),
        "{replayed:?}"
    );
    kinds.extend(replayed);
    let missing: Vec<_> = CANOA_KINDS
        .iter()
        .filter(|kind| !kinds.contains(**kind))
        .collect();
    assert!(missing.is_empty(), "missing {missing:?}; seen {kinds:?}");
}

#[test]
fn version_1_loads_a_session_with_its_history_before_the_answer() {
    let scratch = Scratch::new("load-v1");
    let config = scratch.0.join("config");
    let project = config
        .join("projects")
        .join(support::project_directory(&scratch.0));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::copy(
        fixtures().join("turn.transcript.jsonl"),
        project.join("6f2c1b0e-0000-4000-8000-00000000c0de.jsonl"),
    )
    .unwrap();
    let mut peer = Peer::serve(agent(None, Some(&config)), Settings::new());
    let answer = peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    assert_eq!(answer["result"]["agentCapabilities"]["loadSession"], true);
    let loaded = peer.request(
        "session/load",
        json!({"sessionId": "6f2c1b0e-0000-4000-8000-00000000c0de", "cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    assert!(loaded["result"]["configOptions"].is_array(), "{loaded}");
    let kinds = peer.kinds();
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("user_message_chunk"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "tool_call"),
        "version 1 announces each replayed tool call"
    );
    assert!(
        !kinds
            .iter()
            .any(|kind| kind == "user_message" || kind == "agent_message"),
        "no version 2 kind"
    );
    let refused = peer.request(
        "session/load",
        json!({"sessionId": "../escape", "cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    assert_eq!(refused["error"]["code"], -32602);
}

#[test]
fn what_the_client_gives_a_session_reaches_claude_codes_command_line() {
    let scratch = Scratch::new("parts");
    let args = scratch.0.join("args.json");
    let copy = scratch.0.join("mcp.json");
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults
        .env
        .insert("COQUI_ACP_CLAUDE_ARGS".into(), args.clone().into());
    defaults
        .env
        .insert("COQUI_ACP_CLAUDE_COPY".into(), copy.clone().into());
    let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let plugin = scratch.0.join("canoa-profile");
    let extra = scratch.0.join("extra");
    let opened = peer.request(
        "session/new",
        json!({
            "cwd": scratch.0.to_string_lossy(),
            "mcpServers": [{"type": "stdio", "name": "echo", "command": scratch.0.join("echo").to_string_lossy(), "args": ["-v"], "env": [{"name": "TOKEN", "value": "t"}]},
                           {"type": "http", "name": "docs", "url": "https://docs.example.invalid/mcp", "headers": []}],
            "systemPrompt": {"mode": "append", "content": [{"type": "text", "text": "Answer briefly."}]},
            "additionalDirectories": [extra.to_string_lossy()],
            "_meta": {"claudeCode": {"options": {"plugins": [{"type": "local", "path": plugin.to_string_lossy()}]}}}
        }),
    );
    assert!(opened["result"]["sessionId"].is_string(), "{opened}");
    let args: Vec<String> = serde_json::from_slice(&std::fs::read(&args).unwrap()).unwrap();
    let has = |expected: &str| args.iter().any(|arg| arg == expected);
    assert!(has("--append-system-prompt=Answer briefly."), "{args:?}");
    assert!(
        has(&format!("--add-dir={}", extra.to_string_lossy())),
        "{args:?}"
    );
    assert!(
        has(&format!("--plugin-dir={}", plugin.to_string_lossy())),
        "{args:?}"
    );
    assert!(
        args.iter().any(|arg| arg.starts_with("--mcp-config=")),
        "{args:?}"
    );
    let config: Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["echo"]["type"], "stdio");
    assert_eq!(config["mcpServers"]["docs"]["type"], "http");
    let relative = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "_meta": {"claudeCode": {"options": {"plugins": [{"type": "local", "path": "relative"}]}}}}),
    );
    assert_eq!(
        relative["error"]["code"], -32602,
        "a plugin's path must be absolute"
    );
}

#[test]
fn a_client_server_cannot_take_the_name_of_one_the_host_gave() {
    let scratch = Scratch::new("names");
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.mcp_servers = vec![coqui_acp_claude::harness::McpServer::new(
        "files",
        coqui_acp_claude::harness::McpTransport::Stdio {
            command: "/opt/files".into(),
            args: Vec::new(),
            env: Vec::new(),
        },
    )];
    let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let command = scratch.0.join("x").to_string_lossy().into_owned();
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": [{"type": "stdio", "name": "files", "command": command, "args": [], "env": []}]}),
    );
    assert_eq!(opened["error"]["code"], -32602, "{opened}");
}

#[test]
fn a_load_finds_only_the_transcript_of_its_own_directory() {
    let scratch = Scratch::new("load-elsewhere");
    let config = scratch.0.join("config");
    let elsewhere = config.join("projects").join("C--some-other-project");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::copy(
        fixtures().join("turn.transcript.jsonl"),
        elsewhere.join("6f2c1b0e-0000-4000-8000-00000000c0de.jsonl"),
    )
    .unwrap();
    let mut peer = Peer::serve(agent(None, Some(&config)), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let loaded = peer.request(
        "session/load",
        json!({"sessionId": "6f2c1b0e-0000-4000-8000-00000000c0de", "cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    assert_eq!(loaded["error"]["code"], -32602, "{loaded}");
    assert!(
        peer.updates.is_empty(),
        "nothing of another directory's session is told"
    );
}

#[test]
fn a_host_can_refuse_plugins_from_a_client() {
    let scratch = Scratch::new("no-plugins");
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    let mut peer = Peer::serve(
        ClaudeAgent::new().defaults(defaults).client_plugins(false),
        Settings::new(),
    );
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let plugin = scratch.0.join("canoa-profile");
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "_meta": {"claudeCode": {"options": {"plugins": [{"type": "local", "path": plugin.to_string_lossy()}]}}}}),
    );
    assert_eq!(opened["error"]["code"], -32602, "{opened}");
}
