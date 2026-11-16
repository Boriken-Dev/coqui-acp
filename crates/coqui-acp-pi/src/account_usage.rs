//! The plan limits of a Pi account: none.
//!
//! This is the account's side, not the harness's. Pi has no command or view that reports a plan's limits, and what a
//! Pi account holds is a provider's key or a sign-in kept in `auth.json`, which Pi keeps for its own use: this crate
//! reads no vendor's limits with it. A reader that asks a vendor for the limits of a sign-in Pi made is that
//! vendor's, with the credential that vendor's account kind names.

use alloc::string::ToString;

use coqui_acp_api::guatiao::library::ProviderError;
use coqui_acp_api::{AccountGrant, UsageReport};
use coqui_acp_harness::stdio::accounts::apply;

use crate::accounts::{API_KEY, DEF, OAUTH};

/// What `grant` has of plan limits, which for a Pi account is [`UsageReport::unsupported`], saying why. The grant is
/// read first, so that one the provider refuses is refused as it is everywhere.
///
/// # Errors
///
/// `BAD_VALUE`: an id that is not one, an unlisted kind, a setting the kind lacks, a credential missing or not the
/// kind's.
pub fn read_limits(grant: &AccountGrant) -> Result<UsageReport, ProviderError> {
    apply(&DEF, grant)?;
    let reason = match grant.kind.as_str() {
        API_KEY => "a provider's key is billed by use and has no plan limits",
        OAUTH => "Pi reports no plan limits for the sign-in it keeps",
        _ => "Pi reports no plan limits",
    };
    let mut report = UsageReport::unsupported(reason.to_string());
    report.account = Some(grant.id.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coqui_acp_api::Credentials;

    #[test]
    fn a_key_and_a_sign_in_are_not_offered_and_a_refused_grant_is_refused() {
        let key = AccountGrant::new("k", API_KEY)
            .settings(coqui_acp_harness::provider::schema(
                &serde_json::json!({"variable": "OPENAI_API_KEY"}),
            ))
            .credentials(Credentials::from_iter([("api_key", "sk-canary")]))
            .state_dir("/some/dir");
        let report = read_limits(&key).expect("a key");
        assert!(!report.supported);
        assert_eq!(report.account.as_deref(), Some("k"));
        assert!(
            !report
                .unsupported_because
                .unwrap_or_default()
                .contains("canary")
        );
        let strange = AccountGrant::new("s", "no-such-kind");
        assert!(read_limits(&strange).is_err());
    }
}
