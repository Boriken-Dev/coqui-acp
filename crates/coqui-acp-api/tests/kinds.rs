//! The contract other programs are built against: the kinds' hashes and description, the models'
//! keys and schemas, and the requirement names a harness matches on.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;

use coqui_acp_api::guatiao::{FromValue, Map, Schema, Value};
use coqui_acp_api::{
    AccountCatalog, AccountGrant, AccountKeeperVtable, AccountKindInfo, AccountLoss, AccountRef,
    AccountRenewal, AcpHarnessVtable, Bucket, CacheLifetime, ChannelSinkVtable, ChannelVtable,
    Credentials, EndpointSlot, ErrorOutput, ExportPage, FileInfo, HarnessInfo, HistoryLoss,
    ImportReport, LimitHit, LinkState, LoginAnswer, LoginForm, LoginMethodInfo, LoginOutcome,
    LoginPrompterVtable, LoginRequest, ModelInfo, ModelListing, ModelSpend, ModelsRequest, NAMES,
    NonDefault, OpenUrl, ProcessExit, ProcessSpawnerVtable, RenewReport, RenewRequest, Requirement,
    SeedPoint, SessionExport, SessionImport, SessionQuery, SessionRecordFile, SpawnSpec,
    SpawnedProcessVtable, SpawnerInfo, Spend, StartRequest, Takes, TerminalSize, UsageQuery,
    UsageReport, UsageSupport,
};
use guatiao_serde::Serializable;
use serde_json::Value as Json;

fn file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn pretty(value: &Json) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("written");
    text.push('\n');
    text
}

/// Compares `fresh` with the committed file, writing it first when asked to.
fn committed(path: &str, fresh: &str) {
    let path = file(path);
    if std::env::var_os("COQUI_ACP_API_WRITE_KINDS").is_some() {
        std::fs::write(&path, fresh).expect("the file is written");
    }
    let committed = std::fs::read_to_string(&path).expect("the file is committed");
    assert!(committed == fresh, "regenerate {}", path.display());
}

/// The harness kind and every object kind its slots name, as one document, so a provider in another
/// language has every kind its `start` names.
fn rendered() -> String {
    let document = format!(
        "{{\"kinds\":[{},{},{},{},{},{},{}]}}",
        AcpHarnessVtable::description(),
        ChannelVtable::description(),
        ChannelSinkVtable::description(),
        ProcessSpawnerVtable::description(),
        SpawnedProcessVtable::description(),
        AccountKeeperVtable::description(),
        LoginPrompterVtable::description()
    );
    pretty(&serde_json::from_str(&document).expect("a kind's description is JSON"))
}

/// Regenerate with `COQUI_ACP_API_WRITE_KINDS=1 cargo test -p coqui-acp-api --test kinds`.
#[test]
fn the_committed_description_is_what_this_build_describes() {
    committed("include/coqui_acp_api.kinds.json", &rendered());
}

fn schema_of<T: Schema>() -> Json {
    let value = T::schema().expect("a model describes itself");
    serde_json::to_value(Serializable::from(&value)).expect("a schema is JSON")
}

/// Every model as `(name, schema)`, in the order the header lists them.
fn schemas() -> Vec<(&'static str, Json)> {
    vec![
        ("StartRequest", schema_of::<StartRequest>()),
        ("Requirement", schema_of::<Requirement>()),
        ("SessionQuery", schema_of::<SessionQuery>()),
        ("SessionImport", schema_of::<SessionImport>()),
        ("SessionExport", schema_of::<SessionExport>()),
        ("ExportPage", schema_of::<ExportPage>()),
        ("SpawnSpec", schema_of::<SpawnSpec>()),
        ("TerminalSize", schema_of::<TerminalSize>()),
        ("HarnessInfo", schema_of::<HarnessInfo>()),
        ("Takes", schema_of::<Takes>()),
        ("ModelListing", schema_of::<ModelListing>()),
        ("ModelInfo", schema_of::<ModelInfo>()),
        ("SessionRecordFile", schema_of::<SessionRecordFile>()),
        ("NonDefault", schema_of::<NonDefault>()),
        ("ImportReport", schema_of::<ImportReport>()),
        ("SeedPoint", schema_of::<SeedPoint>()),
        ("HistoryLoss", schema_of::<HistoryLoss>()),
        ("SpawnerInfo", schema_of::<SpawnerInfo>()),
        ("ProcessExit", schema_of::<ProcessExit>()),
        ("LinkState", schema_of::<LinkState>()),
        ("ErrorOutput", schema_of::<ErrorOutput>()),
        ("UsageQuery", schema_of::<UsageQuery>()),
        ("UsageReport", schema_of::<UsageReport>()),
        ("Bucket", schema_of::<Bucket>()),
        ("LimitHit", schema_of::<LimitHit>()),
        ("Spend", schema_of::<Spend>()),
        ("ModelSpend", schema_of::<ModelSpend>()),
        ("UsageSupport", schema_of::<UsageSupport>()),
        ("CacheLifetime", schema_of::<CacheLifetime>()),
        ("FileInfo", schema_of::<FileInfo>()),
        ("Credentials", schema_of::<Credentials>()),
        ("AccountGrant", schema_of::<AccountGrant>()),
        ("AccountRef", schema_of::<AccountRef>()),
        ("AccountCatalog", schema_of::<AccountCatalog>()),
        ("AccountKindInfo", schema_of::<AccountKindInfo>()),
        ("EndpointSlot", schema_of::<EndpointSlot>()),
        ("LoginMethodInfo", schema_of::<LoginMethodInfo>()),
        ("LoginRequest", schema_of::<LoginRequest>()),
        ("LoginOutcome", schema_of::<LoginOutcome>()),
        ("RenewRequest", schema_of::<RenewRequest>()),
        ("RenewReport", schema_of::<RenewReport>()),
        ("ModelsRequest", schema_of::<ModelsRequest>()),
        ("AccountRenewal", schema_of::<AccountRenewal>()),
        ("AccountLoss", schema_of::<AccountLoss>()),
        ("OpenUrl", schema_of::<OpenUrl>()),
        ("LoginForm", schema_of::<LoginForm>()),
        ("LoginAnswer", schema_of::<LoginAnswer>()),
    ]
}

/// Regenerated as the kinds are.
#[test]
fn the_committed_schemas_are_what_this_build_describes() {
    let document: serde_json::Map<String, Json> = schemas()
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect();
    committed(
        "include/coqui_acp_api.schemas.json",
        &pretty(&Json::Object(document)),
    );
}

/// Adding, removing or renaming a key of a model breaks providers already built: a request model
/// refuses a key it does not declare, and an answer's reader drops one. This names every key, so
/// changing one is a decision.
#[test]
fn every_models_keys_are_pinned() {
    let pinned: [(&str, &[&str], &[&str]); 44] = [
        (
            "StartRequest",
            &[
                "account",
                "cache_lifetime",
                "environment",
                "requirements",
                "working_roots",
            ],
            &[],
        ),
        ("Requirement", &["name", "value"], &["name"]),
        (
            "SessionQuery",
            &["environment", "session_id", "working_directory"],
            &["session_id", "working_directory"],
        ),
        (
            "SessionImport",
            &[
                "account",
                "environment",
                "history",
                "session_id",
                "working_directory",
            ],
            &["session_id", "working_directory"],
        ),
        (
            "SessionExport",
            &[
                "account",
                "after",
                "environment",
                "max_bytes",
                "session_id",
                "working_directory",
            ],
            &["session_id", "working_directory"],
        ),
        ("ExportPage", &["next", "text"], &["text"]),
        ("ImportReport", &["compacted", "losses", "seeded_from"], &[]),
        ("SeedPoint", &["digest", "line"], &["line"]),
        (
            "HistoryLoss",
            &["count", "detail", "kind"],
            &["count", "kind"],
        ),
        (
            "SpawnSpec",
            &[
                "args",
                "clear_environment",
                "cwd",
                "environment",
                "error_output",
                "program",
                "remove_environment",
                "terminal",
            ],
            &["program"],
        ),
        ("TerminalSize", &["cols", "rows"], &["cols", "rows"]),
        (
            "HarnessInfo",
            &[
                "cache_lifetime",
                "resumes_wider",
                "takes",
                "title",
                "usage",
                "versions",
            ],
            &["title", "versions"],
        ),
        (
            "Takes",
            &[
                "additional_directories",
                "audio",
                "embedded_context",
                "images",
                "load",
                "mcp_http",
                "mcp_sse",
                "resume",
                "system_prompt",
            ],
            &[],
        ),
        (
            "ModelListing",
            &["known", "models", "unknown_because"],
            &["known"],
        ),
        ("ModelInfo", &["description", "id", "name"], &["id"]),
        (
            "SessionRecordFile",
            &["format", "path"],
            &["format", "path"],
        ),
        (
            "NonDefault",
            &["default", "setting", "value"],
            &["default", "setting", "value"],
        ),
        (
            "SpawnerInfo",
            &[
                "arch",
                "ends_with_host",
                "files",
                "kills_escaped",
                "local",
                "os",
                "separate_error",
                "survives_link_loss",
                "terminal",
            ],
            &["arch", "local", "os"],
        ),
        ("ProcessExit", &["code", "running", "signal"], &["running"]),
        ("LinkState", &["drops", "up"], &["drops", "up"]),
        ("UsageQuery", &["account", "environment"], &[]),
        (
            "UsageReport",
            &[
                "account",
                "buckets",
                "limit",
                "observed_at",
                "revision",
                "sources",
                "spend",
                "supported",
                "unsupported_because",
            ],
            &["observed_at", "revision", "supported"],
        ),
        (
            "Bucket",
            &[
                "evidence",
                "id",
                "label",
                "limit",
                "models",
                "observed_at",
                "refill",
                "remaining",
                "source",
                "status",
                "unit",
                "used",
            ],
            &[
                "id",
                "models",
                "observed_at",
                "refill",
                "source",
                "status",
                "unit",
            ],
        ),
        (
            "LimitHit",
            &[
                "bucket",
                "observed_at",
                "resets_at",
                "retry_after",
                "source",
                "text",
            ],
            &["observed_at", "source"],
        ),
        (
            "Spend",
            &[
                "by_model",
                "cache_read",
                "cache_write",
                "cost_micro_usd",
                "input",
                "output",
                "reasoning",
                "session_id",
            ],
            &["cache_read", "cache_write", "input", "output", "session_id"],
        ),
        (
            "ModelSpend",
            &[
                "cache_read",
                "cache_write",
                "cost_micro_usd",
                "input",
                "model",
                "output",
            ],
            &["cache_read", "cache_write", "input", "model", "output"],
        ),
        (
            "UsageSupport",
            &["needs_session", "needs_turn", "sources", "spend"],
            &[],
        ),
        ("FileInfo", &["len", "modified_ms"], &["len", "modified_ms"]),
        (
            "AccountGrant",
            &[
                "credentials",
                "id",
                "kind",
                "revision",
                "settings",
                "state_dir",
            ],
            &["id", "kind"],
        ),
        (
            "AccountRef",
            &["id", "kind", "revision", "settings", "state_dir"],
            &["id", "kind"],
        ),
        (
            "AccountCatalog",
            &["credential_variables", "kinds", "logins"],
            &[],
        ),
        (
            "AccountKindInfo",
            &[
                "description",
                "endpoints",
                "history_portable",
                "id",
                "notices",
                "renews",
                "rotates",
                "settings",
                "state_dir",
                "title",
            ],
            &["id", "title"],
        ),
        ("EndpointSlot", &["default", "key", "purpose"], &["purpose"]),
        (
            "LoginMethodInfo",
            &[
                "description",
                "id",
                "inputs",
                "produces",
                "remote",
                "steps",
                "title",
            ],
            &["id", "produces", "title"],
        ),
        (
            "LoginRequest",
            &["inputs", "method", "state_dir", "timeout_ms"],
            &["method", "state_dir"],
        ),
        (
            "LoginOutcome",
            &[
                "checked",
                "credentials",
                "expires_at",
                "kind",
                "label",
                "settings",
            ],
            &["kind"],
        ),
        ("RenewRequest", &["account"], &["account"]),
        (
            "RenewReport",
            &["credentials", "expires_at", "not_renewed_because"],
            &[],
        ),
        ("ModelsRequest", &["account", "environment"], &[]),
        (
            "AccountRenewal",
            &["credentials", "expires_at", "from_revision", "id"],
            &["credentials", "from_revision", "id"],
        ),
        ("AccountLoss", &["id", "reason"], &["id", "reason"]),
        ("OpenUrl", &["expires_ms", "url", "user_code"], &["url"]),
        ("LoginForm", &["schema", "title"], &["title"]),
        ("LoginAnswer", &["secrets", "values"], &[]),
    ];
    let schemas = schemas();
    assert_eq!(pinned.len(), 44);
    for (name, keys, required) in pinned {
        let schema = &schemas
            .iter()
            .find(|(n, _)| *n == name)
            .expect("every pinned model has a schema")
            .1;
        let mut found: Vec<&str> = schema["properties"]
            .as_object()
            .expect("a model is an object")
            .keys()
            .map(String::as_str)
            .collect();
        found.sort_unstable();
        assert_eq!(found, keys, "{name}'s keys");
        let mut needed: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        needed.sort_unstable();
        assert_eq!(needed, required, "{name}'s required keys");
        assert_eq!(schema["additionalProperties"], false, "{name} is sealed");
    }
    assert_eq!(
        schemas
            .iter()
            .find(|(n, _)| *n == "ErrorOutput")
            .expect("described")
            .1["enum"],
        serde_json::json!(["separate", "merged", "discarded"])
    );
    assert_eq!(
        schemas
            .iter()
            .find(|(n, _)| *n == "CacheLifetime")
            .expect("described")
            .1["enum"],
        serde_json::json!(["default", "short", "long"])
    );
}

/// What a host sends a provider refuses a key it does not declare, so an older provider never
/// starts without a narrowing a newer host added.
#[test]
fn a_request_with_a_key_nobody_declared_is_refused() {
    let mut start = Map::new();
    start.set("narrower_than_you_know", true);
    assert!(StartRequest::from_value(&Value::from(start)).is_err());
    let mut requirement = Map::new();
    requirement.set("name", "read_only");
    requirement.set("scope", "everything");
    assert!(Requirement::from_value(&Value::from(requirement)).is_err());
    let mut query = Map::new();
    query.set("session_id", "0f8fad5b-d9cb-469f-a165-70867728950e");
    query.set("working_directory", "/");
    query.set("history", true);
    assert!(SessionQuery::from_value(&Value::from(query)).is_err());
    let mut import = Map::new();
    import.set("session_id", "0f8fad5b-d9cb-469f-a165-70867728950e");
    import.set("working_directory", "/");
    import.set("history_format", "another");
    assert!(SessionImport::from_value(&Value::from(import)).is_err());
    let mut export = Map::new();
    export.set("session_id", "0f8fad5b-d9cb-469f-a165-70867728950e");
    export.set("working_directory", "/");
    export.set("page", 2);
    assert!(SessionExport::from_value(&Value::from(export)).is_err());
    let mut spec = Map::new();
    spec.set("program", "sh");
    spec.set("sandbox", "strict");
    assert!(SpawnSpec::from_value(&Value::from(spec)).is_err());
    let mut usage = Map::new();
    usage.set("account", "pool/alice");
    usage.set("refresh", true);
    assert!(UsageQuery::from_value(&Value::from(usage)).is_err());
    let grant = |extra: Option<(&str, bool)>| {
        let mut grant = Map::new();
        grant.set("id", "account-1");
        grant.set("kind", "harness-login");
        if let Some((key, value)) = extra {
            grant.set(key, value);
        }
        Value::from(grant)
    };
    assert!(AccountGrant::from_value(&grant(None)).is_ok());
    assert!(AccountGrant::from_value(&grant(Some(("plaintext", true)))).is_err());
    assert!(AccountRef::from_value(&grant(Some(("credentials", true)))).is_err());
    let mut login = Map::new();
    login.set("method", "subscription");
    login.set("state_dir", "/tmp/login");
    assert!(LoginRequest::from_value(&Value::from(login.clone())).is_ok());
    login.set("browser", "firefox");
    assert!(LoginRequest::from_value(&Value::from(login)).is_err());
    let mut renew = Map::new();
    renew.set("account", grant(None));
    renew.set("force", true);
    assert!(RenewRequest::from_value(&Value::from(renew)).is_err());
    let mut models = Map::new();
    models.set("filter", "all");
    assert!(ModelsRequest::from_value(&Value::from(models)).is_err());
}

/// A credential is a secret wherever it crosses: its schema says so, for `redact` and forms, and
/// its `Debug` names the keys and never a value.
#[test]
fn every_credential_is_marked_sensitive() {
    for (name, schema) in schemas() {
        for key in ["credentials", "secrets"] {
            if let Some(credentials) = schema["properties"].get(key) {
                assert_eq!(credentials["x-sensitive"], true, "{name}.{key}");
                assert_eq!(credentials["writeOnly"], true, "{name}.{key}");
                assert_eq!(
                    credentials["additionalProperties"]["type"], "string",
                    "{name}.{key}"
                );
            }
        }
    }
    let grant = AccountGrant::new("account-1", "kind")
        .credentials(Credentials::from_iter([("api_key", "do-not-print-me")]));
    let shown = format!("{grant:?} {:?}", grant.credentials.expose());
    assert!(!shown.contains("do-not-print-me"), "{shown}");
    assert!(shown.contains("api_key"), "{shown}");
}

/// A grant and a catalog read back as they were written, credentials included.
#[test]
fn a_grant_crosses_whole() {
    use coqui_acp_api::guatiao::ToValue;
    let grant = AccountGrant::new("account-1", "echo-login")
        .credentials(Credentials::from_iter([("auth_file", "{}")]))
        .state_dir("/tmp/account-1")
        .revision(7);
    let value = grant.to_value().expect("written");
    assert_eq!(AccountGrant::from_value(&value).expect("read"), grant);
    let bare = Map::new();
    assert!(
        AccountGrant::from_value(&Value::from(bare)).is_err(),
        "a grant needs an id and a kind"
    );
}

/// The environment is a secret wherever it crosses: its schema says so, for `redact` and forms.
#[test]
fn every_environment_is_marked_sensitive() {
    for (name, schema) in schemas() {
        if let Some(environment) = schema["properties"].get("environment") {
            assert_eq!(environment["x-sensitive"], true, "{name}");
            assert_eq!(environment["writeOnly"], true, "{name}");
            assert_eq!(
                environment["additionalProperties"]["type"], "string",
                "{name}"
            );
        }
    }
}

/// Changing a slot, a model's name or a kind's name breaks every built provider. This names the
/// numbers, so doing that is a decision.
#[test]
fn the_kinds_hashes_are_pinned() {
    assert_eq!(AcpHarnessVtable::FLOOR_HASH, HARNESS_HASH);
    assert_eq!(ChannelVtable::FLOOR_HASH, CHANNEL_HASH);
    assert_eq!(ChannelSinkVtable::FLOOR_HASH, SINK_HASH);
    assert_eq!(ProcessSpawnerVtable::FLOOR_HASH, SPAWNER_HASH);
    assert_eq!(SpawnedProcessVtable::FLOOR_HASH, PROCESS_HASH);
    assert_eq!(AccountKeeperVtable::FLOOR_HASH, KEEPER_HASH);
    assert_eq!(LoginPrompterVtable::FLOOR_HASH, PROMPTER_HASH);
}

const HARNESS_HASH: u32 = 4111118208;
const CHANNEL_HASH: u32 = 3787630980;
const SINK_HASH: u32 = 4115266557;
const SPAWNER_HASH: u32 = 3394663830;
const PROCESS_HASH: u32 = 3306532587;
const KEEPER_HASH: u32 = 708146915;
const PROMPTER_HASH: u32 = 2606822037;

const HASHES: [(&str, u32); 7] = [
    ("acp_harness", HARNESS_HASH),
    ("channel", CHANNEL_HASH),
    ("channel_sink", SINK_HASH),
    ("process_spawner", SPAWNER_HASH),
    ("spawned_process", PROCESS_HASH),
    ("account_keeper", KEEPER_HASH),
    ("login_prompter", PROMPTER_HASH),
];

#[test]
fn the_api_header_says_those_hashes() {
    let header = std::fs::read_to_string(file("AGENTS.md")).expect("the header ships");
    for (kind, hash) in HASHES {
        assert!(
            header.contains(&hash.to_string()),
            "AGENTS.md lacks {kind}'s {hash}"
        );
    }
}

#[test]
fn the_python_kinds_carry_those_hashes() {
    let module = std::fs::read_to_string(file("python/coqui_acp_api_kinds.py"))
        .expect("the Python kinds are committed");
    for (kind, hash) in HASHES {
        assert!(
            module.contains(&format!("kind=\"{kind}\", floor_hash={hash}")),
            "python/coqui_acp_api_kinds.py lacks {kind} at {hash}: regenerate it"
        );
    }
}

/// A harness matches these as text, in every language: renaming one breaks every harness.
#[test]
fn the_requirement_names_are_pinned() {
    assert_eq!(
        NAMES,
        [
            "read_only",
            "no_commands",
            "answer_only",
            "no_project_settings",
            "no_project_mcp",
            "files_under",
            "read_only_paths",
            "denied_paths",
            "network",
            "sandboxed_scripts",
            "only_skills",
        ]
    );
    let header = std::fs::read_to_string(file("AGENTS.md")).expect("the header ships");
    for name in NAMES {
        assert!(
            header.contains(&format!("`{name}`")),
            "AGENTS.md lacks {name}"
        );
    }
}
