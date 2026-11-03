//! One ACP session and the provider session behind it.
//!
//! A provider may block, fail or panic in any call, and nothing here can stop a thread stuck in one. So every
//! call runs on a thread that holds only this session up, is timed by the connection's supervisor, and an
//! overdue one ends the session while the thread stays until the provider lets go of it.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::panic::AssertUnwindSafe;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;
use std::panic::catch_unwind;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Instant;

use agent_client_protocol::{Client, ConnectionTo, Error as RpcError, Responder, UntypedMessage};
use guatiao::library::{Object, ProviderError};
use serde_json::{Value, json};

use super::EVENT_NOTIFICATION;
use super::connection::Connection;
use super::updates::{self, Ending};
use crate::kind::{
    AgentEvent, AgentMessage, AgentSession, FAILED_EVENT, ResumeRequest, StartRequest,
};

/// How often a closing session looks again for a `start` still in flight.
const START_POLL: Duration = Duration::from_millis(10);

/// What a session starts from, kept for the first prompt.
pub(super) struct Launch {
    pub(super) working_directory: String,
    pub(super) context: Vec<String>,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    pub(super) meta: Option<guatiao::Value>,
}

impl Launch {
    fn start(&self, prompt: String) -> StartRequest {
        StartRequest {
            working_directory: self.working_directory.clone(),
            prompt,
            context: self.context.clone(),
            environment: self.environment.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            meta: self.meta.clone(),
        }
    }

    fn resume(&self, session_id: &str) -> ResumeRequest {
        ResumeRequest {
            session_id: session_id.to_string(),
            working_directory: self.working_directory.clone(),
            prompt: None,
            context: self.context.clone(),
            environment: self.environment.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            meta: self.meta.clone(),
        }
    }
}

enum Turn {
    Idle,
    /// A prompt waits for its answer. `cancelled` is when `session/cancel` came.
    Active {
        responder: Box<Responder<Value>>,
        cancelled: Option<Instant>,
    },
    /// The prompt was answered `cancelled` and the provider has not ended the turn. What it sends is the
    /// old turn's, so a new prompt has to wait, or the old ending would answer it.
    Draining,
}

struct State {
    turn: Turn,
    /// What the provider sent since the turn began, against [`Settings::max_turn_bytes`].
    ///
    /// [`Settings::max_turn_bytes`]: super::Settings::max_turn_bytes
    bytes: usize,
    /// Why the session cannot go on.
    dead: Option<String>,
    open_tools: Vec<String>,
    provider_id: Option<String>,
    /// The `session/resume` waiting for the provider's.
    resuming: Option<Responder<Value>>,
    /// `session/close` calls waiting for the provider's.
    closing: Vec<Responder<Value>>,
    closed: bool,
}

#[derive(Clone, Copy)]
enum What {
    Start,
    Resume,
    SendMessage,
    NextEvent,
    SessionId,
    Interrupt,
    Close,
}

impl What {
    const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resume => "resume",
            Self::SendMessage => "send_message",
            Self::NextEvent => "next_event",
            Self::SessionId => "session_id",
            Self::Interrupt => "interrupt",
            Self::Close => "close",
        }
    }
}

struct Call {
    id: u64,
    what: What,
    started: Instant,
    limit: Duration,
}

pub(super) struct Operation {
    pub(super) id: String,
    launch: Launch,
    connection: Arc<Connection>,
    cx: ConnectionTo<Client>,
    session: Mutex<Option<Arc<Object<dyn AgentSession>>>>,
    state: Mutex<State>,
    calls: Mutex<Vec<Call>>,
    next_call: AtomicU64,
    threads: AtomicUsize,
    closing: AtomicBool,
    close_called: AtomicBool,
    interrupting: AtomicBool,
    starting: AtomicBool,
}

/// Counts a thread of the session, which leaves the connection's books with its last one.
struct Running(Arc<Operation>);

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.threads.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.finish_if_done();
        }
    }
}

fn failure(text: String) -> RpcError {
    RpcError::internal_error().data(text)
}

impl Operation {
    pub(super) fn new(
        id: String,
        launch: Launch,
        connection: Arc<Connection>,
        cx: ConnectionTo<Client>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            launch,
            connection,
            cx,
            session: Mutex::new(None),
            state: Mutex::new(State {
                turn: Turn::Idle,
                bytes: 0,
                dead: None,
                open_tools: Vec::new(),
                provider_id: None,
                resuming: None,
                closing: Vec::new(),
                closed: false,
            }),
            calls: Mutex::new(Vec::new()),
            next_call: AtomicU64::new(0),
            threads: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            close_called: AtomicBool::new(false),
            interrupting: AtomicBool::new(false),
            starting: AtomicBool::new(false),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn calls(&self) -> MutexGuard<'_, Vec<Call>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn provider_session(&self) -> Option<Arc<Object<dyn AgentSession>>> {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Runs `work` on a thread of this session. A session that cannot start one answers `false`.
    fn spawn(self: &Arc<Self>, name: &str, work: impl FnOnce(&Arc<Self>) + Send + 'static) -> bool {
        self.threads.fetch_add(1, Ordering::SeqCst);
        let running = Running(self.clone());
        thread::Builder::new()
            .name(format!("coqui-acp-{name}"))
            .spawn(move || {
                work(&running.0);
                drop(running);
            })
            .is_ok()
    }

    /// One provider call: timed by the supervisor, and a panic stops here. The panic's words stay out of
    /// what the client is told, since they can hold anything the provider held.
    fn call<T>(
        &self,
        what: What,
        limit: Duration,
        work: impl FnOnce() -> Result<T, ProviderError>,
    ) -> Result<T, String> {
        let id = self.next_call.fetch_add(1, Ordering::SeqCst);
        self.calls().push(Call {
            id,
            what,
            started: Instant::now(),
            limit,
        });
        let outcome = catch_unwind(AssertUnwindSafe(work));
        self.calls().retain(|call| call.id != id);
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!("the provider panicked in {}", what.name())),
        }
    }

    fn notify(&self, method: &str, params: Value) {
        if let Ok(message) = UntypedMessage::new(method, params) {
            let _ = self.cx.send_notification(message);
        }
    }

    fn update(&self, params: Value) {
        self.notify("session/update", params);
    }

    /// Answers the prompt, ending the turn's tool calls first so none is left running.
    fn answer(&self, responder: Responder<Value>, ending: Ending, open_tools: Vec<String>) {
        let status = if ending == Ending::Stop("end_turn") {
            "completed"
        } else {
            "failed"
        };
        for tool in open_tools {
            self.update(updates::tool_finished(&self.id, &tool, status));
        }
        let _ = match ending {
            Ending::Stop(stop) => responder.respond(json!({"stopReason": stop})),
            Ending::Failure(text) => responder.respond_with_error(failure(text)),
        };
    }

    /// Ends the current turn, if one is waiting: `next` is what the session is in afterwards. ACP answers a
    /// cancelled turn `cancelled`, however the provider came to end it.
    fn end_turn(&self, ending: Ending, next: Turn) {
        let (responder, cancelled, open_tools) = {
            let mut state = self.state();
            match core::mem::replace(&mut state.turn, next) {
                Turn::Active {
                    responder,
                    cancelled,
                } => (responder, cancelled, core::mem::take(&mut state.open_tools)),
                other => {
                    state.turn = other;
                    return;
                }
            }
        };
        let ending = if cancelled.is_some() {
            Ending::Stop("cancelled")
        } else {
            ending
        };
        self.answer(*responder, ending, open_tools);
    }

    /// Ends the session for a reason the client should hear.
    fn abandon(self: &Arc<Self>, reason: String) {
        {
            let mut state = self.state();
            if state.dead.is_some() {
                return;
            }
            state.dead = Some(reason.clone());
        }
        if let Some(responder) = self.state().resuming.take() {
            let _ = responder.respond_with_error(failure(reason.clone()));
        }
        // With no prompt waiting there is no answer to carry it, and the client should still hear.
        if !matches!(self.state().turn, Turn::Active { .. }) {
            let event = AgentEvent::new(FAILED_EVENT, Some(reason.clone()));
            self.notify(
                EVENT_NOTIFICATION,
                json!({"sessionId": self.id, "event": event}),
            );
        }
        self.end_turn(Ending::Failure(reason), Turn::Idle);
        self.begin_close();
    }

    /// Takes a prompt. Everything the provider is asked happens on a thread of its own.
    pub(super) fn prompt(self: &Arc<Self>, text: String, responder: Responder<Value>) {
        let refused = {
            let mut state = self.state();
            let why = if let Some(reason) = &state.dead {
                Some(format!("this session has ended: {reason}"))
            } else if self.closing.load(Ordering::SeqCst) {
                Some("this session is closing".to_string())
            } else {
                match state.turn {
                    Turn::Idle => None,
                    Turn::Active { .. } => Some("a turn is in progress".to_string()),
                    Turn::Draining => {
                        Some("the provider has not ended the cancelled turn".to_string())
                    }
                }
            };
            match why {
                Some(why) => Some((why, responder)),
                None => {
                    state.bytes = 0;
                    state.turn = Turn::Active {
                        responder: Box::new(responder),
                        cancelled: None,
                    };
                    None
                }
            }
        };
        if let Some((why, responder)) = refused {
            let _ = responder.respond_with_error(failure(why));
            return;
        }
        if !self.spawn("prompt", move |this| this.deliver_prompt(text)) {
            self.end_turn(
                Ending::Failure("no thread could be started for the prompt".to_string()),
                Turn::Idle,
            );
        }
    }

    fn deliver_prompt(self: &Arc<Self>, text: String) {
        let outcome = match self.provider_session() {
            Some(session) => self.call(
                What::SendMessage,
                self.connection.settings.call_timeout,
                || session.send_message(AgentMessage { text }),
            ),
            None => self.start(text),
        };
        if let Err(why) = outcome {
            let label = updates::label(&why, self.connection.settings.max_label_bytes);
            self.end_turn(Ending::Failure(label), Turn::Idle);
            // The provider never took it, so no ending will come for a turn already answered.
            let mut state = self.state();
            if matches!(state.turn, Turn::Draining) {
                state.turn = Turn::Idle;
            }
        }
    }

    fn start(self: &Arc<Self>, prompt: String) -> Result<(), String> {
        let request = self.launch.start(prompt);
        let provider = self.connection.provider.clone();
        let limit = self.connection.settings.start_timeout;
        self.starting.store(true, Ordering::SeqCst);
        let started = self.call(What::Start, limit, || provider.start(request));
        let adopted = started.map(|session| self.adopt(session));
        self.starting.store(false, Ordering::SeqCst);
        adopted
    }

    /// Resumes on a thread of its own, and answers `session/resume` with how it went.
    pub(super) fn resume(self: &Arc<Self>, responder: Responder<Value>) {
        self.state().resuming = Some(responder);
        let spawned = self.spawn("resume", |this| {
            let request = this.launch.resume(&this.id);
            let provider = this.connection.provider.clone();
            let limit = this.connection.settings.start_timeout;
            this.starting.store(true, Ordering::SeqCst);
            let resumed = this.call(What::Resume, limit, || provider.resume(request));
            let adopted = resumed.map(|session| this.adopt(session));
            this.starting.store(false, Ordering::SeqCst);
            let responder = this.state().resuming.take();
            match adopted {
                Ok(()) => {
                    if let Some(responder) = responder {
                        let _ = responder.respond(json!({}));
                    }
                }
                // Nothing is held, so the identity is free again before the client hears.
                Err(why) => {
                    this.state().closed = true;
                    this.connection.remove(this);
                    if let Some(responder) = responder {
                        let label = updates::label(&why, this.connection.settings.max_label_bytes);
                        let _ = responder.respond_with_error(failure(label));
                    }
                }
            }
        });
        if !spawned {
            self.state().closed = true;
            self.connection.remove(self);
            if let Some(responder) = self.state().resuming.take() {
                let _ =
                    responder.respond_with_error(failure("no thread could be started".to_string()));
            }
        }
    }

    /// Keeps a session the provider handed over and starts reading its events. One that arrives after the
    /// session ended is closed here, so no harness outlives what asked for it.
    fn adopt(self: &Arc<Self>, session: Object<dyn AgentSession>) {
        let session = Arc::new(session);
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        if self.closing.load(Ordering::SeqCst) {
            let _ = self.close_provider(&session);
            return;
        }
        if !self.spawn("pump", {
            let session = session.clone();
            move |this| this.pump(&session)
        }) {
            self.abandon("no thread could be started to read the provider".to_string());
            return;
        }
        // A cancel that came while the provider was starting had nothing to reach.
        let cancelled = matches!(
            self.state().turn,
            Turn::Active {
                cancelled: Some(_),
                ..
            } | Turn::Draining
        );
        if cancelled {
            self.interrupt();
        }
    }

    /// Reads the provider's events until the session closes or the provider fails.
    fn pump(self: &Arc<Self>, session: &Object<dyn AgentSession>) {
        let settings = &self.connection.settings;
        let poll = settings.poll.max(Duration::from_millis(1));
        let poll_ms = i64::try_from(poll.as_millis()).unwrap_or(i64::MAX);
        let limit = poll.saturating_add(settings.call_timeout);
        self.announce_identity(session);
        while !self.closing.load(Ordering::SeqCst) {
            let began = Instant::now();
            match self.call(What::NextEvent, limit, || session.next_event(poll_ms)) {
                // The client has been told this session is over, so nothing more of it goes out.
                Ok(Some(_)) if self.closing.load(Ordering::SeqCst) => return,
                Ok(Some(event)) => {
                    self.announce_identity(session);
                    self.deliver(&event);
                }
                // A provider that returns at once must not make this loop spin.
                Ok(None) => thread::sleep(poll.saturating_sub(began.elapsed())),
                Err(why) => {
                    self.abandon(updates::label(&why, settings.max_label_bytes));
                    return;
                }
            }
        }
    }

    /// Tells the client the provider's own identity for this session, once it is known: what a later
    /// `session/resume` names. A harness's first event may be what reports it.
    fn announce_identity(self: &Arc<Self>, session: &Object<dyn AgentSession>) {
        if self.state().provider_id.is_some() {
            return;
        }
        let limit = self.connection.settings.call_timeout;
        let Ok(Some(id)) = self.call(What::SessionId, limit, || session.session_id()) else {
            return;
        };
        let id = updates::label(&id, self.connection.settings.max_label_bytes);
        self.state().provider_id = Some(id.clone());
        self.notify(
            EVENT_NOTIFICATION,
            json!({"sessionId": self.id, "providerSessionId": id}),
        );
    }

    /// Passes one event on, and ends the turn if it does.
    fn deliver(self: &Arc<Self>, event: &AgentEvent) {
        let settings = &self.connection.settings;
        let ending = updates::ending(event, settings.max_label_bytes);
        let (overflowed, drained) = {
            let mut state = self.state();
            if matches!(state.turn, Turn::Draining) {
                if ending.is_some() {
                    state.turn = Turn::Idle;
                }
                (false, true)
            } else {
                state.bytes = state.bytes.saturating_add(updates::weight(event));
                (state.bytes > settings.max_turn_bytes, false)
            }
        };
        if drained {
            return;
        }
        if overflowed {
            self.abandon("the provider sent more than this agent passes on".to_string());
            return;
        }
        self.forward(event);
        if let Some(ending) = ending {
            self.end_turn(ending, Turn::Idle);
        }
    }

    fn forward(&self, event: &AgentEvent) {
        let settings = &self.connection.settings;
        if event.kind == crate::kind::TEXT_EVENT
            && let Some(text) = &event.text
        {
            for chunk in updates::message_chunks(&self.id, text, settings.max_update_bytes) {
                self.update(chunk);
            }
        }
        for (title, name) in updates::tools(event, settings.max_label_bytes) {
            let id = uuid::Uuid::new_v4().to_string();
            self.update(updates::tool_call(
                &self.id,
                &id,
                &title,
                &name,
                "in_progress",
            ));
            self.state().open_tools.push(id);
        }
        for name in updates::denied_tools(event) {
            let id = uuid::Uuid::new_v4().to_string();
            let name = updates::label(name, settings.max_label_bytes);
            self.update(updates::tool_call(&self.id, &id, &name, &name, "failed"));
        }
        if let Some(usage) = updates::usage(&self.id, event) {
            self.update(usage);
        }
        if updates::needs_extension(event) {
            let provider = self.state().provider_id.clone();
            self.notify(
                EVENT_NOTIFICATION,
                json!({
                    "sessionId": self.id,
                    "providerSessionId": provider,
                    "event": updates::bounded(event, settings.max_update_bytes),
                }),
            );
        }
    }

    /// `session/cancel`: remembers it, whichever call it arrives during, and asks the provider.
    pub(super) fn cancel(self: &Arc<Self>) {
        {
            let mut state = self.state();
            let Turn::Active { cancelled, .. } = &mut state.turn else {
                return;
            };
            if cancelled.is_some() {
                return;
            }
            *cancelled = Some(Instant::now());
        }
        self.interrupt();
    }

    /// One `interrupt` at a time, however many `session/cancel` a client sends.
    fn interrupt(self: &Arc<Self>) {
        let Some(session) = self.provider_session() else {
            return;
        };
        if self.interrupting.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = self.spawn("interrupt", move |this| {
            let limit = this.connection.settings.call_timeout;
            let _ = this.call(What::Interrupt, limit, || session.interrupt());
            this.interrupting.store(false, Ordering::SeqCst);
        });
        if !spawned {
            self.interrupting.store(false, Ordering::SeqCst);
        }
    }

    /// `session/close`: answered once the provider has closed, or when it runs out of time.
    pub(super) fn close(self: &Arc<Self>, responder: Responder<Value>) {
        let done = {
            let mut state = self.state();
            if state.closed {
                Some(responder)
            } else {
                state.closing.push(responder);
                None
            }
        };
        if let Some(responder) = done {
            let _ = responder.respond(json!({}));
            return;
        }
        self.begin_close();
    }

    /// Ends the session: a waiting prompt is answered `cancelled`, and the provider's `close` runs on a
    /// thread of its own. A `start` in flight closes what it returns, if it returns.
    pub(super) fn begin_close(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(responder) = self.state().resuming.take() {
            let _ = responder.respond_with_error(failure("this session is closing".to_string()));
        }
        self.end_turn(Ending::Stop("cancelled"), Turn::Idle);
        let spawned = self.spawn("close", |this| {
            let limit = this.connection.settings.close_timeout;
            let began = Instant::now();
            while this.starting.load(Ordering::SeqCst) && began.elapsed() < limit {
                thread::sleep(START_POLL);
            }
            let outcome = if this.starting.load(Ordering::SeqCst) {
                Err("the provider is still starting this session".to_string())
            } else {
                this.provider_session()
                    .map_or(Ok(()), |session| this.close_provider(&session))
            };
            this.closed(outcome);
        });
        if !spawned {
            self.closed(Err("no thread could be started to close it".to_string()));
        }
    }

    /// The provider's `close`, once.
    fn close_provider(&self, session: &Object<dyn AgentSession>) -> Result<(), String> {
        if self.close_called.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let limit = self.connection.settings.close_timeout;
        self.call(What::Close, limit, || session.close())
    }

    fn closed(self: &Arc<Self>, outcome: Result<(), String>) {
        let waiting = {
            let mut state = self.state();
            state.closed = true;
            core::mem::take(&mut state.closing)
        };
        for responder in waiting {
            let _ = match &outcome {
                Ok(()) => responder.respond(json!({})),
                Err(why) => responder.respond_with_error(failure(updates::label(
                    why,
                    self.connection.settings.max_label_bytes,
                ))),
            };
        }
    }

    /// Frees the session's place once nothing of it is left running.
    fn finish_if_done(&self) {
        if self.state().closed && self.threads.load(Ordering::SeqCst) == 0 {
            self.connection.remove(self);
        }
    }

    /// What only time can end: a provider call past its limit, and a cancelled turn past its grace.
    pub(super) fn check(self: &Arc<Self>, now: Instant) {
        let overdue = self
            .calls()
            .iter()
            .find(|call| now.duration_since(call.started) > call.limit)
            .map(|call| (call.what, call.limit));
        match overdue {
            Some((What::Close, limit)) => {
                let why = format!("the provider did not close within {} ms", limit.as_millis());
                self.closed(Err(why));
            }
            Some((what, limit)) => {
                self.abandon(format!(
                    "the provider did not answer {} within {} ms",
                    what.name(),
                    limit.as_millis()
                ));
            }
            None => {}
        }
        let grace = self.connection.settings.cancel_grace;
        let late = matches!(
            self.state().turn,
            Turn::Active { cancelled: Some(at), .. } if now.duration_since(at) > grace
        );
        if late {
            self.end_turn(Ending::Stop("cancelled"), Turn::Draining);
        }
    }
}
