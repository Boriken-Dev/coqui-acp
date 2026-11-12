//! The host side of secret prompts through its public API: a person's frontend stands in with the real sealing and bindings of
//! `coqui_acp::secret`, and a probe stands in for the command runner.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use coqui_acp::secret::unprotected;
use coqui_acp::secret::{
    Capability, DEFAULT_LIFETIME, Error as SecretError, IdentityId, Limits, Method, Prover,
    Receiver, RequestParams, RequestReply, Sealed, ShowDeliver, ShowDone, ShowOpen, ShowOpened,
    Verifier,
};
use coqui_acp_secrets::{
    Action, Approval, CommandOutput, CommandSpec, Decision, Error, Event, FOLDER_NOTICE, Frontend,
    Mode, ProcessRunner, Runner, Secrets, SecretsBuilder, SessionConfig, SessionTools, Settings,
};
use data_encoding::{HEXLOWER, HEXUPPER};
use serde_json::{Value, json};
use zeroize::Zeroizing;

const NOW: u64 = 1_700_000_000;
/// A program every machine has on its search path, outside any test directory.
const SH: &str = if cfg!(windows) { "cmd" } else { "sh" };

/// What the person types: the plain answers' JSON text and the secret values by field.
type Typed = (String, Vec<(String, String)>);
/// The commands a probe ran: the command as shown and the variable that carried the secret.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;
/// Made-up, with characters every encoding treats differently.
const PASSWORD: &str = "Tr0ub4dor&3 /x+y=?\u{e9}\"q";

fn scalar(first: u8) -> Vec<u8> {
    (first..first + 32).collect()
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "user": {"type": "string"},
            "password": {"type": "string", "writeOnly": true}
        },
        "required": ["user", "password"]
    })
}

/// Every form of a value an accident could print, written here on its own, apart from the crate's scrubber.
fn forms(value: &str) -> Vec<(&'static str, String)> {
    let json = serde_json::to_string(value).unwrap();
    let percent: String = value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    vec![
        ("raw", value.to_owned()),
        ("base64", STANDARD.encode(value)),
        ("base64 unpadded", STANDARD_NO_PAD.encode(value)),
        ("base64url", URL_SAFE.encode(value)),
        ("base64url unpadded", URL_SAFE_NO_PAD.encode(value)),
        ("hex", HEXLOWER.encode(value.as_bytes())),
        ("HEX", HEXUPPER.encode(value.as_bytes())),
        ("percent", percent),
        ("json", json[1..json.len() - 1].to_owned()),
    ]
}

/// The forms of `value` found in `text`.
fn leaks(text: &str, value: &str) -> Vec<&'static str> {
    forms(value)
        .into_iter()
        .filter(|(_, form)| text.contains(form.as_str()))
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn the_scanner_sees_every_form_it_looks_for() {
    // A positive control: a scan that passes is believed only because it is seen to fail on a planted value.
    for (name, form) in forms(PASSWORD) {
        let planted = format!("log line before {form} and after");
        assert!(
            leaks(&planted, PASSWORD).contains(&name),
            "the scan missed {name}"
        );
    }
    assert!(leaks("nothing here", PASSWORD).is_empty());
}

/// The person's frontend. It really collects and shows, with the extension's sealing and bindings.
struct Person {
    prover: Prover,
    verifier: Verifier,
    claimed: IdentityId,
    host: IdentityId,
    capability: Capability,
    clock: Arc<AtomicU64>,
    /// Answers to requests: the plain text and the secret values by field.
    answers: Mutex<VecDeque<Typed>>,
    /// Seconds the person takes before answering a request.
    slow: AtomicU64,
    decisions: Mutex<VecDeque<Decision>>,
    asked: Mutex<Vec<Approval>>,
    requests: AtomicUsize,
    wire: Mutex<Vec<String>>,
    pending: Mutex<Option<(Receiver, String)>>,
    shown: Mutex<Vec<BTreeMap<String, String>>>,
    show_answer: Mutex<Option<&'static str>>,
    /// Whether the session's permission mode approves without asking.
    bypass: std::sync::atomic::AtomicBool,
    /// Runs while the person is deciding.
    while_deciding: Mutex<Option<Box<dyn Fn() + Send>>>,
}

struct Keys {
    host: Prover,
    person: Prover,
}

fn keys() -> Keys {
    Keys {
        host: unprotected::ecdsa_p256(&scalar(1)).unwrap(),
        person: unprotected::ecdsa_p256(&scalar(2)).unwrap(),
    }
}

impl Person {
    fn new(keys: &Keys, clock: &Arc<AtomicU64>) -> Arc<Self> {
        Self::with(keys, clock, unprotected::ecdsa_p256(&scalar(2)).unwrap())
    }

    /// A frontend that claims the owner's identity but proves with `prover`.
    fn with(keys: &Keys, clock: &Arc<AtomicU64>, prover: Prover) -> Arc<Self> {
        Arc::new(Self {
            verifier: Verifier::builder(keys.person.id())
                .trust(keys.host.id())
                .build(),
            claimed: keys.person.id(),
            host: keys.host.id(),
            prover,
            capability: Capability::all(),
            clock: Arc::clone(clock),
            answers: Mutex::default(),
            slow: AtomicU64::new(0),
            decisions: Mutex::default(),
            asked: Mutex::default(),
            requests: AtomicUsize::new(0),
            wire: Mutex::default(),
            pending: Mutex::default(),
            shown: Mutex::default(),
            show_answer: Mutex::default(),
            while_deciding: Mutex::default(),
            bypass: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn will_type(&self, plain: &str, secrets: &[(&str, &str)]) {
        self.answers.lock().unwrap().push_back((
            plain.to_owned(),
            secrets
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
    }

    fn will_decide(&self, decisions: &[Decision]) {
        self.decisions
            .lock()
            .unwrap()
            .extend(decisions.iter().copied());
    }

    fn asked(&self) -> Vec<Approval> {
        self.asked.lock().unwrap().clone()
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
}

impl Frontend for Person {
    fn identity(&self) -> IdentityId {
        self.claimed
    }

    fn capability(&self) -> Capability {
        self.capability.clone()
    }

    fn request(&self, params: &RequestParams) -> Result<RequestReply, Error> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        // What crosses the pipe is JSON.
        let wire = params.to_value().to_string();
        let params = RequestParams::from_value(&serde_json::from_str(&wire).unwrap()).unwrap();
        self.wire.lock().unwrap().push(wire);
        let Some((plain, secrets)) = self.answers.lock().unwrap().pop_front() else {
            return Ok(RequestReply::Decline);
        };
        let collector = self
            .verifier
            .collector(
                &self.host,
                Method::Request,
                &params.context,
                &params.recipient,
                self.now(),
            )
            .map_err(Error::from)?;
        let sealed: BTreeMap<String, Sealed> = secrets
            .iter()
            .map(|(name, value)| (name.clone(), collector.seal(name, value).unwrap()))
            .collect();
        let auth = collector
            .authenticate(&self.prover, &plain, &sealed, self.now(), DEFAULT_LIFETIME)
            .map_err(Error::from)?;
        // The person takes their time: the host's key binding may be over by the time the answer is back.
        self.clock
            .fetch_add(self.slow.load(Ordering::SeqCst), Ordering::SeqCst);
        let reply = RequestReply::Accept {
            plain,
            sealed,
            auth,
        };
        self.wire.lock().unwrap().push(reply.to_value().to_string());
        Ok(reply)
    }

    fn show_open(&self, open: &ShowOpen) -> Result<ShowOpened, Error> {
        self.wire
            .lock()
            .unwrap()
            .push(open.context.values.clone().unwrap_or_default());
        if let Some(answer) = *self.show_answer.lock().unwrap() {
            return Ok(if answer == "decline" {
                ShowOpened::Decline
            } else {
                ShowOpened::Cancel
            });
        }
        let receiver = Receiver::new(
            Method::Show,
            &open.context,
            &Limits::new(),
            &self.prover,
            &self.host,
            self.now(),
            DEFAULT_LIFETIME,
        )
        .map_err(Error::from)?;
        let recipient = receiver.recipient().clone();
        *self.pending.lock().unwrap() = Some((receiver, open.context.values.clone().unwrap()));
        Ok(ShowOpened::Ready(recipient))
    }

    fn show_deliver(&self, deliver: &ShowDeliver) -> Result<ShowDone, Error> {
        self.wire
            .lock()
            .unwrap()
            .push(ShowDeliver::to_value(deliver).to_string());
        let (receiver, plain) = self
            .pending
            .lock()
            .unwrap()
            .take()
            .expect("no open pending");
        let opened = receiver
            .open_values(
                &self.verifier,
                &self.host,
                &plain,
                &deliver.sealed,
                &deliver.auth,
                self.now(),
            )
            .map_err(Error::from)?;
        self.shown.lock().unwrap().push(
            opened
                .secrets
                .iter()
                .map(|(name, value)| (name.clone(), value.expose_str().to_owned()))
                .collect(),
        );
        Ok(ShowDone::Shown)
    }

    fn mode(&self) -> Mode {
        if self.bypass.load(Ordering::SeqCst) {
            Mode::Bypass
        } else {
            Mode::Ask
        }
    }

    fn approve(&self, approval: &Approval) -> Result<Decision, Error> {
        self.asked.lock().unwrap().push(approval.clone());
        // The hook runs once and is out of the lock while it runs, so a call it makes is not held up by itself.
        let hook = self.while_deciding.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(self
            .decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Decision::Deny))
    }
}

/// Stands in for the command runner: what it prints is made from the secret it was given.
struct Probe {
    runs: Arc<AtomicUsize>,
    seen: Seen,
    output: Output,
}

impl Runner for Probe {
    fn run(&self, spec: &CommandSpec<'_>) -> Result<CommandOutput, Error> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let secret = spec
            .secret
            .map(|(name, bytes)| (name.to_owned(), String::from_utf8(bytes.to_vec()).unwrap()));
        self.seen.lock().unwrap().push((
            spec.command.display(),
            secret.as_ref().map(|(name, _)| name.clone()),
        ));
        let mut printed = (self.output)(secret.as_ref().map_or("", |(_, value)| value.as_str()));
        // As the real runner does, it keeps no more than it was asked for.
        let cut = printed.len() > spec.max_output;
        printed.truncate(spec.max_output);
        Ok(CommandOutput {
            status: Some(0),
            stdout: Zeroizing::new(printed),
            stderr: Zeroizing::new(Vec::new()),
            truncated: cut,
            timed_out: false,
        })
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("coqui-acp-secrets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Setup {
    secrets: Arc<Secrets>,
    tools: SessionTools,
    person: Arc<Person>,
    clock: Arc<AtomicU64>,
    runs: Arc<AtomicUsize>,
    seen: Seen,
    events: Arc<Mutex<Vec<Event>>>,
    root: Dir,
}

/// Prints the secret in every form an accident might.
fn chatty(value: &str) -> Vec<u8> {
    format!(
        "raw={value}\nb64={}\nmid={}\nhex={}\njson={}\nurl={}\n",
        STANDARD.encode(value),
        STANDARD.encode(format!("user:{value}:tail")),
        HEXLOWER.encode(value.as_bytes()),
        serde_json::to_string(value).unwrap(),
        forms(value)
            .iter()
            .find(|(n, _)| *n == "percent")
            .unwrap()
            .1,
    )
    .into_bytes()
}

fn builder(keys: &Keys, clock: &Arc<AtomicU64>) -> SecretsBuilder {
    let clock = Arc::clone(clock);
    Secrets::builder(
        unprotected::ecdsa_p256(&scalar(1)).unwrap(),
        Verifier::builder(keys.host.id())
            .trust(keys.person.id())
            .build(),
    )
    .clock(move || clock.load(Ordering::SeqCst))
}

type Output = Box<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

fn setup_with(
    name: &str,
    settings: Settings,
    person: impl FnOnce(&Keys, &Arc<AtomicU64>) -> Arc<Person>,
) -> Setup {
    setup_out(name, settings, person, Box::new(chatty))
}

fn setup_out(
    name: &str,
    settings: Settings,
    person: impl FnOnce(&Keys, &Arc<AtomicU64>) -> Arc<Person>,
    output: Output,
) -> Setup {
    let keys = keys();
    let clock = Arc::new(AtomicU64::new(NOW));
    let person = person(&keys, &clock);
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let secrets = builder(&keys, &clock)
        .settings(settings)
        .runner(Probe {
            runs: Arc::clone(&runs),
            seen: Arc::clone(&seen),
            output,
        })
        .observer(move |event| sink.lock().unwrap().push(event.clone()))
        .build();
    let root = Dir::new(name);
    let tools = secrets
        .open_session(
            "s1",
            SessionConfig::new(
                Arc::clone(&person) as Arc<dyn Frontend>,
                vec![root.path().to_owned()],
            ),
        )
        .unwrap();
    Setup {
        secrets: Arc::new(secrets),
        tools,
        person,
        clock,
        runs,
        seen,
        events,
        root,
    }
}

fn setup(name: &str) -> Setup {
    setup_with(name, Settings::default(), Person::new)
}

fn call(tools: &SessionTools, name: &str, args: &Value) -> (Value, bool) {
    let result = tools.call(name, args);
    (serde_json::from_str(&result.text).unwrap(), result.is_error)
}

/// Asks for the password and returns its handle.
fn ask(setup: &Setup) -> String {
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let (result, error) = call(
        &setup.tools,
        "secret_request",
        &json!({"message": "The deploy needs a login", "schema": schema()}),
    );
    assert!(!error, "{result}");
    result["handles"]["password"].as_str().unwrap().to_owned()
}

#[test]
fn a_request_gives_the_agent_the_plain_values_and_a_handle_and_nothing_else() {
    let setup = setup("request");
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let result = setup.tools.call(
        "secret_request",
        &json!({"message": "The deploy needs a login", "schema": schema()}),
    );
    assert!(!result.is_error, "{}", result.text);
    let value: Value = serde_json::from_str(&result.text).unwrap();
    assert_eq!(value["status"], "accepted");
    assert_eq!(value["plain"], json!({"user": "ana"}));
    let handle = value["handles"]["password"].as_str().unwrap();
    assert!(handle.starts_with("secret_"));
    assert_eq!(value["handles"].as_object().unwrap().len(), 1);
    assert!(leaks(&result.text, PASSWORD).is_empty());
    assert_eq!(setup.tools.handle_count(), 1);
    // The frontend was asked once, and what crossed to it and back holds only sealed text.
    assert_eq!(setup.person.requests.load(Ordering::SeqCst), 1);
    for line in setup.person.wire.lock().unwrap().iter() {
        assert!(leaks(line, PASSWORD).is_empty(), "{line}");
    }
}

#[test]
fn a_declined_or_cancelled_request_makes_no_handle() {
    let setup = setup("declined");
    let (result, error) = call(
        &setup.tools,
        "secret_request",
        &json!({"message": "m", "schema": schema()}),
    );
    assert!(!error);
    assert_eq!(result["status"], "declined");
    assert_eq!(setup.tools.handle_count(), 0);
}

#[test]
fn a_form_with_no_secret_field_or_a_bad_schema_is_refused_before_the_person_is_asked() {
    let setup = setup("badform");
    let plain_only = json!({"type": "object", "properties": {"user": {"type": "string"}}});
    let bad = json!({"type": "object", "properties": {"p": {"type": "string", "writeOnly": true, "default": "x"}}});
    for schema in [plain_only, bad, json!(3)] {
        let (_, error) = call(
            &setup.tools,
            "secret_request",
            &json!({"message": "m", "schema": schema}),
        );
        assert!(error);
    }
    assert_eq!(setup.person.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn a_value_never_appears_in_a_result_an_update_a_history_an_event_or_a_debug_print() {
    let setup = setup("scan");
    let home = setup.root.path().to_owned();
    let mut surfaces: Vec<String> = Vec::new();
    // What a host would record of the conversation: the harness's own updates, built from the calls and their results.
    let mut transcript: Vec<String> = Vec::new();
    let mut record = |tools: &SessionTools, name: &str, args: Value| -> Value {
        let result = tools.call(name, &args);
        transcript.push(
            json!({"method": "session/update", "params": {"update": {
                "sessionUpdate": "tool_call", "title": name, "rawInput": args}}})
            .to_string(),
        );
        transcript.push(
            json!({"method": "session/update", "params": {"update": {
                "sessionUpdate": "tool_call_update", "status": "completed",
                "content": [{"type": "content", "content": {"type": "text", "text": result.text}}]}}})
            .to_string(),
        );
        surfaces.push(result.text.clone());
        serde_json::from_str(&result.text).unwrap()
    };

    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let requested = record(
        &setup.tools,
        "secret_request",
        json!({"message": "login", "schema": schema()}),
    );
    let handle = requested["handles"]["password"]
        .as_str()
        .unwrap()
        .to_owned();

    setup
        .person
        .will_decide(&[Decision::Once, Decision::Once, Decision::Once]);
    let ran = record(
        &setup.tools,
        "secret_use",
        json!({"handle": handle, "env": {"name": "REGISTRY_PASSWORD", "command": [SH, "--password-env", "REGISTRY_PASSWORD"]}}),
    );
    assert!(ran["stdout"].as_str().unwrap().contains(&handle));
    let wrote = record(
        &setup.tools,
        "secret_use",
        json!({"handle": handle, "file": {"path": "creds/../.env"}}),
    );
    // A `..` is refused, the plain path is written.
    assert!(wrote["error"].is_string());
    let wrote = record(
        &setup.tools,
        "secret_use",
        json!({"handle": handle, "file": {"path": ".env"}}),
    );
    assert_eq!(wrote["status"], "written");
    let shown = record(
        &setup.tools,
        "secret_show",
        json!({"message": "Your login", "schema": schema(), "values": {"user": "ana"},
               "secrets": {"password": {"handle": handle}}}),
    );
    assert_eq!(shown["status"], "shown");
    // Failures say why without a value.
    record(
        &setup.tools,
        "secret_use",
        json!({"handle": "secret_nope", "file": {"path": "x"}}),
    );
    record(&setup.tools, "secret_use", json!({"handle": handle}));

    // The work was really done with the real value, so the scan is not of an empty run.
    assert_eq!(
        std::fs::read_to_string(home.join(".env")).unwrap(),
        PASSWORD
    );
    assert_eq!(setup.seen.lock().unwrap().len(), 1);
    let shown_to_person = setup.person.shown.lock().unwrap();
    assert_eq!(shown_to_person[0]["password"], PASSWORD);

    // The MCP route holds it too.
    let mcp = setup
        .tools
        .handle_message(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "secret_use", "arguments": {"handle": handle, "env": {"name": "X_PW", "command": "echo"}}}}))
        .unwrap();
    surfaces.push(mcp.to_string());
    surfaces.extend(transcript);
    surfaces.extend(
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|event| format!("{event:?}")),
    );
    surfaces.push(format!(
        "{:?} {:?} {:?}",
        setup.secrets,
        setup.tools,
        setup.secrets.settings()
    ));
    surfaces.push(format!("{:?}", setup.tools.definitions()));
    surfaces.extend(setup.person.asked().iter().map(|ask| format!("{ask:?}")));
    assert!(surfaces.len() > 12);
    for text in &surfaces {
        assert!(
            leaks(text, PASSWORD).is_empty(),
            "{:?} in {text}",
            leaks(text, PASSWORD)
        );
    }
    // What the frontend saw of the host's side holds no value either.
    for line in setup.person.wire.lock().unwrap().iter() {
        assert!(leaks(line, PASSWORD).is_empty());
    }
}

#[test]
fn scrubbing_replaces_the_value_and_its_encodings_in_output() {
    let setup = setup("scrub");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "REGISTRY_PASSWORD", "command": "login"}}),
    );
    assert!(!error, "{result}");
    let stdout = result["stdout"].as_str().unwrap();
    assert!(leaks(stdout, PASSWORD).is_empty(), "{stdout}");
    // The probe printed six lines; each secret form became the handle.
    assert_eq!(stdout.matches(handle.as_str()).count(), 6, "{stdout}");
    assert!(stdout.contains("raw=secret_"));
}

#[test]
fn a_use_without_approval_does_not_run_or_write() {
    let setup = setup("deny");
    let handle = ask(&setup);
    // Nothing scripted: the person says no.
    let (command, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}}),
    );
    assert!(error);
    assert!(
        command["error"]
            .as_str()
            .unwrap()
            .contains("did not approve")
    );
    let (file, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "secret.txt"}}),
    );
    assert!(error, "{file}");
    assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
    assert!(!setup.root.path().join("secret.txt").exists());
    assert_eq!(setup.person.asked().len(), 2);
    // An explicit no is the same.
    setup.person.will_decide(&[Decision::Deny]);
    let (_, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}}),
    );
    assert!(error);
    assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn a_uses_approval_follows_the_permission_mode_and_a_session_allowance_is_exact() {
    // A secret's use is a permission like the agent's own. Asked in the other modes, with allow once and allow for the session.
    let setup = setup("modes");
    let handle = ask(&setup);
    let use_it = |command: &str| {
        call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "env": {"name": "PW", "command": [SH, command]}}),
        )
    };
    setup.person.will_decide(&[Decision::Once, Decision::Deny]);
    assert!(!use_it("deploy").1);
    assert!(
        use_it("deploy").1,
        "the second use of the same command is asked again"
    );
    assert_eq!(setup.person.asked().len(), 2);
    assert_eq!(setup.runs.load(Ordering::SeqCst), 1);

    setup.person.will_decide(&[Decision::Session]);
    assert!(!use_it("backup").1);
    assert!(
        !use_it("backup").1,
        "allowed for the session: not asked again"
    );
    assert_eq!(setup.person.asked().len(), 3);
    assert!(use_it("other").1, "another command is asked: and denied");
    assert_eq!(setup.person.asked().len(), 4);
    assert_eq!(setup.runs.load(Ordering::SeqCst), 3);
    // The approval names the secret by its field, the command in full, the program by its full path, and the folder notice.
    let asked = setup.person.asked();
    assert_eq!(asked[0].secret, "password");
    assert_eq!(asked[0].notice, Some(FOLDER_NOTICE));
    assert!(
        matches!(&asked[0].action, Action::RunCommand { variable, command, program, .. }
        if variable == "PW" && command == &format!("{SH} deploy") && program.is_absolute()
            && !program.starts_with(setup.root.path()))
    );
}

#[test]
fn in_bypass_a_use_is_approved_without_asking_and_a_change_of_mode_applies_at_once() {
    let setup = setup("bypass");
    let handle = ask(&setup);
    let args = json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}});
    setup.person.bypass.store(true, Ordering::SeqCst);
    assert!(!call(&setup.tools, "secret_use", &args).1);
    let (written, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "bypassed.key"}}),
    );
    assert!(!error, "{written}");
    assert!(
        setup.person.asked().is_empty(),
        "Bypass answers; nobody is asked"
    );
    assert_eq!(setup.runs.load(Ordering::SeqCst), 1);
    assert!(
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Event::Bypassed { .. }))
    );
    // Back to asking: the next use is put to the person, who says no.
    setup.person.bypass.store(false, Ordering::SeqCst);
    assert!(call(&setup.tools, "secret_use", &args).1);
    assert_eq!(setup.person.asked().len(), 1);
    // A show from a command is a use too.
    setup.person.bypass.store(true, Ordering::SeqCst);
    let (shown, error) = call(
        &setup.tools,
        "secret_show",
        &json!({"message": "m", "schema": schema(), "values": {"user": "ana"},
                "secrets": {"password": {"command": [SH, "read"]}}}),
    );
    assert!(!error, "{shown}");
    assert_eq!(setup.person.asked().len(), 1);
}

#[test]
fn a_session_allowance_is_for_one_secret_only() {
    let setup = setup("secretscope");
    let first = ask(&setup);
    let second = ask(&setup);
    setup
        .person
        .will_decide(&[Decision::Session, Decision::Deny]);
    let run = |handle: &str| {
        call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}}),
        )
        .1
    };
    assert!(!run(&first));
    assert!(!run(&first));
    assert!(
        run(&second),
        "the same command with another secret is asked"
    );
    assert_eq!(setup.person.asked().len(), 2);
}

#[test]
fn a_file_outside_the_roots_is_refused_and_the_person_is_not_asked() {
    let setup = setup("roots");
    let handle = ask(&setup);
    let outside = Dir::new("roots-outside");
    let target = outside.path().join("stolen.txt");
    setup.person.will_decide(&[Decision::Once; 8]);
    for path in [
        "../escape.txt".to_owned(),
        "sub/../../escape.txt".to_owned(),
        target.display().to_string(),
        "missing_dir/file.txt".to_owned(),
        String::new(),
    ] {
        let (result, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path}}),
        );
        assert!(error, "{path}: {result}");
    }
    assert!(!target.exists());
    assert!(
        !setup
            .root
            .path()
            .parent()
            .unwrap()
            .join("escape.txt")
            .exists()
    );
    assert!(setup.person.asked().is_empty());
}

#[cfg(unix)]
#[test]
fn a_link_cannot_take_a_secret_out_of_the_roots() {
    use std::os::unix::fs::symlink;
    let setup = setup("links");
    let handle = ask(&setup);
    let outside = Dir::new("links-outside");
    symlink(outside.path(), setup.root.path().join("door")).unwrap();
    std::fs::write(outside.path().join("victim"), "keep").unwrap();
    symlink(
        outside.path().join("victim"),
        setup.root.path().join("victim_link"),
    )
    .unwrap();
    setup.person.will_decide(&[Decision::Once; 4]);
    for path in ["door/stolen.txt", "victim_link"] {
        let (_, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path, "overwrite": true}}),
        );
        assert!(error, "{path}");
    }
    assert!(!outside.path().join("stolen.txt").exists());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("victim")).unwrap(),
        "keep"
    );
}

#[test]
fn a_file_is_written_under_the_roots_after_approval_and_the_person_is_told_the_agent_can_read_it() {
    let setup = setup("write");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 4]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "config/app.key"}}),
    );
    assert!(error, "the directory does not exist: {result}");
    std::fs::create_dir(setup.root.path().join("config")).unwrap();
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "config/app.key"}}),
    );
    assert!(!error, "{result}");
    assert_eq!(result["under_roots"], true);
    let file = setup.root.path().join("config").join("app.key");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), PASSWORD);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let asked = setup.person.asked();
    assert!(matches!(
        &asked[0].action,
        Action::WriteFile {
            under_roots: true,
            overwrite: false,
            mode: 0o600,
            ..
        }
    ));
    // An existing file is not replaced unless the call says so.
    let (_, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "config/app.key"}}),
    );
    assert!(error);
    let (_, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "config/app.key", "overwrite": true}}),
    );
    assert!(!error);
    // A mode wider than the settings allow is refused before anyone is asked.
    let before = setup.person.asked().len();
    let (_, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "wide.key", "mode": "0644"}}),
    );
    assert!(error);
    assert_eq!(setup.person.asked().len(), before);
}

#[test]
fn a_write_directory_of_the_settings_is_allowed_and_is_not_readable_by_the_agent() {
    let private = Dir::new("private-area");
    let mut settings = Settings::default();
    settings.write_dirs = vec![private.path().to_owned()];
    assert_eq!(settings.non_defaults().len(), 1);
    let setup = setup_with("writedirs", settings, Person::new);
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once]);
    let target = private.path().join("token");
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": target.display().to_string()}}),
    );
    assert!(!error, "{result}");
    assert_eq!(result["under_roots"], false);
    assert!(matches!(
        &setup.person.asked()[0].action,
        Action::WriteFile {
            under_roots: false,
            ..
        }
    ));
}

#[test]
fn the_sessions_end_clears_the_store_and_the_tools_refuse() {
    let setup = setup("end");
    let handle = ask(&setup);
    setup
        .person
        .will_decide(&[Decision::Session, Decision::Session]);
    let args = json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}});
    assert!(!call(&setup.tools, "secret_use", &args).1);
    assert_eq!(setup.tools.handle_count(), 1);

    setup.secrets.end_session("s1");
    assert_eq!(setup.tools.handle_count(), 0);
    let (result, error) = call(&setup.tools, "secret_use", &args);
    assert!(error, "{result}");
    assert!(
        call(
            &setup.tools,
            "secret_request",
            &json!({"message": "m", "schema": schema()})
        )
        .1
    );
    assert_eq!(
        setup.secrets.session("s1").unwrap_err(),
        Error::UnknownSession
    );
    assert_eq!(setup.runs.load(Ordering::SeqCst), 1);
    assert!(
        setup
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Event::Ended { cleared: 1, .. }))
    );
    // A new session with the same id starts empty: the old handle and the old "for the session" are gone.
    let again = setup
        .secrets
        .open_session(
            "s1",
            SessionConfig::new(
                Arc::clone(&setup.person) as Arc<dyn Frontend>,
                vec![setup.root.path().to_owned()],
            ),
        )
        .unwrap();
    assert_eq!(again.handle_count(), 0);
    assert!(call(&again, "secret_use", &args).1);
    // Ending twice, or a session that was never open, does nothing.
    setup.secrets.end_session("s1");
    setup.secrets.end_session("never");
}

#[test]
fn a_handle_belongs_to_its_session() {
    let setup = setup("sessions");
    let handle = ask(&setup);
    let other_person = Person::new(&keys(), &setup.clock);
    let other = setup
        .secrets
        .open_session(
            "s2",
            SessionConfig::new(
                other_person as Arc<dyn Frontend>,
                vec![setup.root.path().to_owned()],
            ),
        )
        .unwrap();
    let (result, error) = call(
        &other,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}}),
    );
    assert!(error, "{result}");
    assert!(result["error"].as_str().unwrap().contains("no such secret"));
    assert_eq!(
        setup
            .secrets
            .open_session(
                "s2",
                SessionConfig::new(setup.person.clone() as Arc<dyn Frontend>, vec![])
            )
            .unwrap_err(),
        Error::Path("a session needs at least one root")
    );
}

#[test]
fn a_request_goes_only_to_the_owners_frontend_and_only_its_identity_is_believed() {
    // Two sessions, two frontends: each is asked only for its own.
    let setup = setup("owner");
    let keys = keys();
    let attached = Person::new(&keys, &setup.clock);
    let other = setup
        .secrets
        .open_session(
            "s2",
            SessionConfig::new(
                Arc::clone(&attached) as Arc<dyn Frontend>,
                vec![setup.root.path().to_owned()],
            ),
        )
        .unwrap();
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    assert!(
        !call(
            &setup.tools,
            "secret_request",
            &json!({"message": "m", "schema": schema()})
        )
        .1
    );
    assert_eq!(setup.person.requests.load(Ordering::SeqCst), 1);
    assert_eq!(attached.requests.load(Ordering::SeqCst), 0);
    let _ = other;

    // A client that is not the owner answers a session it is attached to: it proves with a key the host does not know.
    let impostor = Person::with(
        &keys,
        &setup.clock,
        unprotected::ecdsa_p256(&scalar(9)).unwrap(),
    );
    let stolen = setup
        .secrets
        .open_session(
            "s3",
            SessionConfig::new(
                Arc::clone(&impostor) as Arc<dyn Frontend>,
                vec![setup.root.path().to_owned()],
            ),
        )
        .unwrap();
    impostor.will_type(r#"{"user":"mallory"}"#, &[("password", "attacker-chosen")]);
    let (result, error) = call(
        &stolen,
        "secret_request",
        &json!({"message": "m", "schema": schema()}),
    );
    assert!(error, "{result}");
    assert_eq!(stolen.handle_count(), 0);
}

#[test]
fn an_answer_after_the_binding_ended_is_refused() {
    let setup = setup_with("late", Settings::default(), |keys, clock| {
        let person = Person::new(keys, clock);
        person
            .slow
            .store(DEFAULT_LIFETIME.as_secs() + 31, Ordering::SeqCst);
        person
    });
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let (result, error) = call(
        &setup.tools,
        "secret_request",
        &json!({"message": "m", "schema": schema()}),
    );
    assert!(error, "{result}");
    assert_eq!(setup.tools.handle_count(), 0);
}

#[test]
fn every_request_has_a_new_id() {
    let setup = setup("ids");
    for _ in 0..3 {
        ask(&setup);
    }
    let ids: Vec<String> = setup
        .person
        .wire
        .lock()
        .unwrap()
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| value["requestId"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
}

#[test]
fn a_handle_expires_and_the_store_has_a_limit() {
    let mut settings = Settings::default();
    settings.max_handles = 2;
    settings.handle_lifetime = Duration::from_secs(60);
    let setup = setup_with("limits", settings, Person::new);
    let first = ask(&setup);
    ask(&setup);
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", "third")]);
    let (result, error) = call(
        &setup.tools,
        "secret_request",
        &json!({"message": "m", "schema": schema()}),
    );
    assert!(error, "{result}");
    assert_eq!(
        setup.person.requests.load(Ordering::SeqCst),
        2,
        "refused before the person was asked"
    );
    setup.clock.fetch_add(61, Ordering::SeqCst);
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": first, "env": {"name": "PW", "command": [SH, "deploy"]}}),
    );
    assert!(error, "{result}");
    assert_eq!(setup.tools.handle_count(), 0);
}

#[test]
fn the_tools_follow_what_the_frontend_offered() {
    let setup = setup_with("capability", Settings::default(), |keys, clock| {
        let mut person = Person::new(keys, clock);
        let person_mut = Arc::get_mut(&mut person).unwrap();
        let mut capability = Capability::all();
        capability.request = false;
        person_mut.capability = capability;
        person
    });
    let names: Vec<&str> = setup
        .tools
        .definitions()
        .iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["secret_show"]);
    assert!(
        call(
            &setup.tools,
            "secret_request",
            &json!({"message": "m", "schema": schema()})
        )
        .1
    );
    assert!(
        call(
            &setup.tools,
            "secret_use",
            &json!({"handle": "h", "file": {"path": "x"}})
        )
        .1
    );
    assert_eq!(setup.person.requests.load(Ordering::SeqCst), 0);
    let all = setup.secrets.open_session(
        "s9",
        SessionConfig::new(
            Person::new(&keys(), &setup.clock) as Arc<dyn Frontend>,
            vec![setup.root.path().to_owned()],
        ),
    );
    let names: Vec<&str> = all
        .as_ref()
        .unwrap()
        .definitions()
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["secret_request", "secret_use", "secret_show"]);
}

#[test]
fn a_show_from_a_command_needs_approval_and_the_output_never_comes_back() {
    let setup = setup("showcmd");
    let show = |decision: Option<Decision>| {
        if let Some(decision) = decision {
            setup.person.will_decide(&[decision]);
        }
        call(
            &setup.tools,
            "secret_show",
            &json!({"message": "Your new key", "schema": schema(), "values": {"user": "ana"},
                    "secrets": {"password": {"command": [SH, "read", "key"]}}}),
        )
    };
    let (result, error) = show(None);
    assert!(error, "{result}");
    assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&setup.person.asked()[0].action, Action::ShowCommand { command, .. } if command == &format!("{SH} read key"))
    );

    let (result, error) = show(Some(Decision::Once));
    assert!(!error, "{result}");
    assert_eq!(result, json!({"status": "shown"}));
    assert_eq!(setup.runs.load(Ordering::SeqCst), 1);
    // The probe printed lines; the person was shown the first one's text as the value (the trailing newline is not part of it).
    let shown = setup.person.shown.lock().unwrap();
    assert_eq!(shown.len(), 1);
    assert!(shown[0]["password"].starts_with("raw="));
}

#[test]
fn a_show_can_be_declined_and_a_show_needs_each_secret_field_named_once() {
    let setup = setup("showdecline");
    let handle = ask(&setup);
    *setup.person.show_answer.lock().unwrap() = Some("decline");
    let (result, error) = call(
        &setup.tools,
        "secret_show",
        &json!({"message": "m", "schema": schema(), "secrets": {"password": {"handle": handle}}}),
    );
    assert!(!error, "{result}");
    assert_eq!(result["status"], "declined");
    for secrets in [
        json!({}),
        json!({"password": {"handle": handle}, "user": {"handle": handle}}),
        json!({"password": {}}),
        json!({"password": {"handle": handle, "file": "x"}}),
    ] {
        let (_, error) = call(
            &setup.tools,
            "secret_show",
            &json!({"message": "m", "schema": schema(), "secrets": secrets}),
        );
        assert!(error, "{secrets}");
    }
}

#[test]
fn a_show_from_a_file_outside_the_roots_is_refused() {
    let setup = setup("showfile");
    let outside = Dir::new("showfile-outside");
    std::fs::write(outside.path().join("key"), "k").unwrap();
    std::fs::write(setup.root.path().join("key"), "inside\n").unwrap();
    let (result, error) = call(
        &setup.tools,
        "secret_show",
        &json!({"message": "m", "schema": schema(),
                "secrets": {"password": {"file": outside.path().join("key").display().to_string()}}}),
    );
    assert!(error, "{result}");
    let (result, error) = call(
        &setup.tools,
        "secret_show",
        &json!({"message": "m", "schema": schema(), "values": {"user": "ana"}, "secrets": {"password": {"file": "key"}}}),
    );
    assert!(!error, "{result}");
    assert_eq!(setup.person.shown.lock().unwrap()[0]["password"], "inside");
    assert!(
        setup.person.asked().is_empty(),
        "a file under the roots is not a use"
    );
}

#[test]
fn a_variable_that_changes_how_programs_start_cannot_carry_a_secret() {
    let setup = setup("variables");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 4]);
    for name in ["PATH", "ld_preload", "NODE_OPTIONS", "1X", "A-B", ""] {
        let (_, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "env": {"name": name, "command": ["x"]}}),
        );
        assert!(error, "{name}");
    }
    assert!(setup.person.asked().is_empty());
}

#[test]
fn the_tools_speak_mcp() {
    let setup = setup("mcp");
    let init = setup
        .tools
        .handle_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}))
        .unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(
        setup
            .tools
            .handle_message(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none()
    );
    let list = setup
        .tools
        .handle_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["secret_request", "secret_use", "secret_show"]);
    assert!(list["result"]["tools"][0]["inputSchema"]["required"].is_array());
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let call = setup
        .tools
        .handle_message(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "secret_request", "arguments": {"message": "m", "schema": schema()}}}))
        .unwrap();
    assert_eq!(call["result"]["isError"], false);
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(serde_json::from_str::<Value>(text).unwrap()["handles"]["password"].is_string());
    let missing = setup
        .tools
        .handle_message(&json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}))
        .unwrap();
    assert_eq!(missing["error"]["code"], -32601);

    // Over a line transport.
    let input = format!(
        "{}\nnot json\n\n{}\n",
        json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/list"})
    );
    let mut out = Vec::new();
    setup.tools.serve(input.as_bytes(), &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["id"], 5);
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["id"], 6);
}

#[test]
fn the_defaults_are_the_safe_choice_and_a_looser_setting_is_reported() {
    let defaults = Settings::default();
    assert!(defaults.non_defaults().is_empty());
    assert_eq!(defaults.file_mode, 0o600);
    assert!(defaults.write_dirs.is_empty());
    let mut loose = Settings::default();
    loose.max_handles = 99;
    loose.command_timeout = Duration::from_secs(600);
    loose.max_file_mode = 0o755;
    loose.inherit_env.push("AWS_SECRET_ACCESS_KEY".into());
    let named: Vec<&str> = loose.non_defaults().iter().map(|n| n.setting).collect();
    assert_eq!(
        named,
        [
            "max_handles",
            "command_timeout",
            "max_file_mode",
            "inherit_env"
        ]
    );
}

#[test]
fn the_process_runner_gives_the_secret_by_environment_only_and_ends_a_slow_command() {
    let (command, expected_empty) = if cfg!(windows) {
        (
            "echo [%SECRET_TEST_VAR%][%CARGO_MANIFEST_DIR%]",
            "[%CARGO_MANIFEST_DIR%]",
        )
    } else {
        (
            "printf '[%s][%s]' \"$SECRET_TEST_VAR\" \"$CARGO_MANIFEST_DIR\"",
            "[]",
        )
    };
    let line = coqui_acp_secrets::CommandLine::Shell(command.into());
    let settings = Settings::default();
    let cwd = std::env::temp_dir();
    let output = ProcessRunner
        .run(&CommandSpec {
            command: &line,
            program: &system_shell(),
            path: None,
            cwd: &cwd,
            secret: Some(("SECRET_TEST_VAR", b"s3cret")),
            timeout: Duration::from_secs(30),
            max_output: 1024,
            inherit_env: &settings.inherit_env,
        })
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status, Some(0), "{text}");
    assert!(text.contains("[s3cret]"), "{text}");
    assert!(
        text.contains(expected_empty),
        "the host's environment is not inherited: {text}"
    );

    let host_path = std::env::var_os("PATH");
    let slow = coqui_acp_secrets::CommandLine::Shell(
        if cfg!(windows) {
            "ping -n 20 127.0.0.1 > nul"
        } else {
            "sleep 20"
        }
        .into(),
    );
    let started = std::time::Instant::now();
    let output = ProcessRunner
        .run(&CommandSpec {
            command: &slow,
            program: &system_shell(),
            path: host_path.as_deref(),
            cwd: &cwd,
            secret: None,
            timeout: Duration::from_millis(300),
            max_output: 1024,
            inherit_env: &settings.inherit_env,
        })
        .unwrap();
    assert!(output.timed_out && output.status.is_none());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_verifier_error_names_a_rule_and_never_a_value() {
    let error = Error::from(SecretError::BadProof);
    assert!(!error.to_string().contains("secret_"));
}

#[cfg(unix)]
#[test]
fn a_directory_swapped_for_a_link_while_the_person_decides_gets_no_secret() {
    use std::os::unix::fs::symlink;
    let setup = setup("swap");
    let handle = ask(&setup);
    let outside = Dir::new("swap-outside");
    let sub = setup.root.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let (swap_sub, swap_to) = (sub.clone(), outside.path().to_owned());
    *setup.person.while_deciding.lock().unwrap() = Some(Box::new(move || {
        std::fs::remove_dir(&swap_sub).unwrap();
        symlink(&swap_to, &swap_sub).unwrap();
    }));
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "sub/token"}}),
    );
    assert!(error, "{result}");
    assert!(!outside.path().join("token").exists());
}

#[test]
fn a_path_with_a_control_character_is_refused() {
    let setup = setup("controls");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 3]);
    for path in ["a\nb", "a\u{202e}b", "tab\there"] {
        let (_, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path}}),
        );
        assert!(error, "{path:?}");
    }
    assert!(setup.person.asked().is_empty());
}

#[test]
fn output_cut_inside_the_secret_leaves_no_piece_of_it() {
    let mut settings = Settings::default();
    settings.max_output_bytes = 64;
    // The command prints filler and then the secret, so the limit falls inside the secret.
    let setup = setup_out(
        "straddle",
        settings,
        Person::new,
        Box::new(|value| format!("{}{value}{}", "a".repeat(60), "z".repeat(40)).into_bytes()),
    );
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "deploy"]}}),
    );
    assert!(!error, "{result}");
    let stdout = result["stdout"].as_str().unwrap();
    assert!(
        stdout.len() <= 64 && result["truncated"] == true,
        "{stdout}"
    );
    assert!(!stdout.contains(&PASSWORD[..3]), "{stdout}");
}

fn system_shell() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32")
            .join("cmd.exe")
    } else {
        PathBuf::from("/bin/sh")
    }
}

#[test]
fn a_program_planted_in_the_folder_does_not_take_the_place_of_a_system_tool() {
    let setup = setup("planted");
    let handle = ask(&setup);
    // Files an agent could plant under the roots, named like the tools a command or the shell would start.
    for name in ["deploy", "deploy.cmd", "deploy.exe", "cmd.exe", "sh"] {
        std::fs::write(setup.root.path().join(name), "planted").unwrap();
    }
    setup.person.will_decide(&[Decision::Once; 4]);
    // A bare name that is only in the folder is not found: the folder is not on the search path.
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": ["deploy"]}}),
    );
    assert!(error, "{result}");
    assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
    assert!(setup.person.asked().is_empty());
    // A name that exists as a system tool is that tool, whatever is planted beside the command.
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"]}}),
    );
    assert!(!error, "{result}");
    // Shell text starts the platform's shell by full path.
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": "echo hi"}}),
    );
    assert!(!error, "{result}");
    let asked = setup.person.asked();
    for approval in &asked {
        let Action::RunCommand { program, .. } = &approval.action else {
            panic!("{approval:?}")
        };
        assert!(
            program.is_absolute() && !program.starts_with(setup.root.path()),
            "{program:?}"
        );
    }
    let Action::RunCommand { program, .. } = &asked[1].action else {
        panic!()
    };
    assert_eq!(program.file_name(), system_shell().file_name());
}

#[test]
fn a_program_named_by_a_path_is_shown_as_the_file_it_is() {
    let setup = setup("pathprog");
    let handle = ask(&setup);
    let script = if cfg!(windows) { "run.cmd" } else { "run.sh" };
    std::fs::write(setup.root.path().join(script), "echo").unwrap();
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [format!("./{script}")]}}),
    );
    assert!(!error, "{result}");
    let Action::RunCommand { program, .. } = &setup.person.asked()[0].action else {
        panic!()
    };
    assert_eq!(program.file_name().unwrap().to_str().unwrap(), script);
    assert!(program.is_absolute());
}

#[test]
fn writing_over_a_hard_link_leaves_the_file_it_links_to_alone() {
    let setup = setup("hardlink");
    let handle = ask(&setup);
    let outside = Dir::new("hardlink-outside");
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, "keep me").unwrap();
    let link = setup.root.path().join("linked.txt");
    std::fs::hard_link(&victim, &link).unwrap();
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "linked.txt", "overwrite": true}}),
    );
    assert!(!error, "{result}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), PASSWORD);
    // No temporary file is left beside it.
    let names: Vec<_> = std::fs::read_dir(setup.root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
}

#[test]
fn what_the_person_reads_cannot_hide_anything() {
    let setup = setup("hidden");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 4]);
    // Paths with format characters, line separators, no-break spaces or a long run of spaces are refused.
    for path in [
        "a\u{200b}b",
        "a\u{2028}b",
        "a\u{a0}b",
        "a\u{202e}b",
        "a     b",
        "a\u{feff}",
    ] {
        let (_, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path}}),
        );
        assert!(error, "{path:?}");
    }
    assert!(setup.person.asked().is_empty());
    // A command shows them written out.
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "a\u{200e}b\u{2029}c", "d       e"]}}),
    );
    assert!(!error, "{result}");
    let Action::RunCommand { command, .. } = &setup.person.asked()[0].action else {
        panic!()
    };
    assert!(
        command
            .chars()
            .all(|c| !c.is_control() && c != '\u{200e}' && c != '\u{2029}'),
        "{command}"
    );
    assert!(
        command.contains("\\u{200e}") && command.contains("<7 spaces>"),
        "{command}"
    );
}

#[cfg(windows)]
#[test]
fn network_and_device_paths_are_refused_before_any_file_is_touched() {
    let setup = setup("winpaths");
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 6]);
    for path in [
        r"\\server\share\x.txt",
        r"\\?\UNC\server\share\x.txt",
        r"\\.\C:\x.txt",
        r"\\?\GLOBALROOT\Device\x",
        r"C:x.txt",
        r"D:\elsewhere\x.txt",
    ] {
        let (result, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path}}),
        );
        assert!(error, "{path}: {result}");
    }
    // A verbatim disk path under the root is the same as the plain one.
    let inside = format!(r"\\?\{}", setup.root.path().join("ok.txt").display());
    let plain = setup.root.path().join("ok2.txt").display().to_string();
    for path in [inside, plain] {
        let (result, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "file": {"path": path}}),
        );
        assert!(!error, "{path}: {result}");
    }
    assert!(setup.person.asked().len() == 2);
}

#[test]
fn only_one_use_runs_at_a_time() {
    let setup = setup("onceuse");
    let handle = ask(&setup);
    let (tools, inner) = (setup.tools.clone(), Arc::new(Mutex::new(None)));
    let sink = Arc::clone(&inner);
    let again = json!({"handle": handle, "env": {"name": "PW", "command": [SH, "second"]}});
    *setup.person.while_deciding.lock().unwrap() = Some(Box::new(move || {
        *sink.lock().unwrap() = Some(tools.call("secret_use", &again));
    }));
    setup.person.will_decide(&[Decision::Once]);
    let (_, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "first"]}}),
    );
    assert!(!error);
    let second = inner.lock().unwrap().take().unwrap();
    assert!(
        second.is_error && second.text.contains("still open"),
        "{}",
        second.text
    );
    assert_eq!(setup.runs.load(Ordering::SeqCst), 1);
}

#[test]
fn a_session_that_ends_while_the_person_decides_gets_no_use() {
    let setup = setup("endwhile");
    let handle = ask(&setup);
    let (secrets, hook_ran) = (Arc::clone(&setup.secrets), Arc::new(AtomicUsize::new(0)));
    let counter = Arc::clone(&hook_ran);
    *setup.person.while_deciding.lock().unwrap() = Some(Box::new(move || {
        secrets.end_session("s1");
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"]}}),
    );
    assert!(error, "{result}");
    assert_eq!(hook_ran.load(Ordering::SeqCst), 1);
    assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn a_variable_on_the_denied_list_cannot_carry_a_secret_and_the_list_is_the_persons_to_edit() {
    let mut settings = Settings::default();
    settings.denied_variables.push("SSLKEYLOGFILE".into());
    settings.denied_variables.push("MYAPP_*".into());
    let setup = setup_with("variables2", settings, Person::new);
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once; 12]);
    // The defaults, a name the person added, and a prefix they added; case does not matter.
    for name in [
        "PATH",
        "ld_preload",
        "DYLD_INSERT_LIBRARIES",
        "sslkeylogfile",
        "MYAPP_TOKEN_FILE",
    ] {
        let (_, error) = call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "env": {"name": name, "command": [SH]}}),
        );
        assert!(error, "{name}");
    }
    assert!(setup.person.asked().is_empty());
    // Not on the list: allowed (the list is not a catalogue).
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "HISTFILE", "command": [SH]}}),
    );
    assert!(!error, "{result}");
    // Emptying the list is a looser setting and is reported.
    let mut loose = Settings::default();
    loose.denied_variables.clear();
    assert!(
        loose
            .non_defaults()
            .iter()
            .any(|n| n.setting == "denied_variables")
    );
}

#[test]
fn the_plain_values_of_a_show_are_bounded() {
    let mut settings = Settings::default();
    settings.max_values_bytes = 32;
    let setup = setup_with("valuesbound", settings, Person::new);
    let handle = ask(&setup);
    let (result, error) = call(
        &setup.tools,
        "secret_show",
        &json!({"message": "m", "schema": schema(), "values": {"user": "x".repeat(100)},
                "secrets": {"password": {"handle": handle}}}),
    );
    assert!(error, "{result}");
    assert_eq!(setup.person.requests.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn a_timeout_ends_everything_the_command_started() {
    let dir = Dir::new("tree");
    let pid_file = dir.path().join("pid");
    let text = format!("sleep 60 & echo $! > {}; wait", pid_file.display());
    let line = coqui_acp_secrets::CommandLine::Shell(text);
    let settings = Settings::default();
    let output = ProcessRunner
        .run(&CommandSpec {
            command: &line,
            program: &system_shell(),
            path: None,
            cwd: dir.path(),
            secret: None,
            timeout: Duration::from_millis(1500),
            max_output: 1024,
            inherit_env: &settings.inherit_env,
        })
        .unwrap();
    assert!(output.timed_out);
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    std::thread::sleep(Duration::from_millis(300));
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the grandchild {pid} is still running");
}

#[test]
fn a_value_copied_before_the_session_ended_is_not_used_after() {
    use std::sync::OnceLock;
    let keys = keys();
    let clock = Arc::new(AtomicU64::new(NOW));
    let person = Person::new(&keys, &clock);
    let runs = Arc::new(AtomicUsize::new(0));
    let slot: Arc<OnceLock<Arc<Secrets>>> = Arc::new(OnceLock::new());
    let ender = Arc::clone(&slot);
    let secrets = Arc::new(
        builder(&keys, &clock)
            .runner(Probe {
                runs: Arc::clone(&runs),
                seen: Arc::default(),
                output: Box::new(chatty),
            })
            // The session ends just as the command is about to start, after the value was copied for it.
            .observer(move |event| {
                if matches!(event, Event::Ran { .. }) {
                    ender.get().unwrap().end_session("s1");
                }
            })
            .build(),
    );
    slot.set(Arc::clone(&secrets)).ok();
    let root = Dir::new("lateend");
    let tools = secrets
        .open_session(
            "s1",
            SessionConfig::new(
                Arc::clone(&person) as Arc<dyn Frontend>,
                vec![root.path().to_owned()],
            ),
        )
        .unwrap();
    person.will_type(r#"{"user":"ana"}"#, &[("password", PASSWORD)]);
    let (made, _) = call(
        &tools,
        "secret_request",
        &json!({"message": "m", "schema": schema()}),
    );
    let handle = made["handles"]["password"].as_str().unwrap().to_owned();
    person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"]}}),
    );
    assert!(error, "{result}");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

/// The output is the secret over and over and the limit falls inside one of them: whatever is kept is whole occurrences, scrubbed.
fn probe_cut(limit: usize) {
    let mut settings = Settings::default();
    settings.max_output_bytes = limit;
    let setup = setup_out(
        &format!("cut{limit}"),
        settings,
        Person::new,
        Box::new(|value| value.repeat(10).into_bytes()),
    );
    let handle = ask(&setup);
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"]}}),
    );
    assert!(!error, "{result}");
    let stdout = result["stdout"].as_str().unwrap();
    // Only whole occurrences were kept, each replaced by its handle (the result is then clipped, which can cut the last handle, and
    // a handle is longer than the secret): what is there is a prefix of the handles, and holds no byte of a partial occurrence.
    let expected = handle.repeat(limit / PASSWORD.len());
    assert!(
        !stdout.is_empty() && expected.starts_with(stdout),
        "limit {limit}: {stdout}"
    );
    assert!(stdout.starts_with(handle.as_str()) || stdout.len() < handle.len());
    assert!(
        !stdout.contains("Tr0") && !stdout.contains(&PASSWORD[..3]),
        "{stdout}"
    );
    assert_eq!(result["truncated"], true);
}

#[test]
fn a_cut_inside_a_repeated_secret_leaves_only_whole_scrubbed_occurrences() {
    probe_cut(59);
    probe_cut(124);
    probe_cut(PASSWORD.len() * 3);
}

#[test]
fn two_commands_that_read_alike_are_not_one_allowance() {
    // A backslash-n and a newline, four spaces and the text `<4 spaces>`, were shown alike and shared a "for the session".
    let setup = setup("ambiguous");
    let handle = ask(&setup);
    let run = |arg: &str| {
        call(
            &setup.tools,
            "secret_use",
            &json!({"handle": handle, "env": {"name": "PW", "command": [SH, arg]}}),
        )
        .1
    };
    for (first, second) in [
        ("a\\nb", "a\nb"),
        ("x    y", "x<4 spaces>y"),
        ("a\\u{200b}b", "a\u{200b}b"),
    ] {
        setup
            .person
            .will_decide(&[Decision::Session, Decision::Deny]);
        let before = setup.person.asked().len();
        assert!(!run(first));
        assert!(
            run(second),
            "{second:?} was run on the allowance of {first:?}"
        );
        let asked = setup.person.asked();
        assert_eq!(asked.len(), before + 2, "the second command was asked");
        let shown = |a: &Approval| match &a.action {
            Action::RunCommand { command, .. } => command.clone(),
            other => panic!("{other:?}"),
        };
        assert_ne!(
            shown(&asked[before]),
            shown(&asked[before + 1]),
            "{first:?} / {second:?}"
        );
    }
}

#[test]
fn a_hidden_character_in_a_field_name_is_written_out_in_the_approval() {
    let setup = setup("fieldname");
    let name = "pass\u{200b}word";
    let schema = json!({"type": "object", "properties": {"user": {"type": "string"},
        name: {"type": "string", "writeOnly": true}}, "required": ["user", name]});
    setup
        .person
        .will_type(r#"{"user":"ana"}"#, &[(name, PASSWORD)]);
    let (made, error) = call(
        &setup.tools,
        "secret_request",
        &json!({"message": "m", "schema": schema}),
    );
    assert!(!error, "{made}");
    let handle = made["handles"][name].as_str().unwrap().to_owned();
    setup.person.will_decide(&[Decision::Once]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"]}}),
    );
    assert!(!error, "{result}");
    assert_eq!(setup.person.asked()[0].secret, "pass\\u{200b}word");
}

#[cfg(unix)]
#[test]
fn a_resolved_path_with_a_hidden_character_is_refused() {
    // The agent's text has none, but a link it made inside the folder leads to a directory that does.
    let setup = setup("hiddenresolved");
    let handle = ask(&setup);
    std::fs::create_dir(setup.root.path().join("real\u{200b}dir")).unwrap();
    std::os::unix::fs::symlink(
        setup.root.path().join("real\u{200b}dir"),
        setup.root.path().join("door"),
    )
    .unwrap();
    setup.person.will_decide(&[Decision::Once; 2]);
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "file": {"path": "door/token"}}),
    );
    assert!(error, "{result}");
    let (result, error) = call(
        &setup.tools,
        "secret_use",
        &json!({"handle": handle, "env": {"name": "PW", "command": [SH, "x"], "cwd": "door"}}),
    );
    assert!(error, "{result}");
    assert!(setup.person.asked().is_empty());
}

#[cfg(unix)]
#[test]
fn a_background_process_does_not_outlive_a_command_that_exited() {
    let dir = Dir::new("bg");
    let pid_file = dir.path().join("pid");
    // The shell exits at once; the sleep it started holds the secret in its environment.
    let line = coqui_acp_secrets::CommandLine::Shell(format!(
        "sleep 60 & echo $! > {}",
        pid_file.display()
    ));
    let settings = Settings::default();
    let output = ProcessRunner
        .run(&CommandSpec {
            command: &line,
            program: &system_shell(),
            path: None,
            cwd: dir.path(),
            secret: Some(("SECRET_TEST_VAR", b"s3cret")),
            timeout: Duration::from_secs(30),
            max_output: 1024,
            inherit_env: &settings.inherit_env,
        })
        .unwrap();
    assert_eq!(output.status, Some(0));
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    std::thread::sleep(Duration::from_millis(300));
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the background process {pid} is still running");
}
