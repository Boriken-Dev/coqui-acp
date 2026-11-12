//! Sessions seeded by replaying a history as context, for an agent with no way to import one.
//!
//! `import_session` records, at the host's UUID, the context its history makes
//! ([`coqui_acp_history::replay`]). The first `session/resume` or `session/load` of that UUID then
//! starts a fresh session in the agent (`session/new`), answers the host as if the session had
//! been opened, and speaks of it by the UUID from then on: the UUID the host sends is the agent's
//! own id on the way in, and the agent's id is the UUID on the way out. The first prompt of the
//! session carries the context as its first block, once.

use alloc::collections::BTreeMap;
use alloc::string::String;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

/// The most sessions waiting to be opened, so a host that imports and never opens cannot make the
/// provider hold histories without limit.
const MAX_WAITING: usize = 64;

/// A session imported by replay that no connection has opened yet.
#[derive(Debug)]
struct Waiting {
    /// The replay block that goes first in the session's first prompt; none for an empty history.
    context: Option<Value>,
}

/// Imported sessions waiting for a connection to open them, shared by the provider's connections.
#[derive(Debug, Default)]
pub(crate) struct Replays {
    waiting: Mutex<BTreeMap<String, Waiting>>,
}

impl Replays {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Waiting>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `context` for `session`; `false` when `session` already waits or too many wait.
    pub(crate) fn add(&self, session: &str, context: Option<Value>) -> Result<(), Refusal> {
        let mut waiting = self.lock();
        if waiting.contains_key(session) {
            return Err(Refusal::Named);
        }
        if waiting.len() >= MAX_WAITING {
            return Err(Refusal::Full);
        }
        waiting.insert(session.to_owned(), Waiting { context });
        Ok(())
    }

    fn take(&self, session: &str) -> Option<Waiting> {
        self.lock().remove(session)
    }
}

/// Why a session could not be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The id already names an imported session.
    Named,
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
    pub(crate) fn outbound(&mut self, replays: &Replays, message: &mut Value) {
        let Some(object) = message.as_object_mut() else {
            return;
        };
        let method = object.get("method").and_then(Value::as_str).unwrap_or("");
        let id = object
            .get("id")
            .filter(|id| id.is_string() || id.is_number());
        let id = id.map(Value::to_string);
        let session = object
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(session) = session else {
            return;
        };
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
            self.opening.insert(id, (session, waiting.context));
            return;
        }
        let Some((agent, context)) = self.live.get_mut(&session) else {
            return;
        };
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
    }

    /// Rewrites a message of the agent for the host: the answer to a stand-in open becomes the
    /// answer to the resume or load, and the agent's id becomes the UUID. Whether it changed the message.
    pub(crate) fn inbound(&mut self, replays: &Replays, message: &mut Value) -> bool {
        let Some(object) = message.as_object_mut() else {
            return false;
        };
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
                // The agent refused to open: the import waits for the next try.
                _ => {
                    let _ = replays.add(&uuid, context);
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
