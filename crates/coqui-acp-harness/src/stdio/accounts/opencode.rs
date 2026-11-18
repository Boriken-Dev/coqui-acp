//! OpenCode (`opencode acp`), measured on 1.19.6 and 2.3.2.
//!
//! OpenCode keeps what it signs in with and every session in its data folder, `$XDG_DATA_HOME/opencode` (`auth.json`,
//! `opencode.db`), and honours `XDG_DATA_HOME` on every system measured (Windows included, where it ignores
//! `%APPDATA%`). A provider's key is read from that provider's own environment variable, and an inline
//! `OPENCODE_AUTH_CONTENT` replaces the whole of `auth.json` for a run. An account starts OpenCode with a data folder of
//! its own, so no credential in the person's `auth.json` meets the account's, and its sessions are the account's.
//!
//! Not measured: a real sign-in (`opencode auth login` is a picker that needs a terminal), the fields of an OAuth entry in
//! `auth.json`, whether it rewrites the file when it refreshes one, the folder on macOS, and every provider variable beyond
//! the ones listed. A provider the catalog adds is not in the list a start clears.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use std::ffi::OsString;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    ASK, Credentials, LoginForm, LoginMethodInfo, LoginOutcome, LoginPrompter, LoginRequest,
    ProcessSpawner,
};
use serde_json::json;

use coqui_acp_api::standard;

use super::{AcceptDef, Credential, HarnessDef, KindDef, Setting, SlotDef};
use crate::login::reason;
use crate::process::Launch;
use crate::provider::schema;
use crate::stdio::login::{self, Ctx};

/// The kind of a sign-in kept in `auth.json`.
pub const LOGIN: &str = "opencode-login";
/// The kind a key of another provider's account becomes: an API standard OpenCode has a variable for, read from it.
pub const API_KEY: &str = "opencode-api-key";
/// The kind a sign-in method makes: a key for one provider, read from the variable the person chose.
pub const PROVIDER_KEY: &str = "opencode-provider-key";

/// The file the sign-in is kept in, in the folder `XDG_DATA_HOME` names.
const AUTH_FILE: &str = "opencode/auth.json";

/// The variables a key account may name: each one seen to make OpenCode list a provider as signed in by the environment.
pub const KEY_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENCODE_API_KEY",
    "TOGETHER_API_KEY",
    "CEREBRAS_API_KEY",
    "FIREWORKS_API_KEY",
    "HF_TOKEN",
];

/// Every variable OpenCode reads for a credential or an identity, or that moves where it keeps them, so a start on an
/// account removes them all. Not here, on purpose: `OPENCODE_PERMISSION`, which is the host's access for the session and
/// holds no credential.
const VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENCODE_API_KEY",
    "TOGETHER_API_KEY",
    "CEREBRAS_API_KEY",
    "FIREWORKS_API_KEY",
    "HF_TOKEN",
    // Other providers the catalog names variables for.
    "302AI_API_KEY",
    "DASHSCOPE_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_PROFILE",
    "AWS_REGION",
    // What carries a whole credential store or a whole configuration, which may hold keys.
    "OPENCODE_AUTH_CONTENT",
    "OPENCODE_CONFIG_CONTENT",
    // Where the data folder is.
    "XDG_DATA_HOME",
];

/// A key for the API `$standard`, read by OpenCode from `$variable`: a key account with that variable fixed.
macro_rules! key_of {
    ($standard:expr, $variable:literal) => {
        AcceptDef {
            standard: $standard,
            slots: &[SlotDef {
                credential: "api_key",
                kind: API_KEY,
                variable: Some($variable),
                file: None,
                base_url_setting: None,
                base_url_variable: None,
                dir_variable: Some("XDG_DATA_HOME"),
                settings: &[("variable", $variable)],
            }],
        }
    };
}

/// What OpenCode takes as an account.
pub static DEF: HarnessDef = HarnessDef {
    name: "OpenCode",
    program: "opencode",
    args: &["acp"],
    kinds: &[
        KindDef {
            id: LOGIN,
            provides: &[standard::OPENCODE],
            title: "OpenCode sign-in",
            description: "The `auth.json` OpenCode's own sign-in wrote, kept in a data folder of the account's own with the sessions made on it.",
            credentials: &[Credential {
                key: "auth_json",
                required: true,
            }],
            settings: &[],
            state_dir: true,
            rotates: true,
            history_portable: false,
            renews: false,
            endpoints: &[],
            notices: &[
                "the login is kept in a file that what the agent runs can read",
                "the account has an OpenCode data folder of its own: its sessions are the account's, and the person's own `opencode.json` still applies",
            ],
            credential_variables: &[],
            setting_variables: &[],
            named_variable: None,
            home_variable: Some("XDG_DATA_HOME"),
            file: Some((AUTH_FILE, "auth_json")),
            renew_unsupported: "OpenCode refreshes a sign-in itself when it needs one; it has no command that does so without a model turn",
        },
        KindDef {
            id: API_KEY,
            // Which API the key is for is the `variable` setting's: the account of a standard is made by `accepts`
            // below, which fixes it.
            provides: &[],
            title: "OpenCode provider key",
            description: "A key for one provider, read from the variable `variable` names, with OpenCode started on a data folder of its own so that no key in the person's `auth.json` meets it.",
            credentials: &[Credential {
                key: "api_key",
                required: true,
            }],
            settings: &[Setting {
                name: "variable",
                required: true,
                url: false,
                choices: KEY_VARIABLES,
            }],
            state_dir: true,
            rotates: false,
            history_portable: false,
            renews: false,
            endpoints: &[],
            notices: &[
                "the credential is in the environment of every program the agent runs",
                "the account has an OpenCode data folder of its own: its sessions are the account's, and the person's own `opencode.json` still applies",
            ],
            credential_variables: &[],
            setting_variables: &[],
            named_variable: Some(("variable", "api_key")),
            home_variable: Some("XDG_DATA_HOME"),
            file: None,
            renew_unsupported: "a key is not renewed",
        },
        KindDef {
            id: PROVIDER_KEY,
            provides: &[standard::OPENCODE_KEY],
            title: "OpenCode provider key",
            description: "A key for one provider, read from the variable the account names, made by the `api-key` sign-in; OpenCode starts on a data folder of its own so that no key in the person's `auth.json` meets it.",
            credentials: &[Credential {
                key: "api_key",
                required: true,
            }],
            settings: &[Setting {
                name: "variable",
                required: true,
                url: false,
                choices: KEY_VARIABLES,
            }],
            state_dir: true,
            rotates: false,
            history_portable: false,
            renews: false,
            endpoints: &[],
            notices: &[
                "the credential is in the environment of every program the agent runs",
                "the account has an OpenCode data folder of its own: its sessions are the account's, and the person's own `opencode.json` still applies",
            ],
            credential_variables: &[],
            setting_variables: &[],
            named_variable: Some(("variable", "api_key")),
            home_variable: Some("XDG_DATA_HOME"),
            file: None,
            renew_unsupported: "a key is not renewed",
        },
    ],
    own_standard: Some(standard::OPENCODE),
    // A key of each API OpenCode has a variable for. The endpoint of a provider is the person's own configuration
    // (`provider.<id>.options.baseURL`), which no account moves.
    accepts: &[
        key_of!(standard::ANTHROPIC_API, "ANTHROPIC_API_KEY"),
        key_of!(standard::OPENAI_API, "OPENAI_API_KEY"),
        key_of!(standard::GEMINI_API, "GEMINI_API_KEY"),
        key_of!(standard::XAI_API, "XAI_API_KEY"),
        AcceptDef {
            standard: standard::OPENCODE,
            slots: &[SlotDef {
                credential: "auth_json",
                kind: LOGIN,
                variable: None,
                file: Some(AUTH_FILE),
                base_url_setting: None,
                base_url_variable: None,
                dir_variable: Some("XDG_DATA_HOME"),
                settings: &[],
            }],
        },
    ],
    variables: VARIABLES,
    renew_args: None,
    login_unsupported: "OpenCode signs in with `opencode auth login`, a picker over every provider that needs a terminal, which this provider does not drive; a key account is made with the `api-key` method",
};

/// The ways to sign in: only the key. A provider's sign-in through a browser is a picker in a terminal.
#[must_use]
pub fn logins() -> Vec<LoginMethodInfo> {
    let mut key = LoginMethodInfo::new("api-key", "Provider API key", PROVIDER_KEY);
    key.description = "Asks which provider's variable the key is for, then the key, and checks that OpenCode takes the variable as that provider's sign-in. It sends nothing to the provider.".to_string();
    key.inputs = schema(&json!({
        "type": "object",
        "properties": {"variable": {"type": "string", "enum": KEY_VARIABLES}},
        "required": ["variable"],
        "additionalProperties": false,
    }));
    key.steps = vec![ASK.to_string()];
    key.remote = true;
    vec![key]
}

fn launch(program: &str, args: &[&str], dir: &str, extra: &[(&str, &str)]) -> Launch {
    let mut launch = Launch::new(program)
        .args(args.iter().copied())
        .env("XDG_DATA_HOME", dir);
    for (name, value) in extra {
        launch = launch.env(*name, *value);
    }
    launch.remove_env = VARIABLES.iter().map(OsString::from).collect();
    launch
}

fn bad(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_BAD_VALUE, why)
}

/// `opencode auth list` on the account's folder with the key in its variable: it lists the variable under the
/// environment when OpenCode takes it as a provider's sign-in, and calls no provider.
fn key_is_seen(
    ctx: &Ctx<'_>,
    program: &str,
    dir: &str,
    variable: &str,
    key: &str,
) -> Result<bool, ProviderError> {
    let mut child = ctx.spawn(&launch(program, &["auth", "list"], dir, &[(variable, key)]))?;
    let mut seen = false;
    let mut last = alloc::string::String::new();
    let exit = child.run(ctx, |line| {
        if line.split_whitespace().any(|word| word == variable) {
            seen = true;
        }
        if !line.trim().is_empty() {
            last = line.to_string();
        }
        Ok(())
    })?;
    if exit.code != Some(0) {
        return Err(bad(&alloc::format!(
            "login refused: {}",
            reason(&last, &[key])
        )));
    }
    Ok(seen)
}

fn api_key(
    ctx: &Ctx<'_>,
    program: &str,
    dir: &str,
    variable: &str,
) -> Result<LoginOutcome, ProviderError> {
    if !KEY_VARIABLES.contains(&variable) {
        return Err(bad(
            "login refused: that is no variable OpenCode reads a key from",
        ));
    }
    let form = LoginForm::new(
        "OpenCode provider key",
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
        return Err(bad("login refused: no usable key was given"));
    }
    if !key_is_seen(ctx, program, dir, variable, &key)? {
        return Err(bad(
            "login refused: OpenCode does not take that variable as a provider's sign-in",
        ));
    }
    let mut credentials = Credentials::new();
    credentials.insert("api_key", key);
    let mut outcome = LoginOutcome::new(PROVIDER_KEY, credentials);
    outcome.settings = schema(&json!({"variable": variable}));
    // Whether the provider accepts the key is not known without a model turn.
    outcome.checked = false;
    Ok(outcome)
}

/// Runs the sign-in `request.method` names.
///
/// # Errors
///
/// As [`AccountProvider::login`](coqui_acp_api::AccountProvider::login).
pub fn sign_in(
    program: alloc::string::String,
    request: LoginRequest,
    spawner: Object<dyn ProcessSpawner>,
    prompter: Object<dyn LoginPrompter>,
) -> Result<LoginOutcome, ProviderError> {
    if request.method != "api-key" {
        return Err(ProviderError::new(
            Status::GUATIAO_ERR_BAD_VALUE,
            "this harness offers no such way to sign in",
        ));
    }
    let variable = crate::provider::to_json(&request.inputs)
        .get("variable")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| bad("login refused: the variable the key is for is missing"))?;
    let dir = request.state_dir.clone();
    login::run(spawner, prompter, request.timeout_ms, move |ctx| {
        api_key(ctx, &program, &dir, &variable)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdio::accounts::{apply, catalog};
    use coqui_acp_api::AccountGrant;

    const CANARY: &str = "sk-canary-91c0";

    fn foreign(standards: &[&str], key: &str) -> AccountGrant {
        AccountGrant::new("a", "someone-elses-kind")
            .provides(standards.iter().copied())
            .credentials(Credentials::from_iter([(key, CANARY)]))
            .state_dir("/some/dir")
    }

    fn names(applied: &crate::stdio::accounts::Applied) -> Vec<alloc::string::String> {
        applied
            .environment
            .expose()
            .names()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn opencode_lists_what_it_provides_and_accepts() {
        let catalog = catalog(&DEF);
        assert_eq!(
            catalog.kind(LOGIN).expect("login").provides,
            [standard::OPENCODE]
        );
        assert_eq!(
            catalog.kind(PROVIDER_KEY).expect("key").provides,
            [standard::OPENCODE_KEY]
        );
        assert!(catalog.kind(API_KEY).expect("applier").provides.is_empty());
        let accepted: Vec<&str> = catalog
            .accepts
            .iter()
            .map(|accepted| accepted.standard.as_str())
            .collect();
        assert_eq!(
            accepted,
            [
                standard::ANTHROPIC_API,
                standard::OPENAI_API,
                standard::GEMINI_API,
                standard::XAI_API,
                standard::OPENCODE
            ]
        );
    }

    #[test]
    fn every_variable_a_key_may_use_is_one_a_start_clears_and_the_access_is_not() {
        for variable in KEY_VARIABLES {
            assert!(VARIABLES.contains(variable), "{variable}");
        }
        assert!(!VARIABLES.contains(&"OPENCODE_PERMISSION"));
        assert!(VARIABLES.contains(&"XDG_DATA_HOME"));
    }

    #[test]
    fn a_key_of_each_accepted_api_is_read_from_that_apis_variable_in_a_data_folder_of_its_own() {
        for (standard, variable) in [
            (standard::ANTHROPIC_API, "ANTHROPIC_API_KEY"),
            (standard::OPENAI_API, "OPENAI_API_KEY"),
            (standard::GEMINI_API, "GEMINI_API_KEY"),
            (standard::XAI_API, "XAI_API_KEY"),
        ] {
            let applied = apply(&DEF, &foreign(&[standard], "api_key"))
                .expect("accepted")
                .expect("a kind");
            let seen = names(&applied);
            assert!(seen.contains(&variable.to_string()), "{standard}: {seen:?}");
            assert!(seen.contains(&"XDG_DATA_HOME".to_string()), "{standard}");
            assert!(!alloc::format!("{applied:?}").contains(CANARY));
        }
    }

    #[test]
    fn a_sign_in_from_elsewhere_is_the_file_in_the_data_folder_it_watches() {
        let applied = apply(&DEF, &foreign(&[standard::OPENCODE], "auth_json"))
            .expect("accepted")
            .expect("a kind");
        assert_eq!(applied.watched.expect("watched").path, "opencode/auth.json");
    }

    #[test]
    fn what_opencode_has_no_variable_for_or_cannot_move_is_refused_with_no_value_in_the_reason() {
        for (standards, key) in [
            (&[standard::CLAUDE_CODE][..], "token"),
            (&[standard::CODEX][..], "auth_file"),
            (&[][..], "api_key"),
            (&[standard::ANTHROPIC_API][..], "auth_token"),
        ] {
            let Err(error) = apply(&DEF, &foreign(standards, key)) else {
                panic!("{standards:?} {key} was taken");
            };
            assert!(!error.message().contains(CANARY));
        }
        let moved = foreign(&[standard::OPENAI_API], "api_key")
            .settings(schema(&json!({"base_url": "https://gateway.example/v1"})));
        assert!(apply(&DEF, &moved).is_err());
    }

    #[test]
    fn a_key_account_names_a_variable_from_the_list_and_nothing_else() {
        let key = |variable: &str| {
            AccountGrant::new("a", API_KEY)
                .settings(schema(&json!({"variable": variable})))
                .credentials(Credentials::from_iter([("api_key", CANARY)]))
                .state_dir("/some/dir")
        };
        let applied = apply(&DEF, &key("GROQ_API_KEY"))
            .expect("applies")
            .expect("a kind");
        assert!(names(&applied).contains(&"GROQ_API_KEY".to_string()));
        for bad in [
            "LD_PRELOAD",
            "PATH",
            "OPENCODE_PERMISSION",
            "XDG_DATA_HOME",
            "",
        ] {
            let Err(error) = apply(&DEF, &key(bad)) else {
                panic!("{bad:?} was taken");
            };
            assert!(!error.message().contains(CANARY));
        }
    }
}
