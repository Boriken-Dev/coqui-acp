//! Applying an account to a harness started through a host's spawner: the credential files it keeps in the account's
//! directory, watched for a renewal or a loss, and the keepers that hear of it.
//!
//! One [`Accounts`] lives as long as the provider. A call that runs the harness on an account takes a [`Hold`]: the
//! files of the grant are placed in the directory (only when the rules below say so), the call's keeper is
//! registered, and every look at the files, by this hold or any other on the account, that finds them changed is one
//! event with one sequence, told to every keeper then live for the account.
//!
//! The rules for placing a file, per watched file:
//! - the provider has no record of it and the file is absent: the grant's copy is written;
//! - no record, and a file there: the same text is kept; one changed after the provider began is news (a renewal, or a
//!   loss when it holds no login), told and kept; an older one is replaced by the grant's copy;
//! - a record, and no hold on the file now: the host may have removed or replaced it since, so the grant's copy is
//!   written unless the file is the one last recorded;
//! - a record, and another hold on the file now: the file is kept (it may have been renewed since), unless the grant
//!   carries a newer revision with other credentials, which replaces it.
//!
//! A file whose login is missing or blank is reported lost once; it comes back as a renewal when a login appears in it.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::Hasher;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::collections::hash_map::DefaultHasher;
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::guatiao::{Secret, Status, Wipe};
use coqui_acp_api::{
    AccountKeeper, AccountLoss, AccountRenewal, Credentials, FileInfo, ProcessSpawner, SecretBytes,
};

/// The most a credential file may hold, in bytes.
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// The default of a provider's `renew_poll_ms`.
pub const DEFAULT_POLL_MS: u64 = 30_000;

/// The file methods of a host's spawner, shared with whoever else holds it.
///
/// The spawner is destroyed when the last holder lets go, so a connection's [`Hold`] drops its copy before the
/// connection reports that it has closed.
#[derive(Clone)]
pub struct Files {
    spawner: Arc<Object<dyn ProcessSpawner>>,
}

impl core::fmt::Debug for Files {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Files").finish_non_exhaustive()
    }
}

impl Files {
    /// The file methods of `spawner`.
    #[must_use]
    pub const fn new(spawner: Arc<Object<dyn ProcessSpawner>>) -> Self {
        Self { spawner }
    }

    /// Whether the spawner serves files at all.
    #[must_use]
    pub fn available(&self) -> bool {
        self.spawner.describe().is_ok_and(|info| info.files)
    }

    /// Writes `text` at `path` in the account's directory, replacing a file there, readable by the owner alone where
    /// the system lets.
    ///
    /// # Errors
    ///
    /// The spawner's refusal.
    pub fn write(&self, path: &str, text: &str) -> Result<(), ProviderError> {
        self.spawner.write_file(
            path,
            Secret::new(SecretBytes::new(text.as_bytes().to_vec())),
            true,
        )
    }

    /// The text of `path`; `None` when it is not there.
    ///
    /// # Errors
    ///
    /// The spawner's refusal, other than the file not being there.
    pub fn read(&self, path: &str) -> Result<Option<String>, ProviderError> {
        match self.spawner.read_file(path, MAX_FILE_BYTES) {
            Ok(bytes) => Ok(Some(bytes.expose().to_text())),
            Err(error) if error.status == Status::GUATIAO_ERR_NOT_FOUND => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// What is known of `path`; `None` when it is not there.
    ///
    /// # Errors
    ///
    /// The spawner's refusal.
    pub fn info(&self, path: &str) -> Result<Option<FileInfo>, ProviderError> {
        self.spawner.file_info(path)
    }

    /// Removes `path`; no error when it is not there.
    ///
    /// # Errors
    ///
    /// The spawner's refusal.
    pub fn remove(&self, path: &str) -> Result<(), ProviderError> {
        self.spawner.remove_file(path)
    }
}

/// One file that holds a login.
#[derive(Clone)]
#[non_exhaustive]
pub struct Watched {
    /// Where, relative to the account's directory.
    pub path: String,
    /// The key of the grant's credentials that holds the file's text.
    pub key: String,
    /// Whether a file's text holds a login; one that does not (blank, or a login the harness blanked) is lost.
    pub usable: fn(&str) -> bool,
    /// When the login in the text stops working, as an RFC 3339 time, when the text says.
    pub expires: fn(&str) -> Option<String>,
}

impl core::fmt::Debug for Watched {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watched")
            .field("path", &self.path)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Watched {
    /// The file `path`, held by the credential `key`; usable when it is not blank.
    #[must_use]
    pub fn new(path: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            key: key.into(),
            usable: |text| !text.trim().is_empty(),
            expires: |_| None,
        }
    }

    /// The same file, usable as `usable` says.
    #[must_use]
    pub fn usable(mut self, usable: fn(&str) -> bool) -> Self {
        self.usable = usable;
        self
    }

    /// The same file, expiring as `expires` reads from its text.
    #[must_use]
    pub fn expires(mut self, expires: fn(&str) -> Option<String>) -> Self {
        self.expires = expires;
        self
    }
}

/// What the keepers of one hold are told through: destroyed when its hold ends.
type Slot = Mutex<Option<Object<dyn AccountKeeper>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(text.as_bytes());
    hasher.write_usize(text.len());
    hasher.finish()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .unwrap_or(0)
}

/// A time in milliseconds since the Unix epoch as an RFC 3339 time in UTC.
#[must_use]
pub fn rfc3339_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// What the provider knows of one watched file.
struct Record {
    /// The fingerprint of the login last recorded; `None` when the login is lost.
    fingerprint: Option<u64>,
    /// The revision of the newest grant that placed it.
    revision: u64,
    /// How many holds are on it now.
    live: usize,
}

#[derive(Default)]
struct Entry {
    sequence: u64,
    keepers: Vec<(u64, Arc<Slot>)>,
    next_token: u64,
    files: BTreeMap<(String, String), Record>,
}

/// What the provider knows of its accounts, for as long as it lives.
pub struct Accounts {
    began_ms: i64,
    entries: Mutex<BTreeMap<String, Entry>>,
}

impl core::fmt::Debug for Accounts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Accounts").finish_non_exhaustive()
    }
}

impl Default for Accounts {
    fn default() -> Self {
        Self::new()
    }
}

/// Something a look found.
enum Event {
    Renewed(Credentials, Option<String>),
    Lost(String),
}

/// What a [`Hold::look`] found and told.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Looked {
    /// Each renewal found: the credentials now, whole, and when they expire.
    pub renewed: Vec<(Credentials, Option<String>)>,
    /// Why each login found lost is.
    pub lost: Vec<String>,
}

impl Accounts {
    /// A registry that has seen nothing; files changed from now on are news.
    #[must_use]
    pub fn new() -> Self {
        Self {
            began_ms: now_ms(),
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Holds the account `id` for a call or a connection: registers `keeper` (a call that has none, such as a renewal,
    /// hears nothing and tells the others), places `credentials`' files (see the
    /// module's rules) and answers the hold.
    ///
    /// `start` is a connection, as against a call that asks one thing. `dir` is the account's directory as the
    /// host named it; every path of `watched` is relative to it.
    ///
    /// # Errors
    ///
    /// `GUATIAO_ERR_BAD_VALUE` when a watched file's key is not among the credentials; the spawner's refusal to read
    /// or write a file.
    #[expect(
        clippy::too_many_arguments,
        reason = "one call places everything a grant carries"
    )]
    pub fn hold(
        self: &Arc<Self>,
        files: Files,
        keeper: Option<Object<dyn AccountKeeper>>,
        id: &str,
        dir: &str,
        revision: u64,
        credentials: &Credentials,
        watched: Vec<Watched>,
    ) -> Result<Hold, ProviderError> {
        for file in &watched {
            if credentials.get(&file.key).is_none() {
                return Err(ProviderError::new(
                    Status::GUATIAO_ERR_BAD_VALUE,
                    "a credential the account needs is missing",
                ));
            }
        }
        let listening = keeper.is_some();
        let slot: Arc<Slot> = Arc::new(Mutex::new(keeper));
        let token = {
            let mut entries = lock(&self.entries);
            let entry = entries.entry(id.to_string()).or_default();
            entry.next_token += 1;
            let token = entry.next_token;
            if listening {
                entry.keepers.push((token, Arc::clone(&slot)));
            }
            token
        };
        let hold = Hold(Arc::new(Inner {
            accounts: Arc::clone(self),
            id: id.to_string(),
            dir: dir.to_string(),
            revision,
            credentials: Mutex::new(credentials.clone()),
            watched,
            files: Mutex::new(Some(files)),
            slot,
            token,
            placed: AtomicBool::new(false),
            done: AtomicBool::new(false),
        }));
        // A failure leaves the hold to drop, which lets go of everything it took.
        hold.0.place()?;
        Ok(hold)
    }

    fn tell(&self, id: &str, revision: u64, event: Event) {
        let (sequence, slots) = {
            let mut entries = lock(&self.entries);
            let entry = entries.entry(id.to_string()).or_default();
            entry.sequence += 1;
            (
                entry.sequence,
                entry
                    .keepers
                    .iter()
                    .map(|(_, slot)| Arc::clone(slot))
                    .collect::<Vec<_>>(),
            )
        };
        // No lock of the registry is held while a keeper is called.
        for slot in slots {
            let held = lock(&slot);
            let Some(keeper) = held.as_ref() else {
                continue;
            };
            let _ = match &event {
                Event::Renewed(credentials, expires) => {
                    let mut renewal =
                        AccountRenewal::new(id, revision, sequence, credentials.clone());
                    renewal.expires_at.clone_from(expires);
                    keeper.renewed(renewal)
                }
                Event::Lost(reason) => keeper.lost(AccountLoss::new(id, sequence, reason.clone())),
            };
        }
    }
}

struct Inner {
    accounts: Arc<Accounts>,
    id: String,
    dir: String,
    revision: u64,
    credentials: Mutex<Credentials>,
    watched: Vec<Watched>,
    files: Mutex<Option<Files>>,
    slot: Arc<Slot>,
    token: u64,
    /// The files are placed, so the hold counts on them.
    placed: AtomicBool,
    done: AtomicBool,
}

/// A call's or a connection's hold on an account.
///
/// Ending it ([`finish`](Self::finish), or dropping it) looks once more, lets go of the keeper (destroying it) and of
/// the spawner.
#[derive(Clone)]
#[must_use]
pub struct Hold(Arc<Inner>);

impl core::fmt::Debug for Hold {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hold").finish_non_exhaustive()
    }
}

/// What a file holds now.
enum Now {
    Lost,
    Has(String),
}

impl Inner {
    fn files(&self) -> Result<Files, ProviderError> {
        lock(&self.files)
            .clone()
            .ok_or_else(|| ProviderError::new(Status::GUATIAO_ERR_GONE, "the hold has ended"))
    }

    fn key(&self, file: &Watched) -> (String, String) {
        (self.dir.clone(), file.path.clone())
    }

    fn whole(&self, file: &Watched, text: &str) -> Credentials {
        let mut credentials = lock(&self.credentials).clone();
        credentials.insert(file.key.clone(), text);
        credentials
    }

    fn now(&self, files: &Files, file: &Watched) -> Result<Now, ProviderError> {
        Ok(match files.read(&file.path)? {
            Some(text) if (file.usable)(&text) => Now::Has(text),
            _ => Now::Lost,
        })
    }

    fn place(&self) -> Result<(), ProviderError> {
        let files = self.files()?;
        let credentials = lock(&self.credentials).clone();
        let mut events = Vec::new();
        let mut placed = Vec::new();
        for file in &self.watched {
            let grant = credentials.get(&file.key).unwrap_or_default();
            let key = self.key(file);
            let record = lock(&self.accounts.entries)
                .get(&self.id)
                .and_then(|entry| entry.files.get(&key))
                .map(|r| (r.fingerprint, r.revision, r.live));
            let info = files.info(&file.path)?;
            let (write, news) = match (info, record) {
                (None, None) => (true, None),
                (None, Some((_, _, live))) => (live == 0, None),
                (Some(info), None) => {
                    let text = files.read(&file.path)?.unwrap_or_default();
                    if text == grant {
                        (false, None)
                    } else if info.modified_ms >= self.accounts.began_ms {
                        (false, Some(text))
                    } else {
                        (true, None)
                    }
                }
                (Some(info), Some((recorded, revision, live))) => {
                    let text = files.read(&file.path)?.unwrap_or_default();
                    let newer = self.revision > revision && recorded != Some(fingerprint(grant));
                    if recorded == Some(fingerprint(&text)) || live > 0 {
                        // The file is the one last recorded, or another hold has it and it may have been renewed
                        // since: kept, unless the host's grant is a newer copy.
                        (newer, None)
                    } else if newer {
                        (true, None)
                    } else if info.modified_ms >= self.accounts.began_ms {
                        // Changed since the last look, by no one this provider knows: news.
                        (false, Some(text))
                    } else {
                        (true, None)
                    }
                }
            };
            if write {
                files.write(&file.path, grant)?;
            }
            placed.push((file, write, news, grant.to_string()));
        }
        let mut entries = lock(&self.accounts.entries);
        let entry = entries.entry(self.id.clone()).or_default();
        for (file, write, news, grant) in placed {
            let key = self.key(file);
            let fresh = !entry.files.contains_key(&key);
            let record = entry.files.entry(key).or_insert(Record {
                fingerprint: None,
                revision: self.revision,
                live: 0,
            });
            record.live += 1;
            record.revision = record.revision.max(self.revision);
            if write {
                record.fingerprint = Some(fingerprint(&grant));
            } else if let Some(text) = news {
                if (file.usable)(&text) {
                    record.fingerprint = Some(fingerprint(&text));
                    events.push(Event::Renewed(
                        self.whole(file, &text),
                        (file.expires)(&text),
                    ));
                } else {
                    record.fingerprint = None;
                    events.push(Event::Lost(lost_reason(file)));
                }
            } else if fresh {
                // A file there that holds what the grant does.
                record.fingerprint = Some(fingerprint(&grant));
            }
        }
        drop(entries);
        self.placed.store(true, Ordering::SeqCst);
        for event in events {
            self.accounts.tell(&self.id, self.revision, event);
        }
        Ok(())
    }

    fn look(&self) -> Result<Looked, ProviderError> {
        let files = self.files()?;
        let mut found = Looked::default();
        for file in &self.watched {
            let now = self.now(&files, file)?;
            let key = self.key(file);
            let event = {
                let mut entries = lock(&self.accounts.entries);
                let entry = entries.entry(self.id.clone()).or_default();
                let record = entry.files.entry(key).or_insert(Record {
                    fingerprint: None,
                    revision: self.revision,
                    live: 0,
                });
                match now {
                    Now::Has(text) => {
                        let print = fingerprint(&text);
                        if record.fingerprint == Some(print) {
                            None
                        } else {
                            record.fingerprint = Some(print);
                            Some(Event::Renewed(
                                self.whole(file, &text),
                                (file.expires)(&text),
                            ))
                        }
                    }
                    Now::Lost => {
                        if record.fingerprint.is_some() {
                            record.fingerprint = None;
                            Some(Event::Lost(lost_reason(file)))
                        } else {
                            None
                        }
                    }
                }
            };
            if let Some(event) = event {
                match &event {
                    Event::Renewed(credentials, expires) => {
                        found.renewed.push((credentials.clone(), expires.clone()));
                    }
                    Event::Lost(reason) => found.lost.push(reason.clone()),
                }
                self.accounts.tell(&self.id, self.revision, event);
            }
        }
        Ok(found)
    }

    fn finish(&self) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.placed.load(Ordering::SeqCst) {
            let _ = self.look();
            let mut entries = lock(&self.accounts.entries);
            if let Some(entry) = entries.get_mut(&self.id) {
                for file in &self.watched {
                    if let Some(record) = entry.files.get_mut(&self.key(file)) {
                        record.live = record.live.saturating_sub(1);
                    }
                }
            }
        }
        {
            let mut entries = lock(&self.accounts.entries);
            if let Some(entry) = entries.get_mut(&self.id) {
                entry.keepers.retain(|(token, _)| *token != self.token);
            }
        }
        // Waits for a call to this keeper in flight, then destroys it.
        drop(lock(&self.slot).take());
        drop(lock(&self.files).take());
        lock(&self.credentials).wipe();
    }
}

fn lost_reason(file: &Watched) -> String {
    format!("the login in {} is missing or empty", file.path)
}

impl Hold {
    /// Looks at the files: every renewal or loss found since the last look is told to every keeper live for the
    /// account, and answered.
    ///
    /// # Errors
    ///
    /// The spawner's refusal to read a file, or the hold has ended.
    pub fn look(&self) -> Result<Looked, ProviderError> {
        self.0.look()
    }

    /// The reason the account's login is lost now, when it is: a file the last look found missing or blank.
    #[must_use]
    pub fn lost(&self) -> Option<String> {
        let entries = lock(&self.0.accounts.entries);
        let entry = entries.get(&self.0.id)?;
        self.0
            .watched
            .iter()
            .find(|file| {
                entry
                    .files
                    .get(&self.0.key(file))
                    .is_some_and(|record| record.fingerprint.is_none())
            })
            .map(lost_reason)
    }

    /// The files' texts as they are now, by credential key: the grant's credentials with each watched file's current
    /// text in its place.
    ///
    /// # Errors
    ///
    /// The spawner's refusal to read a file, or the hold has ended.
    pub fn credentials(&self) -> Result<Credentials, ProviderError> {
        let files = self.0.files()?;
        let mut credentials = lock(&self.0.credentials).clone();
        for file in &self.0.watched {
            if let Now::Has(text) = self.0.now(&files, file)? {
                credentials.insert(file.key.clone(), text);
            }
        }
        Ok(credentials)
    }

    /// The expiry the files now say, the earliest of them.
    #[must_use]
    pub fn expires_at(&self) -> Option<String> {
        let files = self.0.files().ok()?;
        self.0
            .watched
            .iter()
            .filter_map(|file| match self.0.now(&files, file) {
                Ok(Now::Has(text)) => (file.expires)(&text),
                _ => None,
            })
            .min()
    }

    /// Looks once more, lets go of the keeper (destroying it) and of the spawner. Idempotent.
    pub fn finish(&self) {
        self.0.finish();
    }

    /// Looks every `poll` on a thread of its own until the returned [`Watcher`] is stopped or dropped. A look that
    /// fails is tried again at the next.
    pub fn watch(&self, poll: Duration) -> Watcher {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let looking = self.clone();
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("coqui-acp-account-watch".into())
            .spawn(move || {
                let (flag, changed) = &*stopping;
                let mut stopped = lock(flag);
                loop {
                    let (next, _) = changed
                        .wait_timeout(stopped, poll)
                        .unwrap_or_else(PoisonError::into_inner);
                    stopped = next;
                    if *stopped {
                        return;
                    }
                    drop(stopped);
                    let _ = looking.0.look();
                    stopped = lock(flag);
                    if *stopped {
                        return;
                    }
                }
            })
            .ok();
        Watcher { stop, thread }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The thread that looks at an account's files on a deadline of its own.
#[must_use]
pub struct Watcher {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl core::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

impl Watcher {
    /// Stops the thread and waits for it.
    pub fn stop(mut self) {
        self.end();
    }

    fn end(&mut self) {
        *lock(&self.stop.0) = true;
        self.stop.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.end();
    }
}
