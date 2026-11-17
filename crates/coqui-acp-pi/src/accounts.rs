//! The accounts Pi takes, read from Pi 1.5.0's source and its documentation; a real sign-in was not run.
//!
//! `PI_CODING_AGENT_DIR` places `auth.json`, the settings and the sessions. Pi's own sign-in (`/login`, `pi auth login`)
//! needs a terminal, so this provider lists no login method: an API key account is made from its settings.

use coqui_acp_api::AccountCatalog;
use coqui_acp_api::standard;
use coqui_acp_harness::stdio::accounts::{
    self, AcceptDef, Credential, HarnessDef, KindDef, Setting, SlotDef,
};

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

/// Every variable Pi reads for a credential, a cloud identity, an endpoint or for where it keeps them, so a start on an
/// account removes them all. From Pi 1.5.0's source and `docs/providers.md`; a provider that Pi's dependencies add is not
/// in it.
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
    // Provider keys beyond the ones an account can name.
    "AI_GATEWAY_API_KEY",
    "ANT_LING_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "BASETEN_API_KEY",
    "CLOUDFLARE_API_KEY",
    "FIREWORKS_API_KEY",
    "GOOGLE_CLOUD_API_KEY",
    "KIMI_API_KEY",
    "META_API_KEY",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MOONSHOT_API_KEY",
    "NVIDIA_API_KEY",
    "OPENCODE_API_KEY",
    "QWEN_TOKEN_PLAN_API_KEY",
    "QWEN_TOKEN_PLAN_CN_API_KEY",
    "RADIUS_API_KEY",
    "TOGETHER_API_KEY",
    "TYPESAFE_API_KEY",
    "XIAOMI_API_KEY",
    "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
    "XIAOMI_TOKEN_PLAN_CN_API_KEY",
    "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
    "ZAI_API_KEY",
    "ZAI_CODING_CN_API_KEY",
    // Endpoints.
    "ANTHROPIC_BASE_URL",
    "OPENAI_BASE_URL",
    "OPENROUTER_BASE_URL",
    "LLAMA_BASE_URL",
    "AZURE_OPENAI_BASE_URL",
    "AZURE_OPENAI_ENDPOINT",
    "AZURE_OPENAI_RESOURCE_NAME",
    "AZURE_OPENAI_API_VERSION",
    "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
    "VERTEX_BASE_URL",
    "CLOUDFLARE_ACCOUNT_ID",
    "CLOUDFLARE_GATEWAY_ID",
    "GCE_METADATA_HOST",
    "AWS_EC2_METADATA_SERVICE_ENDPOINT",
    "PI_INSTALLER_API_BASE",
    "PI_SHARE_VIEWER_URL",
    // Cloud identities.
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_CLOUD_QUOTA_PROJECT",
    "GCLOUD_PROJECT",
    "CLOUDSDK_CONFIG",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_SERVICE_ACCOUNT_ID",
    "ANTHROPIC_WORKSPACE_ID",
];

/// A key for the API `$standard`, read by Pi from `$variable`: a key account with that variable fixed.
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
                dir_variable: Some("PI_CODING_AGENT_DIR"),
                settings: &[("variable", $variable)],
            }],
        }
    };
}

/// What Pi takes as an account.
pub static DEF: HarnessDef = HarnessDef {
    name: "Pi",
    program: "pi",
    args: &[],
    kinds: &[
        KindDef {
            id: API_KEY,
            // Which API the key is for is the `variable` setting's: the account of a standard is made
            // by `accepts` below, which fixes it.
            provides: &[],
            title: "Pi provider key",
            description: "A key for one provider, read from the variable `variable` names, with Pi started in a directory of its own so that no key in `auth.json` and no `baseUrl` in the person's own `models.json` overrides it.",
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
                "the account has a Pi directory of its own, without the person's settings",
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
            provides: &[standard::PI],
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
    own_standard: Some(standard::PI),
    // A key of each API Pi has a variable for. Pi's endpoints are the person's own `models.json`, which a key account's
    // directory leaves out, so none of these moves one.
    accepts: &[
        key_of!(standard::ANTHROPIC_API, "ANTHROPIC_API_KEY"),
        key_of!(standard::OPENAI_API, "OPENAI_API_KEY"),
        key_of!(standard::GEMINI_API, "GEMINI_API_KEY"),
        key_of!(standard::XAI_API, "XAI_API_KEY"),
        AcceptDef {
            standard: standard::PI,
            slots: &[SlotDef {
                credential: "auth_json",
                kind: OAUTH,
                variable: None,
                file: Some("auth.json"),
                base_url_setting: None,
                base_url_variable: None,
                dir_variable: Some("PI_CODING_AGENT_DIR"),
                settings: &[],
            }],
        },
    ],
    variables: VARIABLES,
    renew_args: None,
    login_unsupported: "Pi signs in only inside its terminal interface (`/login`), which this provider does not drive; a key account is made from its settings",
};

/// What the provider lists as the accounts Pi takes: the two kinds, and no login method.
#[must_use]
pub fn catalog() -> AccountCatalog {
    accounts::catalog(&DEF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coqui_acp_api::AccountGrant;
    use coqui_acp_api::Credentials;
    use coqui_acp_harness::stdio::accounts::apply;

    const CANARY: &str = "sk-canary-77aa";

    fn foreign(standards: &[&str], key: &str) -> AccountGrant {
        AccountGrant::new("a", "someone-elses-kind")
            .provides(standards.iter().copied())
            .credentials(Credentials::from_iter([(key, CANARY)]))
            .state_dir("/some/dir")
    }

    #[test]
    fn pi_lists_the_apis_it_has_a_variable_for_and_its_own_sign_in() {
        let catalog = catalog();
        let accepted: Vec<&str> = catalog
            .accepts
            .iter()
            .map(|a| a.standard.as_str())
            .collect();
        assert_eq!(
            accepted,
            [
                standard::ANTHROPIC_API,
                standard::OPENAI_API,
                standard::GEMINI_API,
                standard::XAI_API,
                standard::PI
            ]
        );
        assert_eq!(
            catalog.kind(OAUTH).expect("sign-in").provides,
            [standard::PI]
        );
        assert!(catalog.kind(API_KEY).expect("key").provides.is_empty());
        for slot in catalog.accepts.iter().flat_map(|a| &a.slots) {
            assert!(catalog.kind(&slot.kind).is_some());
            if let Some(variable) = &slot.variable {
                assert!(KEY_VARIABLES.contains(&variable.as_str()), "{variable}");
            }
        }
    }

    #[test]
    fn a_key_of_each_accepted_api_is_read_by_pi_from_that_apis_variable_in_a_directory_of_its_own()
    {
        for (standard, variable) in [
            (standard::ANTHROPIC_API, "ANTHROPIC_API_KEY"),
            (standard::OPENAI_API, "OPENAI_API_KEY"),
            (standard::GEMINI_API, "GEMINI_API_KEY"),
            (standard::XAI_API, "XAI_API_KEY"),
        ] {
            let grant = foreign(&[standard], "api_key");
            let applied = apply(&DEF, &grant).expect("accepted").expect("a kind");
            let names: Vec<String> = applied
                .environment
                .expose()
                .names()
                .map(ToString::to_string)
                .collect();
            assert!(
                names.contains(&variable.to_string()),
                "{standard}: {names:?}"
            );
            assert!(
                names.contains(&"PI_CODING_AGENT_DIR".to_string()),
                "{standard}"
            );
            assert!(!alloc::format!("{applied:?}").contains(CANARY));
        }
    }

    #[test]
    fn pi_takes_what_it_has_no_variable_for_or_an_endpoint_it_cannot_move_by_refusing() {
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
        let moved = foreign(&[standard::OPENAI_API], "api_key").settings(
            coqui_acp_harness::provider::schema(
                &serde_json::json!({"base_url": "https://x.example"}),
            ),
        );
        let Err(error) = apply(&DEF, &moved) else {
            panic!("an endpoint Pi cannot move was taken");
        };
        assert!(
            error.message().contains("cannot move the endpoint"),
            "{}",
            error.message()
        );
    }
}
