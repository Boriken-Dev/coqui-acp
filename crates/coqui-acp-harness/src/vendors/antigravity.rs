//! Antigravity (`agy acp`), measured on 1.5.3 from its help text and binary strings only.
//!
//! Its login is kept in `<gemini-dir>/<app-data-dir>/antigravity-oauth-token` and, on Windows, in the Credential Manager
//! entry `gemini:antigravity`. Which one wins, whether `--gemini-dir` and `--app-data-dir` move the token, whether two
//! logins can exist at once, and what the sign-in `agy acp --login` prints were not measured, so the provider lists no
//! kind but `harness-login` and no login method until they are.

use crate::access::{Access, Approvals, Part, Sandbox, Target, Translation};
use crate::stdio::accounts::HarnessDef;
use crate::stdio::{StdioAccountSettings, StdioDef, StdioSettings, no_logins};

/// What Antigravity takes as an account.
pub static DEF: HarnessDef = HarnessDef {
    name: "Antigravity",
    program: "agy",
    args: &["acp"],
    kinds: &[],
    accepts: &[],
    own_standard: Some(coqui_acp_api::standard::ANTIGRAVITY),
    variables: &[],
    renew_args: None,

    login_unsupported: "agy signs in with `agy acp --login`, whose prompts were not run: it may write the Windows Credential Manager, which a throwaway profile does not isolate, and where it keeps the login and whether two logins can exist at once are unmeasured",
};

/// How the stdio provider runs it.
pub static STDIO: StdioDef = StdioDef {
    accounts: &DEF,
    accounts_description: "Antigravity's sign-in is unmeasured, so it keeps no account yet.",
    logins: no_logins,
    sign_in: None,
    session_ids: None,
    import: None,
    export: None,
    import_unsupported: "Antigravity keeps a conversation as a database of its own protobuf steps, which it does not document, so it cannot be seeded with a history it did not make; set replay_max_bytes to replay the history as context in the first prompt",
    export_unsupported: "Antigravity keeps each conversation as a SQLite database of protobuf steps in its own program's folder (conversations/<id>.db), a schema it does not publish and changes between releases, so this provider does not read one; a host that records the sessions it serves exports them from its own record",
};

crate::stdio_vendor! {
    /// Antigravity, as a provider of `acp_harness` under the id `antigravity`: `agy acp`.
    harness: AntigravityProvider, id: "antigravity",
    /// Antigravity's accounts, as a provider of `account_provider` under the id `antigravity_accounts`.
    accounts: AntigravityAccounts, id: "antigravity_accounts",
    def: STDIO,
    settings: StdioSettings, account_settings: StdioAccountSettings,
}

/// Antigravity's access settings in its words, under the AOS harness id `antigravity`.
pub const TARGET: Target = Target::new("antigravity", translate);

const fn translate(access: Access) -> Translation {
    let sandbox = match access.sandbox {
        Sandbox::Workspace => Part::args(
            &["--sandbox"],
            "`agy acp --sandbox`, which restricts its terminal",
        ),
        Sandbox::ReadOnly => Part::args(
            &["--sandbox"],
            "`agy acp --sandbox`, which restricts its terminal; Antigravity does not say it forbids every write",
        ),
        _ => Part::said("`agy acp` without --sandbox: its terminal is not restricted"),
    };
    let network = if access.network {
        Part::cannot("Antigravity has no network setting: its own applies")
    } else {
        Part::cannot("Antigravity has no network setting")
    };
    let approvals = match access.approvals {
        Approvals::Never => Part::args(
            &["--dangerously-skip-permissions"],
            "`agy acp --dangerously-skip-permissions`",
        ),
        Approvals::Edits => Part::cannot("Antigravity has no accept-edits setting"),
        _ => Part::said("Antigravity asks before it runs a tool, as it does by default"),
    };
    Translation::new(sandbox, network, approvals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdio::accounts::{apply, catalog};
    use coqui_acp_api::standard;
    use coqui_acp_api::{AccountGrant, Credentials, HARNESS_LOGIN};

    #[test]
    fn the_table_is_well_formed() {
        assert_eq!(TARGET.well_formed(), Ok(()));
    }

    #[test]
    fn the_standard_access_adds_no_argument() {
        assert!(Access::STANDARD.translate(&TARGET).args().is_empty());
    }

    #[test]
    fn the_sandbox_and_never_approvals_are_arguments() {
        let mut access = Access::STANDARD;
        access.sandbox = Sandbox::Workspace;
        access.approvals = Approvals::Never;
        let agy = access.translate(&TARGET);
        assert_eq!(
            agy.args(),
            vec!["--sandbox", "--dangerously-skip-permissions"]
        );
        assert!(agy.env().is_empty());
        assert_eq!(agy.json(), None);
    }

    #[test]
    fn its_own_login_provides_its_standard_and_nothing_else_is_taken() {
        let catalog = catalog(&DEF);
        assert_eq!(catalog.kinds.len(), 1);
        assert_eq!(
            catalog.kind(HARNESS_LOGIN).expect("own").provides,
            [standard::ANTIGRAVITY]
        );
        assert!(catalog.accepts.is_empty());
        let grant = AccountGrant::new("a", "someone-elses-kind")
            .provides([standard::XAI_API])
            .credentials(Credentials::from_iter([("api_key", "sk-canary-3b7c")]))
            .state_dir("/some/dir");
        let Err(error) = apply(&DEF, &grant) else {
            panic!("a key was taken");
        };
        assert!(error.message().contains("none"), "{}", error.message());
    }
}
