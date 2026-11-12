use alloc::sync::Arc;
use core::fmt;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use coqui_acp::secret::{Capability, IdentityId, Prover, SecretValue, Verifier};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::files::Places;
use crate::ledger::Ledger;
use crate::run::{ProcessRunner, Runner};
use crate::scrub::Scrubber;
use crate::{Approval, Decision, Error, Event, Frontend, Mode, Settings};

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type Observer = Arc<dyn Fn(&Event) + Send + Sync>;

/// The host side of secret prompts for every session of one host: it keeps each session's secrets and serves its tools.
pub struct Secrets {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) prover: Prover,
    pub(crate) verifier: Verifier,
    pub(crate) settings: Settings,
    pub(crate) runner: Arc<dyn Runner>,
    pub(crate) clock: Clock,
    observer: Option<Observer>,
    pub(crate) ledger: Mutex<Ledger>,
    sessions: Mutex<HashMap<String, Arc<Core>>>,
}

/// What one open session holds.
pub(crate) struct Core {
    pub(crate) id: String,
    pub(crate) frontend: Arc<dyn Frontend>,
    pub(crate) owner: IdentityId,
    pub(crate) capability: Capability,
    pub(crate) places: Places,
    state: Mutex<State>,
}

struct State {
    ended: bool,
    busy: bool,
    handles: HashMap<String, Entry>,
    allowed: HashSet<String>,
}

struct Entry {
    field: String,
    value: SecretValue,
    made: u64,
}

/// A prompt in progress: while it lives no other prompt of the session starts.
pub(crate) struct Busy<'a>(&'a Core);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.busy = false;
        }
    }
}

impl Core {
    fn state(&self) -> Result<MutexGuard<'_, State>, Error> {
        let state = self.state.lock().map_err(|_| Error::UnknownSession)?;
        if state.ended {
            return Err(Error::UnknownSession);
        }
        Ok(state)
    }

    pub(crate) fn busy(&self) -> Result<Busy<'_>, Error> {
        let mut state = self.state()?;
        if state.busy {
            return Err(Error::Busy);
        }
        state.busy = true;
        Ok(Busy(self))
    }

    /// Drops handles past their lifetime and says whether `extra` more fit.
    pub(crate) fn check_room(
        &self,
        extra: usize,
        settings: &Settings,
        now: u64,
    ) -> Result<(), Error> {
        let mut state = self.state()?;
        state.expire(now, settings);
        if state.handles.len() + extra > settings.max_handles {
            return Err(Error::Limit("too many secrets are held in this session"));
        }
        Ok(())
    }

    /// Keeps the opened secrets under new handles: `(field, handle)`. Refused when the session ended meanwhile.
    pub(crate) fn store(
        &self,
        secrets: impl IntoIterator<Item = (String, SecretValue)>,
        settings: &Settings,
        now: u64,
    ) -> Result<Vec<(String, String)>, Error> {
        let mut state = self.state()?;
        state.expire(now, settings);
        let secrets: Vec<_> = secrets.into_iter().collect();
        if state.handles.len() + secrets.len() > settings.max_handles {
            return Err(Error::Limit("too many secrets are held in this session"));
        }
        let mut made = Vec::new();
        for (field, value) in secrets {
            let handle = format!("secret_{}", Uuid::new_v4().simple());
            state.handles.insert(
                handle.clone(),
                Entry {
                    field: field.clone(),
                    value,
                    made: now,
                },
            );
            made.push((field, handle));
        }
        Ok(made)
    }

    /// The field a handle was entered as.
    pub(crate) fn field_of(
        &self,
        handle: &str,
        settings: &Settings,
        now: u64,
    ) -> Result<String, Error> {
        let mut state = self.state()?;
        state.expire(now, settings);
        state
            .handles
            .get(handle)
            .map(|entry| entry.field.clone())
            .ok_or(Error::UnknownHandle)
    }

    /// A copy of a secret's bytes, zeroed when dropped.
    pub(crate) fn value_of(
        &self,
        handle: &str,
        settings: &Settings,
        now: u64,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let mut state = self.state()?;
        state.expire(now, settings);
        state
            .handles
            .get(handle)
            .map(|entry| Zeroizing::new(entry.value.expose().to_vec()))
            .ok_or(Error::UnknownHandle)
    }

    /// Every live secret as `(handle, bytes)` copies, for scrubbing.
    pub(crate) fn scrubber(&self, settings: &Settings) -> Scrubber {
        let Ok(state) = self.state.lock() else {
            return Scrubber::new([], settings.scrub_min_piece);
        };
        Scrubber::new(
            state
                .handles
                .iter()
                .map(|(handle, entry)| (handle.as_str(), entry.value.expose())),
            settings.scrub_min_piece,
        )
    }

    pub(crate) fn allowed(&self, key: &str) -> bool {
        self.state().is_ok_and(|state| state.allowed.contains(key))
    }

    pub(crate) fn allow(&self, key: String) {
        if let Ok(mut state) = self.state() {
            state.allowed.insert(key);
        }
    }

    /// Refuses once the session has ended: a value copied out before it ended is not used after.
    pub(crate) fn check_alive(&self) -> Result<(), Error> {
        self.state().map(|_| ())
    }

    pub(crate) fn handle_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.handles.len())
    }

    /// Ends the session: its secrets are dropped, which zeroes them, and every tool refuses from now on.
    fn end(&self) -> usize {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        state.ended = true;
        let cleared = state.handles.len();
        state.handles.clear();
        state.allowed.clear();
        cleared
    }
}

impl State {
    fn expire(&mut self, now: u64, settings: &Settings) {
        let lifetime = settings.handle_lifetime.as_secs();
        self.handles
            .retain(|_, entry| now.saturating_sub(entry.made) < lifetime);
    }
}

impl Inner {
    pub(crate) fn now(&self) -> u64 {
        (self.clock)()
    }

    pub(crate) fn emit(&self, event: &Event) {
        if let Some(observer) = &self.observer {
            observer(event);
        }
    }

    /// Asks the person, unless this exact action for this secret was allowed for the session already.
    pub(crate) fn approve(&self, core: &Core, approval: &Approval) -> Result<(), Error> {
        // A secret's use is a permission: the session's mode decides, as it does for the agent's own tools.
        if core.frontend.mode() == Mode::Bypass {
            self.emit(&Event::Bypassed {
                session_id: core.id.clone(),
                action: approval.action.clone(),
            });
            return Ok(());
        }
        let key = approval.key();
        if core.allowed(&key) {
            self.emit(&Event::Answered {
                session_id: core.id.clone(),
                decision: Decision::Session,
                remembered: true,
            });
            return Ok(());
        }
        self.emit(&Event::Asked {
            session_id: core.id.clone(),
            action: approval.action.clone(),
        });
        let decision = core.frontend.approve(approval)?;
        self.emit(&Event::Answered {
            session_id: core.id.clone(),
            decision,
            remembered: false,
        });
        match decision {
            Decision::Once => Ok(()),
            Decision::Session => {
                core.allow(key);
                Ok(())
            }
            _ => Err(Error::Refused("the person did not approve it")),
        }
    }
}

/// What a session is opened with.
#[derive(Clone)]
#[non_exhaustive]
pub struct SessionConfig {
    /// The frontend of the session's owner: the only one ever asked.
    pub frontend: Arc<dyn Frontend>,
    /// The session's working roots. A secret is written, and a command started, only under these (and the settings' write
    /// directories). The first is where a relative path starts.
    pub roots: Vec<PathBuf>,
}

impl SessionConfig {
    /// A session for `frontend` working in `roots`.
    #[must_use]
    pub fn new(frontend: Arc<dyn Frontend>, roots: Vec<PathBuf>) -> Self {
        Self { frontend, roots }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("roots", &self.roots)
            .finish_non_exhaustive()
    }
}

/// Builds a [`Secrets`].
pub struct SecretsBuilder {
    prover: Prover,
    verifier: Verifier,
    settings: Settings,
    runner: Arc<dyn Runner>,
    clock: Clock,
    observer: Option<Observer>,
}

impl fmt::Debug for SecretsBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsBuilder")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl SecretsBuilder {
    /// The settings; the default for each one not set.
    #[must_use]
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Where commands run. The default is [`ProcessRunner`].
    #[must_use]
    pub fn runner(mut self, runner: impl Runner + 'static) -> Self {
        self.runner = Arc::new(runner);
        self
    }

    /// The clock, as Unix seconds. The default is the system's.
    #[must_use]
    pub fn clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Hears every [`Event`].
    #[must_use]
    pub fn observer(mut self, observer: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    /// Builds it.
    #[must_use]
    pub fn build(self) -> Secrets {
        Secrets {
            inner: Arc::new(Inner {
                prover: self.prover,
                verifier: self.verifier,
                settings: self.settings,
                runner: self.runner,
                clock: self.clock,
                observer: self.observer,
                ledger: Mutex::default(),
                sessions: Mutex::default(),
            }),
        }
    }
}

impl Secrets {
    /// A host's secrets: `prover` is the host's own identity (what it binds its keys and answers with) and `verifier` knows the
    /// identities its frontends prove with.
    #[must_use]
    pub fn builder(prover: Prover, verifier: Verifier) -> SecretsBuilder {
        SecretsBuilder {
            prover,
            verifier,
            settings: Settings::default(),
            runner: Arc::new(ProcessRunner),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs())
            }),
            observer: None,
        }
    }

    /// The settings in force.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// Opens a session: its store is empty and it serves the tools the frontend's capability offers.
    ///
    /// # Errors
    /// [`Error::SessionOpen`] for an id in use; [`Error::Path`] for a root that is not a directory or no root at all.
    pub fn open_session(
        &self,
        id: &str,
        config: SessionConfig,
    ) -> Result<crate::SessionTools, Error> {
        let places = Places::new(&config.roots, &self.inner.settings.write_dirs)?;
        let core = Arc::new(Core {
            id: id.to_owned(),
            owner: config.frontend.identity(),
            capability: config.frontend.capability(),
            frontend: config.frontend,
            places,
            state: Mutex::new(State {
                ended: false,
                busy: false,
                handles: HashMap::new(),
                allowed: HashSet::new(),
            }),
        });
        let mut sessions = self
            .inner
            .sessions
            .lock()
            .map_err(|_| Error::UnknownSession)?;
        if sessions.contains_key(id) {
            return Err(Error::SessionOpen);
        }
        sessions.insert(id.to_owned(), Arc::clone(&core));
        Ok(crate::SessionTools::new(Arc::clone(&self.inner), core))
    }

    /// The tools of an open session.
    ///
    /// # Errors
    /// [`Error::UnknownSession`].
    pub fn session(&self, id: &str) -> Result<crate::SessionTools, Error> {
        let core = self
            .inner
            .sessions
            .lock()
            .map_err(|_| Error::UnknownSession)?
            .get(id)
            .cloned()
            .ok_or(Error::UnknownSession)?;
        Ok(crate::SessionTools::new(Arc::clone(&self.inner), core))
    }

    /// Ends a session: every secret it held is dropped (and zeroed), every approval "for the session" is forgotten, and the tools
    /// it handed out refuse from now on. Ending a session that is not open does nothing. A host calls this when the session ends,
    /// however it ends.
    pub fn end_session(&self, id: &str) {
        let core = self
            .inner
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(id));
        if let Some(core) = core {
            let cleared = core.end();
            self.inner.emit(&Event::Ended {
                session_id: id.to_owned(),
                cleared,
            });
        }
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sessions = self
            .inner
            .sessions
            .lock()
            .map_or(0, |sessions| sessions.len());
        f.debug_struct("Secrets")
            .field("sessions", &sessions)
            .finish_non_exhaustive()
    }
}
