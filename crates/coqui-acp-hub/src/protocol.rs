//! JSON-RPC and ACP message helpers shared by both sides of the hub.

use serde_json::{Map, Value, json};

/// The `_meta` key under which the hub stamps what it knows about a message.
pub const META_KEY: &str = "dev.boriken.coqui.acp/hub";

/// Sent to every attached client when the set of attached clients changes.
pub const PRESENCE: &str = "_dev.boriken.coqui.acp/hub_presence";
/// Logged and fanned out when a prompt's turn ends.
pub const TURN_ENDED: &str = "_dev.boriken.coqui.acp/hub_turn_ended";
/// Logged and fanned out when a permission request was answered, so the other clients close it.
pub const PERMISSION_CLOSED: &str = "_dev.boriken.coqui.acp/hub_permission_closed";
/// Logged and fanned out once when the agent's connection ends.
pub const ENDED: &str = "_dev.boriken.coqui.acp/hub_ended";

pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL: i64 = -32603;
/// A request the hub refuses for a reason of its own (policy, queue full, session ended).
pub(crate) const REFUSED: i64 = -32000;

pub(crate) fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub(crate) fn request(id: Value, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub(crate) fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(crate) fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub(crate) fn error_with(id: &Value, code: i64, message: &str, data: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message, "data": data } })
}

/// Merges `fields` into the hub's own key of the message's `params._meta`.
pub(crate) fn stamp(message: &mut Value, fields: Map<String, Value>) {
    let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) else {
        return;
    };
    let meta = params.entry("_meta").or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    let Some(meta) = meta.as_object_mut() else {
        return;
    };
    let slot = meta.entry(META_KEY).or_insert_with(|| json!({}));
    match slot.as_object_mut() {
        Some(own) => own.extend(fields),
        None => *slot = Value::Object(fields),
    }
}

pub(crate) fn fields<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// `params._meta[META_KEY]` of a message a client sent.
pub(crate) fn own_meta(message: &Value) -> Option<&Map<String, Value>> {
    message
        .get("params")?
        .get("_meta")?
        .get(META_KEY)?
        .as_object()
}
