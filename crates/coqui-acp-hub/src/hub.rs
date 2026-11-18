use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use std::collections::HashMap;

use serde_json::{Value, json};

use crate::client::{Client, ClientId, Cursor};
use crate::error::HubError;
use crate::log::Log;
use crate::output::{Note, Output, PermissionRequest};
use crate::policy::{Action, AllowAll, Policy, Verdict};
use crate::protocol::{PRESENCE, notification};
use crate::settings::Settings;
use crate::snapshot::{LogSnapshot, SnapshotError, prompt_of};

pub(crate) struct Member {
    pub client: Client,
    pub attached: bool,
}

pub(crate) struct Running {
    pub agent_id: String,
    pub client: ClientId,
    pub req_id: Value,
    pub prompt: u64,
}

pub(crate) struct Queued {
    pub client: ClientId,
    pub req_id: Value,
    pub params: Value,
    pub prompt: u64,
}

pub(crate) struct Pending {
    pub request: PermissionRequest,
    pub agent_id: Value,
    pub addressed: BTreeSet<ClientId>,
    pub withdrawn: BTreeSet<ClientId>,
    pub flagged: bool,
}

/// Where the agent's answer to a request the hub sent it goes.
pub(crate) enum Route {
    Client { client: ClientId, id: Value },
}

/// One live agent session, served to any number of attached clients.
///
/// A `Hub` does no I/O. The host feeds it what arrives from the agent (`from_agent`) and from each client (`from_client`)
/// and sends the `Output`s it returns over its own links. It runs no timers and keeps no clock but the one `poll` is
/// given, and it never ends a session: when to do that is the host's, from `attached_count` and `last_activity`.
pub struct Hub {
    pub(crate) session: String,
    pub(crate) initialize: Value,
    pub(crate) settings: Settings,
    pub(crate) policy: Arc<dyn Policy>,
    pub(crate) members: BTreeMap<ClientId, Member>,
    pub(crate) log: Log,
    pub(crate) next_prompt: u64,
    pub(crate) running: Option<Running>,
    pub(crate) queue: VecDeque<Queued>,
    pub(crate) routes: HashMap<String, Route>,
    pub(crate) agent_routes: HashMap<String, Value>,
    pub(crate) pending: BTreeMap<u64, Pending>,
    pub(crate) next_id: u64,
    pub(crate) now: u64,
    pub(crate) last_activity: u64,
    pub(crate) ended: bool,
}

impl core::fmt::Debug for Hub {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hub")
            .field("session", &self.session)
            .field("attached", &self.attached_count())
            .field("head", &self.log.head())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

/// Starts a [`Hub`].
#[must_use]
pub struct Builder {
    session: String,
    initialize: Value,
    settings: Settings,
    policy: Arc<dyn Policy>,
    log: Log,
    next_prompt: u64,
}

impl core::fmt::Debug for Builder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Builder")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Builder {
    /// The `initialize` result the agent gave the host, which the hub answers every client's `initialize` with (adding
    /// `loadSession` and `sessionCapabilities.resume`, which the hub provides whatever the agent does).
    pub fn initialize_result(mut self, result: Value) -> Self {
        self.initialize = result;
        self
    }

    /// Takes all settings at once.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Who may do what. Default: `AllowAll`.
    pub fn policy(mut self, policy: Arc<dyn Policy>) -> Self {
        self.policy = policy;
        self
    }

    /// Updates kept for replay, at most.
    pub fn max_log_entries(mut self, entries: usize) -> Self {
        self.settings.max_log_entries = entries;
        self
    }

    /// Serialized bytes of updates kept for replay, at most.
    pub fn max_log_bytes(mut self, bytes: usize) -> Self {
        self.settings.max_log_bytes = bytes;
        self
    }

    /// Prompts that may wait behind a running turn.
    pub fn max_queued_prompts(mut self, prompts: usize) -> Self {
        self.settings.max_queued_prompts = prompts;
        self
    }

    /// Whether a cancel also ends the queued prompts.
    pub fn cancel_drops_queue(mut self, drops: bool) -> Self {
        self.settings.cancel_drops_queue = drops;
        self
    }

    /// How long a permission request may wait before `poll` reports it.
    pub fn permission_window_ms(mut self, window: Option<u64>) -> Self {
        self.settings.permission_window_ms = window;
        self
    }

    /// Starts the hub with a log kept from an earlier run (`Hub::log_snapshot`), so the cursors clients hold are still
    /// theirs: the next update is the one after the snapshot's head, and prompt numbers go on after the highest kept.
    /// The settings' bounds apply to what is restored, whichever order they are given in.
    ///
    /// # Errors
    /// `SnapshotError` when an entry has no cursor, is out of order, or is past the snapshot's head.
    pub fn restore_log(mut self, snapshot: LogSnapshot) -> Result<Self, SnapshotError> {
        let highest = snapshot
            .entries
            .iter()
            .filter_map(prompt_of)
            .max()
            .unwrap_or(0);
        self.log = Log::restore(snapshot)?;
        self.next_prompt = highest + 1;
        Ok(self)
    }

    /// The hub: no client attached, nothing logged unless `restore_log` gave it a log.
    #[must_use]
    pub fn build(mut self) -> Hub {
        self.log.trim(&self.settings);
        Hub {
            session: self.session,
            initialize: self.initialize,
            settings: self.settings,
            policy: self.policy,
            members: BTreeMap::new(),
            log: self.log,
            next_prompt: self.next_prompt,
            running: None,
            queue: VecDeque::new(),
            routes: HashMap::new(),
            agent_routes: HashMap::new(),
            pending: BTreeMap::new(),
            next_id: 1,
            now: 0,
            last_activity: 0,
            ended: false,
        }
    }
}

impl Hub {
    /// Describes a hub for the agent session `session` (the id the agent gave `session/new`), already open.
    pub fn builder(session: impl Into<String>) -> Builder {
        Builder {
            session: session.into(),
            initialize: json!({ "protocolVersion": 1, "agentCapabilities": {} }),
            settings: Settings::default(),
            policy: Arc::new(AllowAll),
            log: Log::default(),
            next_prompt: 1,
        }
    }

    /// Registers a client's connection. It is not yet attached: it attaches with `session/load` or `session/resume`.
    ///
    /// # Errors
    /// `HubError::Refused` when the policy refuses `Action::Attach`; `HubError::Duplicate` when the identity is in use.
    pub fn connect(&mut self, client: Client) -> Result<(), HubError> {
        if self.members.contains_key(&client.id) {
            return Err(HubError::Duplicate(client.id));
        }
        if let Verdict::Deny(reason) = self.policy.check(&client, &Action::Attach) {
            return Err(HubError::Refused(reason));
        }
        self.last_activity = self.now;
        self.members.insert(
            client.id.clone(),
            Member {
                client,
                attached: false,
            },
        );
        Ok(())
    }

    /// Drops a client's connection. The session goes on, with the client's queued prompts and the permission requests it
    /// was asked still waiting for others.
    ///
    /// # Errors
    /// `HubError::UnknownClient`.
    pub fn disconnect(&mut self, client: &ClientId) -> Result<Vec<Output>, HubError> {
        let member = self
            .members
            .remove(client)
            .ok_or_else(|| HubError::UnknownClient(client.clone()))?;
        self.last_activity = self.now;
        let mut out = Vec::new();
        if member.attached {
            self.presence(&mut out);
            let ids: Vec<u64> = self.pending.keys().copied().collect();
            for id in ids {
                self.settle_withdrawn(id, &mut out);
            }
        }
        Ok(out)
    }

    /// Sets the hub's clock (milliseconds, from any origin the host keeps steady) and returns what became due: permission
    /// requests that have waited past `Settings::permission_window_ms`. Call it before feeding an input for accurate
    /// `last_activity`.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Output> {
        self.now = self.now.max(now_ms);
        let mut out = Vec::new();
        if let Some(window) = self.settings.permission_window_ms {
            for pending in self.pending.values_mut() {
                let waited = self.now.saturating_sub(pending.request.asked_at);
                if !pending.flagged && waited >= window {
                    pending.flagged = true;
                    out.push(Output::Note(Note::PermissionUnanswered {
                        request: pending.request.id,
                        waiting_ms: waited,
                    }));
                }
            }
        }
        out
    }

    /// The agent session's id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session
    }

    /// The settings in force.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// How many clients are attached.
    #[must_use]
    pub fn attached_count(&self) -> usize {
        self.members.values().filter(|m| m.attached).count()
    }

    /// The attached clients, in identity order.
    pub fn attached(&self) -> impl Iterator<Item = &Client> {
        self.members
            .values()
            .filter(|m| m.attached)
            .map(|m| &m.client)
    }

    /// The hub's clock when anything last happened: an input from the agent or a client, a connect or a disconnect.
    #[must_use]
    pub const fn last_activity(&self) -> u64 {
        self.last_activity
    }

    /// The newest update's cursor: what a client that has seen everything holds.
    #[must_use]
    pub fn head(&self) -> Cursor {
        self.log.head()
    }

    /// The log as it stands, for the host to keep and give a later hub to `Builder::restore_log`. Take it again (or keep
    /// `log_after` of the cursor last kept) whenever a call returned outputs, since each update is logged as it is fanned
    /// out.
    #[must_use]
    pub fn log_snapshot(&self) -> LogSnapshot {
        self.log.snapshot()
    }

    /// The kept updates after `cursor`, oldest first, as they were fanned out: what a host appends to the log it keeps
    /// since the cursor it last kept. Empty when `cursor` is the head; entries the bounds dropped are not in it.
    pub fn log_after(&self, cursor: Cursor) -> impl Iterator<Item = &Value> {
        self.log.after(cursor.0).map(|e| &e.message)
    }

    /// Updates currently kept for replay.
    #[must_use]
    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// Prompts waiting behind the running turn.
    #[must_use]
    pub fn queued_prompts(&self) -> usize {
        self.queue.len()
    }

    /// Whether a turn is running.
    #[must_use]
    pub const fn turn_running(&self) -> bool {
        self.running.is_some()
    }

    /// Whether the agent's connection has ended.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }

    /// The permission requests waiting for an answer.
    pub fn pending_permissions(&self) -> impl Iterator<Item = &PermissionRequest> {
        self.pending.values().map(|p| &p.request)
    }

    pub(crate) fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn touch(&mut self) {
        self.last_activity = self.now;
    }

    /// Keeps a message in the log and sends it to every attached client but its origin.
    pub(crate) fn publish(
        &mut self,
        message: Value,
        origin: Option<ClientId>,
        out: &mut Vec<Output>,
    ) {
        let message = self.log.push(message, &self.settings);
        for (id, member) in &self.members {
            if member.attached && origin.as_ref() != Some(id) {
                out.push(Output::ToClient {
                    client: id.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    pub(crate) fn presence(&self, out: &mut Vec<Output>) {
        let clients: Vec<Value> = self.attached().map(Client::to_json).collect();
        let message = notification(
            PRESENCE,
            json!({ "sessionId": self.session, "clients": clients }),
        );
        for (id, member) in &self.members {
            if member.attached {
                out.push(Output::ToClient {
                    client: id.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    pub(crate) fn allowed(&self, client: &ClientId, action: &Action<'_>) -> Result<(), String> {
        let Some(member) = self.members.get(client) else {
            return Err("not connected".to_owned());
        };
        match self.policy.check(&member.client, action) {
            Verdict::Allow => Ok(()),
            Verdict::Deny(reason) => Err(reason),
        }
    }
}
