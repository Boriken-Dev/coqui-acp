# `coqui-acp-page` — API header

Header-file-style reference for the `coqui-acp-page` crate, so it can be used without reading the source. It ships inside the package and is self-contained.

No Cargo features. MSRV Rust 1.89. Version `0.0.0-alpha.0`, not published. It depends on `wasm-bindgen`, exactly `0.2.128`, the version the `wasm-bindgen` command line must match, plus `js-sys` and `wasm-bindgen-futures`. No `unsafe` code. Everything exists only on `wasm32`: on any other target the crate is empty.

## API

```rust
pub async fn find(wait_ms: i32) -> Result<Option<Acp>, Error>   // by event; None if no provider answered in time

pub struct Acp;
pub async fn Acp::agents(&self) -> Result<Vec<Agent>, Error>    // this origin's grants; never prompts
pub async fn Acp::request(&self, request: &Request) -> Result<Vec<Agent>, Error>   // from a user action, or refused
pub async fn Acp::connect(&self, agent: &Agent) -> Result<Connection, Error>       // never prompts
pub async fn Acp::revoke(&self, agent: &Agent) -> Result<(), Error>

pub struct Request { pub purpose: Option<String>, pub peers: Vec<String>, pub mcp_origins: Vec<String>, pub permissions: bool }  // non_exhaustive, Default
pub fn Request::with_purpose(purpose: &str) -> Request

pub struct Agent;
pub fn Agent::id(&self) -> String        // opaque, stable, different per origin
pub fn Agent::label(&self) -> String     // text from a stranger, or the person's own name for it
pub fn Agent::peer(&self) -> Option<String>
pub fn Agent::verified(&self) -> bool    // whether messages are protected by what a proof established

pub struct Connection;
pub fn Connection::agent(&self) -> Agent
pub async fn Connection::send(&self, message: &str) -> Result<(), Error>
pub async fn Connection::receive(&self) -> Result<Option<String>, Error>   // None when it has ended
pub fn Connection::close(&self) -> Result<(), Error>
pub async fn Connection::closed(&self) -> Result<String, Error>            // page, agent, navigation, revoked or failed

pub struct Error { pub name: String, pub message: String }  // non_exhaustive; a DOMException's: NotAllowedError, NotFoundError, NetworkError
```

## Gotchas

- `request` needs transient user activation, and an `await` before it can spend that.
- Check `Connection::agent().verified()`, not the `Agent` from `agents()`, which describes the last time it was connected.
- A connection belongs to its document: navigating closes it, and the reason is in `closed`.
- A page with a content security policy needs `script-src 'wasm-unsafe-eval'` to load the module.
