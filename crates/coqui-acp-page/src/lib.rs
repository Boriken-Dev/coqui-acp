//! A person's Agent Client Protocol agents, from a Rust page compiled to WebAssembly: a thin layer over `document.acp`, the page
//! contract in `docs/browser.md`. The messages are strings, one complete JSON-RPC message each, so any ACP library fits on top.
//!
//! The provider, usually a browser extension, holds the connection and proves the agent. A page holds nothing but what the
//! person granted it. Everything here exists only on `wasm32`.
#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::rc::Rc;
use core::cell::RefCell;
use core::fmt;

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// What the provider refused, or what failed on the way there.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Error {
    /// The `DOMException` name: `NotAllowedError`, `NotFoundError` or `NetworkError` from a provider.
    pub name: String,
    /// What it said.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl core::error::Error for Error {}

impl From<JsValue> for Error {
    fn from(value: JsValue) -> Self {
        let text = |key: &str| get(&value, key).as_string();
        Self {
            name: text("name").unwrap_or_else(|| "Error".to_owned()),
            message: text("message")
                .or_else(|| value.as_string())
                .unwrap_or_default(),
        }
    }
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        JsValue::from_str(&error.to_string())
    }
}

fn get(object: &JsValue, key: &str) -> JsValue {
    Reflect::get(object, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// A method call, which also throws rather than rejects when a page's globals have been replaced.
fn call(object: &JsValue, method: &str, arguments: &[&JsValue]) -> Result<JsValue, Error> {
    let function: Function = get(object, method).dyn_into().map_err(|_| Error {
        name: "TypeError".to_owned(),
        message: format!("no {method}"),
    })?;
    let list = Array::new();
    for argument in arguments {
        list.push(argument);
    }
    Ok(Reflect::apply(&function, object, &list)?)
}

async fn wait(value: JsValue) -> Result<JsValue, Error> {
    Ok(JsFuture::from(Promise::from(value)).await?)
}

fn usable(acp: &JsValue) -> bool {
    get(acp, "request").is_function() && get(acp, "version").as_f64() == Some(1.0)
}

type Listener = Closure<dyn FnMut(JsValue)>;

/// Finds a provider by event, so markup in the page cannot shadow it. `None` if none answers within `wait_ms`; one may still
/// arrive later, for instance when the person clicks an extension's button.
pub async fn find(wait_ms: i32) -> Result<Option<Acp>, Error> {
    let window = js_sys::global();
    let listener: Rc<RefCell<Option<Listener>>> = Rc::default();
    let held = Rc::clone(&listener);
    let target = window.clone();
    let promise = Promise::new(&mut move |resolve, _reject| {
        let answered = resolve.clone();
        let closure = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let acp = get(&get(&event, "detail"), "acp");
            if usable(&acp) {
                let _ = answered.call1(&JsValue::NULL, &acp);
            }
        });
        let _ = call(
            &target,
            "addEventListener",
            &[&JsValue::from_str("acp:ready"), closure.as_ref()],
        );
        *held.borrow_mut() = Some(closure);
        let _ = call(
            &target,
            "setTimeout",
            &[resolve.as_ref(), &JsValue::from(wait_ms)],
        );
    });
    let constructor: Function = get(&window, "Event").dyn_into()?;
    let event = Reflect::construct(&constructor, &Array::of1(&JsValue::from_str("acp:request")))?;
    call(&window, "dispatchEvent", &[&event])?;
    let found = JsFuture::from(promise).await?;
    if let Some(closure) = listener.borrow_mut().take() {
        call(
            &window,
            "removeEventListener",
            &[&JsValue::from_str("acp:ready"), closure.as_ref()],
        )?;
    }
    Ok(usable(&found).then(|| Acp(found)))
}

/// What a page asks a person for.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Request {
    /// The page's words, shown as the page's words.
    pub purpose: Option<String>,
    /// Peer ids of agents this page already knows, listed first.
    pub peers: Vec<String>,
    /// Origins other than the page's it will name in `mcpServers`, shown to the person.
    pub mcp_origins: Vec<String>,
    /// Asks that the page answer the agent's permission requests.
    pub permissions: bool,
}

impl Request {
    /// A request with these words.
    #[must_use]
    pub fn with_purpose(purpose: &str) -> Self {
        Self {
            purpose: Some(purpose.to_owned()),
            ..Self::default()
        }
    }

    fn to_js(&self) -> Result<JsValue, Error> {
        let options = Object::new();
        let set = |key: &str, value: &JsValue| {
            Reflect::set(&options, &JsValue::from_str(key), value).map(|_| ())
        };
        let strings = |list: &[String]| {
            list.iter()
                .map(|item| JsValue::from_str(item))
                .collect::<Array>()
        };
        if let Some(purpose) = &self.purpose {
            set("purpose", &JsValue::from_str(purpose))?;
        }
        set("peers", &strings(&self.peers))?;
        set("mcpOrigins", &strings(&self.mcp_origins))?;
        set("permissions", &JsValue::from_bool(self.permissions))?;
        Ok(options.into())
    }
}

/// The provider registry, `document.acp`.
#[derive(Debug, Clone)]
pub struct Acp(JsValue);

impl Acp {
    /// What this origin was granted. Never prompts, and empty when there are no grants.
    ///
    /// # Errors
    /// What the provider refused.
    pub async fn agents(&self) -> Result<Vec<Agent>, Error> {
        agents_from(wait(call(&self.0, "agents", &[])?).await?)
    }

    /// Asks the person, in the provider's own window. Call it from a user action, or it is refused.
    ///
    /// # Errors
    /// `NotAllowedError` for no user activation or a refusal.
    pub async fn request(&self, request: &Request) -> Result<Vec<Agent>, Error> {
        agents_from(wait(call(&self.0, "request", &[&request.to_js()?])?).await?)
    }

    /// Opens a connection, never prompting. It belongs to this document: navigating closes it.
    ///
    /// # Errors
    /// `NotAllowedError` for no grant, `NetworkError` when the agent cannot be reached or does not prove itself.
    pub async fn connect(&self, agent: &Agent) -> Result<Connection, Error> {
        let connection = wait(call(&self.0, "connect", &[&agent.0])?).await?;
        let writer = call(&get(&connection, "writable"), "getWriter", &[])?;
        let reader = call(&get(&connection, "readable"), "getReader", &[])?;
        Ok(Connection {
            connection,
            writer,
            reader,
        })
    }

    /// Gives up this origin's grant of an agent, closing its connections.
    ///
    /// # Errors
    /// What the provider refused.
    pub async fn revoke(&self, agent: &Agent) -> Result<(), Error> {
        wait(call(&self.0, "revoke", &[&agent.0])?)
            .await
            .map(|_| ())
    }
}

fn agents_from(list: JsValue) -> Result<Vec<Agent>, Error> {
    let list: Array = list.dyn_into()?;
    Ok(list.iter().map(Agent).collect())
}

/// An agent this origin was granted.
#[derive(Debug, Clone)]
pub struct Agent(JsValue);

impl Agent {
    /// Opaque and stable, and different for each origin.
    #[must_use]
    pub fn id(&self) -> String {
        get(&self.0, "id").as_string().unwrap_or_default()
    }

    /// What the person calls it, or what it advertised. Trust it as far as any other text.
    #[must_use]
    pub fn label(&self) -> String {
        get(&self.0, "label").as_string().unwrap_or_default()
    }

    /// The peer id it was added with, if any.
    #[must_use]
    pub fn peer(&self) -> Option<String> {
        get(&get(&self.0, "identity"), "peer").as_string()
    }

    /// Whether the messages are protected by what a proof of that key established. A page may refuse an agent without it.
    #[must_use]
    pub fn verified(&self) -> bool {
        get(&get(&self.0, "identity"), "verified").as_bool() == Some(true)
    }
}

/// A connection to an agent, of one complete JSON-RPC message per string.
#[derive(Debug)]
pub struct Connection {
    connection: JsValue,
    writer: JsValue,
    reader: JsValue,
}

impl Connection {
    /// The agent, as it stands now that the provider has connected.
    #[must_use]
    pub fn agent(&self) -> Agent {
        Agent(get(&self.connection, "agent"))
    }

    /// Sends one message.
    ///
    /// # Errors
    /// When the connection has ended.
    pub async fn send(&self, message: &str) -> Result<(), Error> {
        wait(call(&self.writer, "write", &[&JsValue::from_str(message)])?)
            .await
            .map(|_| ())
    }

    /// The next message, or `None` when the connection has ended.
    ///
    /// # Errors
    /// When the connection failed.
    pub async fn receive(&self) -> Result<Option<String>, Error> {
        let read = wait(call(&self.reader, "read", &[])?).await?;
        if get(&read, "done").as_bool() == Some(true) {
            return Ok(None);
        }
        Ok(get(&read, "value").as_string())
    }

    /// Closes it from this side.
    ///
    /// # Errors
    /// When the page's globals have been replaced.
    pub fn close(&self) -> Result<(), Error> {
        call(&self.connection, "close", &[]).map(|_| ())
    }

    /// Why it ended, once it has: `page`, `agent`, `navigation`, `revoked` or `failed`.
    ///
    /// # Errors
    /// When the page's globals have been replaced.
    pub async fn closed(&self) -> Result<String, Error> {
        let closed = wait(get(&self.connection, "closed")).await?;
        Ok(get(&closed, "reason").as_string().unwrap_or_default())
    }
}
