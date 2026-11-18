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
    StartRequest,
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
    // An id that could be taken for an option, a path or another id's shape never reaches the program.
    for bad in [
        "--help",
        "-s",
        "ses_../x",
        "ses_a b",
        "",
        "0b9c1a2e-0000-4000-8000-000000000000",
    ] {
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
        (KEY, "api-key", "LD_PRELOAD"),
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_account_provider_offers_the_sign_in_and_the_key_it_makes() {
    let offer = accounts().offers().expect("offers");
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
