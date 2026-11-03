//! One client: its requests, its sessions and what watches them.
//!
//! Handlers run on the connection's executor and never call a provider: every provider call has a thread of
//! its own, so a provider that blocks cannot stop the connection answering.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use agent_client_protocol::{Client, ConnectionTo, Error as RpcError, Responder, UntypedMessage};
use futures::channel::oneshot;
use guatiao::Alloc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::operation::{Launch, Operation};
use super::{AcpFacade, EVENTS_CAPABILITY, FacadeAuth, LAUNCH_META_KEY, Settings, updates};
use crate::kind::AgentProvider;

/// The longest session identifier a client may name: it reaches the provider.
const MAX_SESSION_ID: usize = 4096;

/// How often deadlines are checked, at most.
const TICK: Duration = Duration::from_millis(20);

/// What a client may put under [`LAUNCH_META_KEY`].
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LaunchMeta {
    context: Vec<String>,
    environment: BTreeMap<String, String>,
    model: Option<String>,
    effort: Option<String>,
    meta: Option<Value>,
}

pub(super) struct Connection {
    pub(super) provider: Arc<dyn AgentProvider>,
    pub(super) settings: Settings,
    auth: Option<FacadeAuth>,
    name: String,
    version: String,
    authenticated: AtomicBool,
    auth_failures: AtomicU32,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    sessions: Mutex<HashMap<String, Arc<Operation>>>,
    wake: Mutex<Option<Sender<()>>>,
}

fn invalid(what: &str) -> RpcError {
    RpcError::invalid_params().data(what.to_string())
}

fn session_id(params: &Value) -> Result<&str, RpcError> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the request names no sessionId"))
}

impl Connection {
    pub(super) fn new(facade: &AcpFacade, stop: oneshot::Sender<()>) -> Arc<Self> {
        Arc::new(Self {
            provider: facade.provider.clone(),
            settings: facade.settings.clone(),
            authenticated: AtomicBool::new(facade.auth.is_none()),
            auth: facade.auth.clone(),
            name: facade.name.clone(),
            version: facade.version.clone(),
            auth_failures: AtomicU32::new(0),
            stop: Mutex::new(Some(stop)),
            sessions: Mutex::new(HashMap::new()),
            wake: Mutex::new(None),
        })
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Operation>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The thread that holds every session to its time limits.
    pub(super) fn supervise(self: &Arc<Self>) -> std::io::Result<JoinHandle<()>> {
        let (wake, woken) = mpsc::channel::<()>();
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
        let this = self.clone();
        thread::Builder::new()
            .name("coqui-acp-supervisor".to_string())
            .spawn(move || {
                while matches!(woken.recv_timeout(TICK), Err(RecvTimeoutError::Timeout)) {
                    let now = Instant::now();
                    let sessions: Vec<_> = this.sessions().values().cloned().collect();
                    for session in sessions {
                        session.check(now);
                    }
                }
            })
    }

    /// Ends every session and waits for their providers, up to the close limit. How many are still in a
    /// provider's hands.
    pub(super) fn shut_down(&self) -> usize {
        let sessions: Vec<_> = self.sessions().values().cloned().collect();
        for session in sessions {
            session.begin_close();
        }
        let deadline = Instant::now() + self.settings.close_timeout + TICK;
        while !self.sessions().is_empty() && Instant::now() < deadline {
            thread::sleep(TICK);
        }
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.sessions().len()
    }

    /// Takes a finished session out, freeing its place: this one, not a later one under its identity.
    pub(super) fn remove(&self, operation: &Operation) {
        let mut sessions = self.sessions();
        if sessions
            .get(&operation.id)
            .is_some_and(|held| core::ptr::eq(&**held, operation))
        {
            sessions.remove(&operation.id);
        }
    }

    fn operation(&self, params: &Value) -> Result<Arc<Operation>, RpcError> {
        let id = session_id(params)?;
        self.sessions()
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::resource_not_found(Some(id.to_string())))
    }

    pub(super) fn request(
        self: &Arc<Self>,
        message: &UntypedMessage,
        responder: Responder<Value>,
        cx: ConnectionTo<Client>,
    ) {
        let params = message.params();
        let outcome = match message.method() {
            "initialize" => Ok(self.initialize()),
            "authenticate" => self.authenticate(params),
            _ if !self.authenticated.load(Ordering::SeqCst) => Err(RpcError::auth_required()),
            "session/new" => self.new_session(params, cx),
            "session/prompt" => {
                return self.prompt(params, responder);
            }
            "session/resume" => {
                return self.resume(params, responder, cx);
            }
            "session/close" => {
                return match self.operation(params) {
                    Ok(operation) => operation.close(responder),
                    Err(error) => drop(responder.respond_with_error(error)),
                };
            }
            _ => Err(RpcError::method_not_found()),
        };
        let _ = responder.respond_with_result(outcome);
    }

    pub(super) fn notification(&self, message: &UntypedMessage) {
        if message.method() != "session/cancel" {
            return;
        }
        if let Ok(operation) = self.operation(message.params()) {
            operation.cancel();
        }
    }

    fn initialize(&self) -> Value {
        let methods = self
            .auth
            .iter()
            .map(|auth| json!({"id": auth.method, "name": auth.name}))
            .collect::<Vec<_>>();
        json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {},
                "mcpCapabilities": {"http": false, "sse": false},
                "sessionCapabilities": {"resume": {}, "close": {}},
                "_meta": {EVENTS_CAPABILITY: true},
            },
            "authMethods": methods,
            "agentInfo": {"name": self.name, "version": self.version},
        })
    }

    fn authenticate(&self, params: &Value) -> Result<Value, RpcError> {
        let Some(auth) = &self.auth else {
            return Ok(json!({}));
        };
        let method = params.get("methodId").and_then(Value::as_str);
        let meta = params.get("_meta").and_then(Value::as_object);
        if method == Some(auth.method.as_str()) && meta.is_some_and(|meta| (auth.check)(meta)) {
            self.authenticated.store(true, Ordering::SeqCst);
            return Ok(json!({}));
        }
        if self.auth_failures.fetch_add(1, Ordering::SeqCst) + 1 >= self.settings.max_auth_failures
            && let Some(stop) = self
                .stop
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        {
            let _ = stop.send(());
        }
        Err(RpcError::auth_required().data("the credential was not accepted".to_string()))
    }

    /// What `session/new` and `session/resume` ask for, checked before anything is held for it.
    fn launch(&self, params: &Value) -> Result<Launch, RpcError> {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("the request names no cwd"))?;
        if !Path::new(cwd).is_absolute() {
            return Err(invalid("cwd is not an absolute path"));
        }
        if params
            .get("mcpServers")
            .and_then(Value::as_array)
            .is_some_and(|servers| !servers.is_empty())
        {
            return Err(invalid(
                "this agent connects to no MCP server of its client",
            ));
        }
        let meta: LaunchMeta = match params
            .get("_meta")
            .and_then(|meta| meta.get(LAUNCH_META_KEY))
        {
            // Its words would quote what the client sent, which may hold a credential.
            Some(raw) => serde_json::from_value(raw.clone()).map_err(|_| {
                invalid("_meta holds no launch: {context, environment, model, effort, meta}")
            })?,
            None => LaunchMeta::default(),
        };
        let mut environment = self.settings.environment.clone();
        for (name, value) in meta.environment {
            if !self.settings.client_environment.contains(&name) {
                return Err(invalid(&format!(
                    "this agent takes no environment value {name} from its client"
                )));
            }
            environment.insert(name, value);
        }
        let opaque = meta
            .meta
            .as_ref()
            .map(|meta| guatiao_serde::adapt::to_value_in(meta, Alloc::rust()))
            .transpose()
            .map_err(|_| invalid("_meta cannot be carried to the provider"))?;
        Ok(Launch {
            working_directory: cwd.to_string(),
            context: meta.context,
            environment,
            model: meta.model,
            effort: meta.effort,
            meta: opaque,
        })
    }

    /// A place for a session, or why not.
    fn admit(self: &Arc<Self>, operation: Arc<Operation>) -> Result<(), RpcError> {
        let mut sessions = self.sessions();
        if sessions.len() >= self.settings.max_sessions {
            return Err(RpcError::internal_error()
                .data("this connection holds as many sessions as it may".to_string()));
        }
        if sessions.contains_key(&operation.id) {
            return Err(invalid("that session is already open on this connection"));
        }
        sessions.insert(operation.id.clone(), operation);
        Ok(())
    }

    fn new_session(
        self: &Arc<Self>,
        params: &Value,
        cx: ConnectionTo<Client>,
    ) -> Result<Value, RpcError> {
        let launch = self.launch(params)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.admit(Operation::new(id.clone(), launch, self.clone(), cx))?;
        Ok(json!({"sessionId": id}))
    }

    fn prompt(&self, params: &Value, responder: Responder<Value>) {
        let operation = match self.operation(params) {
            Ok(operation) => operation,
            Err(error) => {
                let _ = responder.respond_with_error(error);
                return;
            }
        };
        let Some(text) = updates::prompt_text(params) else {
            let _ = responder.respond_with_error(invalid(
                "this agent takes a prompt of text and resource links only",
            ));
            return;
        };
        if text.len() > self.settings.max_prompt_bytes {
            let _ =
                responder.respond_with_error(invalid("the prompt is longer than this agent takes"));
            return;
        }
        operation.prompt(text, responder);
    }

    /// A session the provider issued, under the identity it issued. Its answer waits for the provider, so
    /// a session that no longer exists fails here, in the provider's words.
    fn resume(
        self: &Arc<Self>,
        params: &Value,
        responder: Responder<Value>,
        cx: ConnectionTo<Client>,
    ) {
        let opened = session_id(params).and_then(|id| {
            if id.len() > MAX_SESSION_ID {
                return Err(invalid("that session identifier is too long"));
            }
            let launch = self.launch(params)?;
            let operation = Operation::new(id.to_string(), launch, self.clone(), cx);
            self.admit(operation.clone())?;
            Ok(operation)
        });
        match opened {
            Ok(operation) => operation.resume(responder),
            Err(error) => {
                let _ = responder.respond_with_error(error);
            }
        }
    }
}
