//! Pi's accounts as a provider of the `account_provider` kind.
//!
//! The one account it keeps is the sign-in Pi's own `/login` wrote into `auth.json`, which it watches for a change nobody
//! told the host of. Pi signs in only inside its terminal interface and refreshes a sign-in itself, so it signs no one in
//! and renews nothing, and it has no plan limits to read. It runs no agent: the harness
//! ([`PiProvider`](crate::PiProvider)) applies an account the host hands it.

use alloc::string::String;
use alloc::sync::Arc;
use core::time::Duration;
use std::ffi::OsString;
use std::path::PathBuf;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{FromValue, Provider, Schema, Status, ToValue};
use coqui_acp_api::{
    AccountKeeper, AccountModelsQuery, AccountOffer, AccountProvider, AccountProviderInfo,
    AccountUsage, AccountUsageQuery, AccountWatch, HARNESS_LOGIN, LoginOutcome, LoginPrompter,
    LoginRequest, ModelInfo, ModelListing, ProcessSpawner, RenewReport, RenewRequest, UnknownKind,
    WatchRequest,
};
use coqui_acp_harness::ThroughSpawner;
use coqui_acp_harness::account::{Accounts, DEFAULT_POLL_MS};
use coqui_acp_harness::stdio::accounts::Keeping;

use crate::account_usage::read_limits;
use crate::accounts::DEF;
use crate::provider::apply_to;
use crate::{PiLaunch, PiSession, PiSettings};

/// How the Pi account provider is configured.
#[derive(ToValue, FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct AccountSettings {
    /// How often, in milliseconds, a watch looks at the file an account keeps its sign-in in; 0 looks only when the watch
    /// begins and ends.
    #[map(default = 30_000)]
    pub renew_poll_ms: u64,
    /// The Pi program it lists models with: a path, or a name looked up on the `PATH` where the spawner runs it. On
    /// Windows npm's shim is `pi.cmd`.
    #[map(default = String::from("pi"))]
    pub executable: String,
    /// A Node executable, an absolute path, that runs Pi's CLI `script` instead of an `executable` looked up on the
    /// `PATH`. Set with `script`.
    pub node: Option<String>,
    /// Pi's CLI script, an absolute path, which `node` runs.
    pub script: Option<String>,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            renew_poll_ms: DEFAULT_POLL_MS,
            executable: "pi".into(),
            node: None,
            script: None,
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
    /// What it knows of the files it placed for a call that lists models, and who hears of them.
    accounts: Arc<Accounts>,
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
            accounts: Arc::new(Accounts::new()),
        }
    }

    fn unconfigured() -> Self {
        Self::new(AccountSettings::default())
    }

    /// What Pi is started with for a call: the program and the environment the call adds.
    fn launch(&self, environment: &coqui_acp_api::Environment) -> PiLaunch {
        let mut launch = PiLaunch::new(".");
        launch.executable = OsString::from(&self.settings.executable);
        launch.node = self.settings.node.as_ref().map(PathBuf::from);
        launch.script = self.settings.script.as_ref().map(PathBuf::from);
        for (name, value) in environment.iter() {
            launch.env.insert(name.into(), value.into());
        }
        launch
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
        if query.account.kind == HARNESS_LOGIN {
            return self.keeping.usage(&query, "Pi reports no usage allowances");
        }
        self.keeping.own(&query.account)?;
        Ok(AccountUsage::from_report(read_limits(&query.account)?))
    }

    fn models(
        &self,
        query: AccountModelsQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<ModelListing, ProviderError> {
        let grant = &query.account;
        let local = grant.kind == HARNESS_LOGIN;
        if local {
            grant.check_id()?;
        } else {
            self.keeping.own(grant)?;
        }
        let mut launch = self.launch(query.environment.expose());
        let spawner = Arc::new(spawner);
        // The login's file is placed for the call and a login Pi renews meanwhile is told to the keeper.
        let hold = if local {
            drop(keeper);
            None
        } else {
            apply_to(grant, &mut launch, &spawner, (&self.accounts, Some(keeper)))?
        };
        let listing = listing(&launch, &ThroughSpawner::shared(spawner));
        if let Some(hold) = hold {
            hold.finish();
        }
        Ok(listing)
    }
}

/// The models Pi lists once started on `launch`, with no turn.
fn listing(launch: &PiLaunch, spawn: &ThroughSpawner) -> ModelListing {
    match PiSession::read_models(launch, spawn, PiSettings::new()) {
        Ok(models) if models.is_empty() => {
            ModelListing::unknown("Pi lists no model: none has a credential")
        }
        Ok(models) => ModelListing::of(
            models
                .into_iter()
                .map(|model| {
                    let mut info = ModelInfo::new(model.id);
                    info.name = Some(model.name);
                    info.efforts = model.efforts;
                    info.default_effort = model.default_effort;
                    info.is_default = model.is_default;
                    let mut about = vec![model.provider];
                    if model.reasoning {
                        about.push("reasoning".to_string());
                    }
                    if let Some(window) = model.context_window {
                        about.push(format!("{}K context", window / 1000));
                    }
                    info.description = Some(about.join(", "));
                    info
                })
                .collect(),
        ),
        Err(error) => ModelListing::unavailable(
            match &error {
                crate::Error::Spawn(why) if why.kind() == std::io::ErrorKind::NotFound => {
                    UnknownKind::NotInstalled
                }
                _ => UnknownKind::Failed,
            },
            format!("Pi could not be asked: {error}"),
        ),
    }
}
