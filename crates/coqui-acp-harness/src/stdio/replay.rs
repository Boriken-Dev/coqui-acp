//! Sessions seeded by replaying a history as context, for an agent with no way to import one.
//!
//! `import_session` records, at the host's UUID, the context its history makes
//! ([`coqui_acp_history::replay`]). The first `session/resume` or `session/load` of that UUID then
//! starts a fresh session in the agent (`session/new`), answers the host as if the session had
//! been opened, and speaks of it by the UUID from then on: the UUID the host sends is the agent's
//! own id on the way in, and the agent's id is the UUID on the way out. The first prompt of the
//! session carries the context as its first block, once.
//!
//! An import is bound to nothing but its UUID, so it is for a UUID the host minted: an id the
//! agent has already reported (any connection of this provider) is refused, and an import nobody
//! opens expires.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use core::time::Duration;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use serde_json::{Value, json};

/// The most sessions waiting to be opened, so a host that imports and never opens cannot make the
/// provider hold histories without limit.
const MAX_WAITING: usize = 64;
/// How long an import waits to be opened.
const WAITING_FOR: Duration = Duration::from_secs(60 * 60);
/// The most agent session ids remembered as live.
const MAX_SEEN: usize = 4096;

/// A session imported by replay that no connection has opened yet.
#[derive(Debug)]
struct Waiting {
    /// The replay block that goes first in the session's first prompt; none for an empty history.
    context: Option<Value>,
    since: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    waiting: BTreeMap<String, Waiting>,
    /// Session ids the agent has reported, oldest first.
    seen: BTreeSet<String>,
    order: VecDeque<String>,
}

/// Imported sessions waiting for a connection to open them, shared by the provider's connections.
#[derive(Debug)]
pub(crate) struct Replays {
    inner: Mutex<Inner>,
    waits: Duration,
}

impl Default for Replays {
    fn default() -> Self {
        Self::expiring(WAITING_FOR)
    }
}

impl Replays {
    /// Imports that expire after `waits`.
    pub(crate) fn expiring(waits: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            waits,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn purge(&self, inner: &mut Inner) {
        let waits = self.waits;
        inner
            .waiting
            .retain(|_, waiting| waiting.since.elapsed() < waits);
    }

    /// Keeps `context` for `session`.
    pub(crate) fn add(&self, session: &str, context: Option<Value>) -> Result<(), Refusal> {
        let mut inner = self.lock();
        self.purge(&mut inner);
        if inner.seen.contains(session) {
            return Err(Refusal::Live);
        }
        if inner.waiting.contains_key(session) {
            return Err(Refusal::Named);
        }
        if inner.waiting.len() >= MAX_WAITING {
            return Err(Refusal::Full);
        }
        inner.waiting.insert(
            session.to_owned(),
            Waiting {
                context,
                since: Instant::now(),
            },
        );
        Ok(())
    }

    fn take(&self, session: &str) -> Option<Waiting> {
        let mut inner = self.lock();
        self.purge(&mut inner);
        inner.waiting.remove(session)
    }

    /// Notes that the agent speaks of `session`, so no import may take its id.
    fn see(&self, session: &str) {
        let mut inner = self.lock();
        if inner.seen.insert(session.to_owned()) {
            inner.order.push_back(session.to_owned());
            while inner.order.len() > MAX_SEEN {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.seen.remove(&oldest);
                }
            }
        }
    }
}

/// Why a session could not be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The id already names an imported session.
    Named,
    /// The agent has reported the id as a session of its own.
    Live,
    /// Too many imported sessions wait.
    Full,
}

/// One connection's sessions opened from an import.
#[derive(Debug, Default)]
pub(crate) struct Mapping {
    /// `session/new` requests standing in for an open, by request id.
    opening: BTreeMap<String, (String, Option<Value>)>,
    /// UUID to the agent's id and the context not yet delivered.
    live: BTreeMap<String, (String, Option<Value>)>,
    /// The agent's id to the UUID.
    by_agent: BTreeMap<String, String>,
}

impl Mapping {
    /// Rewrites a message of the host for the agent: an open of an imported session becomes
    /// `session/new`, and the UUID becomes the agent's id, the first prompt carrying the context.
    /// Answers the request id of an open it turned into `session/new`, for [`Mapping::undo`].
    pub(crate) fn outbound(&mut self, replays: &Replays, message: &mut Value) -> Option<String> {
        let object = message.as_object_mut()?;
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");
        let id = object
            .get("id")
            .filter(|id| id.is_string() || id.is_number());
        let id = id.map(Value::to_string);
        let session = object
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_owned)?;
        if matches!(method, "session/load" | "session/resume")
            && let Some(id) = id
            && let Some(waiting) = replays.take(&session)
        {
            let params = object.get("params").cloned().unwrap_or(Value::Null);
            let mut fresh = json!({"cwd": params.get("cwd").cloned().unwrap_or(Value::Null)});
            fresh["mcpServers"] = params
                .get("mcpServers")
                .cloned()
                .unwrap_or_else(|| json!([]));
            if let Some(directories) = params.get("additionalDirectories") {
                fresh["additionalDirectories"] = directories.clone();
            }
            object.insert("method".into(), json!("session/new"));
            object.insert("params".into(), fresh);
            self.opening.insert(id.clone(), (session, waiting.context));
            return Some(id);
        }
        let (agent, context) = self.live.get_mut(&session)?;
        let agent = agent.clone();
        if method == "session/prompt"
            && let Some(context) = context.take()
            && let Some(prompt) = object
                .get_mut("params")
                .and_then(|params| params.get_mut("prompt"))
                .and_then(Value::as_array_mut)
        {
            prompt.insert(0, context);
        }
        if let Some(params) = object.get_mut("params").and_then(Value::as_object_mut) {
            params.insert("sessionId".into(), Value::String(agent));
        }
        None
    }

    /// Takes back an open that was turned into `session/new` but never sent, because the request
    /// was refused: the import waits again.
    pub(crate) fn undo(&mut self, replays: &Replays, request: &str) {
        if let Some((uuid, context)) = self.opening.remove(request) {
            let _ = replays.add(&uuid, context);
        }
    }

    /// Rewrites a message of the agent for the host: the answer to a stand-in open becomes the
    /// answer to the resume or load, and the agent's id becomes the UUID. Whether it changed the
    /// message.
    pub(crate) fn inbound(&mut self, replays: &Replays, message: &mut Value) -> bool {
        let Some(object) = message.as_object_mut() else {
            return false;
        };
        // Every id the agent speaks of is live, whichever connection it is on.
        if let Some(session) = object
            .get("result")
            .and_then(|result| result.get("sessionId"))
            .and_then(Value::as_str)
        {
            replays.see(session);
        }
        if let Some(session) = object
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
        {
            replays.see(session);
        }
        if !object.contains_key("method")
            && let Some(id) = object.get("id").map(Value::to_string)
            && let Some((uuid, context)) = self.opening.remove(&id)
        {
            let opened = object
                .get_mut("result")
                .and_then(Value::as_object_mut)
                .and_then(|result| result.remove("sessionId"));
            match opened {
                Some(Value::String(agent)) => {
                    self.by_agent.insert(agent.clone(), uuid.clone());
                    self.live.insert(uuid, (agent, context));
                }
                _ => {
                    // The agent did not open a session: the import waits for the next try, and the
                    // host is told so, not that the open worked.
                    let _ = replays.add(&uuid, context);
                    if object.contains_key("result") {
                        let id = object.get("id").cloned().unwrap_or(Value::Null);
                        *message = json!({"jsonrpc": "2.0", "id": id, "error": {
                            "code": -32603, "message": "the agent did not open a session"}});
                    }
                }
            }
            return true;
        }
        let Some(params) = object.get_mut("params").and_then(Value::as_object_mut) else {
            return false;
        };
        if let Some(Value::String(session)) = params.get("sessionId")
            && let Some(uuid) = self.by_agent.get(session)
        {
            let uuid = uuid.clone();
            params.insert("sessionId".into(), Value::String(uuid));
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resume(id: u64, session: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "session/resume",
            "params": {"sessionId": session, "cwd": "/w", "mcpServers": []}})
    }

    #[test]
    fn an_agent_answer_without_a_session_id_is_a_failure_and_the_import_waits_again() {
        for answer in [
            json!({"jsonrpc": "2.0", "id": 7, "result": {}}),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"sessionId": 5}}),
        ] {
            let replays = Replays::default();
            replays.add("u1", None).unwrap();
            let mut mapping = Mapping::default();
            let mut request = resume(7, "u1");
            assert_eq!(mapping.outbound(&replays, &mut request), Some("7".into()));
            assert_eq!(request["method"], "session/new");
            let mut answer = answer;
            assert!(mapping.inbound(&replays, &mut answer));
            assert!(answer.get("result").is_none(), "{answer}");
            assert_eq!(answer["error"]["code"], -32603);
            assert_eq!(
                replays.add("u1", None),
                Err(Refusal::Named),
                "it waits again"
            );
        }
    }

    #[test]
    fn a_refused_request_gives_the_import_back() {
        let replays = Replays::default();
        replays.add("u1", None).unwrap();
        let mut mapping = Mapping::default();
        let mut request = resume(3, "u1");
        let key = mapping.outbound(&replays, &mut request).unwrap();
        assert!(replays.take("u1").is_none(), "taken while the open is out");
        mapping.undo(&replays, &key);
        assert!(replays.take("u1").is_some());
        assert!(mapping.opening.is_empty());
    }

    #[test]
    fn an_id_the_agent_reported_cannot_be_imported() {
        let replays = Replays::default();
        let mut mapping = Mapping::default();
        let mut news = json!({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "live-1"}});
        mapping.inbound(&replays, &mut news);
        let mut update = json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "live-2", "update": {}}});
        mapping.inbound(&replays, &mut update);
        assert_eq!(replays.add("live-1", None), Err(Refusal::Live));
        assert_eq!(replays.add("live-2", None), Err(Refusal::Live));
        assert!(replays.add("minted", None).is_ok());
    }

    #[test]
    fn an_import_nobody_opens_expires() {
        let replays = Replays::expiring(Duration::from_millis(10));
        replays.add("u1", None).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(replays.take("u1").is_none());
        assert!(replays.add("u1", None).is_ok(), "the id is free again");
    }
}
