//! OpenCode through the stdio providers, against the stand-in `fake_stdio_cli` playing `opencode`: what an account starts
//! it with, the sessions of a data folder (listed and opened by the agent, exported by `opencode export`), and the
//! sign-in that makes a key account.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{Map, Status, Value};
use coqui_acp_api::{
    AccountGrant, AccountProvider, AcpHarness, Channel, ChannelSink, Credentials, Environment,
    LoginAnswer, LoginForm, LoginPrompter, LoginRequest, OpenUrl, ProcessSpawner, SessionExport,
    SessionImport, StartRequest,
};
use coqui_acp_harness::LocalSpawner;
use coqui_acp_harness::stdio::{
    OpenCodeAccountSettings, OpenCodeAccounts, OpenCodeProvider, OpenCodeSettings, StdioHarness,
    StdioProvider, StdioSettings,
};
use coqui_acp_history::{Body, Document};
use serde_json::json;

const KEY: &str = "opencode-key-value-5d2e";

fn fake() -> String {
    let tests = std::env::current_exe().expect("the test knows where it is");
    tests
        .parent()
        .and_then(Path::parent)
        .expect("a test binary is in target/<profile>/deps")
        .join("examples")
        .join(format!("fake_stdio_cli{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned()
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("coqui-acp-opencode-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn provider() -> OpenCodeProvider {
    let mut settings = OpenCodeSettings::new();
    settings.executable = Some(fake());
    OpenCodeProvider::new(settings)
}

fn text(path: &Path) -> &str {
    path.to_str().expect("text")
}

/// A key account whose data folder is `dir`.
fn key_grant(dir: &Path) -> AccountGrant {
    let mut settings = Map::new();
    settings.set("variable", Value::from("OPENAI_API_KEY"));
    AccountGrant::new("a-key", "opencode-api-key")
        .settings(Value::from(settings))
        .credentials(Credentials::from_iter([("api_key", KEY)]))
        .state_dir(text(dir))
}

/// The folder of sessions the stand-in lists: `<data>/opencode/fixture`.
fn fixtures(dir: &Path) -> PathBuf {
    let folder = dir.join("opencode").join("fixture");
    std::fs::create_dir_all(&folder).expect("a fixture folder");
    folder
}

/// What `opencode export` prints for a session of one prompt, one command and one answer: the shape measured on 1.19.6,
/// with a path of no one's.
fn export_of(id: &str) -> serde_json::Value {
    json!({
        "info": {
            "id": id, "slug": "proud-river", "projectID": "global", "directory": "/work/project",
            "title": "list the files", "agent": "build",
            "model": {"id": "stub-1", "providerID": "stub", "variant": "default"},
            "version": "1.19.6", "time": {"created": 1_795_002_585_122_i64, "updated": 1_795_002_586_105_i64},
        },
        "messages": [
            {"info": {"role": "user", "id": "msg_1", "time": {"created": 1_795_002_585_133_i64}, "agent": "build"},
             "parts": [{"type": "text", "text": "list the files", "id": "prt_1"}]},
            {"info": {"role": "assistant", "id": "msg_2", "providerID": "stub", "modelID": "stub-1", "finish": "tool-calls",
                      "time": {"created": 1_795_002_585_519_i64, "completed": 1_795_002_586_079_i64}},
             "parts": [
                {"type": "step-start", "id": "prt_2"},
                {"type": "reasoning", "text": "thinking about it", "id": "prt_3"},
                {"type": "tool", "tool": "bash", "callID": "call_1", "id": "prt_4",
                 "state": {"status": "completed", "input": {"command": "ls", "description": "list"},
                           "output": "a.txt\nb.txt\n", "title": "ls", "metadata": {"exit": 0}}},
                {"type": "step-finish", "reason": "tool-calls", "id": "prt_5"},
             ]},
            {"info": {"role": "assistant", "id": "msg_3", "providerID": "stub", "modelID": "stub-1", "finish": "stop",
                      "time": {"created": 1_795_002_586_081_i64}},
             "parts": [{"type": "step-start", "id": "prt_6"}, {"type": "text", "text": "two files", "id": "prt_7"}]},
        ],
    })
}

/// A session in `dir`'s fixture folder that the stand-in lists, opens and exports.
fn session_in(dir: &Path, id: &str) {
    let session = json!({
        "title": "list the files", "directory": "/work/project", "updated": "2026-11-18T12:00:00Z",
        "turns": [["list the files", "two files"]], "export": export_of(id),
    });
    std::fs::write(
        fixtures(dir).join(format!("{id}.json")),
        session.to_string(),
    )
    .expect("a session");
}

struct Sink(mpsc::Sender<serde_json::Value>);

impl ChannelSink for Sink {
    fn receive(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        let _ = self.0.send(serde_json::from_slice(bytes).expect("JSON"));
        Ok(())
    }

    fn closed(&mut self, _status: u32, _reason: &str) {}
}

/// A connection to the stand-in through the provider, speaking in requests.
struct Conn {
    channel: Object<dyn Channel>,
    received: mpsc::Receiver<serde_json::Value>,
    next: i64,
}

impl Conn {
    fn open(request: StartRequest, dir: &Path) -> Self {
        let (sender, received) = mpsc::channel();
        let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(dir).into_object();
        let channel = provider()
            .start(request, spawner, Sink(sender).into_object())
            .expect("it starts");
        Self {
            channel,
            received,
            next: 0,
        }
    }

    fn call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> (Vec<serde_json::Value>, serde_json::Value) {
        self.next += 1;
        let id = self.next;
        self.channel
            .send(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                    .to_string()
                    .as_bytes(),
            )
            .expect("taken");
        let mut updates = Vec::new();
        loop {
            let message = self
                .received
                .recv_timeout(Duration::from_secs(30))
                .expect("an answer");
            if message["id"] == json!(id) && message.get("method").is_none() {
                return (updates, message);
            }
            updates.push(message);
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.channel.close(0, "");
    }
}

fn logged(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON value a line"))
        .collect()
}

fn names_of(entry: &serde_json::Value) -> Vec<String> {
    entry["env_names"]
        .as_array()
        .expect("names")
        .iter()
        .map(|name| name.as_str().expect("text").to_owned())
        .collect()
}

fn has(names: &[String], wanted: &str) -> bool {
    names.iter().any(|name| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(wanted)
        } else {
            name == wanted
        }
    })
}

/// A key account starts OpenCode on a data folder of its own with its key in the variable it names, no credential the host
/// had in the environment, and the access the host set kept.
#[test]
fn a_key_account_starts_opencode_on_its_own_data_folder_with_only_its_key() {
    let dir = scratch("starts");
    let log = dir.join("fake.log");
    let environment: Environment = [
        ("ANTHROPIC_API_KEY", "the-hosts-own-key"),
        ("OPENCODE_AUTH_CONTENT", "{\"x\":{}}"),
        ("OPENCODE_CONFIG_CONTENT", "{}"),
        ("XDG_DATA_HOME", "/somewhere/else"),
        ("OPENCODE_PERMISSION", "{\"bash\":\"ask\"}"),
        ("COQUI_ACP_FAKE_STDIO_LOG", text(&log)),
    ]
    .into_iter()
    .collect();
    let request = StartRequest::builder()
        .environment(environment)
        .account(key_grant(&dir))
        .build();
    {
        let mut conn = Conn::open(request, &dir);
        let (_, answer) = conn.call("initialize", &json!({"protocolVersion": 1}));
        assert!(answer.get("result").is_some(), "{answer}");
    }
    let entries = logged(&log);
    let entry = entries.last().expect("a start");
    let names = names_of(entry);
    for gone in [
        "ANTHROPIC_API_KEY",
        "OPENCODE_AUTH_CONTENT",
        "OPENCODE_CONFIG_CONTENT",
    ] {
        assert!(!has(&names, gone), "{gone} reached OpenCode: {names:?}");
    }
    assert!(has(&names, "OPENAI_API_KEY"), "{names:?}");
    assert!(
        has(&names, "OPENCODE_PERMISSION"),
        "the access was dropped: {names:?}"
    );
    assert_eq!(entry["directories"]["XDG_DATA_HOME"], json!(text(&dir)));
    assert_eq!(entry["args"], json!(["acp"]));
    assert!(!entry.to_string().contains(KEY));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Sessions are the data folder's: the account's own are listed and opened by the agent, another account's are not, and a
/// session that is not there is the agent's refusal, relayed.
#[test]
fn an_earlier_session_lists_and_resumes_from_the_accounts_data_folder() {
    let mine = scratch("sessions-mine");
    let other = scratch("sessions-other");
    session_in(&mine, "ses_earlierOne");
    fixtures(&other);
    for (dir, expected) in [(&mine, 1_usize), (&other, 0)] {
        let request = StartRequest::builder().account(key_grant(dir)).build();
        let mut conn = Conn::open(request, dir);
        let (_, answer) = conn.call("initialize", &json!({"protocolVersion": 1}));
        let capabilities = &answer["result"]["agentCapabilities"];
        assert_eq!(capabilities["loadSession"], json!(true), "{answer}");
        assert!(
            capabilities["sessionCapabilities"].get("list").is_some(),
            "{answer}"
        );
        let (_, listed) = conn.call("session/list", &json!({}));
        let sessions = listed["result"]["sessions"].as_array().expect("sessions");
        assert_eq!(sessions.len(), expected, "{listed}");
        let (updates, loaded) = conn.call(
            "session/load",
            &json!({"sessionId": "ses_earlierOne", "cwd": "/work/project", "mcpServers": []}),
        );
        if expected == 1 {
            assert_eq!(sessions[0]["sessionId"], json!("ses_earlierOne"));
            assert_eq!(sessions[0]["title"], json!("list the files"));
            assert!(loaded.get("error").is_none(), "{loaded}");
            let kinds: Vec<&str> = updates
                .iter()
                .filter_map(|update| update["params"]["update"]["sessionUpdate"].as_str())
                .collect();
            assert_eq!(
                kinds,
                ["user_message_chunk", "agent_message_chunk"],
                "{updates:?}"
            );
            let (_, resumed) = conn.call(
                "session/resume",
                &json!({"sessionId": "ses_earlierOne", "cwd": "/work/project", "mcpServers": []}),
            );
            assert!(resumed.get("error").is_none(), "{resumed}");
        } else {
            assert!(
                loaded.get("error").is_some(),
                "another account's session opened: {loaded}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&other);
}

fn page_of(request: SessionExport, dir: &Path) -> Result<coqui_acp_api::ExportPage, ProviderError> {
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(dir).into_object();
    provider().export_session(request, spawner)
}

fn export_all(id: &str, dir: &Path, max_bytes: u64) -> (String, usize) {
    let mut whole = String::new();
    let mut pages = 0;
    let mut after: Option<String> = None;
    loop {
        let mut request = SessionExport::new(id, "/work/project")
            .account(key_grant(dir))
            .max_bytes(max_bytes);
        if let Some(after) = &after {
            request = request.after(after.clone());
        }
        let page = page_of(request, dir).expect("a page");
        whole.push_str(&page.text);
        pages += 1;
        match page.next {
            Some(next) => after = Some(next),
            None => return (whole, pages),
        }
    }
}

/// `opencode export` of a session of the account's folder is a history the format reads: the prompt, the reasoning, the
/// command with its result, the answer and how the turn ended; paged by the byte bound the caller gives.
#[test]
fn a_session_of_the_data_folder_is_exported_as_a_history() {
    let dir = scratch("export");
    session_in(&dir, "ses_earlierOne");
    let (whole, pages) = export_all("ses_earlierOne", &dir, 1 << 20);
    assert_eq!(pages, 1);
    let document = Document::from_slice(whole.as_bytes()).expect("a history the format reads");
    document.check().expect("a history that keeps the rules");
    assert_eq!(document.header.origin.harness, "opencode");
    assert_eq!(
        document.header.origin.session_id.as_deref(),
        Some("ses_earlierOne")
    );
    assert_eq!(document.header.origin.version.as_deref(), Some("1.19.6"));
    assert_eq!(document.header.cwd.as_deref(), Some("/work/project"));
    assert_eq!(document.header.model.as_deref(), Some("stub/stub-1"));
    assert_eq!(document.header.mode.as_deref(), Some("build"));
    assert!(
        document
            .header
            .created_at
            .as_deref()
            .is_some_and(|at| at.starts_with("2026-11-18")),
        "{:?}",
        document.header.created_at
    );
    let updates: Vec<&str> = document
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Update { update } => update["sessionUpdate"].as_str(),
            _ => None,
        })
        .collect();
    assert_eq!(
        updates,
        [
            "agent_thought_chunk",
            "tool_call",
            "tool_call_update",
            "agent_message_chunk"
        ],
        "{whole}"
    );
    let announced = document
        .lines
        .iter()
        .find_map(|line| match &line.body {
            Body::Update { update } if update["sessionUpdate"] == "tool_call" => {
                Some(update.clone())
            }
            _ => None,
        })
        .expect("a call");
    assert_eq!(announced["kind"], json!("execute"));
    assert_eq!(announced["rawInput"]["command"], json!("ls"));
    assert!(matches!(
        document.lines.last().map(|line| &line.body),
        Some(Body::TurnEnd { .. })
    ));
    // Small pages carry the same history.
    let (small, many) = export_all("ses_earlierOne", &dir, 600);
    assert!(many > 1, "{many}");
    assert_eq!(small, whole);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_export_refuses_what_is_not_a_session_of_the_account() {
    let mine = scratch("export-mine");
    let other = scratch("export-other");
    session_in(&mine, "ses_earlierOne");
    fixtures(&other);
    let log = mine.join("fake.log");
    let environment: Environment = [("COQUI_ACP_FAKE_STDIO_LOG", text(&log))]
        .into_iter()
        .collect();
    // Another account's folder does not hold it.
    let gone = page_of(
        SessionExport::new("ses_earlierOne", "/work/project").account(key_grant(&other)),
        &other,
    )
    .expect_err("not there");
    assert_eq!(
        u32::from(gone.status),
        u32::from(Status::GUATIAO_ERR_NOT_FOUND)
    );
    // An id that could be taken for an option or a path never reaches the program; a UUID is the host's name for a session
    // written into the folder, so it is looked for there and, not being there, is not found.
    let unknown = page_of(
        SessionExport::new("0b9c1a2e-0000-4000-8000-000000000000", "/work/project")
            .account(key_grant(&mine)),
        &mine,
    )
    .expect_err("no such session");
    assert_eq!(
        u32::from(unknown.status),
        u32::from(Status::GUATIAO_ERR_NOT_FOUND)
    );
    // An id that could be taken for an option or a path never reaches the program.
    for bad in ["--help", "-s", "ses_../x", "ses_a b", ""] {
        let refused = page_of(
            SessionExport::new(bad, "/work/project")
                .environment(environment.clone())
                .account(key_grant(&mine)),
            &mine,
        )
        .expect_err(bad);
        assert_eq!(
            u32::from(refused.status),
            u32::from(Status::GUATIAO_ERR_BAD_VALUE),
            "{bad}"
        );
    }
    assert!(logged(&log).is_empty(), "the program ran for a refused id");
    // The account's key is no part of what an export starts the program with.
    page_of(
        SessionExport::new("ses_earlierOne", "/work/project")
            .environment(environment)
            .account(key_grant(&mine)),
        &mine,
    )
    .expect("exported");
    let entries = logged(&log);
    let entry = entries.last().expect("a run");
    assert_eq!(entry["args"], json!(["export", "--pure", "ses_earlierOne"]));
    assert_eq!(entry["directories"]["XDG_DATA_HOME"], json!(text(&mine)));
    assert!(!has(&names_of(entry), "OPENAI_API_KEY"), "{entry}");
    assert!(!entry.to_string().contains(KEY));
    let _ = std::fs::remove_dir_all(&mine);
    let _ = std::fs::remove_dir_all(&other);
}

struct Typing {
    key: String,
    asked: std::sync::Mutex<Vec<String>>,
}

impl LoginPrompter for Typing {
    fn say(&self, _text: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    fn open_url(&self, _open: OpenUrl) -> Result<(), ProviderError> {
        Ok(())
    }

    fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        self.asked.lock().expect("lock").push(form.title);
        Ok(LoginAnswer::new(
            Value::from(Map::new()),
            Credentials::from_iter([("api_key", self.key.as_str())]),
        ))
    }

    fn cancelled(&self) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

fn accounts() -> OpenCodeAccounts {
    OpenCodeAccounts::new(OpenCodeAccountSettings {
        executable: Some(fake()),
        renew_poll_ms: 50,
        command_timeout_ms: 60_000,
    })
}

fn sign_in(
    key: &str,
    method: &str,
    variable: &str,
    dir: &Path,
) -> Result<coqui_acp_api::LoginOutcome, ProviderError> {
    let mut inputs = Map::new();
    inputs.set("variable", Value::from(variable));
    accounts().login(
        LoginRequest::new(method, text(dir)).inputs(Value::from(inputs)),
        LocalSpawner::new().files(dir).into_object(),
        Typing {
            key: key.to_owned(),
            asked: std::sync::Mutex::new(Vec::new()),
        }
        .into_object(),
    )
}

/// The `api-key` sign-in asks for the key, has OpenCode list the variable as a provider's sign-in, and makes a key account
/// that names the variable; nothing it was told shows in the error of a refusal.
#[test]
fn the_api_key_sign_in_makes_a_provider_key_account() {
    let dir = scratch("signin");
    let outcome = sign_in(KEY, "api-key", "GROQ_API_KEY", &dir).expect("signed in");
    assert_eq!(outcome.kind, "opencode-provider-key");
    assert!(
        !outcome.checked,
        "a key is not checked against its provider"
    );
    assert_eq!(
        coqui_acp_harness::provider::to_json(&outcome.settings),
        json!({"variable": "GROQ_API_KEY"})
    );
    assert_eq!(outcome.credentials.expose().get("api_key"), Some(KEY));
    for (key, method, variable) in [
        ("", "api-key", "GROQ_API_KEY"),
        ("a\nb", "api-key", "GROQ_API_KEY"),
        (KEY, "api-key", "AOS_SOMETHING"),
        (KEY, "api-key", "1ST_KEY"),
        (KEY, "api-key", "TWO WORDS"),
        (KEY, "api-key", ""),
        (KEY, "device", "GROQ_API_KEY"),
    ] {
        let refused = sign_in(key, method, variable, &dir).expect_err("refused");
        assert_eq!(
            u32::from(refused.status),
            u32::from(Status::GUATIAO_ERR_BAD_VALUE)
        );
        assert!(!refused.message().contains(KEY), "{}", refused.message());
    }
    // A variable no provider's catalog entry names is the person's to choose: the account carries the key in it.
    for variable in ["ACME_GATEWAY_KEY", "LD_PRELOAD", "lower_case_key"] {
        let outcome = sign_in(KEY, "api-key", variable, &dir).expect("any variable name is taken");
        assert_eq!(
            coqui_acp_harness::provider::to_json(&outcome.settings),
            json!({"variable": variable})
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_account_provider_offers_the_sign_in_and_the_key_it_makes() {
    let offer = accounts().offers().expect("offers");
    // The sign-in form suggests the providers' variables and takes any other name.
    let key = offer
        .logins
        .iter()
        .find(|login| login.id == "api-key")
        .expect("the key method");
    let inputs = coqui_acp_harness::provider::to_json(&key.inputs);
    let variable = &inputs["properties"]["variable"];
    assert!(
        variable["examples"]
            .as_array()
            .is_some_and(|names| names.iter().any(|name| name == "OPENAI_API_KEY")),
        "{inputs}"
    );
    assert!(variable.get("enum").is_none(), "{inputs}");
    let kinds: Vec<&str> = offer.kinds.iter().map(|kind| kind.id.as_str()).collect();
    assert_eq!(kinds, ["opencode-login", "opencode-provider-key"]);
    let methods: Vec<&str> = offer.logins.iter().map(|login| login.id.as_str()).collect();
    assert_eq!(methods, ["api-key"]);
    assert_eq!(offer.logins[0].produces, "opencode-provider-key");
    let catalog = provider().accounts().expect("catalog");
    for kind in [
        "harness-login",
        "opencode-login",
        "opencode-api-key",
        "opencode-provider-key",
    ] {
        assert!(catalog.kind(kind).is_some(), "{kind}");
    }
    // The host's access for the session is no credential: an account's start must not take it away.
    assert!(
        catalog
            .credential_variables
            .iter()
            .any(|name| name == "OPENCODE_AUTH_CONTENT")
    );
    assert!(
        !catalog
            .credential_variables
            .iter()
            .any(|name| name == "OPENCODE_PERMISSION")
    );
}

/// The one provider and the generic one are the same harness.
#[test]
fn opencode_is_the_stdio_provider_with_the_harness_fixed() {
    let own = provider().describe().expect("described");
    let mut settings = StdioSettings::new(StdioHarness::OpenCode);
    settings.executable = Some(fake());
    let generic = StdioProvider::new(settings).describe().expect("described");
    assert_eq!(own.title, generic.title);
    assert_eq!(own.title, "OpenCode");
    assert_eq!(
        provider().program().expect("program").as_deref(),
        Some(fake().as_str())
    );
}

/// A history a Claude session made: a prompt, reasoning, a command with its result, an answer, and a second prompt.
fn claude_history() -> String {
    use coqui_acp_history::conversation::{Call, Recorder, Stamp};
    use coqui_acp_history::{Header, Origin, Stop, StopReason};
    let at = |second: u32| Stamp::at(Some(format!("2026-11-18T12:00:{second:02}.000Z")));
    let mut rec = Recorder::new();
    rec.prompt(
        vec![json!({"type": "text", "text": "list the files"})],
        at(1),
    );
    rec.thought("looking", at(2));
    let mut call = Call::new("toolu_1");
    call.name = Some("Bash".to_owned());
    call.title = Some("ls".to_owned());
    call.status = Some("pending".to_owned());
    call.raw_input = Some(json!({"command": "ls"}));
    rec.update(call.announce(), at(3));
    call.status = Some("completed".to_owned());
    call.raw_output = Some(json!("a.txt b.txt"));
    rec.update(call.finish(), at(4));
    rec.message("two files", at(5));
    rec.end_turn(Stop::Acp(StopReason::EndTurn), None, at(6));
    rec.prompt(vec![json!({"type": "text", "text": "thanks"})], at(7));
    rec.message("welcome", at(8));
    rec.end_turn(Stop::Acp(StopReason::EndTurn), None, at(9));
    let mut header = Header::new(
        "0b9c1a2e-0000-4000-8000-000000000001",
        Origin::new("claude"),
    );
    header.created_at = Some("2026-11-18T12:00:00.000Z".to_owned());
    header.model = Some("claude-sonnet-4-5".to_owned());
    let mut document = Document::new(header);
    document.lines = rec.finish();
    document.seal().expect("a history");
    document.to_string().expect("text")
}

const HOST_ID: &str = "7d8c1f4e-2b9a-4c63-8f10-5a6b7c8d9e0f";
const STORED_ID: &str = "ses_7d8c1f4e2b9a4c638f105a6b7c8d9e0f";

/// A history is written into the account's data folder through `opencode import`, in the session's working directory, with
/// the file removed after and no key passed; the host then opens, lists, prompts and exports the session by its UUID while
/// the program speaks of it by the id it stored it under.
#[test]
fn a_history_is_written_into_the_accounts_folder_and_the_session_is_the_hosts_uuid() {
    let dir = scratch("import");
    let work = scratch("import-work");
    let log = dir.join("fake.log");
    let environment: Environment = [("COQUI_ACP_FAKE_STDIO_LOG", text(&log))]
        .into_iter()
        .collect();
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
    let report = provider()
        .import_session(
            SessionImport::new(HOST_ID, text(&work), claude_history())
                .environment(environment.clone())
                .account(key_grant(&dir)),
            spawner,
        )
        .expect("imported");
    assert!(
        report
            .losses
            .iter()
            .any(|loss| loss.kind == "thinking" && loss.count == 1),
        "{:?}",
        report.losses
    );
    assert!(
        !report.replayed(),
        "it is a session of OpenCode's, not a replay"
    );
    // The program was run on the file in the session's folder, as the account, and the file is gone.
    let entries = logged(&log);
    let entry = entries.last().expect("a run");
    assert_eq!(entry["args"][0], json!("import"));
    let file = entry["args"][1].as_str().expect("a path");
    assert!(
        file.starts_with(text(&dir)) && file.ends_with(&format!("{STORED_ID}.json")),
        "{file}"
    );
    assert!(!Path::new(file).exists(), "the history stays in a file");
    assert_eq!(
        std::fs::canonicalize(entry["cwd"].as_str().expect("a directory")).ok(),
        std::fs::canonicalize(&work).ok()
    );
    assert_eq!(entry["directories"]["XDG_DATA_HOME"], json!(text(&dir)));
    assert!(!has(&names_of(entry), "OPENAI_API_KEY"), "{entry}");
    assert!(!entry.to_string().contains(KEY));
    // The session is in the account's folder under the id made from the UUID.
    let stored = fixtures(&dir).join(format!("{STORED_ID}.json"));
    let session: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&stored).expect("stored")).expect("JSON");
    assert_eq!(
        session["turns"],
        json!([["list the files", "two files"], ["thanks", "welcome"]])
    );
    // The host opens it by the UUID; every id on the way out is the UUID.
    let request = StartRequest::builder().account(key_grant(&dir)).build();
    let mut conn = Conn::open(request, &dir);
    conn.call("initialize", &json!({"protocolVersion": 1}));
    let (_, listed) = conn.call("session/list", &json!({}));
    assert_eq!(
        listed["result"]["sessions"][0]["sessionId"],
        json!(HOST_ID),
        "{listed}"
    );
    let cwd = text(&work);
    let (updates, loaded) = conn.call(
        "session/load",
        &json!({"sessionId": HOST_ID, "cwd": cwd, "mcpServers": []}),
    );
    assert!(loaded.get("error").is_none(), "{loaded}");
    let kinds: Vec<&str> = updates
        .iter()
        .filter_map(|update| update["params"]["update"]["sessionUpdate"].as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "user_message_chunk",
            "agent_message_chunk",
            "user_message_chunk",
            "agent_message_chunk"
        ]
    );
    assert!(
        updates
            .iter()
            .all(|update| update["params"]["sessionId"] == json!(HOST_ID))
    );
    let (updates, answered) = conn.call(
        "session/prompt",
        &json!({"sessionId": HOST_ID, "prompt": [{"type": "text", "text": "hello"}]}),
    );
    assert_eq!(
        answered["result"]["stopReason"],
        json!("end_turn"),
        "{answered}"
    );
    assert_eq!(updates[0]["params"]["sessionId"], json!(HOST_ID));
    assert_eq!(
        updates[0]["params"]["update"]["content"]["text"],
        json!("echo: hello")
    );
    drop(conn);
    // Read back out by the UUID, it is the history that went in.
    let page = page_of(
        SessionExport::new(HOST_ID, cwd)
            .account(key_grant(&dir))
            .max_bytes(1 << 20),
        &dir,
    )
    .expect("exported");
    let document = Document::from_slice(page.text.as_bytes()).expect("a history");
    assert_eq!(document.header.id, HOST_ID);
    let prompts: Vec<String> = document
        .lines
        .iter()
        .filter_map(|line| match &line.body {
            Body::Prompt { prompt } => prompt[0]["text"].as_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["list the files", "thanks"]);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&work);
}

/// With nowhere to write the session it is not written, and the reason is the account's; only when the provider is set
/// to replay does it fall back, and then the history is context in a prompt.
#[test]
fn a_session_with_no_account_folder_is_refused_or_replayed_as_the_provider_is_set() {
    let work = scratch("import-no-account");
    let spawner =
        || -> Object<dyn ProcessSpawner> { LocalSpawner::new().files(&work).into_object() };
    let refused = provider()
        .import_session(
            SessionImport::new(HOST_ID, text(&work), claude_history()),
            spawner(),
        )
        .expect_err("no account to write into");
    assert_eq!(
        u32::from(refused.status),
        u32::from(Status::GUATIAO_ERR_UNSUPPORTED)
    );
    assert!(
        refused.message().contains("no account"),
        "{}",
        refused.message()
    );
    // Not a UUID: refused whatever else is so.
    let bad = provider()
        .import_session(
            SessionImport::new("ses_abc", text(&work), claude_history()).account(key_grant(&work)),
            spawner(),
        )
        .expect_err("not a UUID");
    assert_eq!(
        u32::from(bad.status),
        u32::from(Status::GUATIAO_ERR_BAD_VALUE)
    );
    let mut settings = OpenCodeSettings::new();
    settings.executable = Some(fake());
    settings.replay_max_bytes = 1 << 20;
    let replaying = OpenCodeProvider::new(settings);
    let report = replaying
        .import_session(
            SessionImport::new(HOST_ID, text(&work), claude_history()),
            spawner(),
        )
        .expect("replayed");
    assert!(report.replayed(), "{:?}", report.losses);
    let _ = std::fs::remove_dir_all(&work);
}

/// Against the real program, which a person's machine may have: `COQUI_ACP_OPENCODE_REAL` names it,
/// `COQUI_ACP_OPENCODE_DATA` a data folder (what `XDG_DATA_HOME` is set to) that holds a session whose id is
/// `COQUI_ACP_OPENCODE_SESSION`. Run it with a home, configuration folder and `OPENCODE_DISABLE_EXTERNAL_SKILLS=1`
/// of the test's own in the environment, never the person's. It is skipped (it passes) when they are not set.
#[test]
fn the_real_program_lists_opens_and_exports_a_session_of_a_data_folder() {
    let (Some(program), Some(data), Some(id)) = (
        std::env::var_os("COQUI_ACP_OPENCODE_REAL"),
        std::env::var_os("COQUI_ACP_OPENCODE_DATA"),
        std::env::var("COQUI_ACP_OPENCODE_SESSION").ok(),
    ) else {
        eprintln!("skipped: COQUI_ACP_OPENCODE_REAL, _DATA and _SESSION are not set");
        return;
    };
    let dir = PathBuf::from(data);
    let mut settings = OpenCodeSettings::new();
    settings.executable = Some(program.to_string_lossy().into_owned());
    let real = OpenCodeProvider::new(settings);
    let (sender, received) = mpsc::channel();
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
    let channel = real
        .start(
            StartRequest::builder().account(key_grant(&dir)).build(),
            spawner,
            Sink(sender).into_object(),
        )
        .expect("it starts");
    let mut conn = Conn {
        channel,
        received,
        next: 0,
    };
    let (_, init) = conn.call("initialize", &json!({"protocolVersion": 1}));
    assert_eq!(
        init["result"]["agentInfo"]["name"],
        json!("OpenCode"),
        "{init}"
    );
    assert_eq!(
        init["result"]["agentCapabilities"]["loadSession"],
        json!(true)
    );
    let (_, listed) = conn.call("session/list", &json!({}));
    let sessions = listed["result"]["sessions"].as_array().expect("sessions");
    assert!(
        sessions
            .iter()
            .any(|session| session["sessionId"] == json!(id)),
        "{listed}"
    );
    let cwd = sessions
        .iter()
        .find(|session| session["sessionId"] == json!(id))
        .and_then(|session| session["cwd"].as_str())
        .expect("a directory")
        .to_owned();
    let (updates, loaded) = conn.call(
        "session/load",
        &json!({"sessionId": id, "cwd": cwd, "mcpServers": []}),
    );
    assert!(loaded.get("error").is_none(), "{loaded}");
    assert!(!updates.is_empty(), "nothing was replayed");
    drop(conn);
    let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
    let page = real
        .export_session(
            SessionExport::new(&id, &cwd).account(key_grant(&dir)),
            spawner,
        )
        .expect("exported");
    let document = Document::from_slice(page.text.as_bytes()).expect("a history");
    document.check().expect("it keeps the rules");
    assert_eq!(document.header.origin.harness, "opencode");
    assert_eq!(
        document.header.origin.session_id.as_deref(),
        Some(id.as_str())
    );
    assert!(
        document
            .lines
            .iter()
            .any(|line| matches!(&line.body, Body::Prompt { .. }))
    );
}

/// Against the real program: a history a Claude session made is written into the data folder, opened by the host's UUID,
/// replayed by `session/load`, and a follow-up prompt reaches the model with that history in its request. A second history
/// that holds a compaction summary reaches the model as the summary, without what it replaced.
///
/// `COQUI_ACP_OPENCODE_REAL` names the program, `COQUI_ACP_OPENCODE_IMPORT_DATA` an empty data folder, and
/// `COQUI_ACP_OPENCODE_STUB_LOG` a file the stand-in model that the program's configuration names appends each request
/// body to, a line each. Run it with a home, configuration folder and `OPENCODE_DISABLE_EXTERNAL_SKILLS=1` of the test's
/// own in the environment, never the person's. It is skipped (it passes) when they are not set.
#[test]
fn the_real_program_continues_a_history_written_into_its_store() {
    let (Some(program), Some(data), Some(stub)) = (
        std::env::var_os("COQUI_ACP_OPENCODE_REAL"),
        std::env::var_os("COQUI_ACP_OPENCODE_IMPORT_DATA"),
        std::env::var_os("COQUI_ACP_OPENCODE_STUB_LOG"),
    ) else {
        eprintln!("skipped: COQUI_ACP_OPENCODE_REAL, _IMPORT_DATA and _STUB_LOG are not set");
        return;
    };
    let dir = PathBuf::from(data);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("a working directory");
    let stub = PathBuf::from(stub);
    let mut settings = OpenCodeSettings::new();
    settings.executable = Some(program.to_string_lossy().into_owned());
    let real = OpenCodeProvider::new(settings);
    let requests = || -> Vec<String> {
        std::fs::read_to_string(&stub)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    for (id, summarised) in [
        (HOST_ID, false),
        ("2c4e6a80-1b3d-4f57-9a9c-0d1e2f3a4b5c", true),
    ] {
        let history = if summarised {
            claude_history_with_a_summary()
        } else {
            claude_history()
        };
        let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
        let report = real
            .import_session(
                SessionImport::new(id, text(&work), history).account(key_grant(&dir)),
                spawner,
            )
            .expect("the program takes the history");
        assert!(!report.replayed(), "{:?}", report.losses);
        let (sender, received) = mpsc::channel();
        let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
        let channel = real
            .start(
                StartRequest::builder().account(key_grant(&dir)).build(),
                spawner,
                Sink(sender).into_object(),
            )
            .expect("it starts");
        let mut conn = Conn {
            channel,
            received,
            next: 0,
        };
        conn.call("initialize", &json!({"protocolVersion": 1}));
        let (_, listed) = conn.call("session/list", &json!({}));
        let sessions = listed["result"]["sessions"].as_array().expect("sessions");
        let mine = sessions
            .iter()
            .find(|session| session["sessionId"] == json!(id))
            .unwrap_or_else(|| panic!("the session is listed by its UUID: {listed}"));
        assert_eq!(
            std::fs::canonicalize(mine["cwd"].as_str().expect("a directory")).ok(),
            std::fs::canonicalize(&work).ok(),
            "{mine}"
        );
        let (updates, loaded) = conn.call(
            "session/load",
            &json!({"sessionId": id, "cwd": text(&work), "mcpServers": []}),
        );
        assert!(loaded.get("error").is_none(), "{loaded}");
        assert!(
            updates
                .iter()
                .all(|update| update["params"]["sessionId"] == json!(id)),
            "{updates:?}"
        );
        let said: Vec<String> = updates
            .iter()
            .filter_map(|update| {
                update["params"]["update"]["content"]["text"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        let before = requests().len();
        let follow_up = if summarised {
            "and after that?"
        } else {
            "and now what?"
        };
        let (_, answered) = conn.call(
            "session/prompt",
            &json!({"sessionId": id, "prompt": [{"type": "text", "text": follow_up}]}),
        );
        assert_eq!(
            answered["result"]["stopReason"],
            json!("end_turn"),
            "{answered}"
        );
        drop(conn);
        let sent = requests();
        let request = sent[before..]
            .iter()
            .find(|body| body.contains(follow_up))
            .unwrap_or_else(|| panic!("no request carried the follow-up: {sent:?}"));
        if summarised {
            // The summary is what the model reads of what it replaced.
            assert!(
                said.iter().all(|text| !text.contains("old answer")),
                "{said:?}"
            );
            assert!(request.contains("we listed the files"), "{request}");
            assert!(request.contains("next answer"), "{request}");
            assert!(!request.contains("old answer"), "{request}");
        } else {
            assert!(said.iter().any(|text| text == "list the files"), "{said:?}");
            assert!(said.iter().any(|text| text == "two files"), "{said:?}");
            for wanted in [
                "list the files",
                "two files",
                "a.txt b.txt",
                "thanks",
                "welcome",
            ] {
                assert!(request.contains(wanted), "{wanted} is not in {request}");
            }
            assert!(request.contains("\"bash\""), "{request}");
            assert!(!request.contains("looking"), "{request}");
        }
        // The follow-up is in the session, which the host reads back by its UUID.
        let spawner: Object<dyn ProcessSpawner> = LocalSpawner::new().files(&dir).into_object();
        let page = real
            .export_session(
                SessionExport::new(id, text(&work)).account(key_grant(&dir)),
                spawner,
            )
            .expect("exported");
        let document = Document::from_slice(page.text.as_bytes()).expect("a history");
        document.check().expect("it keeps the rules");
        assert_eq!(document.header.id, id);
        assert!(
            document.lines.iter().any(|line| matches!(
                &line.body,
                Body::Prompt { prompt } if prompt[0]["text"] == json!(follow_up)
            )),
            "{}",
            page.text
        );
    }
}

/// A history whose second turn follows a compaction: what the summary replaced is not in the file.
fn claude_history_with_a_summary() -> String {
    use coqui_acp_history::conversation::Recorder;
    use coqui_acp_history::conversation::Stamp;
    use coqui_acp_history::{Header, Origin, Stop, StopReason};
    let at = |second: u32| Stamp::at(Some(format!("2026-11-18T12:00:{second:02}.000Z")));
    let mut rec = Recorder::new();
    rec.prompt(vec![json!({"type": "text", "text": "old question"})], at(1));
    rec.message("old answer", at(2));
    rec.end_turn(Stop::Acp(StopReason::EndTurn), None, at(3));
    rec.compaction(
        Some("we listed the files".to_owned()),
        None,
        Some(900),
        at(4),
    );
    rec.prompt(
        vec![json!({"type": "text", "text": "next question"})],
        at(5),
    );
    rec.message("next answer", at(6));
    rec.end_turn(Stop::Acp(StopReason::EndTurn), None, at(7));
    let mut header = Header::new(
        "0b9c1a2e-0000-4000-8000-000000000002",
        Origin::new("claude"),
    );
    header.created_at = Some("2026-11-18T12:00:00.000Z".to_owned());
    header.model = Some("claude-sonnet-4-5".to_owned());
    let mut document = Document::new(header);
    document.lines = rec.finish();
    document.seal().expect("a history");
    document.to_string().expect("text")
}
