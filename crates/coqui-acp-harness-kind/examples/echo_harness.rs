//! An ACP agent that answers each prompt with its text, as an `acp_harness` library any Guatiao host
//! can load. This file is a `cdylib` and nothing else, as a crate calling `providers!` has to be.
//!
//! It keeps every rule of the channel: `send` waits for room in a queue of a few messages and `close`
//! wakes it; one thread of its own answers, in order, and is the only caller of the sink; a sink
//! that refuses ends the stream. A prompt of `close` is answered, then the provider ends the
//! connection itself.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use coqui_acp_harness_kind::guatiao::library::{Object, ProviderError};
use coqui_acp_harness_kind::guatiao::{self, Provider, Status};
use coqui_acp_harness_kind::{
    AcpHarness, Channel, ChannelSink, HarnessInfo, ModelInfo, ModelListing, NO_COMMANDS, READ_ONLY,
    Requirement, StartRequest,
};
use serde_json::{Value, json};

/// How many messages wait for the sink before `send` waits too.
const QUEUE: usize = 4;

/// The requirements an echo holds to: it writes nothing and runs nothing.
const HOLDS: [&str; 2] = [READ_ONLY, NO_COMMANDS];

/// The threads this library started, joined when it is unloaded.
static THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

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

#[derive(Default, Provider)]
#[provider(AcpHarness, id = "echo_harness", crate = ::coqui_acp_harness_kind::guatiao)]
struct EchoHarness;

impl AcpHarness for EchoHarness {
    fn describe(&self) -> Result<HarnessInfo, ProviderError> {
        Ok(HarnessInfo::new("Echo", vec![1]))
    }

    fn start(
        &self,
        request: StartRequest,
        sink: Object<dyn ChannelSink>,
    ) -> Result<Object<dyn Channel>, ProviderError> {
        if request
            .requirements
            .iter()
            .any(|r| !HOLDS.contains(&r.name.as_str()))
        {
            return Err(ProviderError::new(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "the echo cannot hold every requirement asked",
            ));
        }
        let queue = Arc::new(Queue::default());
        let thread_queue = Arc::clone(&queue);
        let roots = request.working_roots;
        let thread = std::thread::Builder::new()
            .name("echo_harness".into())
            .spawn(move || deliver(sink, &thread_queue, &roots))
            .map_err(|_| {
                ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "no thread to answer on")
            })?;
        let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
        let (done, running): (Vec<_>, Vec<_>) = threads.drain(..).partition(|t| t.is_finished());
        *threads = running;
        for finished in done {
            let _ = finished.join();
        }
        threads.push(thread);
        Ok(Connection { queue }.into_object())
    }

    fn refusal(
        &self,
        requirement: Requirement,
        _environment: BTreeMap<String, String>,
    ) -> Result<Option<String>, ProviderError> {
        Ok((!HOLDS.contains(&requirement.name.as_str()))
            .then(|| "the echo does not know this requirement".to_owned()))
    }

    fn models(
        &self,
        _environment: BTreeMap<String, String>,
    ) -> Result<ModelListing, ProviderError> {
        Ok(ModelListing::of(vec![ModelInfo::new("echo")]))
    }
}

#[derive(Default)]
struct Inner {
    items: VecDeque<Vec<u8>>,
    /// Either side has ended the connection: `send` answers GONE from here on.
    closed: bool,
    /// The status the host closed with, when it was the host.
    host_status: Option<u32>,
}

#[derive(Default)]
struct Queue {
    inner: Mutex<Inner>,
    /// A message was taken or the connection ended: `send` waits on it.
    space: Condvar,
    /// A message arrived or the host closed: the thread waits on it.
    arrived: Condvar,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the connection from the provider's side, dropping what is still queued.
    fn end(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.items.clear();
        drop(inner);
        self.space.notify_all();
    }
}

/// The host's end of one connection.
struct Connection {
    queue: Arc<Queue>,
}

impl Channel for Connection {
    fn send(&self, bytes: &[u8]) -> Result<(), ProviderError> {
        let mut inner = self.queue.lock();
        loop {
            if inner.closed {
                return Err(ProviderError::new(
                    Status::GUATIAO_ERR_GONE,
                    "the connection is closed",
                ));
            }
            if inner.items.len() < QUEUE {
                break;
            }
            inner = self
                .queue
                .space
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        inner.items.push_back(bytes.to_vec());
        drop(inner);
        self.queue.arrived.notify_one();
        Ok(())
    }

    fn close(&self, status: u32) {
        let mut inner = self.queue.lock();
        if inner.closed {
            return;
        }
        inner.closed = true;
        inner.host_status = Some(status);
        drop(inner);
        self.queue.space.notify_all();
        self.queue.arrived.notify_all();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close(Status::GUATIAO_OK.into());
    }
}

/// The connection's thread, and the only caller of the sink.
fn deliver(mut sink: Object<dyn ChannelSink>, queue: &Queue, roots: &[String]) {
    let mut agent = Agent::default();
    let status = 'connection: loop {
        let message = {
            let mut inner = queue.lock();
            loop {
                if let Some(message) = inner.items.pop_front() {
                    queue.space.notify_all();
                    break Some(message);
                }
                if inner.closed {
                    break None;
                }
                inner = queue
                    .arrived
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(message) = message else {
            let status = queue.lock().host_status.unwrap_or(0);
            queue.end();
            break status;
        };
        let (answers, then_close) = agent.answer(&message, roots);
        for answer in answers {
            if let Err(refused) = sink.receive(answer.to_string().as_bytes()) {
                queue.end();
                break 'connection refused.status.into();
            }
        }
        if then_close {
            queue.end();
            break 0;
        }
    };
    sink.closed(status);
}

/// The ACP side: what each message from the client is answered with.
#[derive(Default)]
struct Agent {
    sessions: u64,
}

impl Agent {
    /// The messages to send back, and whether the connection ends after them.
    fn answer(&mut self, bytes: &[u8], roots: &[String]) -> (Vec<Value>, bool) {
        let Ok(message) = serde_json::from_slice::<Value>(bytes) else {
            return (
                vec![error(&Value::Null, -32700, "not a JSON-RPC message")],
                false,
            );
        };
        let (Some(method), Some(id)) = (message["method"].as_str(), message.get("id")) else {
            // A notification (`session/cancel` among them: every turn ends at once) or an answer.
            return (Vec::new(), false);
        };
        let params = &message["params"];
        match method {
            "initialize" => (
                vec![result(
                    id,
                    json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": false,
                            "promptCapabilities": {},
                            "mcpCapabilities": {"http": false, "sse": false},
                        },
                        "authMethods": [],
                        "agentInfo": {"name": "echo_harness", "title": "Echo", "version": "1.0.0"},
                    }),
                )],
                false,
            ),
            "session/new" => {
                let cwd = params["cwd"].as_str().unwrap_or_default();
                if !within(cwd, roots) {
                    return (
                        vec![error(id, -32602, "the cwd is outside the working roots")],
                        false,
                    );
                }
                self.sessions += 1;
                (
                    vec![result(
                        id,
                        json!({"sessionId": format!("echo-{}", self.sessions)}),
                    )],
                    false,
                )
            }
            "session/prompt" => {
                let text: String = params["prompt"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|block| block["text"].as_str())
                    .collect();
                let update = json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": params["sessionId"],
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": format!("echo: {text}")},
                        },
                    },
                });
                let end = result(id, json!({"stopReason": "end_turn"}));
                (vec![update, end], text == "close")
            }
            _ => (vec![error(id, -32601, "method not found")], false),
        }
    }
}

/// Whether `cwd` is one of `roots` or under one; any is, with none given.
fn within(cwd: &str, roots: &[String]) -> bool {
    roots.is_empty()
        || roots.iter().any(|root| {
            let root = root.trim_end_matches(['/', '\\']);
            cwd == root
                || cwd
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with(['/', '\\']))
        })
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

guatiao::providers!(
    id = "echo_harness",
    version = env!("CARGO_PKG_VERSION"),
    providers = [EchoHarness],
    unload = threads_done
);
