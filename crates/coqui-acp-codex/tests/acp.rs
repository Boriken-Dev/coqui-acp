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
    // The person's own command line offers every mode; a host's entry gets the capped default.
    defaults.modes = Some(coqui_acp_codex::harness::Mode::ALL.to_vec());
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
    // The recording's thread is `untrusted` with a read-only sandbox, which no mode sets: it shows as it is.
    let (current, offered) = mode_of(&opened["result"]["configOptions"]).expect("a mode option");
    assert_eq!(current, "custom");
    assert_eq!(
        offered,
        ["custom", "manual", "accept_edits", "auto", "plan"]
    );
    let option = opened["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["configId"] == "mode")
        .unwrap();
    assert_eq!(option["options"][0]["name"], "Custom: untrusted, readOnly");
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
        ["custom", "manual", "accept_edits", "auto", "plan", "bypass"]
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

/// A session a client opens on a host's entry: `approval` and `sandbox` as canoa.cfg names them, the thread's state as
/// the stand-in answers it, and what was written to the stand-in.
fn host_session(
    scratch: &Scratch,
    approval: &str,
    sandbox: &str,
    state: Option<&str>,
    configure: impl FnOnce(&mut CodexLaunch),
) -> (Peer, String, Value, std::path::PathBuf) {
    let written = scratch.0.join("inputs.jsonl");
    let mut defaults = CodexLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults.approval_policy = Some(approval.into());
    defaults.sandbox = Some(sandbox.into());
    defaults.env.insert(
        "COQUI_ACP_CODEX_REPLAY".into(),
        fixtures().join("turn").into(),
    );
    defaults
        .env
        .insert("COQUI_ACP_CODEX_INPUTS".into(), written.clone().into());
    if let Some(state) = state {
        defaults
            .env
            .insert("COQUI_ACP_CODEX_THREAD_STATE".into(), state.into());
    }
    configure(&mut defaults);
    let mut peer = Peer::serve(CodexAgent::new().defaults(defaults), Settings::new());
    let opened = v2_session(&mut peer, &scratch.0, json!({}));
    let session = opened["result"]["sessionId"].as_str().unwrap().to_string();
    (
        peer,
        session,
        opened["result"]["configOptions"].clone(),
        written,
    )
}

fn choose(peer: &mut Peer, session: &str, value: &str) -> Value {
    peer.request(
        "session/set_config_option",
        json!({"sessionId": session, "configId": "mode", "value": value, "type": "id"}),
    )
}

/// A client reaches only the mode the host configured and the stricter ones.
#[test]
fn a_client_may_choose_the_configured_mode_and_stricter_ones_only() {
    let scratch = Scratch::new("capped");
    let (mut peer, session, options, written) =
        host_session(&scratch, "on-request", "workspace-write", None, |_| {});
    let (_, offered) = mode_of(&options).unwrap();
    assert_eq!(offered, ["custom", "manual", "accept_edits", "plan"]);
    for loosening in ["auto", "bypass"] {
        assert!(
            choose(&mut peer, &session, loosening)["error"].is_object(),
            "{loosening}"
        );
    }
    assert!(choose(&mut peer, &session, "accept_edits")["result"].is_object());
    run_a_turn(&mut peer, &session);
    assert_eq!(the_turn_start(&written)["approvalsReviewer"], "user");

    let scratch = Scratch::new("capped-readonly");
    let (_, _, options, _) = host_session(&scratch, "on-request", "read-only", None, |_| {});
    assert_eq!(
        mode_of(&options).unwrap().1,
        ["custom", "plan"],
        "a read-only host is offered Plan only"
    );

    let scratch = Scratch::new("capped-bypass-off");
    let (_, _, options, _) = host_session(&scratch, "never", "danger-full-access", None, |_| {});
    assert!(
        !mode_of(&options).unwrap().1.contains(&"bypass".to_string()),
        "Bypass needs its own switch"
    );
}

/// A thread whose policy and sandbox are Plan's has not been put in Plan: it is shown as it is, and choosing Plan names
/// it to Codex, which has no collaboration mode of its own to read back.
#[test]
fn a_threads_policy_alone_is_not_plan_and_choosing_plan_applies_it() {
    let scratch = Scratch::new("shown");
    let (mut peer, session, options, written) = host_session(
        &scratch,
        "on-request",
        "read-only",
        Some("on-request,readOnly,user"),
        |_| {},
    );
    assert_eq!(mode_of(&options).unwrap().0, "custom");
    assert!(
        !written_to(&written)
            .iter()
            .any(|input| input["method"] == "turn/start"),
        "no turn yet"
    );
    let shown = choose(&mut peer, &session, "plan");
    assert_eq!(
        mode_of(&shown["result"]["configOptions"]).unwrap().0,
        "plan"
    );
    run_a_turn(&mut peer, &session);
    assert_eq!(
        the_turn_start(&written)["collaborationMode"]["mode"],
        "plan"
    );
}

/// A Plan turn takes the place of Codex's built-in Plan instructions with the session's own, which the person is told.
#[test]
fn a_plan_turn_with_instructions_of_its_own_says_it_replaces_the_built_in_ones() {
    let scratch = Scratch::new("plan-notice");
    let (mut peer, session, _, _) =
        host_session(&scratch, "on-request", "workspace-write", None, |launch| {
            launch.developer_instructions = Some("Answer briefly.".into());
        });
    choose(&mut peer, &session, "plan");
    let from = peer.updates.len();
    run_a_turn(&mut peer, &session);
    assert!(
        peer.updates[from..].iter().any(|update| {
            update["sessionUpdate"] == "notice"
                && update.to_string().contains("built-in Plan instructions")
        }),
        "{:?}",
        peer.updates
    );
}

/// A collaboration mode takes the place of the thread's model, effort and developer instructions, so a turn in a mode
/// names them: the session's own, never null.
#[test]
fn a_turn_in_a_mode_keeps_the_sessions_effort_and_instructions() {
    let scratch = Scratch::new("keeps");
    let (mut peer, session, _, written) =
        host_session(&scratch, "on-request", "workspace-write", None, |launch| {
            launch.effort = Some("low".into());
            launch.developer_instructions = Some("Answer briefly.".into());
        });
    choose(&mut peer, &session, "plan");
    run_a_turn(&mut peer, &session);
    let params = the_turn_start(&written);
    let settings = &params["collaborationMode"]["settings"];
    assert_eq!(settings["reasoning_effort"], "low", "{params}");
    assert_eq!(
        settings["developer_instructions"], "Answer briefly.",
        "{params}"
    );
    assert!(!params.to_string().contains("null"), "{params}");
}

const USAGE: &str = "dev.boriken.coqui.acp/usage";

/// An agent whose stand-in answers `account/rateLimits/read` with the contents of `limits`.
fn reading(limits: &std::path::Path) -> CodexAgent {
    let mut defaults = CodexLaunch::new(".");
    defaults.executable = replay_program().into();
    defaults
        .env
        .insert("COQUI_ACP_CODEX_RATE_LIMITS".into(), limits.into());
    defaults
        .env
        .insert("COQUI_ACP_CODEX_THREAD".into(), "thread-usage".into());
    CodexAgent::new().defaults(defaults)
}

fn opt_in(peer: &mut Peer) -> Value {
    peer.request(
        "initialize",
        json!({"protocolVersion": 2, "capabilities": {"_meta": {USAGE: {"notify": true, "account": "pool/codex"}}},
               "info": {"name": "test", "version": "0"}}),
    )
}

/// The recorded `account/rateLimits/read` answer of Codex 0.166.0, as a file the stand-in answers from.
fn write_limits(path: &std::path::Path, weekly_percent: u64) {
    let mut answer = support::recorded_166("rate_limits_read")
        .into_iter()
        .find_map(|line| line.get("result").cloned())
        .expect("the recorded answer");
    answer["rateLimits"]["secondary"]["usedPercent"] = json!(weekly_percent);
    answer["rateLimitsByLimitId"]["codex"]["secondary"]["usedPercent"] = json!(weekly_percent);
    std::fs::write(path, serde_json::to_vec(&answer).unwrap()).unwrap();
}

fn weekly(report: &Value) -> Value {
    report["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["id"] == "codex:weekly")
        .unwrap_or_else(|| panic!("no weekly bucket in {report}"))
        .clone()
}

#[test]
fn a_session_tells_a_client_that_asked_where_the_account_stands_and_a_poll_reads_it_again() {
    let scratch = Scratch::new("usage");
    let limits = scratch.0.join("limits.json");
    write_limits(&limits, 3);
    let mut peer = Peer::serve(reading(&limits), Settings::new());
    let initialized = opt_in(&mut peer);
    let advertised = &initialized["result"]["capabilities"]["_meta"][USAGE];
    assert_eq!(advertised["version"], 1, "{initialized}");
    assert_eq!(advertised["spend"], true);
    assert!(
        advertised.get("needs_turn").is_none(),
        "Codex is read without a turn: {advertised}"
    );

    // The read as the session opens reaches a client that asked, with the host's account.
    let opened = peer.request("session/new", json!({"cwd": scratch.0.to_string_lossy()}));
    assert!(opened["result"]["sessionId"].is_string(), "{opened}");
    // What a session says while it opens is sent once its answer has gone: a round trip reads it.
    for _ in 0..40 {
        if !peer.usage_updates.is_empty() {
            break;
        }
        peer.request("session/list", json!({}));
        std::thread::sleep(Duration::from_millis(50));
    }
    let told = peer
        .usage_updates
        .first()
        .expect("a usage_update at open")
        .clone();
    let report = &told["report"];
    assert_eq!(report["account"], "pool/codex");
    let five = report["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["id"] == "codex:five_hour")
        .unwrap();
    assert_eq!(
        (
            &five["used"],
            &five["limit"],
            &five["unit"],
            &five["status"]
        ),
        (&json!(0), &json!(1000), &json!("permille"), &json!("ok"))
    );
    assert_eq!(five["refill"]["kind"], "fixed_window");
    assert_eq!(five["refill"]["resets_at"], 1_794_248_766);
    assert_eq!(five["refill"]["length"], json!([18_000, 0]));
    assert_eq!(weekly(report)["used"], 30);
    let revision = report["revision"].as_u64().unwrap();

    // Asked again with the account unchanged, the revision stays.
    let again = peer.request(USAGE_METHOD, json!({}));
    assert_eq!(again["result"]["revision"], revision, "{again}");

    // Codex moved on: the next poll sees it and the client is told, with no turn between.
    write_limits(&limits, 9);
    let polled = peer.request(USAGE_METHOD, json!({}));
    assert_eq!(weekly(&polled["result"])["used"], 90, "{polled}");
    assert!(polled["result"]["revision"].as_u64().unwrap() > revision);
    peer.request(USAGE_METHOD, json!({})); // the notification was read while waiting for this answer
    assert_eq!(
        weekly(&peer.usage_updates.last().unwrap()["report"])["used"],
        90
    );
}

#[test]
fn a_poll_with_no_session_starts_the_app_server_for_the_read_and_ends_it() {
    let scratch = Scratch::new("usage-bare");
    let limits = scratch.0.join("limits.json");
    write_limits(&limits, 12);
    let mut peer = Peer::serve(reading(&limits), Settings::new());
    opt_in(&mut peer);
    let polled = peer.request(USAGE_METHOD, json!({}));
    assert_eq!(polled["result"]["supported"], true, "{polled}");
    assert_eq!(weekly(&polled["result"])["used"], 120);
    assert_eq!(polled["result"]["account"], "pool/codex");
}

const USAGE_METHOD: &str = "_coqui_acp/usage";
