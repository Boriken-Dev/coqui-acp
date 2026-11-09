//! The checks, each answering what went wrong.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::time::Instant;

use guatiao::Secret;
use serde_json::{Value, json};

use super::Contract;
use super::host::{self, Conn, GRACE, PATIENCE, open, status};
use crate::{
    AcpHarness, Environment, FILES_UNDER, Requirement, SessionRecordQuery, StartRequest,
    is_session_id,
};

// `guatiao_status` numbers, as a channel's `close` and `closed` carry them.
const OK: u32 = 0;
const BAD_VALUE: u32 = 7;
const GONE: u32 = 12;
const UNSUPPORTED: u32 = 13;

/// A value that must never come back from a provider: put in an environment, looked for in every
/// answer.
const CANARY: &str = "coqui-acp-contract-canary-6b1d";

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}

type Outcome = Result<(), String>;

pub(super) fn all(contract: &Contract, harness: &dyn AcpHarness) -> Vec<(&'static str, Outcome)> {
    let place = match Place::make() {
        Ok(place) => place,
        Err(why) => return vec![("the suite's directories", Err(why))],
    };
    let checks: Vec<(&'static str, Outcome)> = vec![
        ("describes itself", describes_itself(contract, harness)),
        ("requirements only narrow", requirements(contract, harness)),
        ("no environment value comes back", keeps_secrets(harness)),
        ("speaks ACP", speaks_acp(contract, harness, &place)),
        (
            "answers what is not a request",
            malformed(contract, harness, &place),
        ),
        (
            "uses the host's session id",
            chosen_session_id(contract, harness, &place),
        ),
        (
            "reopens a session by its id",
            reopens(contract, harness, &place),
        ),
        ("a cancel ends the turn", cancel(contract, harness, &place)),
        (
            "the host answers permission",
            permission(contract, harness, &place),
        ),
        (
            "asks only what the client offers",
            client_capabilities(contract, harness, &place),
        ),
        ("holds a sender back", back_pressure(contract, harness)),
        (
            "close wakes a waiting send",
            close_wakes_send(contract, harness),
        ),
        (
            "close reaches an idle provider",
            close_reaches_idle(contract, harness),
        ),
        (
            "close answers what is pending",
            close_answers_pending(contract, harness, &place),
        ),
        (
            "the provider closes once",
            provider_closes(contract, harness, &place),
        ),
        ("a sink that has gone ends it", gone_sink(contract, harness)),
        (
            "working roots hold",
            working_roots(contract, harness, &place),
        ),
        (
            "a message over the limit ends it",
            message_limit(contract, harness),
        ),
        (
            "serves a real client",
            super::transport::serves_the_client(harness, &place.root),
        ),
    ];
    place.remove();
    checks
}

/// The directories sessions are opened in: `root`, with `root/inside`, a sibling `outside`, and
/// `root/link` leading to `outside` when the platform let the suite make a link.
pub(super) struct Place {
    base: PathBuf,
    pub(super) root: String,
    inside: String,
    outside: String,
    beside: String,
    link: Option<String>,
}

impl Place {
    fn make() -> Result<Self, String> {
        let temp = std::fs::canonicalize(std::env::temp_dir())
            .map_err(|e| format!("no temporary directory: {e}"))?;
        let base = temp.join(format!("acp-harness-contract-{}", fresh_uuid()));
        let root = base.join("root");
        let outside = base.join("outside");
        for dir in [root.join("inside"), outside.clone()] {
            std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {dir:?}: {e}"))?;
        }
        let link = root.join("link");
        let linked = make_link(&outside, &link).then(|| text(&link));
        Ok(Self {
            root: text(&root),
            inside: text(&root.join("inside")),
            outside: text(&outside),
            beside: format!("{}-beside", text(&root)),
            link: linked,
            base,
        })
    }

    fn remove(self) {
        if let Some(link) = &self.link {
            let _ = std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link));
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A path as text, without the `\\?\` prefix a canonical Windows path carries.
fn text(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

/// A junction, which Windows lets anyone make, unlike a symbolic link.
#[cfg(windows)]
fn make_link(target: &Path, link: &Path) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|out| out.status.success())
}

#[cfg(not(any(unix, windows)))]
fn make_link(_target: &Path, _link: &Path) -> bool {
    false
}

/// A version 4 UUID no other call picks: the clock and the process, hashed with keys `RandomState`
/// draws afresh for each half. Unique, not secret.
pub(super) fn fresh_uuid() -> String {
    use core::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut bytes = [0u8; 16];
    for half in bytes.chunks_mut(8) {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        half.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn initialize(id: u64, version: u16, client_capabilities: Value) -> Value {
    request(
        id,
        "initialize",
        json!({"protocolVersion": version, "clientCapabilities": client_capabilities}),
    )
}

fn new_session(id: u64, cwd: &str) -> Value {
    request(id, "session/new", json!({"cwd": cwd, "mcpServers": []}))
}

fn prompt(id: u64, session: &str, text: &str) -> Value {
    request(
        id,
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
    )
}

fn code(message: &Value) -> Option<i64> {
    message["error"]["code"].as_i64()
}

/// Opens a connection, initializes it and opens one session in `cwd`.
fn open_session(
    contract: &Contract,
    harness: &dyn AcpHarness,
    request: StartRequest,
    cwd: &str,
    capabilities: Value,
) -> Result<(Conn, String), String> {
    let mut conn = open(harness, request)?;
    conn.send(&initialize(0, contract.version, capabilities))?;
    let answer = conn.answer(0)?;
    ensure!(
        answer["result"]["protocolVersion"] == json!(contract.version),
        "initialize was answered {answer}"
    );
    conn.send(&new_session(1, cwd))?;
    let answer = conn.answer(1)?;
    let id = answer["result"]["sessionId"]
        .as_str()
        .ok_or_else(|| format!("session/new was answered {answer}"))?
        .to_owned();
    ensure!(is_session_id(&id), "the session id {id:?} is not a UUID");
    Ok((conn, id))
}

/// The text of every chunk until request `id` is answered, and its answer. An agent's request in
/// between is an error.
fn turn(conn: &mut Conn, id: u64) -> Result<(String, Value), String> {
    let mut text = String::new();
    loop {
        let message = conn.next()?;
        if message.get("method") == Some(&json!("session/update")) {
            if let Some(more) = message["params"]["update"]["content"]["text"].as_str() {
                text.push_str(more);
            }
            continue;
        }
        if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
            return Ok((text, message));
        }
        return Err(format!("the agent sent {message} during the turn"));
    }
}

fn describes_itself(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    let info = harness.describe().map_err(|e| e.message().to_owned())?;
    ensure!(!info.title.is_empty(), "no title");
    ensure!(
        info.versions.contains(&contract.version),
        "describe lists ACP versions {:?}, not {}",
        info.versions,
        contract.version
    );
    harness.program().map_err(|e| e.message().to_owned())?;
    harness
        .models(Secret::default())
        .map_err(|e| e.message().to_owned())?;
    harness
        .session_record(SessionRecordQuery::new(fresh_uuid(), "/"))
        .map_err(|e| e.message().to_owned())?;
    harness.non_defaults().map_err(|e| e.message().to_owned())?;
    Ok(())
}

/// Answers whether `start` refused `request` with `expected`, both objects destroyed.
fn refused_with(harness: &dyn AcpHarness, request: StartRequest, expected: u32) -> Outcome {
    match host::start(harness, request) {
        Ok(conn) => {
            conn.channel().close(OK);
            Err(format!("started where it had to refuse with {expected}"))
        }
        Err((error, conn)) => {
            ensure!(
                status(&error) == expected,
                "refused with {} ({}), not {expected}",
                status(&error),
                error.message()
            );
            conn.destroyed()
        }
    }
}

fn requirements(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    let refusal = |requirements: Vec<Requirement>| {
        harness
            .refusal(requirements, Secret::default())
            .map_err(|e| status(&e))
    };
    let with = |requirements: &[Requirement]| {
        let mut request = StartRequest::new();
        request.requirements = requirements.to_vec();
        request
    };
    for held in &contract.holds {
        ensure!(
            refusal(vec![held.clone()]) == Ok(None),
            "refusal of {} with no environment was not None",
            held.name
        );
        open(harness, with(core::slice::from_ref(held)))?.close_with(OK)?;
    }
    ensure!(
        refusal(contract.holds.clone()) == Ok(None),
        "the requirements it holds were refused together"
    );
    open(harness, with(&contract.holds))?.close_with(OK)?;

    let unknown = Requirement::new("no_such_requirement");
    ensure!(
        matches!(refusal(vec![unknown.clone()]), Ok(Some(_))),
        "a requirement it does not know was not refused"
    );
    let mut asked = contract.holds.clone();
    asked.push(unknown);
    ensure!(
        matches!(refusal(asked.clone()), Ok(Some(_))),
        "a requirement it does not know, beside ones it holds, was not refused"
    );
    refused_with(harness, with(&asked), UNSUPPORTED)?;

    let unreadable: Vec<(&str, Vec<Requirement>)> = vec![
        (
            "a path that is not absolute",
            vec![Requirement::from(crate::Narrowing::FilesUnder(vec![
                "relative".into(),
            ]))],
        ),
        ("a list given no value", vec![Requirement::new(FILES_UNDER)]),
        (
            "a value given to a name that takes none",
            vec![Requirement::with_value(crate::READ_ONLY, "yes")],
        ),
        (
            "a name given twice",
            vec![
                Requirement::new(crate::NO_COMMANDS),
                Requirement::new(crate::NO_COMMANDS),
            ],
        ),
    ];
    for (what, requirements) in unreadable {
        ensure!(
            refusal(requirements.clone()) == Err(BAD_VALUE),
            "refusal of {what} was not GUATIAO_ERR_BAD_VALUE"
        );
        refused_with(harness, with(&requirements), BAD_VALUE)
            .map_err(|why| format!("{what}: {why}"))?;
    }
    refused_with(
        harness,
        StartRequest::builder().session_id("not-a-uuid").build(),
        BAD_VALUE,
    )
    .map_err(|why| format!("a session id that is not a UUID: {why}"))
}

fn keeps_secrets(harness: &dyn AcpHarness) -> Outcome {
    let environment: Environment = [("COQUI_ACP_CONTRACT_SECRET", CANARY)]
        .into_iter()
        .collect();
    let secret = || Secret::new(environment.clone());
    let unknown = vec![Requirement::new("no_such_requirement")];
    let answered = format!(
        "{:?} {:?} {:?}",
        harness.refusal(unknown.clone(), secret()),
        harness.models(secret()),
        harness.session_record(
            SessionRecordQuery::new(fresh_uuid(), "/").environment(environment.clone())
        ),
    );
    ensure!(
        !answered.contains(CANARY),
        "an answer carried an environment value"
    );
    let request = StartRequest::builder()
        .environment(environment.clone())
        .requirement(Requirement::new("no_such_requirement"))
        .build();
    match host::start(harness, request) {
        Ok(conn) => {
            conn.channel().close(OK);
            Err("started with a requirement it does not know".to_owned())
        }
        Err((error, conn)) => {
            ensure!(
                !error.message().contains(CANARY),
                "a refusal carried an environment value"
            );
            conn.destroyed()
        }
    }
}

fn speaks_acp(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    conn.send(&prompt(2, &session, "hello"))?;
    let update = conn.next()?;
    ensure!(
        update["method"] == "session/update" && update["params"]["sessionId"] == json!(session),
        "a prompt's first message was {update}"
    );
    ensure!(
        update["params"]["update"]["content"]["text"] == "echo: hello",
        "the prompt was echoed as {update}"
    );
    let answer = conn.answer(2)?;
    ensure!(
        answer["result"]["stopReason"] == "end_turn",
        "the turn ended {answer}"
    );
    conn.close_with(OK)
}

fn malformed(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, _) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    let cases: [(&str, Vec<u8>, i64, Value); 8] = [
        ("text that is not JSON", b"not JSON".to_vec(), -32700, Value::Null),
        (
            "a batch",
            br#"[{"jsonrpc":"2.0","id":3,"method":"initialize","params":{}}]"#.to_vec(),
            -32600,
            Value::Null,
        ),
        ("a number", b"5".to_vec(), -32600, Value::Null),
        (
            "a method that is not text",
            br#"{"jsonrpc":"2.0","id":4,"method":5}"#.to_vec(),
            -32600,
            json!(4),
        ),
        (
            "params that are a list",
            br#"{"jsonrpc":"2.0","id":5,"method":"session/new","params":[]}"#.to_vec(),
            -32602,
            json!(5),
        ),
        (
            "a cwd that is not text",
            br#"{"jsonrpc":"2.0","id":6,"method":"session/new","params":{"cwd":5,"mcpServers":[]}}"#
                .to_vec(),
            -32602,
            json!(6),
        ),
        (
            "a prompt that is not a list",
            br#"{"jsonrpc":"2.0","id":7,"method":"session/prompt","params":{"sessionId":"x","prompt":"hi"}}"#
                .to_vec(),
            -32602,
            json!(7),
        ),
        (
            "a method nobody has",
            br#"{"jsonrpc":"2.0","id":8,"method":"no/such","params":{}}"#.to_vec(),
            -32601,
            json!(8),
        ),
    ];
    for (what, bytes, expected, id) in cases {
        conn.send_bytes(&bytes)?;
        let answer = conn.next()?;
        ensure!(
            code(&answer) == Some(expected) && answer["id"] == id,
            "{what} was answered {answer}, not {expected} for id {id}"
        );
    }
    conn.send(&notification("no/such", json!({})))?;
    conn.send(&initialize(9, contract.version, json!({})))?;
    let answer = conn.next()?;
    ensure!(
        answer["id"] == 9,
        "a notification nobody has was answered: {answer}"
    );
    conn.close_with(OK)
}

fn chosen_session_id(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let chosen = fresh_uuid();
    let request = || StartRequest::builder().session_id(chosen.clone()).build();
    let (mut conn, first) = open_session(contract, harness, request(), &place.root, json!({}))?;
    ensure!(
        first == chosen,
        "the first session is {first}, not the host's {chosen}"
    );
    conn.send(&new_session(2, &place.root))?;
    let answer = conn.answer(2)?;
    let second = answer["result"]["sessionId"].as_str().unwrap_or_default();
    ensure!(
        is_session_id(second) && second != chosen,
        "a second session/new was answered {answer}"
    );
    conn.close_with(OK)?;

    let mut again = open(harness, request())?;
    again.send(&initialize(0, contract.version, json!({})))?;
    again.answer(0)?;
    again.send(&new_session(1, &place.root))?;
    let answer = again.answer(1)?;
    ensure!(
        code(&answer).is_some(),
        "a session/new under an id that names a session already was answered {answer}"
    );
    again.close_with(OK)
}

fn reopens(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let takes = harness
        .describe()
        .map_err(|e| e.message().to_owned())?
        .takes;
    let methods: Vec<&str> = [
        ("session/load", takes.load),
        ("session/resume", takes.resume),
    ]
    .into_iter()
    .filter_map(|(method, taken)| taken.then_some(method))
    .collect();
    for method in methods {
        let chosen = fresh_uuid();
        let (conn, _) = open_session(
            contract,
            harness,
            StartRequest::builder().session_id(chosen.clone()).build(),
            &place.root,
            json!({}),
        )?;
        conn.close_with(OK)?;

        let mut conn = open(harness, StartRequest::new())?;
        conn.send(&initialize(0, contract.version, json!({})))?;
        conn.answer(0)?;
        let reopen = |id: u64, session: &str, cwd: &str| {
            request(
                id,
                method,
                json!({"sessionId": session, "cwd": cwd, "mcpServers": []}),
            )
        };
        conn.send(&reopen(1, &chosen, &place.root))?;
        let answer = conn.answer(1)?;
        ensure!(
            code(&answer).is_none(),
            "{method} of a known session was answered {answer}"
        );
        conn.send(&prompt(2, &chosen, "again"))?;
        let (text, _) = turn(&mut conn, 2)?;
        ensure!(
            text == "echo: again",
            "the reopened session answered {text:?}"
        );
        conn.send(&reopen(3, &fresh_uuid(), &place.root))?;
        let answer = conn.answer(3)?;
        ensure!(
            code(&answer).is_some(),
            "{method} of an unknown session was answered {answer}"
        );
        conn.close_with(OK)?;

        let mut conn = open(
            harness,
            StartRequest::builder()
                .working_root(place.inside.clone())
                .build(),
        )?;
        conn.send(&initialize(0, contract.version, json!({})))?;
        conn.answer(0)?;
        conn.send(&reopen(1, &chosen, &place.root))?;
        let answer = conn.answer(1)?;
        ensure!(
            code(&answer) == Some(-32602),
            "{method} outside the working roots was answered {answer}"
        );
        conn.close_with(OK)?;
    }
    Ok(())
}

fn cancel(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    conn.send(&prompt(2, &session, "wait"))?;
    ensure!(conn.quiet()?, "a waiting turn answered before any cancel");
    conn.send(&notification(
        "session/cancel",
        json!({"sessionId": session}),
    ))?;
    let (_, answer) = turn(&mut conn, 2)?;
    ensure!(
        answer["result"]["stopReason"] == "cancelled",
        "a cancelled turn ended {answer}"
    );
    conn.close_with(OK)
}

fn permission(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    let selected =
        |option: &str| json!({"result": {"outcome": {"outcome": "selected", "optionId": option}}});
    let cases = [
        ("allow", selected("allow"), "echo: allowed", "end_turn"),
        ("reject", selected("reject"), "echo: refused", "end_turn"),
        (
            "an error",
            json!({"error": {"code": -32603, "message": "the host failed"}}),
            "echo: refused",
            "end_turn",
        ),
        (
            "an outcome it does not know",
            json!({"result": {"outcome": {"outcome": "maybe"}}}),
            "echo: refused",
            "end_turn",
        ),
        (
            "a selected option it never offered",
            selected("always"),
            "echo: refused",
            "end_turn",
        ),
        (
            "cancelled",
            json!({"result": {"outcome": {"outcome": "cancelled"}}}),
            "",
            "cancelled",
        ),
    ];
    for (id, (what, reply, text, stop)) in (10u64..).zip(cases) {
        conn.send(&prompt(id, &session, "ask"))?;
        let asked = loop {
            let message = conn.next()?;
            if message.get("method") == Some(&json!("session/update")) {
                continue;
            }
            break message;
        };
        ensure!(
            asked["method"] == "session/request_permission"
                && asked["params"]["sessionId"] == json!(session)
                && asked.get("id").is_some(),
            "a prompt of `ask` sent {asked}"
        );
        let offered: Vec<&str> = asked["params"]["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["optionId"].as_str())
            .collect();
        ensure!(
            offered.contains(&"allow") && offered.contains(&"reject"),
            "the permission offered {offered:?}"
        );
        let mut answer = reply;
        answer["jsonrpc"] = json!("2.0");
        answer["id"] = asked["id"].clone();
        conn.send(&answer)?;
        let (said, end) = turn(&mut conn, id)?;
        ensure!(
            said == text && end["result"]["stopReason"] == stop,
            "answered {what}, the turn said {said:?} and ended {end}"
        );
    }
    conn.close_with(OK)
}

fn client_capabilities(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let file = format!("{}/notes.txt", place.root);
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    conn.send(&prompt(2, &session, &format!("read {file}")))?;
    let (text, _) = turn(&mut conn, 2)?;
    ensure!(
        text == "echo: no fs",
        "a read with no fs offered said {text:?}"
    );
    ensure!(
        !conn.asked("fs/read_text_file") && !conn.asked("terminal/create"),
        "the agent asked what the client never offered"
    );
    conn.close_with(OK)?;

    let offered = json!({"fs": {"readTextFile": true, "writeTextFile": false}, "terminal": false});
    let (mut conn, session) =
        open_session(contract, harness, StartRequest::new(), &place.root, offered)?;
    conn.send(&prompt(2, &session, &format!("read {file}")))?;
    let asked = loop {
        let message = conn.next()?;
        if message.get("method") != Some(&json!("session/update")) {
            break message;
        }
    };
    ensure!(
        asked["method"] == "fs/read_text_file" && asked["params"]["path"] == json!(file),
        "a read with fs offered sent {asked}"
    );
    conn.send(&json!({"jsonrpc": "2.0", "id": asked["id"], "result": {"content": "hi"}}))?;
    let (text, _) = turn(&mut conn, 2)?;
    ensure!(
        text == "echo: hi",
        "the file's content was echoed as {text:?}"
    );
    conn.close_with(OK)
}

/// Sends `count` messages on a thread of its own, counting each one taken, until one is refused.
fn sender(
    channel: Arc<guatiao::library::Object<dyn guatiao_channel::Channel>>,
    version: u16,
    count: u64,
) -> (Arc<AtomicUsize>, std::thread::JoinHandle<Option<u32>>) {
    let taken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&taken);
    let thread = std::thread::spawn(move || {
        for id in 0..count {
            let message = initialize(id, version, json!({})).to_string();
            if let Err(e) = channel.send(message.as_bytes()) {
                return Some(status(&e));
            }
            counted.fetch_add(1, Ordering::SeqCst);
        }
        None
    });
    (taken, thread)
}

/// Waits until `taken` stops growing, and answers where it stopped.
fn settled(taken: &AtomicUsize) -> usize {
    let mut last = taken.load(Ordering::SeqCst);
    loop {
        std::thread::sleep(GRACE);
        let now = taken.load(Ordering::SeqCst);
        if now == last {
            return now;
        }
        last = now;
    }
}

fn back_pressure(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    const MANY: u64 = 64;
    let mut conn = open(harness, StartRequest::new())?;
    conn.hold();
    let (taken, thread) = sender(conn.shared_channel(), contract.version, MANY);
    let held = settled(&taken);
    ensure!(held < MANY as usize, "a held sink held no sender back");
    ensure!(conn.received() == 0, "a held sink took a message");
    conn.release();
    ensure!(
        thread
            .join()
            .map_err(|_| "the sender panicked".to_owned())?
            .is_none(),
        "a send was refused once the sink took again"
    );
    for id in 0..MANY {
        let answer = conn.next()?;
        ensure!(answer["id"] == id, "answers came out of order: {answer}");
    }
    conn.close_with(OK)
}

fn close_wakes_send(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    let conn = open(harness, StartRequest::new())?;
    conn.hold();
    let (taken, thread) = sender(conn.shared_channel(), contract.version, 64);
    settled(&taken);
    let closing = Instant::now();
    conn.channel().close(OK);
    let woken = thread
        .join()
        .map_err(|_| "the sender panicked".to_owned())?;
    ensure!(
        woken == Some(GONE),
        "a send waiting for room was answered {woken:?}, not GONE, by close"
    );
    ensure!(
        closing.elapsed() < PATIENCE,
        "close took too long to wake a send"
    );
    let after = conn.channel().send(b"{}").map_err(|e| status(&e));
    ensure!(
        after == Err(GONE),
        "a send after close was answered {after:?}"
    );
    conn.release();
    ensure!(
        conn.closed()? == OK,
        "closed did not carry the host's status"
    );
    conn.destroyed()
}

fn close_reaches_idle(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    for code in [OK, UNSUPPORTED] {
        let mut conn = open(harness, StartRequest::new())?;
        conn.send(&initialize(0, contract.version, json!({})))?;
        conn.answer(0)?;
        // The provider now waits for a message; close has to reach it there.
        std::thread::sleep(GRACE);
        conn.close_with(code)?;
    }
    Ok(())
}

fn close_answers_pending(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    conn.send(&prompt(2, &session, "wait"))?;
    ensure!(conn.quiet()?, "a waiting turn answered before the close");
    conn.channel().close(OK);
    let (_, answer) = turn(&mut conn, 2)?;
    ensure!(
        answer["result"]["stopReason"] == "cancelled",
        "a turn the host closed on ended {answer}"
    );
    ensure!(
        conn.closed()? == OK,
        "closed did not carry the host's status"
    );
    conn.destroyed()
}

fn provider_closes(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let (mut conn, session) = open_session(
        contract,
        harness,
        StartRequest::new(),
        &place.root,
        json!({}),
    )?;
    conn.send(&prompt(2, &session, "close"))?;
    let (_, answer) = turn(&mut conn, 2)?;
    ensure!(
        answer["result"]["stopReason"] == "end_turn",
        "the prompt was not answered before closing: {answer}"
    );
    ensure!(
        conn.closed()? == OK,
        "the provider's own end was not closed(OK)"
    );
    ensure!(
        conn.spawner_gone_at_close()?,
        "the spawner was still alive when the provider called closed"
    );
    let after = conn.channel().send(b"{}").map_err(|e| status(&e));
    ensure!(
        after == Err(GONE),
        "the first send after closed was answered {after:?}"
    );
    conn.channel().close(OK);
    conn.destroyed()
}

fn gone_sink(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    let conn = open(harness, StartRequest::new())?;
    conn.go_away();
    conn.send(&initialize(0, contract.version, json!({})))?;
    let closed = conn.closed()?;
    ensure!(
        closed == GONE,
        "a sink answering GONE ended it with {closed}"
    );
    conn.destroyed()
}

fn working_roots(contract: &Contract, harness: &dyn AcpHarness, place: &Place) -> Outcome {
    let rooted = StartRequest::builder()
        .working_root(place.root.clone())
        .build();
    let (mut conn, _) = open_session(contract, harness, rooted, &place.root, json!({}))?;
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let mut cases: Vec<(String, Value, bool)> = vec![
        ("the root".into(), json!({"cwd": place.root}), true),
        (
            "a directory inside".into(),
            json!({"cwd": place.inside}),
            true,
        ),
        (
            "a sibling sharing its prefix".into(),
            json!({"cwd": place.beside}),
            false,
        ),
        (
            "a path climbing out with `..`".into(),
            json!({"cwd": format!("{}{separator}..{separator}outside", place.root)}),
            false,
        ),
        ("a relative path".into(), json!({"cwd": "inside"}), false),
        (
            "an additional directory outside".into(),
            json!({"cwd": place.root, "additionalDirectories": [place.outside]}),
            false,
        ),
        (
            "an additional directory inside".into(),
            json!({"cwd": place.root, "additionalDirectories": [place.inside]}),
            true,
        ),
    ];
    if cfg!(windows) {
        cases.push((
            "the root in other case".into(),
            json!({"cwd": place.root.to_uppercase()}),
            true,
        ));
    }
    if let Some(link) = &place.link {
        cases.push((
            "a link inside leading outside".into(),
            json!({"cwd": link}),
            false,
        ));
    }
    for (id, (what, mut params, admitted)) in (10u64..).zip(cases) {
        params["mcpServers"] = json!([]);
        conn.send(&request(id, "session/new", params))?;
        let answer = conn.answer(id)?;
        if admitted {
            ensure!(
                answer["result"]["sessionId"].is_string(),
                "{what} was refused: {answer}"
            );
        } else {
            ensure!(
                code(&answer) == Some(-32602),
                "{what} was answered {answer}"
            );
        }
    }
    conn.close_with(OK)
}

fn message_limit(contract: &Contract, harness: &dyn AcpHarness) -> Outcome {
    let Some(limit) = contract.message_limit else {
        return Ok(());
    };
    let conn = open(harness, StartRequest::new())?;
    let head = r#"{"jsonrpc":"2.0","method":"x","params":{"pad":""#;
    let tail = r#""}}"#;
    let pad = (limit + 1).saturating_sub(head.len() + tail.len());
    let message = format!("{head}{}{tail}", "a".repeat(pad));
    let sent = conn
        .channel()
        .send(message.as_bytes())
        .map_err(|e| status(&e));
    ensure!(
        sent == Err(BAD_VALUE),
        "a message over the limit was answered {sent:?}"
    );
    let closed = conn.closed()?;
    ensure!(
        closed == BAD_VALUE,
        "a message over the limit ended it with {closed}"
    );
    conn.channel().close(OK);
    conn.destroyed()
}
