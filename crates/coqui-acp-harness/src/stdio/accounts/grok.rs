//! Grok (`grok agent stdio`), measured on 1.0.77.
//!
//! `GROK_HOME` places `auth.json`, the sessions and the logs. A sign-in file that the harness cannot refresh is deleted
//! by it with exit code 0, which the watcher reports as a loss. What is not listed is not measured: the keys inside
//! `auth.json`, the defaults of the endpoints, and the `GROK_AUTH_PROVIDER_COMMAND` kind, which runs a program named in
//! settings and awaits the owner's answer.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    ASK, AUTH, Credentials, LoginForm, LoginMethodInfo, LoginOutcome, LoginPrompter, LoginRequest,
    MODEL_API, OPEN_URL, OpenUrl, PROXY, ProcessSpawner, SpawnSpec, USER_CODE,
};
use serde_json::json;

use super::{Credential, Endpoint, HarnessDef, KindDef, Setting};
use crate::provider::schema;
use crate::stdio::login::{self, Ctx};

/// The kind of a login made by `grok login`, kept in `auth.json`.
pub const LOGIN: &str = "grok-login";
/// The kind of a key in `XAI_API_KEY`.
pub const API_KEY: &str = "xai-api-key";
/// The kind of a login through the person's own identity provider.
pub const OIDC: &str = "grok-oidc";

const AUTH_FILE: &str = "auth.json";

const URL_SETTINGS: &[Setting] = &[
    Setting {
        name: "proxy_base_url",
        required: false,
        url: true,
        choices: &[],
    },
    Setting {
        name: "xai_api_base_url",
        required: false,
        url: true,
        choices: &[],
    },
];

const OIDC_SETTINGS: &[Setting] = &[
    Setting {
        name: "issuer",
        required: true,
        url: true,
        choices: &[],
    },
    Setting {
        name: "client_id",
        required: true,
        url: false,
        choices: &[],
    },
    Setting {
        name: "proxy_base_url",
        required: false,
        url: true,
        choices: &[],
    },
];

const PROVIDER_VARIABLES: &[(&str, &str)] = &[
    ("proxy_base_url", "GROK_CLI_CHAT_PROXY_BASE_URL"),
    ("xai_api_base_url", "GROK_XAI_API_BASE_URL"),
];

const API_ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        purpose: PROXY,
        key: Some("proxy_base_url"),
        default: None,
    },
    Endpoint {
        purpose: MODEL_API,
        key: Some("xai_api_base_url"),
        default: None,
    },
];

const OIDC_ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        purpose: AUTH,
        key: Some("issuer"),
        default: None,
    },
    Endpoint {
        purpose: PROXY,
        key: Some("proxy_base_url"),
        default: None,
    },
];

/// Every variable Grok reads for a credential, an identity provider or a program that makes a token, so a start on an
/// account removes them all.
const VARIABLES: &[&str] = &[
    "XAI_API_KEY",
    "GROK_CODE_XAI_API_KEY",
    "GROK_AUTH",
    "GROK_OIDC_ISSUER",
    "GROK_OIDC_CLIENT_ID",
    "GROK_AUTH_PROVIDER_COMMAND",
    "GROK_AUTH_PROVIDER_COMMAND_LABEL",
    "GROK_AUTH_TOKEN_TTL",
    "GROK_CLI_CHAT_PROXY_BASE_URL",
    "GROK_XAI_API_BASE_URL",
    "GROK_HOME",
];

/// What Grok takes as an account.
pub static DEF: HarnessDef = HarnessDef {
    name: "Grok",
    program: "grok",
    args: &["agent", "stdio"],
    kinds: &[
        KindDef {
            id: LOGIN,
            title: "Grok login",
            description: "A login made by `grok login`, kept in `auth.json` of Grok's home directory.",
            credentials: &[Credential {
                key: "auth_json",
                required: true,
            }],
            settings: URL_SETTINGS,
            state_dir: true,
            rotates: true,
            history_portable: false,
            renews: true,
            endpoints: API_ENDPOINTS,
            notices: &["the login is kept in a file that what the agent runs can read"],
            credential_variables: &[],
            setting_variables: PROVIDER_VARIABLES,
            named_variable: None,
            home_variable: Some("GROK_HOME"),
            file: Some((AUTH_FILE, "auth_json")),
            renew_unsupported: "",
        },
        KindDef {
            id: API_KEY,
            title: "xAI API key",
            description: "A key read from `XAI_API_KEY`. A login in `auth.json` of Grok's home wins over it, so give the account a directory of its own.",
            credentials: &[Credential {
                key: "api_key",
                required: true,
            }],
            settings: URL_SETTINGS,
            state_dir: false,
            rotates: false,
            history_portable: true,
            renews: false,
            endpoints: API_ENDPOINTS,
            notices: &[
                "the credential is in the environment of every program the agent runs",
                "Grok prefers a login in its home directory to the key",
            ],
            credential_variables: &[("api_key", "XAI_API_KEY")],
            setting_variables: PROVIDER_VARIABLES,
            named_variable: None,
            home_variable: Some("GROK_HOME"),
            file: None,
            renew_unsupported: "a key is not renewed",
        },
        KindDef {
            id: OIDC,
            title: "Grok through an identity provider",
            description: "A login through the person's own OpenID Connect provider; Grok signs in on its first start and keeps the tokens in `auth.json`.",
            credentials: &[Credential {
                key: "auth_json",
                required: false,
            }],
            settings: OIDC_SETTINGS,
            state_dir: true,
            rotates: true,
            history_portable: false,
            renews: true,
            endpoints: OIDC_ENDPOINTS,
            notices: &["the login is kept in a file that what the agent runs can read"],
            credential_variables: &[],
            setting_variables: &[
                ("issuer", "GROK_OIDC_ISSUER"),
                ("client_id", "GROK_OIDC_CLIENT_ID"),
                ("proxy_base_url", "GROK_CLI_CHAT_PROXY_BASE_URL"),
            ],
            named_variable: None,
            home_variable: Some("GROK_HOME"),
            file: Some((AUTH_FILE, "auth_json")),
            renew_unsupported: "",
        },
    ],
    variables: VARIABLES,
    // `grok models` renews a login within `GROK_AUTH_EARLY_INVALIDATION_SECS` (300) of its end.
    renew_args: Some(&["models"]),
    login_unsupported: "",
};

/// The ways to sign in.
///
/// The identity-provider kind has no method: its sign-in needs a provider to answer and was not run.
#[must_use]
pub fn logins() -> Vec<LoginMethodInfo> {
    let mut device = LoginMethodInfo::new("device", "Sign in with a code", LOGIN);
    device.description =
        "`grok login`: open a page on any machine and confirm the code it shows.".to_string();
    device.steps = vec![OPEN_URL.to_string(), USER_CODE.to_string()];
    device.remote = true;
    let mut browser = LoginMethodInfo::new("browser", "Sign in with a browser", LOGIN);
    browser.description =
        "`grok login --oauth`: the browser returns to a port on the machine where Grok runs."
            .to_string();
    browser.steps = vec![OPEN_URL.to_string()];
    let mut key = LoginMethodInfo::new("api-key", "xAI API key", API_KEY);
    key.description = "Asks for the key and checks it with `grok models`.".to_string();
    key.steps = vec![ASK.to_string()];
    key.remote = true;
    vec![device, browser, key]
}

fn spec(program: &str, args: &[&str], dir: &str) -> SpawnSpec {
    let mut environment = coqui_acp_api::Environment::new();
    environment.insert("GROK_HOME", dir);
    let mut spec = SpawnSpec::new(program).environment(environment);
    for arg in args {
        spec = spec.arg(*arg);
    }
    spec.remove_environment = VARIABLES.iter().map(|v| (*v).to_string()).collect();
    spec
}

#[derive(Default)]
struct Seen {
    url: Option<alloc::string::String>,
    code: Option<alloc::string::String>,
    after_confirm: bool,
    waiting: bool,
    opened: bool,
}

impl Seen {
    fn line(&mut self, ctx: &Ctx, line: &str, device: bool) -> Result<(), ProviderError> {
        let text = line.trim();
        if self.url.is_none()
            && (text.starts_with("https://") || text.starts_with("http://"))
            && let Some(url) = text.split_whitespace().next()
        {
            self.url = Some(url.to_string());
        }
        if text.starts_with("Confirm this code") {
            self.after_confirm = true;
        } else if self.after_confirm && !text.is_empty() && self.code.is_none() {
            self.code = Some(text.to_string());
            self.after_confirm = false;
        }
        if text.starts_with("Waiting for authorization") {
            self.waiting = true;
        }
        let ready = self.url.is_some() && (!device || self.code.is_some() || self.waiting);
        if ready && !self.opened {
            self.opened = true;
            let mut open = OpenUrl::new(self.url.clone().unwrap_or_default());
            open.user_code.clone_from(&self.code);
            ctx.open_url(open)?;
        }
        Ok(())
    }
}

fn internal(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_INTERNAL, why)
}

fn code_text(exit: &coqui_acp_api::ProcessExit) -> alloc::string::String {
    exit.code.map_or_else(
        || "was ended".to_string(),
        |code| alloc::format!("ended with code {code}"),
    )
}

/// `grok models` on the sign-in's directory, which asks nothing of a model and renews a login near its end.
fn check(ctx: &Ctx, program: &str, dir: &str) -> Result<bool, ProviderError> {
    let mut child = ctx.spawn(spec(program, &["models"], dir))?;
    let exit = child.run(ctx, |_| Ok(()))?;
    Ok(exit.code == Some(0))
}

fn browser_or_device(
    ctx: &Ctx,
    program: &str,
    dir: &str,
    device: bool,
) -> Result<LoginOutcome, ProviderError> {
    let args: &[&str] = if device {
        &["login"]
    } else {
        &["login", "--oauth"]
    };
    let mut child = ctx.spawn(spec(program, args, dir))?;
    let mut seen = Seen::default();
    let exit = child.run(ctx, |line| seen.line(ctx, line, device))?;
    if exit.code != Some(0) {
        return Err(internal(&alloc::format!(
            "`grok login` {}",
            code_text(&exit)
        )));
    }
    let text = ctx
        .read_file(AUTH_FILE)
        .map_err(|_| internal("`grok login` wrote no login file"))?;
    if text.trim().is_empty() {
        return Err(internal("`grok login` wrote an empty login file"));
    }
    let checked = check(ctx, program, dir)?;
    // `grok models` may have renewed the login: the newer copy is the one to keep.
    let text = ctx
        .read_file(AUTH_FILE)
        .ok()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(text);
    let mut outcome = LoginOutcome::new(LOGIN, login::one("auth_json", &text));
    outcome.checked = checked;
    Ok(outcome)
}

fn api_key(ctx: &Ctx, program: &str, dir: &str) -> Result<LoginOutcome, ProviderError> {
    let form = LoginForm::new(
        "xAI API key",
        schema(&json!({
            "type": "object",
            "properties": {"api_key": crate::provider::credential()},
            "required": ["api_key"],
        })),
    );
    let answer = ctx.ask(form)?;
    let key = answer
        .secrets
        .expose()
        .get("api_key")
        .unwrap_or_default()
        .to_string();
    if key.trim().is_empty() || key.chars().any(char::is_control) {
        return Err(ProviderError::new(
            Status::GUATIAO_ERR_BAD_VALUE,
            "login refused: no usable key was given",
        ));
    }
    let mut spec = spec(program, &["models"], dir);
    let mut environment = coqui_acp_api::Environment::new();
    environment.insert("GROK_HOME", dir);
    environment.insert("XAI_API_KEY", key.as_str());
    spec = spec.environment(environment);
    let mut child = ctx.spawn(spec)?;
    let exit = child.run(ctx, |_| Ok(()))?;
    if exit.code != Some(0) {
        return Err(ProviderError::new(
            Status::GUATIAO_ERR_BAD_VALUE,
            &alloc::format!(
                "login refused: Grok did not accept the key ({})",
                code_text(&exit)
            ),
        ));
    }
    let mut credentials = Credentials::new();
    credentials.insert("api_key", key);
    let mut outcome = LoginOutcome::new(API_KEY, credentials);
    outcome.checked = true;
    Ok(outcome)
}

/// Runs the sign-in `request.method` names.
///
/// # Errors
///
/// As [`AcpHarness::login`](coqui_acp_api::AcpHarness::login).
pub fn sign_in(
    program: alloc::string::String,
    request: LoginRequest,
    spawner: Object<dyn ProcessSpawner>,
    prompter: Object<dyn LoginPrompter>,
) -> Result<LoginOutcome, ProviderError> {
    let method = request.method.clone();
    if !matches!(method.as_str(), "device" | "browser" | "api-key") {
        return Err(ProviderError::new(
            Status::GUATIAO_ERR_BAD_VALUE,
            "this harness offers no such way to sign in",
        ));
    }
    let dir = request.state_dir.clone();
    let timeout = request.timeout_ms;
    login::run(spawner, prompter, timeout, move |ctx| {
        match method.as_str() {
            "device" => browser_or_device(ctx, &program, &dir, true),
            "browser" => browser_or_device(ctx, &program, &dir, false),
            _ => api_key(ctx, &program, &dir),
        }
    })
}
