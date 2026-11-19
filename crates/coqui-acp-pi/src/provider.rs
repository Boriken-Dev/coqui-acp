//! Pi as a provider of the `acp_harness` kind.
//!
//! Each connection serves a [`PiAgent`] over the channel, and the agent starts `pi --mode rpc` through the spawner the host
//! handed over, so Pi runs wherever the host's spawner runs it. The permission gate is a file written on this machine for
//! Pi to load, so a spawner that runs Pi elsewhere is refused.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::ffi::OsString;
use std::path::PathBuf;

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{FromValue, Provider, Schema, Secret, Status, ToValue};
use coqui_acp_api::{
    AccountCatalog, AccountGrant, AccountKeeper, AccountRef, AcpHarness, Channel, ChannelSink,
    Environment, ExportPage, HarnessInfo, ImportReport, Narrowing, ProcessSpawner, Requirement,
    SessionExport, SessionImport, StartRequest,
};
use coqui_acp_harness::Settings;
use coqui_acp_harness::account::{Accounts, Files, Hold, Watched, same_name};
use coqui_acp_harness::acp::Harness;
use coqui_acp_harness::provider::{Serving, shared, takes_of};
use coqui_acp_harness::stdio::accounts::{Applied, apply, known, login_usable};
use coqui_acp_harness::{Access, Mode};

use crate::accounts::{self, DEF};
use crate::{PiAgent, PiLaunch};

/// How the Pi provider is configured.
#[derive(ToValue, FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
pub struct ProviderSettings {
    /// The Pi program: a path, or a name looked up on the `PATH` where the spawner runs it. On Windows npm's shim is
    /// `pi.cmd`.
    #[map(default = String::from("pi"))]
    pub executable: String,
    /// A Node executable, an absolute path, that runs Pi's CLI `script` instead of an `executable` looked up on the `PATH`
    /// (a private Node and Pi's `dist/bundle/cli.js`): nothing is looked up and no `.cmd` shim is run. Set with `script`,
    /// and not with an `executable` other than `pi`; neither may lie in a session's directories.
    pub node: Option<String>,
    /// Pi's CLI script, an absolute path, which `node` runs.
    pub script: Option<String>,
    /// The model every session starts on, as `provider/id`; Pi's own when absent.
    pub model: Option<String>,
    /// The thinking level every session starts at; the model's own when absent.
    pub effort: Option<String>,
    /// The mode every session starts in, and the loosest a client may choose unless `modes` names more: `manual` when
    /// absent.
    pub mode: Option<Mode>,
    /// The access every session starts with when `mode` is absent, in the words every harness shares: a read-only sandbox
    /// starts in Plan, else the approvals decide the mode. Pi has no sandbox and no network setting.
    pub access: Option<Access>,
    /// Whether a client may give a session its own access through `_meta` (`dev.boriken.coqui.acp/access`).
    #[map(default = false)]
    pub allow_client_access: bool,
    /// Whether the Bypass mode may be chosen: nothing is asked.
    #[map(default = false)]
    pub allow_bypass: bool,
    /// The modes a client may choose, written by id (`manual`, `accept_edits`, `plan`, `bypass`): these widen the mode and
    /// every stricter one. Bypass is offered only with `allow_bypass`, and `auto` is never offered. Absent, a client
    /// chooses the starting mode and every stricter one.
    pub modes: Option<Vec<Mode>>,
    /// Whether Manual mode asks even before a tool that only reads.
    #[map(default = false)]
    pub ask_for_reads: bool,
    /// Whether Pi loads only the permission gate: none of the person's own extensions and no MCP, which could change what a
    /// call runs after the gate approved it.
    #[map(default = false)]
    pub isolated: bool,
    /// The protected paths, in gitignore syntax (`!` allows, a later line wins, `~/` is the home directory): what the model
    /// may not read or change without leave. Absent takes this crate's recommendations (`coqui_acp_pi::protect::RECOMMENDED`);
    /// a list written here replaces them, so copy the ones to keep.
    pub protected: Option<Vec<String>>,
    /// Whether a session's working directory may hold the places credentials are kept in (a home directory with its `.ssh`):
    /// a search from there reads them, so a session is refused otherwise.
    #[map(default = false)]
    pub allow_credential_roots: bool,
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
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            executable: "pi".into(),
            node: None,
            script: None,
            model: None,
            effort: None,
            mode: None,
            access: None,
            allow_client_access: false,
            allow_bypass: false,
            modes: None,
            ask_for_reads: false,
            isolated: false,
            protected: None,
            allow_credential_roots: false,
            max_acp_version: 2,
            max_sessions: 8,
            max_requests: 32,
            max_message_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Pi, as a provider of `acp_harness` under the id `pi`.
///
/// Register it beside a host's own providers with `Registry::register_local`, or build it into a library with
/// `guatiao::providers!`.
#[derive(Provider, Debug)]
#[provider(
    AcpHarness,
    id = "pi",
    config = ProviderSettings,
    new = PiProvider::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
pub struct PiProvider {
    settings: ProviderSettings,
    /// What the provider knows of the files it placed for its accounts.
    accounts: Arc<Accounts>,
}

impl From<ProviderSettings> for PiProvider {
    fn from(settings: ProviderSettings) -> Self {
        Self::new(settings)
    }
}

fn unsupported(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_UNSUPPORTED, why)
}

/// The reason for the first requirement Pi cannot be held to; none is held. Pi has no sandbox and no read-only mode, a
/// session's mode is the gate's and not a requirement's, and the person's own extensions, skills and MCP servers load unless
/// the provider is `isolated`; the project's own resources never load (`--no-approve`).
fn narrow(requirements: &[Requirement]) -> Result<Option<String>, ProviderError> {
    let narrowings = match Narrowing::read_all(requirements) {
        Ok(narrowings) => narrowings,
        Err(error) if error.status() == Status::GUATIAO_ERR_UNSUPPORTED => {
            return Ok(Some(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    Ok(narrowings.into_iter().next().map(|narrowing| {
        let name = Requirement::from(narrowing).name;
        format!("Pi cannot be held to `{name}`")
    }))
}

impl PiProvider {
    /// A provider configured as `settings` say.
    #[must_use]
    pub fn new(settings: ProviderSettings) -> Self {
        Self {
            settings,
            accounts: Arc::new(Accounts::new()),
        }
    }

    fn unconfigured() -> Self {
        Self::new(ProviderSettings::default())
    }

    /// What a session starts with: the settings and the host's environment.
    fn launch(&self, environment: &Environment) -> PiLaunch {
        let mut launch = PiLaunch::new(".");
        launch.executable = OsString::from(&self.settings.executable);
        launch.node = self.settings.node.as_ref().map(PathBuf::from);
        launch.script = self.settings.script.as_ref().map(PathBuf::from);
        launch.model.clone_from(&self.settings.model);
        launch.effort.clone_from(&self.settings.effort);
        launch.mode = self.settings.mode;
        launch.access = self.settings.access;
        launch.allow_bypass = self.settings.allow_bypass;
        launch.modes.clone_from(&self.settings.modes);
        launch.ask_for_reads = self.settings.ask_for_reads;
        launch.isolated = self.settings.isolated;
        launch.allow_credential_roots = self.settings.allow_credential_roots;
        launch.protected.clone_from(&self.settings.protected);
        for (name, value) in environment.iter() {
            launch.env.insert(name.into(), value.into());
        }
        launch
    }

    /// Takes the hold on the account's login file for a call or a connection, when the kind has one: the grant's copy is
    /// placed there. What happens to the file afterwards is for the account provider's watch to tell.
    fn hold(
        grant: &AccountGrant,
        applied: &Applied,
        spawner: &Arc<Object<dyn ProcessSpawner>>,
        (accounts, keeper): (&Arc<Accounts>, Option<Object<dyn AccountKeeper>>),
    ) -> Result<Option<Hold>, ProviderError> {
        let files = Files::new(Arc::clone(spawner));
        if applied.needs_files && !files.available() {
            return Err(unsupported(&format!(
                "this spawner has no private files, which a {} account needs",
                grant.kind
            )));
        }
        let (Some(watched), Some(dir)) = (applied.watched, grant.state_dir.as_deref()) else {
            return Ok(None);
        };
        accounts
            .hold(
                files,
                keeper,
                &grant.id,
                dir,
                grant.revision,
                grant.credentials.expose(),
                vec![Watched::new(watched.path, watched.key).usable(login_usable)],
            )
            .map(Some)
    }

    /// Applies `account` to `launch`: every credential variable of Pi removed, the kind's set, and the login file placed in
    /// the account's directory through the spawner and watched.
    ///
    /// # Errors
    ///
    /// `BAD_VALUE` for a grant the kind refuses; `UNSUPPORTED` for a kind that needs files from a spawner with none.
    fn apply(
        &self,
        account: Option<&AccountGrant>,
        spawner: &Arc<Object<dyn ProcessSpawner>>,
        launch: &mut PiLaunch,
    ) -> Result<Option<Hold>, ProviderError> {
        let Some(grant) = account else {
            return Ok(None);
        };
        apply_to(grant, launch, spawner, (&self.accounts, None))
    }

    /// Whether the spawner starts programs on this machine, where the permission gate is written.
    fn local(spawner: &Object<dyn ProcessSpawner>) -> Result<(), ProviderError> {
        if spawner.describe()?.local {
            Ok(())
        } else {
            Err(unsupported(
                "Pi is started with a permission gate written on this machine, and this spawner runs programs elsewhere",
            ))
        }
    }
}

/// Applies `grant` to `launch`: every credential variable of Pi removed, the kind's set, and the login file placed in the
/// account's directory through the spawner and held in `accounts`, whose keeper (when one is given) hears what happens to
/// it for as long as the hold lasts.
///
/// # Errors
///
/// `BAD_VALUE` for a grant the kind refuses; `UNSUPPORTED` for a kind that needs files from a spawner with none.
pub(crate) fn apply_to(
    grant: &AccountGrant,
    launch: &mut PiLaunch,
    spawner: &Arc<Object<dyn ProcessSpawner>>,
    keeping: (&Arc<Accounts>, Option<Object<dyn AccountKeeper>>),
) -> Result<Option<Hold>, ProviderError> {
    let Some(applied) = apply(&DEF, grant)? else {
        return Ok(None);
    };
    // What the host's environment holds under a credential name does not reach Pi beside the account's.
    launch.env.retain(|name, _| {
        !DEF.variables
            .iter()
            .any(|variable| same_name(&name.to_string_lossy(), variable))
    });
    launch.remove_env = DEF.variables.iter().map(OsString::from).collect();
    for (name, value) in applied.environment.expose().iter() {
        launch.env.insert(name.into(), value.into());
    }
    PiProvider::hold(grant, &applied, spawner, keeping)
}

impl AcpHarness for PiProvider {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        let agent = PiAgent::new();
        let versions = if self.settings.max_acp_version >= 2 {
            vec![1, 2]
        } else {
            vec![1]
        };
        let mut info = HarnessInfo::new("Pi", versions);
        info.takes = takes_of(&agent.takes());
        // A session's record is Pi's own, in its agent directory, not tied to what a connection was held to.
        info.resumes_wider = true;
        info.usage = agent.usage();
        info.cache_lifetime = Some(
            "ignored: this provider passes Pi no setting for how long its prompt cache lives"
                .to_string(),
        );
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
    ) -> Result<Object<dyn Channel>, ProviderError> {
        if let Some(why) = narrow(&request.requirements)? {
            return Err(unsupported(&why));
        }
        Self::local(&spawner)?;
        let mut launch = self.launch(request.environment.expose());
        let spawner = Arc::new(spawner);
        let hold = self.apply(request.account.as_ref(), &spawner, &mut launch)?;
        let mut settings = Settings::new();
        settings.max_sessions = usize::try_from(self.settings.max_sessions).unwrap_or(usize::MAX);
        settings.max_requests = usize::try_from(self.settings.max_requests).unwrap_or(usize::MAX);
        settings.max_version = if self.settings.max_acp_version >= 2 {
            2
        } else {
            1
        };
        // Pi reads its MCP servers from its own configuration: a session that brings some is refused by the agent.
        settings.working_roots = request.working_roots.iter().map(PathBuf::from).collect();
        settings
            .account
            .clone_from(&request.account.as_ref().map(|account| account.id.clone()));
        let limit = usize::try_from(self.settings.max_message_bytes).unwrap_or(usize::MAX);
        let mut serving = Serving::new(settings, limit);
        if let Some(hold) = hold {
            serving = serving.ending(move || hold.finish());
        }
        let client_access = self.settings.allow_client_access;
        serving.open_shared(spawner, sink, move |spawn| {
            Ok(shared(
                PiAgent::new()
                    .client_access(client_access)
                    .defaults(launch)
                    .spawn(spawn),
            ))
        })
    }

    fn refusal(
        &self,
        requirements: Vec<Requirement>,
        _environment: Secret<Environment>,
        account: Option<AccountRef>,
    ) -> Result<Option<String>, ProviderError> {
        if let Some(why) = narrow(&requirements)? {
            return Ok(Some(why));
        }
        if let Some(account) = &account {
            known(&DEF, account)?;
        }
        Ok(None)
    }

    fn program(&self) -> Result<Option<String>, ProviderError> {
        Ok(Some(
            self.settings
                .node
                .clone()
                .unwrap_or_else(|| self.settings.executable.clone()),
        ))
    }

    fn import_session(
        &self,
        session: SessionImport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ImportReport, ProviderError> {
        if let Some(account) = &session.account {
            known(&DEF, &account.reference())?;
        }
        #[cfg(feature = "history")]
        {
            crate::provider_history::import(&session, &spawner)
        }
        #[cfg(not(feature = "history"))]
        {
            drop(spawner);
            Err(unsupported(
                "this provider does not import a session into Pi yet",
            ))
        }
    }

    fn export_session(
        &self,
        export: SessionExport,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<ExportPage, ProviderError> {
        if let Some(account) = &export.account {
            known(&DEF, &account.reference())?;
        }
        #[cfg(feature = "history")]
        {
            crate::provider_history::export(&export, &spawner)
        }
        #[cfg(not(feature = "history"))]
        {
            drop(spawner);
            Err(unsupported(
                "this provider does not export a Pi session yet",
            ))
        }
    }
}
