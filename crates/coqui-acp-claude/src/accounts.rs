//! The accounts Claude Code takes, as the provider's catalog lists them.
//!
//! Measured on Claude Code 2.1.311. A `rotates` nobody measured is `true`, a `history_portable` nobody measured
//! `false`. `renews` is `false` for every kind: the provider has no `renew` yet.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use coqui_acp_api::{AUTH, AccountCatalog, AccountKindInfo, EndpointSlot, MODEL_API, PROXY};
use coqui_acp_harness::provider::{credential, schema};
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

/// The model endpoint Claude Code uses without `ANTHROPIC_BASE_URL`.
const API: &str = "https://api.anthropic.com";

/// Every variable Claude Code reads for a credential or for which provider to sign in to, so a start on an account
/// removes them all.
const VARIABLES: [&str; 13] = [
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

fn kind(id: &str, title: &str, description: &str, secret: &str) -> AccountKindInfo {
    let mut kind = AccountKindInfo::new(id, title);
    kind.description = description.to_string();
    kind.settings = schema(&settings(secret));
    kind
}

fn login(id: &str, title: &str, description: &str, auth: &str) -> AccountKindInfo {
    let mut kind = kind(id, title, description, "credentials_file");
    kind.endpoints = vec![
        EndpointSlot::new(MODEL_API)
            .key("base_url")
            .default_url(API),
        EndpointSlot::new(AUTH).default_url(auth),
    ];
    kind.rotates = true;
    kind.state_dir = true;
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
    let mut kind = kind(id, title, description, secret);
    let mut slot = EndpointSlot::new(purpose).key("base_url");
    if purpose == MODEL_API {
        slot = slot.default_url(API);
    }
    kind.endpoints = vec![slot];
    kind.history_portable = true;
    kind.notices =
        vec!["the credential is in the environment of every program the agent runs".to_string()];
    kind
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
    catalog.credential_variables = VARIABLES
        .iter()
        .map(|v| (*v).to_string())
        .collect::<Vec<String>>();
    catalog
}
