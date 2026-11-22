//! An `account_provider` library any Guatiao host can load, that keeps the accounts of an imaginary
//! service, the echo's: the Rust echo of `coqui_acp_api::contract::AccountContract`. This file is a
//! `cdylib` and nothing else, as a crate calling `providers!` has to be.
//!
//! It makes accounts of two kinds. `echo-login` is a login kept in `auth.json` of the account's
//! directory, which rotates, renews with no model turn, is watched for a change made behind the
//! host's back, and reports plan usage; it provides the standard `echo-login`. `echo-key` is a key
//! the person pastes, which provides `echo-api` and reports no usage. Its login methods are scripted
//! (`scripted`, `waiting`, `api-key`). Settings: `title`, `logins = false` (offer no method),
//! `usage = false` (report none) and `poll_ms`, how often a watch looks at the file.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{self, FromValue, Provider, Schema, Secret, Status};
use coqui_acp_api::usage::{Bucket, Coverage, Refill, Source, Status as Level, Unit};
use coqui_acp_api::{
    ASK, AccountGrant, AccountKeeper, AccountKindInfo, AccountLoss, AccountModelsQuery,
    AccountOffer, AccountProvider, AccountProviderInfo, AccountRef, AccountRenewal, AccountUsage,
    AccountUsageQuery, AccountWatch, Credentials, EndpointSlot, HARNESS_LOGIN, LoginForm,
    LoginMethodInfo, LoginOutcome, LoginPrompter, LoginRequest, MODEL_API, ModelInfo, ModelListing,
    OPEN_URL, OpenUrl, ProcessSpawner, RenewReport, RenewRequest, SecretBytes, SpawnSpec,
    SpawnedProcess, USER_CODE, WatchRequest, is_account_id,
};
use serde_json::{Value, json};

/// The threads this library started, joined when it is unloaded.
static THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// Remembers a thread of the library's, joining those that have finished.
fn track(thread: JoinHandle<()>) {
    let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
    let (done, running): (Vec<_>, Vec<_>) = threads.drain(..).partition(|t| t.is_finished());
    *threads = running;
    for finished in done {
        let _ = finished.join();
    }
    threads.push(thread);
}

/// The library's say in being unloaded: busy while a thread of its own still runs.
extern "C" fn threads_done() -> Status {
    let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
    if threads.iter().any(|t| !t.is_finished()) {
        return Status::GUATIAO_ERR_BUSY;
    }
    for thread in threads.drain(..) {
        let _ = thread.join();
    }
    Status::GUATIAO_OK
}

/// How an echo is configured.
#[derive(FromValue, Schema, Debug, Clone)]
#[map(crate = ::coqui_acp_api::guatiao)]
struct EchoSettings {
    /// Its name for people.
    #[map(default = String::from("Echo accounts"))]
    title: String,
    /// Whether it lists login methods; when not, it lists none and refuses `login`, saying why.
    #[map(default = true)]
    logins: bool,
    /// Whether it reports plan usage for its login kind.
    #[map(default = true)]
    usage: bool,
    /// How often a watch looks at the account's file, in milliseconds.
    #[map(default = 100)]
    poll_ms: u64,
}

#[derive(Provider)]
#[provider(
    AccountProvider,
    id = "echo_account_provider",
    config = EchoSettings,
    new = EchoAccounts::unconfigured,
    crate = ::coqui_acp_api::guatiao
)]
struct EchoAccounts {
    settings: EchoSettings,
    /// The files of the accounts it watches, and who is listening for each.
    accounts: Accounts,
    /// How many reports of usage it has made.
    revision: AtomicU64,
}

impl EchoAccounts {
    fn configured(settings: EchoSettings) -> Self {
        Self {
            settings,
            accounts: Arc::new(Registry::default()),
            revision: AtomicU64::new(0),
        }
    }

    fn unconfigured() -> Self {
        Self::configured(EchoSettings {
            title: "Echo accounts".into(),
            logins: true,
            usage: true,
            poll_ms: 100,
        })
    }
}

impl TryFrom<EchoSettings> for EchoAccounts {
    type Error = ProviderError;

    fn try_from(settings: EchoSettings) -> Result<Self, ProviderError> {
        Ok(Self::configured(settings))
    }
}

guatiao::providers!(
    id = "echo_account_provider",
    version = env!("CARGO_PKG_VERSION"),
    providers = [EchoAccounts],
    unload = threads_done
);

/// The kind with a file credential: the login the echo keeps in `auth.json` of the account's
/// directory, which rotates and which a watch reads.
const FILE_KIND: &str = "echo-login";
/// The kind with a credential in a variable.
const KEY_KIND: &str = "echo-key";
/// The one file of the file kind, relative to the account's directory.
const FILE: &str = "auth.json";
/// The credential key a file is held under.
const FILE_KEY: &str = "auth_file";
/// The most a credential file may be.
const FILE_MAX: u64 = 1 << 20;

fn schema(json: &Value) -> guatiao::Value {
    guatiao_serde::from_serde(json.clone()).expect("a schema is a value")
}

fn plain(value: &guatiao::Value) -> Value {
    serde_json::to_value(guatiao_serde::Serializable::from(value)).unwrap_or(Value::Null)
}

fn credential() -> Value {
    json!({"type": "string", "writeOnly": true, "x-sensitive": true})
}

fn bad(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_BAD_VALUE, why)
}

/// What the echo makes.
fn offer(settings: &EchoSettings) -> AccountOffer {
    let url = json!({"type": "string", "format": "uri"});
    let mut file = AccountKindInfo::new(FILE_KIND, "Echo login").provides(["echo-login"]);
    file.description = "A login the echo keeps in a file of its own directory.".into();
    file.settings = schema(&json!({
        "type": "object",
        "properties": {FILE_KEY: credential(), "base_url": url},
        "required": [FILE_KEY],
        "additionalProperties": false,
    }));
    file.endpoints = vec![
        EndpointSlot::new(MODEL_API)
            .key("base_url")
            .default_url("https://echo.example"),
    ];
    file.rotates = true;
    file.state_dir = true;
    file.renews = true;
    file.notices = vec!["the login file can be read by what the agent runs".into()];
    let mut key = AccountKindInfo::new(KEY_KIND, "Echo key").provides(["echo-api"]);
    key.description = "A key the echo takes in a variable.".into();
    key.settings = schema(&json!({
        "type": "object",
        "properties": {"api_key": credential(), "base_url": url},
        "required": ["api_key"],
        "additionalProperties": false,
    }));
    key.endpoints = vec![
        EndpointSlot::new(MODEL_API)
            .key("base_url")
            .default_url("https://echo.example"),
    ];
    key.history_portable = true;
    let mut offer = AccountOffer::new().with_local();
    offer.kinds = vec![file, key];
    if settings.usage {
        offer.usage = vec![FILE_KIND.into()];
    }
    if settings.logins {
        let mut scripted = LoginMethodInfo::new("scripted", "Scripted", FILE_KIND);
        scripted.steps = vec![ASK.into(), OPEN_URL.into(), USER_CODE.into()];
        scripted.remote = true;
        scripted.inputs = schema(&json!({
            "type": "object",
            "properties": {"email": {"type": "string", "default": "person@example.invalid"}},
        }));
        let mut waiting = LoginMethodInfo::new("waiting", "Waiting for a page", FILE_KIND);
        waiting.steps = vec![OPEN_URL.into()];
        let mut api_key = LoginMethodInfo::new("api-key", "A key", KEY_KIND);
        api_key.steps = vec![ASK.into()];
        offer.logins = vec![scripted, waiting, api_key];
    }
    offer
}

/// What the echo last saw of an account's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Present,
    Blank,
    Gone,
}

struct AccountFile {
    /// The last content it held, the one the host knows or the file's own since.
    last: String,
    seen: Seen,
    /// The revision of the grant the host holds.
    revision: u64,
    /// What has happened to the account, renewals and losses together: counted here, once.
    sequence: u64,
}

/// A keeper that is live for an account: its watch holds it, and lets go of the object before
/// `stop` answers.
struct Sub {
    keeper: Mutex<Option<Object<dyn AccountKeeper>>>,
}

impl Sub {
    fn of(keeper: Object<dyn AccountKeeper>) -> Arc<Self> {
        Arc::new(Self {
            keeper: Mutex::new(Some(keeper)),
        })
    }

    fn release(&self) {
        drop(
            self.keeper
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}

/// The files of the accounts an instance watches, and who is listening for each.
#[derive(Default)]
struct Registry {
    files: Mutex<BTreeMap<String, AccountFile>>,
    subs: Mutex<BTreeMap<String, Vec<Weak<Sub>>>>,
}

type Accounts = Arc<Registry>;

fn held(accounts: &Accounts) -> MutexGuard<'_, BTreeMap<String, AccountFile>> {
    accounts
        .files
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

enum Event {
    Renewed(AccountRenewal),
    Lost(AccountLoss),
}

/// Tells every keeper live for the account, not the first to ask.
fn tell(accounts: &Accounts, key: &str, event: &Event) {
    let live: Vec<Arc<Sub>> = {
        let mut subs = accounts.subs.lock().unwrap_or_else(PoisonError::into_inner);
        let list = subs.entry(key.to_owned()).or_default();
        list.retain(|w| w.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    };
    for sub in live {
        let keeper = sub.keeper.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(keeper) = keeper.as_ref() {
            let _ = match event {
                Event::Renewed(renewal) => keeper.renewed(renewal.clone()),
                Event::Lost(loss) => keeper.lost(loss.clone()),
            };
        }
    }
}

/// What the echo keeps an account's file under: the same account in another directory is another
/// file.
fn key_of(grant: &AccountGrant) -> String {
    format!(
        "{}\0{}",
        grant.id,
        grant.state_dir.as_deref().unwrap_or_default()
    )
}

/// Checks that a grant's id is one.
fn check_id(grant: &AccountGrant) -> Result<(), ProviderError> {
    if is_account_id(&grant.id) {
        Ok(())
    } else {
        Err(bad(
            "an account id is 1 to 128 bytes with no control character",
        ))
    }
}

/// Checks a grant of a kind the echo lists, and what it needs of `spawner`.
fn read_grant(
    offer: &AccountOffer,
    grant: &AccountGrant,
    spawner: &Object<dyn ProcessSpawner>,
) -> Result<(), ProviderError> {
    if !is_account_id(&grant.id) {
        return Err(bad(
            "an account id is 1 to 128 bytes with no control character",
        ));
    }
    if offer.kind(&grant.kind).is_none() {
        return Err(bad("the echo makes no account of that kind"));
    }
    let settings = plain(&grant.settings);
    let Some(entries) = settings.as_object() else {
        return Err(bad("the settings are not an object"));
    };
    for (key, value) in entries {
        let url = value.as_str().is_some_and(coqui_acp_api::is_http_url);
        if key != "base_url" || !url {
            return Err(bad("the settings do not fit the kind's schema"));
        }
    }
    let held = grant.credentials.expose();
    let wanted = if grant.kind == FILE_KIND {
        FILE_KEY
    } else {
        "api_key"
    };
    if held.len() != 1 || held.get(wanted).is_none_or(str::is_empty) {
        return Err(bad(
            "a credential is missing, or one the kind does not take was given",
        ));
    }
    if grant.kind == FILE_KIND {
        if grant.state_dir.is_none() {
            return Err(bad("this kind needs a directory and none was granted"));
        }
        if !spawner.describe()?.files {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this spawner has no private files, which an echo login needs",
            ));
        }
    }
    Ok(())
}

/// Looks at the account's file and tells every live keeper what happened to it behind the host's
/// back: a changed file is renewed, a blank or deleted one lost. What is seen is recorded under
/// the lock before anyone is told, so two watches never report one change twice.
fn look(
    accounts: &Accounts,
    key: &str,
    spawner: &Object<dyn ProcessSpawner>,
) -> Result<(), ProviderError> {
    let id = key.split('\0').next().unwrap_or(key).to_owned();
    let now = if spawner.file_info(FILE)?.is_none() {
        None
    } else {
        Some(spawner.read_file(FILE, FILE_MAX)?.expose().to_text())
    };
    let event = {
        let mut files = held(accounts);
        let Some(entry) = files.get_mut(key) else {
            return Ok(());
        };
        let loss = |entry: &mut AccountFile, seen: Seen, why: &str| {
            if entry.seen == seen {
                return None;
            }
            entry.seen = seen;
            entry.sequence += 1;
            Some(Event::Lost(AccountLoss::new(&id, entry.sequence, why)))
        };
        match now {
            None => loss(entry, Seen::Gone, "the login file is gone"),
            Some(text) if text.trim().is_empty() => {
                loss(entry, Seen::Blank, "the login file is blank")
            }
            Some(text) => {
                entry.seen = Seen::Present;
                if text == entry.last {
                    None
                } else {
                    entry.last.clone_from(&text);
                    entry.sequence += 1;
                    let mut credentials = Credentials::new();
                    credentials.insert(FILE_KEY, text);
                    Some(Event::Renewed(AccountRenewal::new(
                        &id,
                        entry.revision,
                        entry.sequence,
                        credentials,
                    )))
                }
            }
        }
    };
    if let Some(event) = event {
        tell(accounts, key, &event);
    }
    Ok(())
}

/// A watch of one account: a thread of its own looks at the file, and stopping it ends the thread,
/// which lets go of the spawner and the keeper before it ends.
struct Watching {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Watching {
    fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl AccountWatch for Watching {
    fn stop(&self) -> Result<(), ProviderError> {
        self.halt();
        Ok(())
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.halt();
    }
}

/// How a login ends early.
fn ended(cancelled: bool) -> ProviderError {
    ProviderError::new(
        Status::GUATIAO_ERR_GONE,
        if cancelled {
            "login cancelled"
        } else {
            "login timed out"
        },
    )
}

type Cli = Arc<Mutex<Option<Object<dyn SpawnedProcess>>>>;

/// Ends the notional CLI and everything it started.
fn kill_cli(cli: &Cli) {
    let process = cli.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(process) = process {
        let _ = process.kill();
    }
}

/// A login in progress: its deadline, the prompter and the notional CLI whose tree a cancel kills.
struct Signing {
    deadline: Instant,
    prompter: Arc<Object<dyn LoginPrompter>>,
    cli: Cli,
}

impl Signing {
    /// Ends the login with GONE when the person cancelled or the time is up.
    fn check(&mut self) -> Result<(), ProviderError> {
        let cancelled = self.prompter.cancelled().unwrap_or(true);
        if cancelled || Instant::now() >= self.deadline {
            return Err(self.stop(cancelled));
        }
        Ok(())
    }

    fn stop(&mut self, cancelled: bool) -> ProviderError {
        kill_cli(&self.cli);
        ended(cancelled)
    }

    /// Runs one prompter call: GONE from it is the person cancelling.
    fn prompt<T>(
        &mut self,
        call: impl FnOnce(&Object<dyn LoginPrompter>) -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        match call(&self.prompter) {
            Ok(answer) => {
                self.check()?;
                Ok(answer)
            }
            Err(error) if error.status == Status::GUATIAO_ERR_GONE => Err(self.stop(true)),
            Err(error) => Err(error),
        }
    }
}

fn write(spawner: &Object<dyn ProcessSpawner>, text: &str) -> Result<(), ProviderError> {
    spawner.write_file(
        FILE,
        Secret::new(SecretBytes::new(text.as_bytes().to_vec())),
        true,
    )
}

/// The login itself, on a thread of its own so that the caller can answer at the deadline.
fn run_login(
    method: &str,
    timeout_ms: u64,
    signing: &mut Signing,
    spawner: &Object<dyn ProcessSpawner>,
) -> Result<LoginOutcome, ProviderError> {
    signing.check()?;
    let form = |title: &str, field: &str| {
        LoginForm::new(
            title,
            schema(&json!({
                "type": "object",
                "properties": {field: credential()},
                "required": [field],
            })),
        )
    };
    let refused = |why: &str| bad(&format!("login refused: {why}"));
    let outcome = match method {
        "scripted" => {
            signing.prompt(|p| p.say("Signing in as the echo does"))?;
            let mut open = OpenUrl::new("https://echo.example/device");
            open.user_code = Some("ECHO-0000".into());
            open.expires_ms = Some(timeout_ms);
            signing.prompt(|p| p.open_url(open))?;
            let answer = signing.prompt(|p| p.ask(form("Paste the code", "code")))?;
            let code = answer
                .secrets
                .expose()
                .get("code")
                .unwrap_or_default()
                .to_owned();
            if code.is_empty() || code == "refuse-me" {
                return Err(refused("the echo does not take that code"));
            }
            write(spawner, &format!("{{\"token\":\"{code}\"}}"))?;
            let read = spawner.read_file(FILE, FILE_MAX)?;
            let mut credentials = Credentials::new();
            credentials.insert(FILE_KEY, read.expose().to_text());
            let mut outcome = LoginOutcome::new(FILE_KIND, credentials);
            outcome.label = Some("person@example.invalid".into());
            outcome.expires_at = Some("2099-01-01T00:00:00Z".into());
            outcome.checked = true;
            outcome
        }
        "waiting" => {
            signing.prompt(|p| p.say("Waiting for the page"))?;
            signing.prompt(|p| p.open_url(OpenUrl::new("https://echo.example/browser")))?;
            // The person finishes in the browser; the CLI would then print.
            let until = Instant::now() + Duration::from_millis(400);
            while Instant::now() < until {
                signing.check()?;
                std::thread::sleep(Duration::from_millis(20));
            }
            let text = String::from("{\"token\":\"from-the-browser\"}");
            write(spawner, &text)?;
            let mut credentials = Credentials::new();
            credentials.insert(FILE_KEY, text);
            let mut outcome = LoginOutcome::new(FILE_KIND, credentials);
            outcome.checked = true;
            outcome
        }
        _ => {
            let answer = signing.prompt(|p| p.ask(form("Paste the key", "api_key")))?;
            let key = answer
                .secrets
                .expose()
                .get("api_key")
                .unwrap_or_default()
                .to_owned();
            if key.is_empty() || key == "refuse-me" {
                return Err(refused("the echo does not take that key"));
            }
            let mut credentials = Credentials::new();
            credentials.insert("api_key", key);
            LoginOutcome::new(KEY_KIND, credentials)
        }
    };
    signing.check()?;
    Ok(outcome)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0)
}

impl AccountProvider for EchoAccounts {
    fn describe(&self) -> Result<AccountProviderInfo, ProviderError> {
        let mut info = AccountProviderInfo::new(self.settings.title.clone());
        info.description = "Keeps the accounts of an imaginary service.".into();
        Ok(info)
    }

    fn offers(&self) -> Result<AccountOffer, ProviderError> {
        Ok(offer(&self.settings))
    }

    fn provides(&self, account: AccountRef) -> Result<Vec<String>, ProviderError> {
        if !is_account_id(&account.id) {
            return Err(bad(
                "an account id is 1 to 128 bytes with no control character",
            ));
        }
        // The echo's own login is the standard its file kind provides.
        let kind = if account.kind == HARNESS_LOGIN {
            FILE_KIND
        } else {
            &account.kind
        };
        offer(&self.settings)
            .kind(kind)
            .map(|info| info.provides.clone())
            .ok_or_else(|| bad("the echo makes no account of that kind"))
    }

    fn login(
        &self,
        request: LoginRequest,
        spawner: Object<dyn ProcessSpawner>,
        prompter: Object<dyn LoginPrompter>,
    ) -> Result<LoginOutcome, ProviderError> {
        if !self.settings.logins {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this echo is configured to offer no login method",
            ));
        }
        if !offer(&self.settings)
            .logins
            .iter()
            .any(|m| m.id == request.method)
        {
            return Err(bad("the echo has no such login method"));
        }
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
        let spawner = Arc::new(spawner);
        // The notional CLI, so that a cancel has a tree to kill.
        let cli_spec = SpawnSpec::new("echo-login-cli").cwd(&request.state_dir);
        let cli: Cli = Arc::new(Mutex::new(spawner.spawn(cli_spec).ok()));
        let signing = Signing {
            deadline,
            prompter: Arc::new(prompter),
            cli: Arc::clone(&cli),
        };
        // The login runs on a thread of its own, so that the answer comes at the deadline even when
        // a prompter call has not returned; the thread lets go of what it holds when it does.
        let (tx, rx) = mpsc::channel();
        let worker = Arc::clone(&spawner);
        let method = request.method.clone();
        let timeout_ms = request.timeout_ms;
        let thread = std::thread::Builder::new()
            .name("echo_login".into())
            .spawn(move || {
                let mut signing = signing;
                let result = run_login(&method, timeout_ms, &mut signing, &worker);
                drop(signing);
                drop(worker);
                let _ = tx.send(result);
            })
            .map_err(|_| {
                ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "no thread to sign in on")
            })?;
        track(thread);
        let result = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(_) => Err(ended(false)),
        };
        kill_cli(&cli);
        result
    }

    fn renew(
        &self,
        request: RenewRequest,
        spawner: Object<dyn ProcessSpawner>,
    ) -> Result<RenewReport, ProviderError> {
        let offer = offer(&self.settings);
        let grant = request.account;
        read_grant(&offer, &grant, &spawner)?;
        if !offer.kind(&grant.kind).is_some_and(|k| k.renews) {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this kind cannot be renewed without a model turn",
            ));
        }
        // A login that is blank or gone is lost, whatever the grant says: nothing is renewed from it.
        let key = key_of(&grant);
        let lost = |why: &str| {
            held(&self.accounts)
                .entry(key.clone())
                .or_insert_with(|| AccountFile {
                    last: String::new(),
                    seen: Seen::Present,
                    revision: grant.revision,
                    sequence: 0,
                });
            let _ = look(&self.accounts, &key, &spawner);
            ProviderError::new(Status::GUATIAO_ERR_GONE, why)
        };
        if spawner.file_info(FILE)?.is_none() {
            return Err(lost("the login file is gone"));
        }
        let mut text = spawner.read_file(FILE, FILE_MAX)?.expose().to_text();
        if text.trim().is_empty() {
            return Err(lost("the login file is blank"));
        }
        text.push_str("#renewed");
        write(&spawner, &text)?;
        {
            let mut files = held(&self.accounts);
            let entry = files.entry(key).or_insert_with(|| AccountFile {
                last: String::new(),
                seen: Seen::Present,
                revision: grant.revision,
                sequence: 0,
            });
            entry.last.clone_from(&text);
            entry.seen = Seen::Present;
        }
        let mut credentials = Credentials::new();
        credentials.insert(FILE_KEY, text);
        let mut report = RenewReport::renewed(credentials);
        report.expires_at = Some("2099-01-01T00:00:00Z".into());
        Ok(report)
    }

    fn watch(
        &self,
        request: WatchRequest,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<Object<dyn AccountWatch>, ProviderError> {
        let offer = offer(&self.settings);
        let grant = request.account;
        read_grant(&offer, &grant, &spawner)?;
        if !offer
            .kind(&grant.kind)
            .is_some_and(|k| k.rotates && k.state_dir)
        {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this kind keeps no login to watch",
            ));
        }
        let key = key_of(&grant);
        let text = grant
            .credentials
            .expose()
            .get(FILE_KEY)
            .unwrap_or_default()
            .to_owned();
        {
            let mut files = held(&self.accounts);
            let entry = files.entry(key.clone()).or_insert_with(|| AccountFile {
                last: text,
                seen: Seen::Present,
                revision: grant.revision,
                sequence: 0,
            });
            entry.revision = grant.revision;
        }
        let sub = Sub::of(keeper);
        self.accounts
            .subs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .push(Arc::downgrade(&sub));
        let stop = Arc::new(AtomicBool::new(false));
        let accounts = Arc::clone(&self.accounts);
        let poll = Duration::from_millis(self.settings.poll_ms.max(1));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("echo_watch".into())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    let _ = look(&accounts, &key, &spawner);
                    let until = Instant::now() + poll;
                    while !flag.load(Ordering::SeqCst) && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                sub.release();
                drop(spawner);
            })
            .map_err(|_| {
                ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "no thread to watch on")
            })?;
        Ok(Watching {
            stop,
            thread: Mutex::new(Some(thread)),
        }
        .into_object())
    }

    fn models(
        &self,
        query: AccountModelsQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<ModelListing, ProviderError> {
        drop(keeper);
        let grant = query.account;
        if grant.kind == HARNESS_LOGIN {
            check_id(&grant)?;
            return Ok(ModelListing::of(vec![ModelInfo::new("echo")]));
        }
        read_grant(&offer(&self.settings), &grant, &spawner)?;
        drop(spawner);
        // The account's own kind decides: a key reaches the large model, a login does not.
        let mut models = vec![ModelInfo::new("echo")];
        if grant.kind == KEY_KIND {
            models.push(ModelInfo::new("echo-large"));
        }
        Ok(ModelListing::of(models))
    }

    fn usage(
        &self,
        query: AccountUsageQuery,
        spawner: Object<dyn ProcessSpawner>,
        keeper: Object<dyn AccountKeeper>,
    ) -> Result<AccountUsage, ProviderError> {
        drop(keeper);
        let offer = offer(&self.settings);
        let grant = query.account;
        if grant.kind == HARNESS_LOGIN {
            check_id(&grant)?;
            let mut none = AccountUsage::unsupported("the echo's own login reports no usage");
            none.account = Some(grant.id);
            return Ok(none);
        }
        read_grant(&offer, &grant, &spawner)?;
        drop(spawner);
        let mut usage = if offer.reports_usage(&grant.kind) {
            AccountUsage::new()
        } else {
            let mut none = AccountUsage::unsupported("the echo reports no usage for this kind");
            none.account = Some(grant.id);
            return Ok(none);
        };
        let observed = now();
        let mut five = Bucket::new("five_hour");
        five.label = Some("Five hours".into());
        five.models = Coverage::All;
        five.unit = Unit::Permille;
        five.used = Some(420);
        five.limit = Some(1000);
        five.refill = Refill::FixedWindow {
            length: Some(Duration::from_secs(5 * 3600)),
            resets_at: Some(observed + 3600),
        };
        five.status = Level::Ok;
        five.observed_at = observed;
        let mut large = Bucket::new("seven_day_large");
        large.label = Some("Seven days, the large model".into());
        large.models = Coverage::Only {
            models: vec!["echo-large".into()],
        };
        large.unit = Unit::Permille;
        large.used = Some(1000);
        large.limit = Some(1000);
        large.refill = Refill::FixedWindow {
            length: Some(Duration::from_secs(7 * 24 * 3600)),
            resets_at: Some(observed + 86_400),
        };
        large.status = Level::Exhausted;
        large.observed_at = observed;
        usage.sources = vec![Source::Reported];
        usage.account = Some(grant.id);
        usage.revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        usage.observed_at = observed;
        usage.buckets = vec![five, large];
        Ok(usage)
    }
}
