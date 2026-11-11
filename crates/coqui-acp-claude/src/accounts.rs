//! The accounts Claude Code takes: the catalog the provider lists, and what applying a grant does to Claude Code's
//! environment and configuration directory.
//!
//! Measured on Claude Code 2.1.311. A `rotates` nobody measured is `true`, a `history_portable` nobody measured
//! `false`.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use coqui_acp_api::guatiao::library::ProviderError;
use coqui_acp_api::guatiao::{Status, Wipe};
use coqui_acp_api::{
    ASK, AUTH, AccountCatalog, AccountGrant, AccountKindInfo, AccountRef, Credentials,
    EndpointSlot, HARNESS_LOGIN, LoginMethodInfo, MODEL_API, OPEN_URL, PASTE_CODE, PROXY,
};
use coqui_acp_harness::account::{Watched, rfc3339_ms};
use coqui_acp_harness::provider::{credential, schema, to_json};
use serde_json::{Value, json};

/// The kind of a login made by `claude auth login --claudeai`, kept in `.credentials.json`.
pub const LOGIN: &str = "claude-login";
/// The kind of a login made by `claude auth login --console`.
pub const CONSOLE: &str = "claude-console";
/// The kind of a long-lived token in `CLAUDE_CODE_OAUTH_TOKEN`.
pub const OAUTH_TOKEN: &str = "claude-oauth-token";
/// The kind of a key in `ANTHROPIC_API_KEY`.
pub const API_KEY: &str = "anthropic-api-key";
/// The kind of a token in `ANTHROPIC_AUTH_TOKEN`, sent as a bearer token to a gateway.
pub const AUTH_TOKEN: &str = "anthropic-auth-token";

/// The login method that makes [`LOGIN`] by `claude auth login --claudeai`.
pub const SUBSCRIPTION: &str = "subscription";
/// The login method that makes [`CONSOLE`] by `claude auth login --console`.
pub const CONSOLE_METHOD: &str = "console";
/// The login method that takes a key and makes [`API_KEY`].
pub const API_KEY_METHOD: &str = "api-key";
/// The login method that takes a token and makes [`AUTH_TOKEN`].
pub const AUTH_TOKEN_METHOD: &str = "auth-token";
/// The login method `claude setup-token` would make, which this provider cannot run.
pub const LONG_LIVED_TOKEN: &str = "long-lived-token";

/// The file in Claude Code's configuration directory that holds a login.
pub const CREDENTIALS_FILE: &str = ".credentials.json";
/// The credential key that holds that file's text.
pub const FILE_KEY: &str = "credentials_file";

/// The model endpoint Claude Code uses without `ANTHROPIC_BASE_URL`.
const API: &str = "https://api.anthropic.com";

/// Every variable Claude Code reads for a credential, for where its requests go, or for which provider it signs in to,
/// so a start on an account removes them all and sets only the kind's.
///
/// Measured (`references/account_kinds.md`): the credential variables, `CLAUDE_CODE_USE_BEDROCK` and the six
/// `*_BASE_URL` endpoint variables of its documentation. Named but not run on 2.1.311: the other provider switches and
/// the cloud credentials (`AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`, the Vertex and Foundry settings).
pub(crate) const VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_AWS_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_PROFILE",
    "CLAUDE_CODE_CLIENT_KEY",
    "CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_MANTLE",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AWS_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "CLOUD_ML_REGION",
    "ANTHROPIC_FOUNDRY_RESOURCE",
];

fn settings(secret: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            secret: credential(),
            "base_url": {"type": "string", "format": "uri"},
        },
        "required": [secret],
        "additionalProperties": false,
    })
}

fn login_settings() -> Value {
    json!({
        "type": "object",
        "properties": {
            FILE_KEY: credential(),
            "base_url": {"type": "string", "format": "uri"},
        },
        "required": [FILE_KEY],
        "additionalProperties": false,
    })
}

fn kind(id: &str, title: &str, description: &str, settings: &Value) -> AccountKindInfo {
    let mut kind = AccountKindInfo::new(id, title);
    kind.description = description.to_string();
    kind.settings = schema(settings);
    kind
}

fn login(id: &str, title: &str, description: &str, auth: &str) -> AccountKindInfo {
    let mut kind = kind(id, title, description, &login_settings());
    kind.endpoints = vec![
        EndpointSlot::new(MODEL_API)
            .key("base_url")
            .default_url(API),
        EndpointSlot::new(AUTH).default_url(auth),
    ];
    kind.rotates = true;
    kind.state_dir = true;
    kind.renews = true;
    kind.notices =
        vec!["the login is kept in a file that what the agent runs can read".to_string()];
    kind
}

fn environment(
    id: &str,
    title: &str,
    description: &str,
    secret: &str,
    purpose: &str,
) -> AccountKindInfo {
    let mut kind = kind(id, title, description, &settings(secret));
    let mut slot = EndpointSlot::new(purpose).key("base_url");
    if purpose == MODEL_API {
        slot = slot.default_url(API);
    }
    kind.endpoints = vec![slot];
    kind.history_portable = true;
    kind.notices = vec![
        "the credential is in the environment of every program the agent runs".to_string(),
        "the person's own user settings (apiKeyHelper, an env block) are not loaded, as a stored login in their configuration directory could outrank the credential; the entry's setting_sources says otherwise".to_string(),
    ];
    kind
}

fn method(id: &str, title: &str, description: &str, produces: &str) -> LoginMethodInfo {
    let mut method = LoginMethodInfo::new(id, title, produces);
    method.description = description.to_string();
    method
}

fn web_method(id: &str, title: &str, description: &str, produces: &str) -> LoginMethodInfo {
    let mut method = method(id, title, description, produces);
    method.inputs = schema(&json!({
        "type": "object",
        "properties": {"email": {"type": "string", "description": "An address to fill in on the sign-in page."}},
        "additionalProperties": false,
    }));
    method.steps = vec![OPEN_URL.to_string(), PASTE_CODE.to_string()];
    // The page shows the code to copy back, so the person's browser need not be on the machine where Claude Code runs.
    method.remote = true;
    method
}

fn typed_method(id: &str, title: &str, description: &str, produces: &str) -> LoginMethodInfo {
    let mut method = method(id, title, description, produces);
    method.steps = vec![ASK.to_string()];
    method.remote = true;
    method
}

/// What the provider takes as an account.
#[must_use]
pub fn catalog() -> AccountCatalog {
    let mut catalog = AccountCatalog::new();
    catalog.kinds.extend([
        login(
            LOGIN,
            "Claude subscription login",
            "A login made by `claude auth login --claudeai`, kept in `.credentials.json` of Claude Code's configuration directory.",
            "https://claude.com/cai/oauth/authorize",
        ),
        login(
            CONSOLE,
            "Claude Console login",
            "A login made by `claude auth login --console`, billed to an API organization.",
            "https://platform.claude.com/oauth/authorize",
        ),
        environment(
            OAUTH_TOKEN,
            "Claude long-lived token",
            "A token made by `claude setup-token`, read from `CLAUDE_CODE_OAUTH_TOKEN`.",
            "token",
            MODEL_API,
        ),
        environment(
            API_KEY,
            "Anthropic API key",
            "A key read from `ANTHROPIC_API_KEY`; it takes the place of a subscription login.",
            "api_key",
            MODEL_API,
        ),
        environment(
            AUTH_TOKEN,
            "Anthropic gateway token",
            "A token read from `ANTHROPIC_AUTH_TOKEN` and sent as a bearer token, to the gateway `base_url` names.",
            "auth_token",
            PROXY,
        ),
    ]);
    catalog.logins = vec![
        web_method(
            SUBSCRIPTION,
            "Sign in with a Claude subscription",
            "Runs `claude auth login --claudeai`: a page to open, and the code it shows to paste back.",
            LOGIN,
        ),
        web_method(
            CONSOLE_METHOD,
            "Sign in to the Anthropic Console",
            "Runs `claude auth login --console`: a page to open, and the code it shows to paste back.",
            CONSOLE,
        ),
        typed_method(
            API_KEY_METHOD,
            "Use an Anthropic API key",
            "Takes a key and checks with `claude auth status` that Claude Code accepts it; no model is called.",
            API_KEY,
        ),
        typed_method(
            AUTH_TOKEN_METHOD,
            "Use a gateway token",
            "Takes a bearer token and checks with `claude auth status` that Claude Code accepts it; no model is called.",
            AUTH_TOKEN,
        ),
    ];
    catalog.credential_variables = VARIABLES
        .iter()
        .map(|v| (*v).to_string())
        .collect::<Vec<String>>();
    catalog
}

fn bad(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_BAD_VALUE, why)
}

/// The text setting `key` of `settings`: absent, or a plain `http(s)` address.
fn base_url(settings: &coqui_acp_api::guatiao::Value) -> Result<Option<String>, ProviderError> {
    let settings = guatiao_json(settings);
    let Some(object) = settings.as_object() else {
        return Err(bad("the account's settings are not an object"));
    };
    if object.keys().any(|key| key != "base_url") {
        return Err(bad("the account's settings name something the kind lacks"));
    }
    match object.get("base_url") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(url))
            if (url.starts_with("https://") || url.starts_with("http://"))
                && url.len() > 8
                && !url.chars().any(|c| c.is_control() || c.is_whitespace()) =>
        {
            Ok(Some(url.clone()))
        }
        Some(_) => Err(bad("base_url is not an address")),
    }
}

fn guatiao_json(value: &coqui_acp_api::guatiao::Value) -> Value {
    to_json(value)
}

/// What applying a grant does.
#[non_exhaustive]
pub(crate) struct Applied {
    /// The variables Claude Code is given.
    pub set: Vec<(String, String)>,
    /// The variables removed before, every credential variable; empty for `harness-login`, which clears nothing.
    pub remove: Vec<String>,
    /// The configuration directory, for a kind that keeps its login in one.
    pub dir: Option<String>,
    /// The files that hold the login.
    pub watched: Vec<Watched>,
}

/// The file's text holds a login: not blank, and when it is Claude Code's JSON, a token in it.
pub(crate) fn usable(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => object
            .get("claudeAiOauth")
            .and_then(Value::as_object)
            .is_some_and(|oauth| {
                ["accessToken", "refreshToken"].iter().any(|key| {
                    oauth
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.is_empty())
                })
            }),
        Ok(_) => false,
        // Not the JSON Claude Code writes: whatever it is, it is not blank.
        Err(_) => true,
    }
}

/// When the login in the file's text stops working, as an RFC 3339 time.
pub(crate) fn expires(text: &str) -> Option<String> {
    let millis = expires_ms(text)?;
    Some(rfc3339_ms(millis))
}

/// When it stops working, in milliseconds since the Unix epoch.
pub(crate) fn expires_ms(text: &str) -> Option<i64> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("claudeAiOauth")?
        .get("expiresAt")?
        .as_i64()
}

/// Why a reference to an account is not one the provider takes: `BAD_VALUE` for an id, kind or settings it refuses.
pub(crate) fn check(account: &AccountRef) -> Result<(), ProviderError> {
    account.check_id()?;
    let catalog = catalog();
    let Some(info) = catalog.kind(&account.kind) else {
        return Err(bad("this harness takes no account of that kind"));
    };
    if info.id == HARNESS_LOGIN {
        if !guatiao_json(&account.settings)
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        {
            return Err(bad("that kind takes no settings"));
        }
        return Ok(());
    }
    base_url(&account.settings)?;
    if info.state_dir && account.state_dir.as_deref().is_none_or(str::is_empty) {
        return Err(bad("that kind needs a directory to run in"));
    }
    Ok(())
}

/// What applying `grant` does.
///
/// # Errors
///
/// `BAD_VALUE`: an id that is not one, a kind not listed, settings the kind refuses, a credential missing, empty or
/// one the kind does not take, a missing directory.
pub(crate) fn plan(grant: &AccountGrant) -> Result<Applied, ProviderError> {
    check(&grant.reference())?;
    let credentials: &Credentials = grant.credentials.expose();
    if grant.kind == HARNESS_LOGIN {
        if !credentials.is_empty() {
            return Err(bad("that kind takes no credential"));
        }
        return Ok(Applied {
            set: Vec::new(),
            remove: Vec::new(),
            dir: None,
            watched: Vec::new(),
        });
    }
    let url = base_url(&grant.settings)?;
    let mut set = Vec::new();
    let mut applied = Applied {
        set: Vec::new(),
        remove: VARIABLES.iter().map(|v| (*v).to_string()).collect(),
        dir: None,
        watched: Vec::new(),
    };
    if matches!(grant.kind.as_str(), LOGIN | CONSOLE) {
        let only = credentials.keys().all(|key| key == FILE_KEY);
        let text = credentials.get(FILE_KEY).filter(|text| !text.is_empty());
        if !only || text.is_none() {
            return Err(bad("the login's credentials are missing or not the kind's"));
        }
        let dir = grant.state_dir.clone().unwrap_or_default();
        set.push(("CLAUDE_CONFIG_DIR".to_string(), dir.clone()));
        applied.dir = Some(dir);
        applied.watched = vec![
            Watched::new(CREDENTIALS_FILE, FILE_KEY)
                .usable(usable)
                .expires(expires),
        ];
    } else {
        let key = match grant.kind.as_str() {
            OAUTH_TOKEN => "token",
            API_KEY => "api_key",
            _ => "auth_token",
        };
        let variable = secret_key_variable(&grant.kind);
        let only = credentials.keys().all(|k| k == key);
        let value = credentials.get(key).filter(|value| !value.is_empty());
        let Some(value) = value.filter(|_| only) else {
            return Err(bad("the credential is missing or not the kind's"));
        };
        set.push((variable.to_string(), value.to_string()));
    }
    if let Some(url) = url {
        set.push(("ANTHROPIC_BASE_URL".to_string(), url));
    }
    applied.set = set;
    Ok(applied)
}

fn secret_key_variable(kind: &str) -> &'static str {
    match kind {
        OAUTH_TOKEN => "CLAUDE_CODE_OAUTH_TOKEN",
        API_KEY => "ANTHROPIC_API_KEY",
        _ => "ANTHROPIC_AUTH_TOKEN",
    }
}

impl core::fmt::Debug for Applied {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The variables set may hold a credential: only their names are shown.
        f.debug_struct("Applied")
            .field(
                "set",
                &self.set.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("remove", &self.remove.len())
            .field("dir", &self.dir)
            .field("watched", &self.watched)
            .finish()
    }
}

impl Drop for Applied {
    fn drop(&mut self) {
        for (_, value) in &mut self.set {
            value.wipe();
        }
    }
}
