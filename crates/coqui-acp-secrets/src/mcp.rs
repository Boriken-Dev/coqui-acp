//! The tools as an MCP server over JSON-RPC 2.0, so a host can hand them to an agent on any transport it already has.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use crate::SessionTools;

const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

impl SessionTools {
    /// Answers one JSON-RPC message of an MCP client: `initialize`, `ping`, `tools/list` and `tools/call`. A notification (no `id`)
    /// has no answer. The server has no state beyond the session, so a host serves one connection per session.
    #[must_use]
    pub fn handle_message(&self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let method = message.get("method").and_then(Value::as_str);
        let params = message.get("params").unwrap_or(&Value::Null);
        let reply = match method {
            Some("initialize") => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                Ok(json!({
                    "protocolVersion": asked.filter(|v| SUPPORTED.contains(v)).unwrap_or(SUPPORTED[0]),
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "coqui-acp-secrets", "version": env!("CARGO_PKG_VERSION")},
                }))
            }
            Some("ping") => Ok(json!({})),
            Some("tools/list") => Ok(json!({
                "tools": self.definitions().iter().map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })).collect::<Vec<_>>(),
            })),
            Some("tools/call") => match params.get("name").and_then(Value::as_str) {
                Some(name) => {
                    let result = self.call(name, params.get("arguments").unwrap_or(&json!({})));
                    Ok(json!({
                        "content": [{"type": "text", "text": result.text}],
                        "isError": result.is_error,
                    }))
                }
                None => Err((-32602, "tools/call needs a name")),
            },
            _ => Err((-32601, "method not found")),
        };
        Some(match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, text)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
            }
        })
    }

    /// Answers one line of a stdio transport: the JSON text of a message in, the JSON text of the answer out. A line that is not
    /// JSON is answered with a parse error.
    #[must_use]
    pub fn handle_line(&self, line: &str) -> Option<String> {
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle_message(&message).map(|reply| reply.to_string()),
            Err(_) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})
                    .to_string(),
            ),
        }
    }

    /// Serves the session's tools over a line transport until the reader ends: one message per line each way.
    ///
    /// # Errors
    /// The transport's own.
    pub fn serve(&self, reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line) {
                writeln!(writer, "{reply}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }
}
