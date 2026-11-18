//! OpenCode as a provider of its own: the stdio provider and its account provider with the harness fixed, under the ids
//! `opencode` and `opencode_accounts`, so a host that runs several agents through `stdio` can still say which one an entry
//! and its accounts are. Reading a session out of OpenCode (`opencode export`) is here too.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;
use std::ffi::OsString;

use coqui_acp_api::guatiao::Secret;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{FromValue, Provider, Schema, Status, ToValue};
use coqui_acp_api::{
    AccountCatalog, AccountKeeper, AccountOffer, AccountProvider, AccountProviderInfo, AccountRef,
    AccountUsage, AccountUsageQuery, AccountWatch, AcpHarness, Channel, ChannelSink, Environment,
    ExportPage, HarnessInfo, ImportReport, LINE_TOO_LARGE, LoginOutcome, LoginPrompter,
    LoginRequest, ModelListing, ModelsRequest, ProcessSpawner, RenewReport, RenewRequest,
    Requirement, SessionExport, SessionImport, StartRequest, UsageQuery, UsageReport, WatchRequest,
};
use coqui_acp_history::paging::{PageError, page};

use super::accounts;
use super::{
    StdioAccountSettings, StdioAccounts, StdioHarness, StdioProvider, StdioSettings,
    opencode_history,
};
use crate::ThroughSpawner;
use crate::cli;
use crate::process::Launch;

/// The longest `opencode export` may run, and the most it may print.
const EXPORT_LIMIT: Duration = Duration::from_secs(60);
const EXPORT_MAX_BYTES: usize = 64 << 20;

/// How OpenCode's provider is configured: [`StdioSettings`] with the harness fixed and the ACP version OpenCode speaks.
#[derive(ToValue, FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct OpenCodeSettings {
    /// The program: a path, or a name looked up on the `PATH` where the spawner runs it. `opencode` when absent.
    pub executable: Option<String>,
    /// Requests one connection may have in hand at once: a client that sends more is held back until the sink has taken
    /// answers.
    #[map(default = 32)]
    pub max_requests: u64,
    /// The largest message a connection takes, in bytes; one more ends it.
    #[map(default = 67_108_864)]
    pub max_message_bytes: u64,
    /// How many bytes of an imported history the first prompt of a session carries, as context the agent reads. 0, the
    /// default, imports nothing: OpenCode's store is not written by the provider, and a replay sends the history to the
    /// agent's model, so it is a choice.
    #[map(default = 0)]
    pub replay_max_bytes: u64,
    /// The most bytes of one message, tool input or tool output in a replay; one over it is cut and reported.
    #[map(default = 4096)]
    pub replay_max_item_bytes: u64,
    /// Regular expressions whose matches are removed from the history before it is replayed.
    #[map(default)]
    pub replay_redact_patterns: Vec<String>,
    /// How long an imported history waits for a connection to open its session before it is forgotten, in seconds.
    #[map(default = 3600)]
    pub replay_waiting_secs: u64,
    /// How many session ids the agent reported are remembered, so that no import takes one.
    #[map(default = 4096)]
    pub replay_remembered_ids: u64,
    /// How many imported histories may wait to be opened at once.
    #[map(default = 64)]
    pub replay_waiting_max: u64,
}

impl OpenCodeSettings {
    /// The defaults.
    #[must_use]
    pub fn new() -> Self {
        let own = StdioSettings::new(StdioHarness::OpenCode);
        Self {
            executable: None,
            max_requests: own.max_requests,
            max_message_bytes: own.max_message_bytes,
            replay_max_bytes: own.replay_max_bytes,
            replay_max_item_bytes: own.replay_max_item_bytes,
            replay_redact_patterns: Vec::new(),
            replay_waiting_secs: own.replay_waiting_secs,
            replay_remembered_ids: own.replay_remembered_ids,
            replay_waiting_max: own.replay_waiting_max,
        }
    }
}

impl Default for OpenCodeSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl From<OpenCodeSettings> for StdioSettings {
    fn from(settings: OpenCodeSettings) -> Self {
        let mut own = Self::new(StdioHarness::OpenCode);
        own.executable = settings.executable;
        own.max_requests = settings.max_requests;
        own.max_message_bytes = settings.max_message_bytes;
        own.replay_max_bytes = settings.replay_max_bytes;
        own.replay_max_item_bytes = settings.replay_max_item_bytes;
        own.replay_redact_patterns = settings.replay_redact_patterns;
        own.replay_waiting_secs = settings.replay_waiting_secs;
        own.replay_remembered_ids = settings.replay_remembered_ids;
        own.replay_waiting_max = settings.replay_waiting_max;
        own
    }
}

/// OpenCode, as a provider of `acp_harness` under the id `opencode`: `opencode acp` through the stdio provider.
#[derive(Provider, Debug)]
#[provider(
    AcpHarness,
    id = "opencode",
    config = OpenCodeSettings,
    new = OpenCodeProvider::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
pub struct OpenCodeProvider {
    inner: StdioProvider,
}

impl From<OpenCodeSettings> for OpenCodeProvider {
    fn from(settings: OpenCodeSettings) -> Self {
        Self {
            inner: StdioProvider::new(settings.into()),
        }
    }
}

impl OpenCodeProvider {
    /// A provider configured as `settings` say.
    #[must_use]
    pub fn new(settings: OpenCodeSettings) -> Self {
        settings.into()
    }

    fn unconfigured() -> Self {
        Self::new(OpenCodeSettings::new())
    }
}

impl AcpHarness for OpenCodeProvider {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        self.inner.describe()
    }

    fn accounts(&self) -> Result<AccountCatalog, ProviderError> {
        self.inner.accounts()
    }

    fn start(
        &self,
        request: StartRequest,
        spawner: Object<dyn ProcessSpawner>,
        sink: Object<dyn ChannelSink>,
    ) -> Result<Object<dyn Channel>, ProviderError> {
        self.inner.start(request, spawner, sink)
    }

    fn refusal(
        &self,
        requirements: Vec<Requirement>,
        environment: Secret<Environment>,
        account: Option<AccountRef>,
    ) -> Result<Option<String>, ProviderError> {
        self.inner.refusal(requirements, environment, account)
    }

    fn program(&self) -> Result<Option<String>, ProviderError> {
        self.inner.program()
    }

    fn import_session(
        &self,
        session: SessionImport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ImportReport, ProviderError> {
        self.inner.import_session(session, spawner)
    }

    fn export_session(
        &self,
        export: SessionExport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ExportPage, ProviderError> {
        self.inner.export_session(export, spawner)
    }

    fn models(
        &self,
        request: ModelsRequest,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ModelListing, ProviderError> {
        self.inner.models(request, spawner)
    }

    fn usage(
        &self,
        query: UsageQuery,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<UsageReport, ProviderError> {
        self.inner.usage(query, spawner)
    }
}

/// How OpenCode's account provider is configured.
#[derive(ToValue, FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct OpenCodeAccountSettings {
    /// The program it signs in and checks with: a path, or a name looked up on the `PATH` where the spawner runs it.
    /// `opencode` when absent.
    pub executable: Option<String>,
    /// How often, in milliseconds, a watch looks at the file an account keeps its sign-in in; 0 looks only when the watch
    /// begins and ends, and when a call is made on the account.
    #[map(default = 30_000)]
    pub renew_poll_ms: u64,
    /// The longest, in milliseconds, a command of the harness's own may run; 0 ends it at once.
    #[map(default = 60_000)]
    pub command_timeout_ms: u64,
}

/// OpenCode's accounts, as a provider of `account_provider` under the id `opencode_accounts`: the sign-in kept in
/// `auth.json` and a key for one provider.
#[derive(Provider, Debug)]
#[provider(
    AccountProvider,
    id = "opencode_accounts",
    config = OpenCodeAccountSettings,
    new = OpenCodeAccounts::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
pub struct OpenCodeAccounts {
    inner: StdioAccounts,
}

impl From<OpenCodeAccountSettings> for OpenCodeAccounts {
    fn from(settings: OpenCodeAccountSettings) -> Self {
        let mut own = StdioAccountSettings::new(StdioHarness::OpenCode);
        own.executable = settings.executable;
        own.renew_poll_ms = settings.renew_poll_ms;
        own.command_timeout_ms = settings.command_timeout_ms;
        Self {
            inner: StdioAccounts::new(own),
        }
    }
}

impl OpenCodeAccounts {
    /// A provider configured as `settings` say.
    #[must_use]
    pub fn new(settings: OpenCodeAccountSettings) -> Self {
        settings.into()
    }

    fn unconfigured() -> Self {
        Self::new(OpenCodeAccountSettings {
            executable: None,
            renew_poll_ms: 30_000,
            command_timeout_ms: 60_000,
        })
    }
}

impl AccountProvider for OpenCodeAccounts {
    fn describe(&self) -> Result<AccountProviderInfo, ProviderError> {
        self.inner.describe()
    }

    fn offers(&self) -> Result<AccountOffer, ProviderError> {
        self.inner.offers()
    }

    fn login(
        &self,
        request: LoginRequest,
        spawner: Object<dyn ProcessSpawner>,
        prompter: Object<dyn LoginPrompter>,
    ) -> Result<LoginOutcome, ProviderError> {
        self.inner.login(request, spawner, prompter)
    }

    fn renew(
        &self,
        request: RenewRequest,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<RenewReport, ProviderError> {
        self.inner.renew(request, spawner)
    }

    fn watch(
        &self,
        request: WatchRequest,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn AccountWatch>, ProviderError> {
        self.inner.watch(request, spawner, keeper)
    }

    fn usage(
        &self,
        query: AccountUsageQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<AccountUsage, ProviderError> {
        self.inner.usage(query, spawner, keeper)
    }
}

fn error(status: Status, why: &str) -> ProviderError {
    ProviderError::new(status, why)
}

impl StdioProvider {
    /// A session of OpenCode's as a history: `opencode export --pure <id>` on the account's data folder (or the
    /// environment's, with no account), through the spawner, then paged.
    pub(super) fn export_opencode(
        &self,
        export: &SessionExport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ExportPage, ProviderError> {
        let def = self.def();
        if !opencode_history::is_native_id(&export.session_id) {
            return Err(error(
                Status::GUATIAO_ERR_BAD_VALUE,
                "the session id is not one OpenCode made (`ses_` and letters and digits)",
            ));
        }
        let applied = match &export.account {
            Some(grant) => accounts::apply(def, grant)?,
            None => None,
        };
        let mut launch =
            Launch::new(self.program_name()).args(["export", "--pure", &export.session_id]);
        for (name, value) in export.environment.expose().iter() {
            let credential = def
                .variables
                .iter()
                .any(|variable| accounts::same_name(variable, name));
            if applied.is_none() || !credential {
                launch.env.insert(name.into(), value.into());
            }
        }
        if let Some(applied) = &applied {
            // Only the account's data folder: the export reads sessions and needs no credential.
            for (name, value) in applied.environment.expose().iter() {
                if accounts::same_name(name, "XDG_DATA_HOME") {
                    launch.env.insert(name.into(), value.into());
                }
            }
            launch.remove_env = def.variables.iter().map(OsString::from).collect();
            launch
                .remove_env
                .retain(|name| !accounts::same_name(&name.to_string_lossy(), "XDG_DATA_HOME"));
        }
        let done = cli::run(
            &ThroughSpawner::new(spawner),
            &launch,
            None,
            EXPORT_LIMIT,
            EXPORT_MAX_BYTES,
        )
        .map_err(|_| {
            error(
                Status::GUATIAO_ERR_INTERNAL,
                "the program could not be started to export the session",
            )
        })?;
        if done.timed_out {
            return Err(error(
                Status::GUATIAO_ERR_INTERNAL,
                "the program did not export the session in time",
            ));
        }
        if !done.succeeded() {
            let said = cli::plain(&String::from_utf8_lossy(&done.stderr)).to_lowercase();
            return Err(if said.contains("not found") {
                error(Status::GUATIAO_ERR_NOT_FOUND, "no session has this id")
            } else {
                error(
                    Status::GUATIAO_ERR_INTERNAL,
                    "the program did not export the session",
                )
            });
        }
        let output = String::from_utf8_lossy(&done.stdout);
        let history = opencode_history::export(&output, &export.session_id)
            .map_err(|why| error(Status::GUATIAO_ERR_BAD_VALUE, &why))?
            .to_string()
            .map_err(|why| error(Status::GUATIAO_ERR_BAD_VALUE, &why.to_string()))?;
        match page(&history, export.after.as_deref(), export.max_bytes) {
            Ok(page) => Ok(ExportPage::new(page.text, page.next)),
            Err(PageError::LineTooLarge(size)) => Err(error(
                Status::GUATIAO_ERR_BAD_VALUE,
                &alloc::format!(
                    "{LINE_TOO_LARGE}{size} bytes, more than max_bytes {}",
                    export.max_bytes
                ),
            )),
            Err(other) => Err(error(Status::GUATIAO_ERR_BAD_VALUE, &other.to_string())),
        }
    }
}
