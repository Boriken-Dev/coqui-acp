//! The accounts Pi takes, read from Pi 1.5.0's source and its documentation; a real sign-in was not run.
//!
//! `PI_CODING_AGENT_DIR` places `auth.json`, the settings and the sessions. Pi's own sign-in (`/login`, `pi auth login`)
//! needs a terminal, so the one way this provider signs in is a provider's key, typed in and kept as an account.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use coqui_acp_api::guatiao::schema::{FieldBuilder, KindBuilder, SchemaBuilder};
use coqui_acp_api::standard;
use coqui_acp_api::{ASK, AccountCatalog, LoginMethodInfo};
use coqui_acp_harness::stdio::accounts::{
    self, AcceptDef, Credential, HarnessDef, KindDef, Setting, SlotDef,
};

/// The kind of a key read from one of the variables below.
pub const API_KEY: &str = "pi-api-key";
/// The kind of a sign-in kept in `auth.json`.
pub const OAUTH: &str = "pi-oauth";
/// The login method that makes a [`API_KEY`] account: the variable and the key, typed in.
pub const KEY_METHOD: &str = "api-key";

/// The variables Pi reads a provider's key from: what a form suggests for a key account's `variable`, which may name any
/// other variable (a provider a later Pi adds).
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

/// The API each of those variables carries a key of, for the ones Pi's `accepts` below has a slot for: a key account that
/// names one provides that standard, so any harness that takes it can use the key. A key of another variable provides
/// nothing, and an OAuth or bearer token is not a key of the API.
const KEY_STANDARDS: &[(&str, &str)] = &[
    ("ANTHROPIC_API_KEY", standard::ANTHROPIC_API),
    ("OPENAI_API_KEY", standard::OPENAI_API),
    ("GEMINI_API_KEY", standard::GEMINI_API),
    ("XAI_API_KEY", standard::XAI_API),
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
            // Which API the key is for is the `variable` setting's value (`KEY_STANDARDS`), so the kind lists no
            // standard of its own: a key of a variable Pi has no API for provides none.
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
                choices: &[],
                suggestions: KEY_VARIABLES,
                provides: KEY_STANDARDS,
            }],
            state_dir: true,
            rotates: false,
            history_portable: false,
            renews: false,
            endpoints: &[],
            notices: &[
                "the credential is in the environment of every program the agent runs, in the variable the account names",
                "Pi uses it only where it reads that variable: a provider's own key variable",
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
    login_unsupported: "Pi signs in only inside its terminal interface (`/login`), which this provider does not drive; a provider's key is the one way it signs in (`api-key`)",
};

/// What the provider lists as the accounts Pi takes: the two kinds.
#[must_use]
pub fn catalog() -> AccountCatalog {
    accounts::catalog(&DEF)
}

/// The ways to sign in: a provider's key, with the variable it is read from named first (a provider's own is suggested, any
/// other name works) and the key asked for after.
#[must_use]
pub fn logins() -> Vec<LoginMethodInfo> {
    let mut key = LoginMethodInfo::new(KEY_METHOD, "Provider API key", API_KEY);
    key.description = "Asks which environment variable the key goes in and then the key. It sends nothing to the provider; the account provides the API the variable is a key of.".to_string();
    key.inputs = SchemaBuilder::new()
        .field(
            FieldBuilder::new("variable", KindBuilder::string())
                .title("Variable")
                .description("The environment variable Pi reads the key from.")
                .examples(KEY_VARIABLES.iter().copied())
                .required(),
        )
        .finish()
        .expect("the inputs schema builds");
    key.steps = vec![ASK.to_string()];
    key.remote = true;
    vec![key]
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
        assert!(DEF.kinds.iter().all(KindDef::provides_some));
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
    fn a_key_account_names_any_variable_and_the_form_only_suggests() {
        let info = catalog().kind(API_KEY).expect("key").settings.clone();
        let variable = &coqui_acp_harness::provider::to_json(&info)["properties"]["variable"];
        assert_eq!(variable["examples"], serde_json::json!(KEY_VARIABLES));
        assert!(variable.get("enum").is_none(), "{variable}");
        let key = |name: &str| {
            AccountGrant::new("a", API_KEY)
                .settings(coqui_acp_harness::provider::schema(
                    &serde_json::json!({"variable": name}),
                ))
                .credentials(Credentials::from_iter([("api_key", CANARY)]))
                .state_dir("/some/dir")
        };
        for name in [
            "GROQ_API_KEY",
            "ACME_GATEWAY_KEY",
            "lower_1",
            "PATH",
            "LD_PRELOAD",
        ] {
            let applied = apply(&DEF, &key(name))
                .unwrap_or_else(|error| panic!("{name} refused: {}", error.message()))
                .expect("a kind");
            let names: Vec<String> = applied
                .environment
                .expose()
                .names()
                .map(ToString::to_string)
                .collect();
            assert!(names.contains(&name.to_string()), "{name}: {names:?}");
            assert!(!alloc::format!("{applied:?}").contains(CANARY));
        }
        for bad in [
            "",
            "1ST",
            "A=B",
            "TWO WORDS",
            "KEY-NAME",
            "AOS_X",
            "aos_x",
            "T_OVERLAY_ROOT",
            "t_overlay_state",
        ] {
            let Err(error) = apply(&DEF, &key(bad)) else {
                panic!("{bad:?} was taken");
            };
            assert!(!error.message().contains(CANARY));
        }
    }

    fn key_account(variable: &str) -> AccountGrant {
        AccountGrant::new("a", API_KEY)
            .settings(coqui_acp_harness::provider::schema(
                &serde_json::json!({"variable": variable}),
            ))
            .credentials(Credentials::from_iter([("api_key", CANARY)]))
            .state_dir("/some/dir")
    }

    fn provided_by(grant: &AccountGrant) -> Vec<String> {
        let catalog = catalog();
        accounts::provided(catalog.kind(&grant.kind).expect("kind"), &grant.settings)
    }

    #[test]
    fn a_key_account_provides_the_api_of_the_variable_it_names() {
        for (variable, api) in KEY_STANDARDS {
            assert_eq!(provided_by(&key_account(variable)), [*api], "{variable}");
        }
        // Each pair is a slot of `accepts`, and each slot of a key has its pair: one table said twice would drift.
        let slots: Vec<(&str, &str)> = DEF
            .accepts
            .iter()
            .filter(|accept| accept.standard != standard::PI)
            .flat_map(|accept| {
                accept
                    .slots
                    .iter()
                    .map(|slot| (slot.variable.expect("a key variable"), accept.standard))
            })
            .collect();
        assert_eq!(slots, KEY_STANDARDS);
    }

    #[test]
    fn a_key_account_of_another_variable_or_none_provides_nothing() {
        for variable in [
            "MISTRAL_API_KEY",
            "GROQ_API_KEY",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_AUTH_TOKEN",
            "ACME_GATEWAY_KEY",
        ] {
            assert!(provided_by(&key_account(variable)).is_empty(), "{variable}");
        }
        let bare = AccountGrant::new("a", API_KEY);
        assert!(provided_by(&bare).is_empty());
    }

    #[test]
    fn claude_takes_a_pi_key_that_provides_its_api_and_refuses_one_that_provides_none() {
        let claude = coqui_acp_claude::accounts::catalog();
        let anthropic = key_account("ANTHROPIC_API_KEY");
        let provides = provided_by(&anthropic);
        let taken = claude
            .accepted_by(&provides)
            .expect("Claude takes a key of the Anthropic API");
        assert_eq!(taken.standard, standard::ANTHROPIC_API);
        assert!(
            taken.slots.iter().any(|slot| slot.credential == "api_key"),
            "the credential slot stays api_key"
        );
        claude
            .accepts_reference(&anthropic.provides(provides).reference())
            .expect("accepted");
        let mistral = key_account("MISTRAL_API_KEY");
        assert!(
            claude
                .accepts_reference(
                    &mistral
                        .provides(provided_by(&key_account("MISTRAL_API_KEY")))
                        .reference()
                )
                .is_err()
        );
    }

    #[test]
    fn the_schema_marks_the_setting_whose_value_picks_the_standard() {
        let info = catalog().kind(API_KEY).expect("key").settings.clone();
        let variable = &coqui_acp_harness::provider::to_json(&info)["properties"]["variable"];
        assert_eq!(
            variable[accounts::PROVIDES_BY_VALUE]["OPENAI_API_KEY"],
            serde_json::json!([standard::OPENAI_API])
        );
        assert!(variable[accounts::PROVIDES_BY_VALUE]["MISTRAL_API_KEY"].is_null());
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
