//! A Rust page: asks for an agent, then runs one turn through whatever provider the browser has. Build it with
//! `cargo build -p coqui-acp-page --example page --target wasm32-unknown-unknown`, then `wasm-bindgen --target web`.
#![cfg(target_arch = "wasm32")]
#![allow(missing_docs)]

use coqui_acp_page::{Acp, Agent, Connection, Error, Request, find};
use wasm_bindgen::prelude::*;

async fn provider() -> Result<Acp, JsValue> {
    find(1000)
        .await?
        .ok_or_else(|| JsValue::from_str("no provider"))
}

async fn exchange(
    connection: &Connection,
    id: u32,
    method: &str,
    params: &str,
) -> Result<String, Error> {
    let message = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
    connection.send(&message).await?;
    Ok(connection.receive().await?.unwrap_or_default())
}

fn named(agents: Vec<Agent>, label: &str) -> Result<Agent, JsValue> {
    agents
        .into_iter()
        .find(|agent| agent.label() == label)
        .ok_or_else(|| JsValue::from_str("no agent by that name"))
}

/// Asks the person. Call it from a user action.
#[wasm_bindgen]
pub async fn ask_for_agents(purpose: String) -> Result<u32, JsValue> {
    let agents = provider()
        .await?
        .request(&Request::with_purpose(&purpose))
        .await?;
    Ok(u32::try_from(agents.len()).unwrap_or(u32::MAX))
}

/// One whole turn with an agent this page was granted, and the answers as a JSON array of strings.
#[wasm_bindgen]
pub async fn run_turn(label: String, text: String) -> Result<String, JsValue> {
    let acp = provider().await?;
    let agent = named(acp.agents().await?, &label)?;
    let connection = acp.connect(&agent).await?;
    if !connection.agent().verified() {
        return Err(JsValue::from_str("this page wants a proven agent"));
    }
    let initialized = exchange(&connection, 1, "initialize", r#"{"protocolVersion":1}"#).await?;
    let session = exchange(
        &connection,
        2,
        "session/new",
        r#"{"cwd":"/","mcpServers":[]}"#,
    )
    .await?;
    let start = session.find(r#""sessionId":""#).map_or(0, |at| at + 13);
    let id = session[start..]
        .split('"')
        .next()
        .unwrap_or_default()
        .to_owned();
    let prompt = format!(
        r#"{{"sessionId":"{id}","prompt":[{{"type":"text","text":{}}}]}}"#,
        quoted(&text)
    );
    let answered = exchange(&connection, 3, "session/prompt", &prompt).await?;
    connection.close()?;
    Ok(format!(
        "[{},{},{}]",
        quoted(&initialized),
        quoted(&session),
        quoted(&answered)
    ))
}

fn quoted(text: &str) -> String {
    js_sys::JSON::stringify(&JsValue::from_str(text))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}
