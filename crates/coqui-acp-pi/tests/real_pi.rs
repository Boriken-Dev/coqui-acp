//! Against a real Pi, for what only Pi's own behaviour can show: that the path the gate decides on is the path Pi uses.
//! Nothing real is reached: Pi runs in a throwaway home against a loopback stand-in for the model API, with canary
//! credentials. Skipped unless these name a Pi 1.5.2 and what runs the stand-in:
//!
//! - `COQUI_ACP_PI_REAL`: Pi's executable or npm shim (`pi.cmd`);
//! - `COQUI_ACP_PI_REAL_PATH`: a directory with `node` in it, put first on Pi's `PATH`;
//! - `COQUI_ACP_PI_REAL_PYTHON`: a Python, which runs `tools/record_harnesses/pi/openai_chat_model.py`;
//! - `COQUI_ACP_PI_REAL_NODE` (the gate test): a Node with `.ts` support.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::{Content, Event, Outcome};
use coqui_acp_pi::{GATE_SOURCE, PiLaunch, PiSession, PiSettings, PlainSpawn};
use support::{Events, Scratch};

const KEY: &str = "CANARY-API-KEY-5f3a91";
const SSH: &str = "CANARY-SSH-KEY-77c2d0";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// A stand-in model on a free loopback port, ended on drop.
struct Model(Child, u16);

impl Drop for Model {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn model(python: &str, scratch: &Scratch) -> Model {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/record_harnesses/pi/openai_chat_model.py");
    let mut child = Command::new(python)
        .arg(script)
        .arg(port.to_string())
        .arg(scratch.0.join("requests.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("the stand-in starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    Model(child, port)
}

#[allow(
    dead_code,
    reason = "the scratch directory is removed when the world is dropped"
)]
struct World {
    scratch: Scratch,
    home: PathBuf,
    project: PathBuf,
    _model: Model,
    real: String,
    path: String,
}

fn world(name: &str) -> Option<World> {
    let (real, path, python) = (
        env("COQUI_ACP_PI_REAL")?,
        env("COQUI_ACP_PI_REAL_PATH")?,
        env("COQUI_ACP_PI_REAL_PYTHON")?,
    );
    let scratch = Scratch::new(name);
    let home = scratch.dir("home");
    let agent = home.join(".pi").join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("id_rsa"), SSH).unwrap();
    let project = scratch.dir("project");
    std::fs::write(project.join("notes.txt"), "alpha\n").unwrap();
    let model = model(&python, &scratch);
    let models = serde_json::json!({"providers": {"stub": {
        "baseUrl": format!("http://127.0.0.1:{}/v1", model.1), "api": "openai-completions", "apiKey": KEY,
        "compat": {"supportsReasoningEffort": true, "thinkingFormat": "openai"},
        "models": [{"id": "stub-fast", "name": "Stub Fast", "input": ["text"], "contextWindow": 128_000, "maxTokens": 4096}]}}});
    std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"defaultProvider": "stub", "defaultModel": "stub-fast"}"#,
    )
    .unwrap();
    Some(World {
        scratch,
        home,
        project,
        _model: model,
        real,
        path,
    })
}

impl World {
    fn launch(&self, mode: Mode) -> PiLaunch {
        let mut launch = PiLaunch::new(&self.project);
        launch.executable = self.real.clone().into();
        launch.mode = Some(mode);
        launch.allow_bypass = true;
        for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
            launch.env.insert(name.into(), self.home.clone().into());
        }
        launch.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            self.home.join(".pi").join("agent").into(),
        );
        launch.env.insert("PI_OFFLINE".into(), "1".into());
        launch
            .env
            .insert("PI_SKIP_VERSION_CHECK".into(), "1".into());
        let mut path = vec![PathBuf::from(&self.path)];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        launch
            .env
            .insert("PATH".into(), std::env::join_paths(path).unwrap());
        launch
    }

    fn session(&self, mode: Mode) -> (PiSession, Events) {
        let (sink, events) = Events::new();
        let session = PiSession::start(&self.launch(mode), &PlainSpawn, PiSettings::new(), sink)
            .expect("the real Pi starts with the gate");
        (session, events)
    }

    /// One turn of the stand-in reading or writing `path`, every question rejected; everything Pi and the gate said.
    fn turn(&self, session: &PiSession, events: &mut Events, marker: &str) -> String {
        session
            .prompt(&[Content::Text(marker.into())])
            .expect("taken");
        loop {
            events.until(|event| matches!(event, Event::Permission(_) | Event::TurnEnd(_)));
            match events.seen.last() {
                Some(Event::Permission(_)) => {
                    let Some(Event::Permission(request)) = events.seen.pop() else {
                        unreachable!()
                    };
                    events.seen.push(Event::Notice {
                        severity: coqui_acp_pi::harness::event::Severity::Info,
                        title: format!("ASKED: {}", request.title),
                    });
                    request
                        .responder
                        .answer(Outcome::Selected("reject".into()))
                        .unwrap();
                }
                _ => break,
            }
        }
        let said = format!("{:?}", events.seen);
        events.seen.clear();
        said
    }
}

fn url_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[test]
fn a_real_pi_never_reads_a_credential_the_gate_did_not_allow_whatever_the_path_is_spelled_like() {
    let Some(world) = world("real-reads") else {
        eprintln!("skipped: no real Pi named");
        return;
    };
    let ssh = world.home.join(".ssh").join("id_rsa");
    let (session, mut events) = world.session(Mode::Manual);
    let spellings = [
        "~/.ssh/id_rsa".to_string(),
        "@~/.ssh/id_rsa".to_string(),
        format!("@{}", ssh.display()),
        url_of(&ssh),
        "@~/.pi/agent/models.json".to_string(),
        "~/.pi/agent/models.json".to_string(),
    ];
    for text in spellings {
        let said = world.turn(&session, &mut events, &format!("[[read:{text}]] go"));
        assert!(
            !said.contains(SSH),
            "the SSH key was read through {text:?}: {said}"
        );
        assert!(
            !said.contains(KEY),
            "the API key was read through {text:?}: {said}"
        );
    }
    // A file of the project is read.
    let said = world.turn(&session, &mut events, "[[read:notes.txt]] go");
    assert!(said.contains("alpha"), "{said}");
}

#[test]
fn a_real_pi_writes_nothing_outside_the_project_in_accept_edits_unasked() {
    let Some(world) = world("real-writes") else {
        eprintln!("skipped: no real Pi named");
        return;
    };
    let (session, mut events) = world.session(Mode::AcceptEdits);
    for text in [
        "~/.gitconfig",
        "@~/.bashrc",
        "@notes2.txt",
        "~/AppData/x.bat",
    ] {
        let said = world.turn(&session, &mut events, &format!("[[write:{text}]] go"));
        assert!(
            said.contains("ASKED"),
            "{text:?} was not asked about: {said}"
        );
    }
    for name in [".gitconfig", ".bashrc", "AppData"] {
        assert!(!world.home.join(name).exists(), "{name} was written");
    }
    assert!(!world.project.join("notes2.txt").exists());
    // A plain path inside the project is written without a question, and a bash command still runs under Bypass.
    let said = world.turn(&session, &mut events, "[[write:made.txt]] go");
    assert!(
        !said.contains("ASKED") && world.project.join("made.txt").exists(),
        "{said}"
    );
    session.set_mode(Mode::Bypass);
    let said = world.turn(&session, &mut events, "[[bash:echo from-bash]] go");
    assert!(
        said.contains("from-bash"),
        "Pi's own commands still run with the program search narrowed: {said}"
    );
}

#[test]
fn a_real_pi_asks_about_a_network_path_and_does_not_open_it() {
    let Some(world) = world("real-network") else {
        eprintln!("skipped: no real Pi named");
        return;
    };
    let (session, mut events) = world.session(Mode::Manual);
    for text in [
        "@//10.255.255.1/share/f",
        "@\\\\10.255.255.1\\share\\f",
        "file://10.255.255.1/share/f",
    ] {
        let started = std::time::Instant::now();
        let said = world.turn(&session, &mut events, &format!("[[read:{text}]] go"));
        assert!(
            said.contains("ASKED"),
            "{text:?} was not asked about: {said}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{text:?} was opened"
        );
    }
}

/// The gate cuts a long string at a code point: a surrogate pair is not split.
#[test]
fn the_gate_does_not_split_a_surrogate_pair_when_it_cuts() {
    let Some(node) = env("COQUI_ACP_PI_REAL_NODE") else {
        eprintln!("skipped: no Node named");
        return;
    };
    let scratch = Scratch::new("gate-cut");
    let file = scratch.0.join("gate.mjs");
    std::fs::write(&file, GATE_SOURCE.replace("__NONCE__", "test")).unwrap();
    let probe = scratch.0.join("probe.mjs");
    std::fs::write(
        &probe,
        r#"import gate from "./gate.mjs";
let handler;
gate({ registerCommand() {}, on(name, f) { handler = f; } });
let title;
const ctx = { ui: { select: async (t) => { title = t; return "deny"; } } };
// 4095 letters and a four-byte character: the cut falls in the middle of its pair.
await handler({ toolName: "bash", toolCallId: "x", input: { command: "a".repeat(4095) + "\u{1F600}" + "tail" } }, ctx);
const call = JSON.parse(title.slice("coqui-acp-pi-gate ".length));
const text = call.input.command;
const last = text.charCodeAt(text.length - 1);
if (last >= 0xd800 && last <= 0xdbff) { console.log("SPLIT"); process.exit(1); }
console.log("OK", text.length, JSON.stringify(call.input._cut));
"#,
    )
    .unwrap();
    let out = Command::new(node).arg(&probe).output().expect("node runs");
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success() && said.starts_with("OK 4095"),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
