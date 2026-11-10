//! Claude Code as a provider of the `acp_harness` kind.
//!
//! Each connection serves a [`ClaudeAgent`] over the channel, and the agent starts `claude` through the spawner the
//! host handed over, so Claude Code runs wherever the host's spawner runs it.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::ffi::OsString;
use std::path::PathBuf;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{FromValue, Provider, Schema, Secret, Status};
use coqui_acp_api::{
    AccountCatalog, AccountGrant, AccountKeeper, AccountRef, AcpHarness, CacheLifetime, Channel,
    ChannelSink, Environment, ExportPage, HarnessInfo, ImportReport, LoginOutcome, LoginPrompter,
    LoginRequest, ModelInfo, ModelListing, ModelsRequest, Narrowing, ProcessSpawner, RenewReport,
    RenewRequest, Requirement, SessionExport, SessionImport, StartRequest, UsageQuery, UsageReport,
};
use coqui_acp_harness::acp::Harness;
use coqui_acp_harness::provider::{Reports, Serving, account_refusal, shared, takes_of};
use coqui_acp_harness::{Settings, ThroughSpawner};

use crate::accounts;
use crate::{ClaudeAgent, ClaudeLaunch, ClaudeSession, ClaudeSettings};

/// What a connection with `cache_lifetime` short sets `CLAUDE_CODE_PROMPT_CACHE_TTL` and its sub-agent twin to.
const SHORT_TTL: &str = "5m";
/// The same for `long`.
const LONG_TTL: &str = "1h";
/// Claude Code's variables for how long its prompt cache lives, for the conversation and for its sub-agents.
const TTL_VARIABLES: [&str; 2] = [
    "CLAUDE_CODE_PROMPT_CACHE_TTL",
    "CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL",
];

/// How the Claude Code provider is configured.
#[derive(FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct ProviderSettings {
    /// The Claude Code program: a path, or a name looked up on the `PATH` where the spawner runs it.
    #[map(default = String::from("claude"))]
    pub executable: String,
    /// The model every session starts on; Claude Code's own when absent.
    pub model: Option<String>,
    /// The effort every session starts at (`low`, `medium`, `high`, `xhigh`, `max`); Claude Code's own when absent.
    pub effort: Option<String>,
    /// The permission mode every session starts in, and the loosest a client may choose: `default`, `acceptEdits`,
    /// `auto`, `plan` or `bypassPermissions`. Claude Code's own default, which is `default`, when absent.
    pub permission_mode: Option<String>,
    /// Whether the Bypass mode may be chosen: Claude Code started with `--allow-dangerously-skip-permissions`.
    #[map(default = false)]
    pub allow_bypass: bool,
    /// The highest ACP version `initialize` answers: 1 or 2.
    #[map(default = 2)]
    pub max_acp_version: u64,
    /// Sessions one connection may hold.
    #[map(default = 8)]
    pub max_sessions: u64,
    /// Requests one connection works on at once; a request past it is refused, and a client that sends faster than the
    /// answers are taken is held back.
    #[map(default = 32)]
    pub max_requests: u64,
    /// The largest message a connection takes, in bytes; one more ends it.
    #[map(default = 67_108_864)]
    pub max_message_bytes: u64,
    /// Whether a client may load plugins through `_meta.claudeCode.options.plugins`. Their hooks and MCP servers run
    /// with no permission asked.
    #[map(default = false)]
    pub allow_client_plugins: bool,
    /// Whether a client may give a session MCP servers, which start programs.
    #[map(default = true)]
    pub client_mcp_servers: bool,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            executable: "claude".into(),
            model: None,
            effort: None,
            permission_mode: None,
            allow_bypass: false,
            max_acp_version: 2,
            max_sessions: 8,
            max_requests: 32,
            max_message_bytes: 64 * 1024 * 1024,
            allow_client_plugins: false,
            client_mcp_servers: true,
        }
    }
}

/// Claude Code, as a provider of `acp_harness` under the id `claude`.
///
/// Register it beside a host's own providers with `Registry::register_local`, or build it into a library with
/// `guatiao::providers!`.
#[derive(Provider, Debug)]
#[provider(
    AcpHarness,
    id = "claude",
    config = ProviderSettings,
    new = ClaudeProvider::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
pub struct ClaudeProvider {
    settings: ProviderSettings,
    /// What the connections learned of each account's usage.
    reports: Arc<Reports>,
}

impl From<ProviderSettings> for ClaudeProvider {
    fn from(settings: ProviderSettings) -> Self {
        Self::new(settings)
    }
}

impl ClaudeProvider {
    /// A provider configured as `settings` say.
    #[must_use]
    pub fn new(settings: ProviderSettings) -> Self {
        Self {
            settings,
            reports: Arc::new(Reports::new()),
        }
    }

    fn unconfigured() -> Self {
        Self::new(ProviderSettings::default())
    }

    /// What a session starts with: the settings, the host's environment, the cache hint and the requirements.
    ///
    /// # Errors
    ///
    /// A requirement the kind does not read.
    fn launch(
        &self,
        environment: &Environment,
        cache_lifetime: CacheLifetime,
        requirements: &[Requirement],
    ) -> Result<(ClaudeLaunch, Option<String>), ProviderError> {
        let mut launch = ClaudeLaunch::new(".");
        launch.executable = OsString::from(&self.settings.executable);
        launch.model.clone_from(&self.settings.model);
        launch.effort.clone_from(&self.settings.effort);
        launch
            .permission_mode
            .clone_from(&self.settings.permission_mode);
        launch.allow_bypass = self.settings.allow_bypass;
        for (name, value) in environment.iter() {
            launch.env.insert(name.into(), value.into());
        }
        let ttl = match cache_lifetime {
            CacheLifetime::Short => Some(SHORT_TTL),
            CacheLifetime::Long => Some(LONG_TTL),
            _ => None,
        };
        if let Some(ttl) = ttl {
            for name in TTL_VARIABLES {
                launch.env.insert(name.into(), ttl.into());
            }
        }
        let refusal = narrow(requirements, &mut launch)?;
        Ok((launch, refusal))
    }

    fn support(&self) -> coqui_acp_api::UsageSupport {
        ClaudeAgent::new().usage()
    }
}

/// Holds the requirements a launch can, changing it to; the reason for the first it cannot.
///
/// `no_project_mcp` is `--strict-mcp-config`, which starts the MCP servers `--mcp-config` names and no others, so also
/// none the user's own configuration declares. No other name is held: Claude Code's flags do not leave the instruction
/// files loaded while they drop a directory's settings, and what the rest take away is not measured.
fn narrow(
    requirements: &[Requirement],
    launch: &mut ClaudeLaunch,
) -> Result<Option<String>, ProviderError> {
    let narrowings = match Narrowing::read_all(requirements) {
        Ok(narrowings) => narrowings,
        Err(error) if error.status() == Status::GUATIAO_ERR_UNSUPPORTED => {
            return Ok(Some(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    for narrowing in narrowings {
        match narrowing {
            Narrowing::NoProjectMcp => launch.strict_mcp_config = true,
            other => {
                let name = Requirement::from(other).name;
                return Ok(Some(format!("Claude Code cannot be held to `{name}`")));
            }
        }
    }
    Ok(None)
}

fn unsupported(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_UNSUPPORTED, why)
}

/// Why a call that applies `account` cannot: `None` when it can.
fn account_check(account: Option<&AccountRef>) -> Result<Option<String>, ProviderError> {
    match account {
        Some(account) => account_refusal(&accounts::catalog(), account),
        None => Ok(None),
    }
}

impl AcpHarness for ClaudeProvider {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        let agent = ClaudeAgent::new();
        let versions = if self.settings.max_acp_version >= 2 {
            vec![1, 2]
        } else {
            vec![1]
        };
        let mut info = HarnessInfo::new("Claude Code", versions);
        info.takes = takes_of(&agent.takes());
        if !self.settings.client_mcp_servers {
            info.takes.mcp_http = false;
            info.takes.mcp_sse = false;
        }
        // A session's record is the harness's own, not tied to what a connection was held to.
        info.resumes_wider = true;
        info.usage = agent.usage();
        info.cache_lifetime = Some(format!(
            "sets {} and {} to {SHORT_TTL} for short and {LONG_TTL} for long, and leaves both as the environment has them for default",
            TTL_VARIABLES[0], TTL_VARIABLES[1]
        ));
        Ok(info)
    }

    fn accounts(&self) -> Result<AccountCatalog, ProviderError> {
        Ok(accounts::catalog())
    }

    fn start(
        &self,
        request: StartRequest,
        spawner: Object<dyn ProcessSpawner>,
        sink: Object<dyn ChannelSink>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn Channel>, ProviderError> {
        drop(keeper);
        let (launch, refusal) = self.launch(
            request.environment.expose(),
            request.cache_lifetime,
            &request.requirements,
        )?;
        if let Some(why) = refusal {
            return Err(unsupported(&why));
        }
        let account = request.account.as_ref().map(AccountGrant::reference);
        if let Some(why) = account_check(account.as_ref())? {
            return Err(unsupported(&why));
        }
        let account_id = account.map(|account| account.id);
        let mut settings = Settings::new();
        settings.max_sessions = usize::try_from(self.settings.max_sessions).unwrap_or(usize::MAX);
        settings.max_requests = usize::try_from(self.settings.max_requests).unwrap_or(usize::MAX);
        settings.max_version = if self.settings.max_acp_version >= 2 {
            2
        } else {
            1
        };
        settings.client_mcp_servers = self.settings.client_mcp_servers;
        settings.working_roots = request.working_roots.iter().map(PathBuf::from).collect();
        settings.account.clone_from(&account_id);
        let reports = Arc::clone(&self.reports);
        let support = self.support();
        settings.on_usage = Some(Arc::new(move |report: &UsageReport| {
            reports.observe(&support, account_id.as_deref(), report);
        }));
        let limit = usize::try_from(self.settings.max_message_bytes).unwrap_or(usize::MAX);
        let plugins = self.settings.allow_client_plugins;
        Serving::new(settings, limit).open(spawner, sink, move |spawn| {
            Ok(shared(
                ClaudeAgent::new()
                    .defaults(launch)
                    .spawn(spawn)
                    .client_plugins(plugins),
            ))
        })
    }

    fn refusal(
        &self,
        requirements: Vec<Requirement>,
        _environment: Secret<Environment>,
        account: Option<AccountRef>,
    ) -> Result<Option<String>, ProviderError> {
        let mut launch = ClaudeLaunch::new(".");
        if let Some(why) = narrow(&requirements, &mut launch)? {
            return Ok(Some(why));
        }
        account_check(account.as_ref())
    }

    fn program(&self) -> Result<Option<String>, ProviderError> {
        Ok(Some(self.settings.executable.clone()))
    }

    fn import_session(
        &self,
        session: SessionImport,
        _spawner: Object<dyn ProcessSpawner>,
        _keeper: Object<dyn AccountKeeper>,
    ) -> Result<ImportReport, ProviderError> {
        let _ = session;
        Err(unsupported(
            "this provider does not import a session into Claude Code yet",
        ))
    }

    fn export_session(
        &self,
        export: SessionExport,
        _spawner: Object<dyn ProcessSpawner>,
        _keeper: Object<dyn AccountKeeper>,
    ) -> Result<ExportPage, ProviderError> {
        let _ = export;
        Err(unsupported(
            "this provider does not export a Claude Code session yet",
        ))
    }

    fn models(
        &self,
        request: ModelsRequest,
        spawner: Object<dyn ProcessSpawner>,
        _keeper: Object<dyn AccountKeeper>,
    ) -> Result<ModelListing, ProviderError> {
        if let Some(why) = account_check(
            request
                .account
                .as_ref()
                .map(AccountGrant::reference)
                .as_ref(),
        )? {
            return Err(unsupported(&why));
        }
        let (launch, _) = self.launch(request.environment.expose(), CacheLifetime::Default, &[])?;
        let spawn = ThroughSpawner::new(spawner);
        let session = match ClaudeSession::start(&launch, &spawn, ClaudeSettings::new(), |_| true) {
            Ok(session) => session,
            Err(error) => {
                return Ok(ModelListing::unknown(format!(
                    "Claude Code could not be asked: {error}"
                )));
            }
        };
        let listing = session.models().map_or_else(
            || ModelListing::unknown("Claude Code listed no model"),
            |models| {
                ModelListing::of(
                    models
                        .into_iter()
                        .map(|model| {
                            let mut info = ModelInfo::new(model.id);
                            info.name = Some(model.name);
                            info.description = model.description;
                            info
                        })
                        .collect(),
                )
            },
        );
        session.close();
        Ok(listing)
    }

    fn usage(
        &self,
        query: UsageQuery,
        _spawner: Object<dyn ProcessSpawner>,
        _keeper: Object<dyn AccountKeeper>,
    ) -> Result<UsageReport, ProviderError> {
        // Claude Code learns usage only during a turn: this is what the connections last saw.
        let account = query.account.as_ref().map(AccountGrant::reference);
        if let Some(why) = account_check(account.as_ref())? {
            return Err(unsupported(&why));
        }
        Ok(self
            .reports
            .report(&self.support(), account.as_ref().map(|a| a.id.as_str())))
    }

    fn login(
        &self,
        _request: LoginRequest,
        _spawner: Object<dyn ProcessSpawner>,
        _prompter: Object<dyn LoginPrompter>,
    ) -> Result<LoginOutcome, ProviderError> {
        Err(unsupported(
            "this provider offers no way to sign in to Claude Code yet",
        ))
    }

    fn renew(
        &self,
        request: RenewRequest,
        _spawner: Object<dyn ProcessSpawner>,
    ) -> Result<RenewReport, ProviderError> {
        account_check(Some(&request.account.reference()))?;
        Err(unsupported("this provider renews no Claude Code login yet"))
    }
}
