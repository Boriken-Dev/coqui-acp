//! What a host lets a client choose before a session starts: models, efforts and modes, each from the host's list.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

mod support;

use std::sync::Arc;

use coqui_acp::{Answer, Client, Event, Exchange, Session};
use coqui_acp_provider::{
    AcpFacade, Error, LAUNCH_META_KEY, OptionTarget, OptionValue, SESSION_OPTIONS_CAPABILITY,
    SESSION_OPTIONS_METHOD, Settings, StartOption,
};
use serde_json::{Value, json};
use support::{Probe, Raw, WAIT, cwd, detail, facade, quick};

fn models() -> StartOption {
    StartOption::model(
        vec![
            OptionValue::new("alpha", "Alpha").description("the quick one"),
            OptionValue::new("beta", "Beta"),
        ],
        "alpha",
    )
}

fn efforts() -> StartOption {
    StartOption::effort(
        vec![
            OptionValue::new("low", "Low"),
            OptionValue::new("high", "High"),
        ],
        "low",
    )
}

fn modes() -> StartOption {
    StartOption::new(
        "mode",
        "Mode",
        OptionTarget::Environment("AGENT_MODE".to_owned()),
        vec![
            OptionValue::new("ask", "Ask"),
            OptionValue::new("code", "Code").description("writes files"),
        ],
        "ask",
    )
    .category("mode")
}

fn declaring(options: Vec<StartOption>) -> Settings {
    let mut settings = quick();
    settings.options = options;
    settings
}

/// The events up to and including whatever ends the turn.
fn turn(session: &Session) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        let event = session
            .next_event(WAIT)
            .unwrap_or_else(|| panic!("the turn did not end: {events:?}"));
        let ended = matches!(
            event,
            Event::Completed { .. } | Event::Failed(_) | Event::Cancelled
        );
        events.push(event);
        if ended {
            return events;
        }
    }
}

fn set(raw: &mut Raw, session: &str, option: &str, value: &str) -> Value {
    raw.call(
        "session/set_config_option",
        &json!({"sessionId": session, "configId": option, "value": value}),
    )
}

#[test]
fn the_real_client_chooses_a_declared_model_that_the_provider_starts_with() {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, declaring(vec![models(), efforts()]))
        .serve(agent)
        .expect("a thread starts");
    let session = Client::builder(cwd())
        .model("beta")
        .effort("high")
        .prompt("hi")
        .connect(client)
        .expect("the facade opens a session");
    let events = turn(&session);
    assert!(
        !events.iter().any(|event| matches!(event, Event::Notice(_))),
        "the choices were applied: {events:?}"
    );
    let starts = probe.starts.lock().unwrap();
    assert_eq!(starts[0].model.as_deref(), Some("beta"));
    assert_eq!(starts[0].effort.as_deref(), Some("high"));
    drop(starts);
    session.close();
}

#[test]
fn session_new_answers_the_declared_models_as_acps_model_option() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models()])));
    raw.initialize();
    let answer = raw.call("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
    assert_eq!(
        answer["result"]["configOptions"],
        json!([{
            "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "alpha",
            "options": [
                {"value": "alpha", "name": "Alpha", "description": "the quick one"},
                {"value": "beta", "name": "Beta"},
            ],
        }]),
        "{answer}"
    );
    assert!(answer["result"].get("modes").is_none(), "{answer}");
}

#[test]
fn nothing_chosen_starts_with_the_default() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models(), modes()])));
    let session = raw.open();
    raw.prompt(&session, "hi");
    let starts = probe.starts.lock().unwrap();
    assert_eq!(starts[0].model.as_deref(), Some("alpha"));
    assert_eq!(
        starts[0].environment.get("AGENT_MODE").map(String::as_str),
        Some("ask")
    );
}

#[test]
fn a_set_option_answers_every_option_at_its_value_and_reaches_the_start() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models(), efforts()])));
    let session = raw.open();
    let answer = set(&mut raw, &session, "model", "beta");
    let options = answer["result"]["configOptions"]
        .as_array()
        .expect("every option");
    assert_eq!(options.len(), 2, "{answer}");
    assert_eq!(options[0]["currentValue"], "beta");
    assert_eq!(options[1]["currentValue"], "low");
    raw.prompt(&session, "hi");
    assert_eq!(
        probe.starts.lock().unwrap()[0].model.as_deref(),
        Some("beta")
    );
}

#[test]
fn a_value_outside_the_list_is_refused_and_changes_nothing() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models()])));
    let session = raw.open();
    let answer = set(&mut raw, &session, "model", "gamma");
    assert!(detail(&answer).contains("no value gamma"), "{answer}");
    let answer = set(&mut raw, &session, "temperature", "hot");
    assert!(
        detail(&answer).contains("no option temperature"),
        "{answer}"
    );
    let answer = raw.call(
        "session/set_config_option",
        &json!({"sessionId": session, "configId": "model", "type": "boolean", "value": true}),
    );
    assert!(answer["error"].is_object(), "{answer}");
    raw.prompt(&session, "hi");
    assert_eq!(
        probe.starts.lock().unwrap()[0].model.as_deref(),
        Some("alpha")
    );
}

#[test]
fn a_launch_model_outside_the_list_is_refused() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models()])));
    raw.initialize();
    let answer = raw.call(
        "session/new",
        &json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {"model": "gamma"}}}),
    );
    assert!(detail(&answer).contains("no value gamma"), "{answer}");
    let answer = raw.call(
        "session/new",
        &json!({"cwd": cwd(), "mcpServers": [], "_meta": {LAUNCH_META_KEY: {"model": "beta"}}}),
    );
    assert_eq!(
        answer["result"]["configOptions"][0]["currentValue"], "beta",
        "{answer}"
    );
}

#[test]
fn a_change_after_the_provider_started_is_refused() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models(), modes()])));
    let session = raw.open();
    raw.prompt(&session, "hi");
    let answer = set(&mut raw, &session, "model", "beta");
    assert!(detail(&answer).contains("started"), "{answer}");
    let answer = raw.call(
        "session/set_mode",
        &json!({"sessionId": session, "modeId": "code"}),
    );
    assert!(detail(&answer).contains("started"), "{answer}");
    let answer = raw.prompt(&session, "again");
    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "the session goes on: {answer}"
    );
    assert_eq!(
        probe.starts.lock().unwrap()[0].model.as_deref(),
        Some("alpha")
    );
}

#[test]
fn a_resumed_session_answers_its_options_and_cannot_change_them() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, declaring(vec![models()])));
    raw.initialize();
    let answer = raw.call(
        "session/resume",
        &json!({"sessionId": "old", "cwd": cwd(), "mcpServers": [],
                "_meta": {LAUNCH_META_KEY: {"model": "beta"}}}),
    );
    assert_eq!(
        answer["result"]["configOptions"][0]["currentValue"], "beta",
        "{answer}"
    );
    assert_eq!(
        probe.resumes.lock().unwrap()[0].model.as_deref(),
        Some("beta")
    );
    let answer = set(&mut raw, "old", "model", "alpha");
    assert!(detail(&answer).contains("started"), "{answer}");
}

#[test]
fn a_mode_is_both_an_option_and_a_mode_and_reaches_its_environment_variable() {
    let probe = Probe::new();
    let mut settings = declaring(vec![modes()]);
    settings
        .environment
        .insert("AGENT_MODE".to_owned(), "from the host".to_owned());
    let (mut raw, _served) = Raw::serve(&facade(&probe, settings));
    raw.initialize();
    let answer = raw.call("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
    assert_eq!(
        answer["result"]["modes"],
        json!({"currentModeId": "ask", "availableModes": [
            {"id": "ask", "name": "Ask"},
            {"id": "code", "name": "Code", "description": "writes files"},
        ]}),
        "{answer}"
    );
    assert_eq!(answer["result"]["configOptions"][0]["category"], "mode");
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    let refused = raw.call(
        "session/set_mode",
        &json!({"sessionId": session, "modeId": "yolo"}),
    );
    assert!(detail(&refused).contains("no value yolo"), "{refused}");
    let answer = raw.call(
        "session/set_mode",
        &json!({"sessionId": session, "modeId": "code"}),
    );
    assert_eq!(answer["result"], json!({}), "{answer}");
    raw.prompt(&session, "hi");
    let starts = probe.starts.lock().unwrap();
    assert_eq!(
        starts[0].environment.get("AGENT_MODE").map(String::as_str),
        Some("code")
    );
}

#[test]
fn no_option_is_offered_without_declared_options() {
    let probe = Probe::new();
    let (mut raw, _served) = Raw::serve(&facade(&probe, quick()));
    raw.initialize();
    let answer = raw.call("session/new", &json!({"cwd": cwd(), "mcpServers": []}));
    assert!(answer["result"].get("configOptions").is_none(), "{answer}");
    assert!(answer["result"].get("modes").is_none(), "{answer}");
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    let answer = set(&mut raw, &session, "model", "anything");
    assert!(answer["error"].is_object(), "{answer}");
}

#[test]
fn options_that_cannot_be_served_end_run_before_anything_is_served() {
    let probe = Probe::new();
    for options in [
        vec![models(), models()],
        vec![StartOption::model(vec![OptionValue::new("a", "A")], "b")],
        vec![StartOption::model(Vec::new(), "a")],
        vec![
            modes(),
            StartOption::new(
                "style",
                "Style",
                OptionTarget::Environment("STYLE".to_owned()),
                vec![OptionValue::new("terse", "Terse")],
                "terse",
            )
            .category("mode"),
        ],
    ] {
        let (_raw, lines) = Raw::pair();
        let served = AcpFacade::new(Arc::new(support::TestProvider(probe.clone())))
            .with_settings(declaring(options.clone()))
            .run(lines);
        assert!(
            matches!(served, Err(Error::Settings(_))),
            "{options:?}: {served:?}"
        );
    }
}

fn described(settings: Settings) -> (Arc<Probe>, coqui_acp::Description) {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, settings)
        .serve(agent)
        .expect("a thread starts");
    let description = Exchange::new()
        .describe(client)
        .expect("the facade is described");
    (probe, description)
}

#[test]
fn describe_knows_the_declared_options_modes_and_sessions_and_starts_nothing() {
    let (probe, description) = described(declaring(vec![models(), efforts(), modes()]));
    let Answer::Known(options) = &description.options else {
        panic!("{:?}", description.options);
    };
    let options = serde_json::to_value(options).unwrap();
    let categories: Vec<&str> = options
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["category"].as_str().unwrap())
        .collect();
    assert_eq!(categories, ["model", "thought_level", "mode"]);
    assert_eq!(options[0]["currentValue"], "alpha");
    assert_eq!(
        options[0]["options"][1],
        json!({"value": "beta", "name": "Beta"})
    );
    let Answer::Known(modes) = &description.modes else {
        panic!("{:?}", description.modes);
    };
    assert_eq!(
        serde_json::to_value(modes).unwrap(),
        json!({"currentModeId": "ask", "availableModes": [
            {"id": "ask", "name": "Ask"},
            {"id": "code", "name": "Code", "description": "writes files"},
        ]})
    );
    let Answer::Known(sessions) = &description.sessions else {
        panic!("{:?}", description.sessions);
    };
    assert!(sessions.sessions.is_empty() && sessions.incomplete.is_none());
    assert!(
        probe.starts.lock().unwrap().is_empty(),
        "nothing was started"
    );
}

#[test]
fn describe_says_what_the_host_did_not_declare_is_unknown() {
    let (_probe, description) = described(quick());
    assert!(
        matches!(&description.options, Answer::Unknown(why) if why.contains("declared no options")),
        "{:?}",
        description.options
    );
    assert!(
        matches!(&description.modes, Answer::Unknown(why) if why.contains("declared no modes")),
        "{:?}",
        description.modes
    );
    let (_probe, description) = described(declaring(vec![models()]));
    assert!(matches!(&description.options, Answer::Known(options) if options.len() == 1));
    assert!(
        matches!(&description.modes, Answer::Unknown(_)),
        "{:?}",
        description.modes
    );
}

#[test]
fn a_start_with_a_value_outside_the_described_list_is_refused() {
    let probe = Probe::new();
    let (agent, client) = coqui_acp::duplex();
    facade(&probe, declaring(vec![models()]))
        .serve(agent)
        .expect("a thread starts");
    let refused = Client::builder(cwd())
        .meta(LAUNCH_META_KEY, json!({"model": "gamma"}))
        .prompt("hi")
        .connect(client)
        .expect_err("gamma is not on the host's list");
    assert!(refused.to_string().contains("no value gamma"), "{refused}");
    assert!(probe.starts.lock().unwrap().is_empty());
}

#[test]
fn the_facade_lists_the_sessions_of_this_connection_only() {
    let probe = Probe::new();
    let facade = facade(&probe, quick());
    let (mut raw, _served) = Raw::serve(&facade);
    let init = raw.initialize();
    assert_eq!(
        init["agentCapabilities"]["sessionCapabilities"]["list"],
        json!({})
    );
    assert_eq!(
        init["agentCapabilities"]["_meta"][SESSION_OPTIONS_CAPABILITY],
        json!(true)
    );
    let session = raw.new_session();
    let answer = raw.call("session/list", &json!({}));
    assert_eq!(
        answer["result"],
        json!({"sessions": [{"sessionId": session, "cwd": cwd()}]}),
        "{answer}"
    );
    let elsewhere = raw.call("session/list", &json!({"cwd": "/elsewhere"}));
    assert_eq!(elsewhere["result"], json!({"sessions": []}), "{elsewhere}");
    let (mut other, _served) = Raw::serve(&facade);
    other.initialize();
    let answer = other.call("session/list", &json!({}));
    assert_eq!(answer["result"], json!({"sessions": []}), "{answer}");
}

#[test]
fn the_extension_names_match_the_clients() {
    assert_eq!(
        SESSION_OPTIONS_CAPABILITY,
        coqui_acp::SESSION_OPTIONS_CAPABILITY
    );
    assert_eq!(SESSION_OPTIONS_METHOD, coqui_acp::SESSION_OPTIONS_METHOD);
}
