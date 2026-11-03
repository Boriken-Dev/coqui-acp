//! An agent that answers each message with its text, as a library any Guatiao host can load: the provider
//! written once, in Rust. This file is a `cdylib` and nothing else, as a crate calling `providers!` has to
//! be.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use coqui_acp_provider::guatiao::library::{Object, ProviderError};
use coqui_acp_provider::guatiao::{self, Provider, Status};
use coqui_acp_provider::{
    AgentEvent, AgentMessage, AgentProvider, AgentSession, COMPLETED_EVENT, ResumeRequest,
    StartRequest, TEXT_EVENT,
};

/// One conversation: what it has yet to report, and whether it was closed.
struct Echo {
    id: String,
    events: Mutex<VecDeque<AgentEvent>>,
    ready: Condvar,
}

impl Echo {
    fn new(id: String, first: Option<String>) -> Self {
        let echo = Self {
            id,
            events: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        };
        if let Some(text) = first {
            echo.answer(&text);
        }
        echo
    }

    fn answer(&self, text: &str) {
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        events.push_back(AgentEvent::new(TEXT_EVENT, Some(format!("echo: {text}"))));
        events.push_back(AgentEvent::new(COMPLETED_EVENT, None));
        self.ready.notify_all();
    }
}

impl AgentSession for Echo {
    fn session_id(&self) -> Result<Option<String>, ProviderError> {
        Ok(Some(self.id.clone()))
    }

    fn send_message(&self, message: AgentMessage) -> Result<(), ProviderError> {
        self.answer(&message.text);
        Ok(())
    }

    fn next_event(&self, timeout_ms: i64) -> Result<Option<AgentEvent>, ProviderError> {
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut events, _) = self
            .ready
            .wait_timeout_while(events, timeout, |events| events.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        Ok(events.pop_front())
    }

    fn interrupt(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn close(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// Starts [`Echo`] sessions, and resumes any whose identity it issued.
#[derive(Default, Provider)]
#[provider(AgentProvider, id = "echo_provider", crate = ::coqui_acp_provider::guatiao)]
struct EchoProvider;

impl AgentProvider for EchoProvider {
    fn start(&self, request: StartRequest) -> Result<Object<dyn AgentSession>, ProviderError> {
        Ok(Echo::new("echo-1".to_owned(), Some(request.prompt)).into_object())
    }

    fn resume(&self, request: ResumeRequest) -> Result<Object<dyn AgentSession>, ProviderError> {
        if !request.session_id.starts_with("echo-") {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_NOT_FOUND,
                "this provider issued no such session",
            ));
        }
        Ok(Echo::new(request.session_id, request.prompt).into_object())
    }
}

guatiao::providers!(EchoProvider);
