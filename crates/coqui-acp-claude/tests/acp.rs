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
    ClaudeAgent::new().defaults(defaults).client_plugins(true)
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
    let mut peer = Peer::serve(
        ClaudeAgent::new().defaults(defaults).client_plugins(true),
        Settings::new(),
    );
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
fn plugins_from_a_client_are_refused_unless_the_host_allows_them() {
    let scratch = Scratch::new("no-plugins");
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
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

fn plugin_refused(settings: Settings, cwd: &std::path::Path, plugin: &str) -> Value {
    let mut peer = Peer::serve(agent(None, None), settings);
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    peer.request(
        "session/new",
        json!({"cwd": cwd.to_string_lossy(), "_meta": {"claudeCode": {"options": {"plugins": [
            {"type": "local", "path": plugin}]}}}}),
    )
}

/// What the host narrowed holds for a plugin as for a session's directories.
#[test]
fn a_plugin_is_held_to_the_working_roots_and_the_network_rule() {
    let scratch = Scratch::new("plugin-roots");
    let inside = scratch.0.join("inside");
    let outside = scratch.0.join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let mut settings = Settings::new();
    settings.working_roots = vec![inside.clone()];
    let refused = plugin_refused(settings.clone(), &inside, &outside.to_string_lossy());
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let taken = plugin_refused(settings, &inside, &inside.join("plugin").to_string_lossy());
    assert!(taken.get("result").is_some(), "{taken}");
    let share = if cfg!(windows) {
        r"\\host\share\plugin"
    } else {
        "//host/share/plugin"
    };
    let refused = plugin_refused(Settings::new(), &inside, share);
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let mut open = Settings::new();
    open.network_paths = true;
    let taken = plugin_refused(open, &inside, share);
    assert!(taken.get("result").is_some(), "{taken}");
}

/// A host's own configuration file would silently drop a client's servers.
#[test]
fn a_clients_servers_are_refused_when_the_host_names_its_own_configuration_file() {
    let scratch = Scratch::new("host-file");
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.mcp_config_file = Some(scratch.0.join("mcp.json"));
    let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {}, "info": {"name": "test", "version": "0"}}),
    );
    let refused = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": [{"type": "stdio", "name": "echo",
            "command": scratch.0.join("echo").to_string_lossy(), "args": [], "env": []}]}),
    );
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
}

/// The `mode` option of a session's `configOptions`: its current value and the ids it offers.
fn mode_of(options: &Value) -> Option<(String, Vec<String>)> {
    let option = options
        .as_array()?
        .iter()
        .find(|option| option["id"] == "mode")?;
    assert_eq!(option["category"], "mode");
    let offered = option["options"]
        .as_array()?
        .iter()
        .map(|value| value["value"].as_str().unwrap().to_string())
        .collect();
    Some((option["currentValue"].as_str()?.to_string(), offered))
}

/// What the stand-in was asked, one control request a line.
fn controls(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn mode_agent(controls: &std::path::Path, allow_bypass: bool) -> ClaudeAgent {
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.allow_bypass = allow_bypass;
    // The person's own command line offers every mode; a host's entry gets the capped default.
    defaults.modes = Some(coqui_acp_claude::harness::Mode::ALL.to_vec());
    defaults
        .env
        .insert("COQUI_ACP_CLAUDE_CONTROLS".into(), controls.into());
    ClaudeAgent::new().defaults(defaults)
}

/// Claude Code 2.1.318 takes `set_permission_mode` for every mode but `bypassPermissions`, which it takes only when
/// it was started allowing it.
#[test]
fn the_mode_is_an_option_switched_in_the_session_and_bypass_needs_its_launch_flag() {
    let scratch = Scratch::new("mode");
    let log = scratch.0.join("controls.jsonl");
    let mut peer = Peer::serve(mode_agent(&log, false), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let (current, offered) = mode_of(&opened["result"]["configOptions"]).expect("a mode option");
    assert_eq!(current, "manual");
    assert_eq!(offered, ["manual", "accept_edits", "auto", "plan"]);
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "plan"}),
    );
    assert_eq!(
        mode_of(&changed["result"]["configOptions"]).unwrap().0,
        "plan"
    );
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "accept_edits"}),
    );
    assert_eq!(
        mode_of(&changed["result"]["configOptions"]).unwrap().0,
        "accept_edits"
    );
    let refused = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass"}),
    );
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let modes: Vec<Value> = controls(&log)
        .iter()
        .filter(|control| control["request"]["subtype"] == "set_permission_mode")
        .map(|control| control["request"]["mode"].clone())
        .collect();
    assert_eq!(modes, [json!("plan"), json!("acceptEdits")]);
}

#[test]
fn bypass_is_offered_once_the_host_allows_it_and_switches_to_bypass_permissions() {
    let scratch = Scratch::new("bypass");
    let log = scratch.0.join("controls.jsonl");
    let mut peer = Peer::serve(mode_agent(&log, true), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    let (_, offered) = mode_of(&opened["result"]["configOptions"]).unwrap();
    assert_eq!(
        offered,
        ["manual", "accept_edits", "auto", "plan", "bypass"]
    );
    let changed = peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": "bypass"}),
    );
    assert_eq!(
        mode_of(&changed["result"]["configOptions"]).unwrap().0,
        "bypass",
        "{changed}"
    );
    assert!(
        controls(&log)
            .iter()
            .any(|control| control["request"]["mode"] == "bypassPermissions"),
        "{:?}",
        controls(&log)
    );
}

/// A session Claude Code started in a mode of its own shows that mode, and a mode the table has no name for shows none.
#[test]
fn a_session_started_in_a_mode_shows_it() {
    let scratch = Scratch::new("startmode");
    let log = scratch.0.join("controls.jsonl");
    for (launched, shown) in [("plan", Some("plan")), ("dontAsk", Some("custom"))] {
        let mut defaults = ClaudeLaunch::new(".");
        defaults.executable = replay_program().into();
        defaults.permission_mode = Some(launched.into());
        defaults
            .env
            .insert("COQUI_ACP_CLAUDE_CONTROLS".into(), log.clone().into());
        let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
        peer.request(
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        );
        let opened = peer.request(
            "session/new",
            json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
        );
        let current = mode_of(&opened["result"]["configOptions"]).map(|(current, _)| current);
        assert_eq!(current.as_deref(), shown, "{launched}");
    }
}

/// A session a client opens on an agent with these defaults: its mode option, and the controls the stand-in saw.
fn opened_with(
    scratch: &Scratch,
    configure: impl FnOnce(&mut ClaudeLaunch),
) -> (Peer, String, Value) {
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.env.insert(
        "COQUI_ACP_CLAUDE_CONTROLS".into(),
        scratch.0.join("controls.jsonl").into(),
    );
    configure(&mut defaults);
    let mut peer = Peer::serve(ClaudeAgent::new().defaults(defaults), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let opened = peer.request(
        "session/new",
        json!({"cwd": scratch.0.to_string_lossy(), "mcpServers": []}),
    );
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    (peer, session, opened["result"]["configOptions"].clone())
}

fn set_mode(peer: &mut Peer, session: &str, value: &str) -> Value {
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": value}),
    )
}

/// A client reaches only the host's configured mode and the stricter ones: it cannot loosen what canoa.cfg set.
#[test]
fn a_client_may_choose_the_configured_mode_and_stricter_ones_only() {
    let scratch = Scratch::new("capped");
    let (mut peer, session, options) = opened_with(&scratch, |_| {});
    let (current, offered) = mode_of(&options).unwrap();
    assert_eq!(
        (current.as_str(), offered),
        ("manual", vec!["manual".to_string(), "plan".to_string()])
    );
    for loosening in ["accept_edits", "auto", "bypass"] {
        let refused = set_mode(&mut peer, &session, loosening);
        assert_eq!(refused["error"]["code"], -32602, "{loosening}: {refused}");
    }
    assert!(set_mode(&mut peer, &session, "plan")["result"].is_object());
    let seen: Vec<Value> = controls(&scratch.0.join("controls.jsonl"))
        .iter()
        .filter(|control| control["request"]["subtype"] == "set_permission_mode")
        .map(|control| control["request"]["mode"].clone())
        .collect();
    assert_eq!(
        seen,
        [json!("plan")],
        "nothing looser was ever sent to Claude Code"
    );

    let scratch = Scratch::new("capped-edits");
    let (_, _, options) = opened_with(&scratch, |launch| {
        launch.permission_mode = Some("acceptEdits".into())
    });
    assert_eq!(
        mode_of(&options).unwrap().1,
        ["manual", "accept_edits", "plan"]
    );

    let scratch = Scratch::new("capped-named");
    let (_, _, options) = opened_with(&scratch, |launch| {
        launch.modes = Some(vec![
            coqui_acp_claude::harness::Mode::Auto,
            coqui_acp_claude::harness::Mode::Bypass,
        ]);
    });
    assert_eq!(
        mode_of(&options).unwrap().1,
        ["manual", "auto", "plan"],
        "the default widened by the named modes, and no Bypass without its switch"
    );
}

/// An entry started in `bypassPermissions` with Bypass off shows it and cannot be taken back to it once left.
#[test]
fn a_launch_in_bypass_permissions_does_not_offer_bypass_without_its_switch() {
    let scratch = Scratch::new("bypass-launched");
    let (mut peer, session, options) = opened_with(&scratch, |launch| {
        launch.permission_mode = Some("bypassPermissions".into())
    });
    let (current, _) = mode_of(&options).unwrap();
    assert_eq!(current, "bypass", "what is running is shown");
    assert!(set_mode(&mut peer, &session, "manual")["result"].is_object());
    let again = set_mode(&mut peer, &session, "bypass");
    assert_eq!(again["error"]["code"], -32602, "{again}");
}

/// What Claude Code reports about its own mode reaches the client as a `config_option_update`.
#[test]
fn the_mode_follows_what_claude_code_reports() {
    let scratch = Scratch::new("drift");
    let (mut peer, session, _) = opened_with(&scratch, |launch| {
        launch.modes = Some(coqui_acp_claude::harness::Mode::ALL.to_vec());
        launch
            .env
            .insert("COQUI_ACP_CLAUDE_MODE_LINE".into(), "acceptEdits".into());
    });
    let from = peer.updates.len();
    // The stand-in never ends the turn, so the prompt is sent and not waited for.
    peer.send(
        json!({"jsonrpc": "2.0", "id": 900, "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}}),
    );
    let changed = peer.until(from, |update| {
        update["sessionUpdate"] == "config_option_update"
    });
    assert_eq!(
        mode_of(&changed["configOptions"]).unwrap().0,
        "accept_edits"
    );
}

const USAGE: &str = "dev.boriken.coqui.acp/usage";

#[test]
fn a_client_that_asked_is_told_the_usage_after_each_recorded_rate_limit_event_and_the_agent_says_it_needs_a_turn()
 {
    let scratch = Scratch::new("usage");
    let mut peer = Peer::serve(agent(Some("rate_limit"), None), Settings::new());
    let initialized = peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {"_meta": {USAGE: {"notify": true, "account": "pool/claude"}}},
               "info": {"name": "test", "version": "0"}}),
    );
    let advertised = &initialized["result"]["capabilities"]["_meta"][USAGE];
    assert_eq!(
        advertised,
        &json!({"version": 1, "sources": ["reported", "inferred"], "spend": true, "needs_turn": true}),
        "{initialized}"
    );
    let opened = peer.request("session/new", json!({"cwd": scratch.0.to_string_lossy()}));
    let session = opened["result"]["sessionId"]
        .as_str()
        .expect("a session")
        .to_string();

    // Before a turn Claude Code has said nothing, and a poll cannot make it: supported, nothing yet.
    let before = peer.request(USAGE_METHOD, json!({}));
    assert_eq!(before["result"]["supported"], true, "{before}");
    assert!(before["result"]["buckets"].as_array().unwrap().is_empty());

    let idle = v2_turn(&mut peer, &session, "First.");
    assert_eq!(idle["stopReason"], "end_turn");
    peer.request(USAGE_METHOD, json!({}));
    let report = peer.usage_updates.last().expect("a usage_update").clone();
    let five = report["report"]["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["id"] == "five_hour")
        .unwrap_or_else(|| panic!("{report}"))
        .clone();
    assert_eq!(
        (&five["used"], &five["limit"], &five["status"]),
        (&json!(500), &json!(1000), &json!("ok"))
    );
    assert_eq!(five["refill"]["resets_at"], 1_793_875_411);
    assert_eq!(five["refill"]["length"], json!([18_000, 0]));
    assert_eq!(report["report"]["account"], "pool/claude");

    // The session's spend rides the same report.
    let polled = peer.request(USAGE_METHOD, json!({"sessionId": session}));
    assert_eq!(polled["result"]["spend"]["input"], 11, "{polled}");
    assert_eq!(polled["result"]["spend"]["cost_micro_usd"], 5);
    assert_eq!(polled["result"]["buckets"][0]["id"], "five_hour");
}

#[test]
fn a_recorded_rejection_ends_the_turn_refused_with_the_hit_and_exhausts_the_window() {
    let scratch = Scratch::new("usage-limit");
    let mut peer = Peer::serve(agent(Some("rate_limit"), None), Settings::new());
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {"_meta": {USAGE: {"notify": true}}},
               "info": {"name": "test", "version": "0"}}),
    );
    let opened = peer.request("session/new", json!({"cwd": scratch.0.to_string_lossy()}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    v2_turn(&mut peer, &session, "First.");
    let refused = v2_turn(&mut peer, &session, "Second.");
    assert_eq!(refused["stopReason"], "refusal", "{refused}");
    let hit = &refused["_meta"]["dev.boriken.coqui.acp/limit"];
    assert_eq!(hit["bucket"], "five_hour");
    assert_eq!(hit["resets_at"], 1_793_875_411);
    assert_eq!(hit["source"], "reported");
    assert!(hit["observed_at"].as_i64().unwrap() > 0);
    let polled = peer.request(USAGE_METHOD, json!({}));
    assert_eq!(
        polled["result"]["buckets"][0]["status"], "exhausted",
        "{polled}"
    );
    // The recorded reset has long passed, so the report no longer carries the hit; the bucket stays as it was told.
    assert!(polled["result"]["limit"].is_null(), "{polled}");
}

const USAGE_METHOD: &str = "_coqui_acp/usage";
