//! Pi's accounts as a provider of the `account_provider` kind.
//!
//! The one account it keeps is the sign-in Pi's own `/login` wrote into `auth.json`, which it watches for a change nobody
//! told the host of. Pi signs in only inside its terminal interface and refreshes a sign-in itself, so it signs no one in
//! and renews nothing, and it has no plan limits to read. It runs no agent: the harness
//! ([`PiProvider`](crate::PiProvider)) applies an account the host hands it.

use core::time::Duration;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{FromValue, Provider, Schema, Status, ToValue};
use coqui_acp_api::{
    AccountKeeper, AccountOffer, AccountProvider, AccountProviderInfo, AccountUsage,
    AccountUsageQuery, AccountWatch, LoginOutcome, LoginPrompter, LoginRequest, ProcessSpawner,
    RenewReport, RenewRequest, WatchRequest,
};
use coqui_acp_harness::account::DEFAULT_POLL_MS;
use coqui_acp_harness::stdio::accounts::Keeping;

use crate::account_usage::read_limits;
use crate::accounts::DEF;

/// How the Pi account provider is configured.
#[derive(ToValue, FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct AccountSettings {
    /// How often, in milliseconds, a watch looks at the file an account keeps its sign-in in; 0 looks only when the watch
    /// begins and ends.
    #[map(default = 30_000)]
    pub renew_poll_ms: u64,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            renew_poll_ms: DEFAULT_POLL_MS,
        }
    }
}

/// The accounts of Pi, as a provider of `account_provider` under the id `pi_accounts`.
///
/// Register it beside a host's own providers with `Registry::register_local`, or build it into a library with
/// `guatiao::providers!`.
#[derive(Provider, Debug)]
#[provider(
    AccountProvider,
    id = "pi_accounts",
    config = AccountSettings,
    new = PiAccounts::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
pub struct PiAccounts {
    settings: AccountSettings,
    keeping: Keeping,
}

impl From<AccountSettings> for PiAccounts {
    fn from(settings: AccountSettings) -> Self {
        Self::new(settings)
    }
}

impl PiAccounts {
    /// A provider configured as `settings` say.
    #[must_use]
    pub fn new(settings: AccountSettings) -> Self {
        Self {
            settings,
            keeping: Keeping::new(&DEF),
        }
    }

    fn unconfigured() -> Self {
        Self::new(AccountSettings::default())
    }
}

impl AccountProvider for PiAccounts {
    fn describe(&self) -> Result<AccountProviderInfo, ProviderError> {
        let mut info = AccountProviderInfo::new("Pi accounts");
        info.description =
            "Watches the sign-in Pi's own `/login` wrote; it signs no one in, renews nothing and reads no plan limits."
                .into();
        Ok(info)
    }

    fn offers(&self) -> Result<AccountOffer, ProviderError> {
        Ok(self.keeping.offer(Vec::new()))
    }

    fn login(
        &self,
        _request: LoginRequest,
        _spawner: Object<dyn ProcessSpawner>,
        _prompter: Object<dyn LoginPrompter>,
    ) -> Result<LoginOutcome, ProviderError> {
        Err(ProviderError::new(
            Status::GUATIAO_ERR_UNSUPPORTED,
            DEF.login_unsupported,
        ))
    }

    fn renew(
        &self,
        request: RenewRequest,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<RenewReport, ProviderError> {
        // Pi's table names no renewal command, so this runs none: it answers why the kind is not renewed.
        self.keeping
            .renew(request, spawner, DEF.program, Duration::ZERO)
    }

    fn watch(
        &self,
        request: WatchRequest,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn AccountWatch>, ProviderError> {
        self.keeping.watch(
            request,
            spawner,
            keeper,
            Duration::from_millis(self.settings.renew_poll_ms),
        )
    }

    fn usage(
        &self,
        query: AccountUsageQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<AccountUsage, ProviderError> {
        drop((spawner, keeper));
        self.keeping.own(&query.account)?;
        Ok(AccountUsage::from_report(read_limits(&query.account)?))
    }
}
