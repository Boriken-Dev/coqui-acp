//! Pi (`pi-acp`), read from Pi 1.5.0's source; `pi-acp` itself and a real sign-in were not run.
//!
//! `PI_CODING_AGENT_DIR` places `auth.json`, the settings and the sessions. Pi's own sign-in (`/login`) exists only inside
//! its terminal interface, so this provider lists no login method: an API key account is made from its settings.

use super::{Credential, HarnessDef, KindDef, Setting};

/// The kind of a key read from one of the variables below.
pub const API_KEY: &str = "pi-api-key";
/// The kind of a sign-in kept in `auth.json`.
pub const OAUTH: &str = "pi-oauth";

/// The variables Pi reads a provider's key from. A key account names one.
const KEY_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "HF_TOKEN",
    "COPILOT_GITHUB_TOKEN",
];

/// Every variable Pi reads for a credential or for where it keeps them, so a start on an account removes them all.
const VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "HF_TOKEN",
    "COPILOT_GITHUB_TOKEN",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
];

/// What Pi takes as an account.
pub static DEF: HarnessDef = HarnessDef {
    name: "Pi",
    program: "pi-acp",
    args: &[],
    kinds: &[
        KindDef {
            id: API_KEY,
            title: "Pi provider key",
            description: "A key for one provider, read from the variable `variable` names. A key kept in `auth.json` of Pi's directory wins over it.",
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
            state_dir: false,
            rotates: false,
            history_portable: true,
            renews: false,
            endpoints: &[],
            notices: &[
                "the credential is in the environment of every program the agent runs",
                "Pi prefers a key in its own auth.json to the environment",
            ],
            credential_variables: &[],
            setting_variables: &[],
            named_variable: Some(("variable", "api_key")),
            home_variable: Some("PI_CODING_AGENT_DIR"),
            file: None,
            renew_unsupported: "a key is not renewed",
        },
        KindDef {
            id: OAUTH,
            title: "Pi sign-in",
            description: "The `auth.json` Pi's own sign-in (`/login`) wrote, kept in Pi's directory.",
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
            notices: &["the login is kept in a file that what the agent runs can read"],
            credential_variables: &[],
            setting_variables: &[],
            named_variable: None,
            home_variable: Some("PI_CODING_AGENT_DIR"),
            file: Some(("auth.json", "auth_json")),
            renew_unsupported: "Pi refreshes a sign-in itself, under a lock, when it needs one; it has no command that does so without a model turn",
        },
    ],
    variables: VARIABLES,
    renew_args: None,
    login_unsupported: "Pi signs in only inside its terminal interface (`/login`), which this provider does not drive; a key account is made from its settings",
};
