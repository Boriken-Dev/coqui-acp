//! What one event means on the wire. No I/O, no clock, no thread.
//!
//! A provider's text is untrusted when it reaches the client, so it is only ever data here: a stop reason,
//! an error code and a tool's kind and identifier never come from it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Value, json};

use crate::kind::{
    AgentEvent, BLOCKED_EVENT, CANCELLED_EVENT, COMPLETED_EVENT, DENIED_EVENT, FAILED_EVENT,
    LIMIT_EVENT, TEXT_EVENT, TOOL_EVENT, TURN_CANCELLED,
};

/// The most bytes one byte of [`AgentEvent::data`] takes as JSON.
const JSON_BYTE: usize = 4;

/// About what one notification costs beyond its text.
const NOTIFICATION: usize = 256;

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// The `session/prompt` answer's stop reason.
    Stop(&'static str),
    /// The `session/prompt` fails with this text.
    Failure(String),
}

/// The ending an event means, if any. The stop reasons are ACP's own.
pub(super) fn ending(event: &AgentEvent, max_label_bytes: usize) -> Option<Ending> {
    match event.kind.as_str() {
        COMPLETED_EVENT => Some(Ending::Stop("end_turn")),
        LIMIT_EVENT => Some(Ending::Stop("refusal")),
        CANCELLED_EVENT => Some(Ending::Stop("cancelled")),
        BLOCKED_EVENT if event.text.as_deref() == Some(TURN_CANCELLED) => {
            Some(Ending::Stop("cancelled"))
        }
        FAILED_EVENT => Some(Ending::Failure(label(
            event.text.as_deref().unwrap_or("the agent failed"),
            max_label_bytes,
        ))),
        _ => None,
    }
}

/// `text` cut to at most `limit` bytes on a character boundary.
fn cut(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A provider's words as a label: a tool title or a failure. No control character but a line break or a
/// tab, which a terminal would act on, and bounded.
pub(super) fn label(text: &str, limit: usize) -> String {
    cut(text, limit)
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// A tool's name or title, on one line.
fn title(text: &str, limit: usize) -> String {
    label(text, limit).replace(['\n', '\t'], " ")
}

fn update(session: &str, update: &Value) -> Value {
    json!({"sessionId": session, "update": update})
}

/// A `tool_call` for a tool the provider names. Its kind is `other`, since a name says nothing the client
/// could act on, and its identifier is ours.
pub(super) fn tool_call(session: &str, id: &str, title: &str, name: &str, status: &str) -> Value {
    update(
        session,
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title,
            "name": name,
            "kind": "other",
            "status": status,
        }),
    )
}

/// The update ending a tool call the turn started.
pub(super) fn tool_finished(session: &str, id: &str, status: &str) -> Value {
    update(
        session,
        &json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": status}),
    )
}

/// `text` as `agent_message_chunk` notifications of at most `limit` bytes.
pub(super) fn message_chunks(session: &str, text: &str, limit: usize) -> Vec<Value> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut part = cut(rest, limit);
        if part.is_empty() {
            // A limit below one character still has to make progress.
            let first = rest.chars().next().map_or(rest.len(), char::len_utf8);
            part = &rest[..first];
        }
        rest = &rest[part.len()..];
        chunks.push(update(
            session,
            &json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": part},
            }),
        ));
    }
    chunks
}

/// The `usage_update` for an event reporting a cost. ACP has no field for tokens alone.
pub(super) fn usage(session: &str, event: &AgentEvent) -> Option<Value> {
    let usage = event
        .usage
        .as_ref()
        .filter(|usage| usage.cost_micro_usd > 0)?;
    let used = usage
        .input
        .saturating_add(usage.output)
        .saturating_add(usage.cache_read)
        .saturating_add(usage.cache_write);
    #[allow(
        clippy::cast_precision_loss,
        reason = "a cost is a display figure, and a dollar amount fits a double's exact range"
    )]
    let amount = usage.cost_micro_usd as f64 / 1_000_000.0;
    Some(update(
        session,
        &json!({
            "sessionUpdate": "usage_update",
            "used": used,
            "size": 0,
            "cost": {"amount": amount, "currency": "USD"},
        }),
    ))
}

/// The tools a `denied` event names: its text, split as its documentation says.
pub(super) fn denied_tools(event: &AgentEvent) -> Vec<&str> {
    if event.kind != DENIED_EVENT {
        return Vec::new();
    }
    event
        .text
        .as_deref()
        .unwrap_or_default()
        .split(", ")
        .filter(|name| !name.is_empty())
        .collect()
}

/// The titles to announce for an event's tools, as `(title, name)`: a `tool` event's text titles its one
/// tool.
pub(super) fn tools(event: &AgentEvent, limit: usize) -> Vec<(String, String)> {
    let shared = (event.kind == TOOL_EVENT && event.tools.len() == 1)
        .then_some(event.text.as_deref())
        .flatten();
    event
        .tools
        .iter()
        .map(|name| {
            let name = title(name, limit);
            let shown = shared.map_or_else(|| name.clone(), |text| title(text, limit));
            (shown, name)
        })
        .collect()
}

/// Whether an event has parts no ACP update carries, which then go as [`EVENT_NOTIFICATION`].
///
/// [`EVENT_NOTIFICATION`]: super::EVENT_NOTIFICATION
pub(super) fn needs_extension(event: &AgentEvent) -> bool {
    if event.limit.is_some() || event.usage.is_some() || !event.quota.is_empty() {
        return true;
    }
    match event.kind.as_str() {
        TEXT_EVENT | TOOL_EVENT | COMPLETED_EVENT | FAILED_EVENT | CANCELLED_EVENT
        | DENIED_EVENT => false,
        BLOCKED_EVENT => event.text.as_deref() != Some(TURN_CANCELLED),
        _ => true,
    }
}

/// `event` for [`EVENT_NOTIFICATION`], whose line a transport bounds: its text cut, and bytes that do not
/// fit left out.
///
/// [`EVENT_NOTIFICATION`]: super::EVENT_NOTIFICATION
pub(super) fn bounded(event: &AgentEvent, limit: usize) -> AgentEvent {
    let mut event = event.clone();
    if let Some(text) = &mut event.text {
        text.truncate(cut(text, limit).len());
    }
    if event
        .data
        .as_ref()
        .is_some_and(|data| data.len() > limit / JSON_BYTE)
    {
        event.data = None;
    }
    event
}

/// About how many bytes an event queues for the client: a byte of `data` is a JSON number and a comma, and
/// a tool or a quota window costs a whole notification, whatever its name.
pub(super) fn weight(event: &AgentEvent) -> usize {
    event.kind.len()
        + event.text.as_ref().map_or(0, String::len)
        + event
            .data
            .as_ref()
            .map_or(0, |data| data.len().saturating_mul(JSON_BYTE))
        + event
            .tools
            .iter()
            .map(|tool| tool.len() + NOTIFICATION)
            .sum::<usize>()
        + event.quota.len().saturating_mul(NOTIFICATION)
}

/// The prompt's text: text blocks, and each resource link as its address, so nothing the client sent goes
/// unmentioned. `None` for any other block, which this agent did not advertise taking.
pub(super) fn prompt_text(params: &Value) -> Option<String> {
    let blocks = params.get("prompt")?.as_array()?;
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        let part = match block.get("type").and_then(Value::as_str)? {
            "text" => block.get("text").and_then(Value::as_str)?,
            "resource_link" => block.get("uri").and_then(Value::as_str)?,
            _ => return None,
        };
        parts.push(part.to_string());
    }
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, text: &str) -> AgentEvent {
        AgentEvent::new(kind, Some(text.to_string()))
    }

    #[test]
    fn a_stop_reason_comes_from_the_kind_alone() {
        assert_eq!(
            ending(&event(COMPLETED_EVENT, "max_tokens"), 100),
            Some(Ending::Stop("end_turn"))
        );
        assert_eq!(
            ending(&event(LIMIT_EVENT, "end_turn"), 100),
            Some(Ending::Stop("refusal"))
        );
        assert_eq!(
            ending(&event(BLOCKED_EVENT, TURN_CANCELLED), 100),
            Some(Ending::Stop("cancelled"))
        );
        assert_eq!(ending(&event(BLOCKED_EVENT, "refused"), 100), None);
        assert_eq!(ending(&event("end_turn", ""), 100), None);
    }

    #[test]
    fn a_label_loses_what_a_terminal_would_act_on_and_is_bounded() {
        assert_eq!(label("ok\u{1b}[2J\u{7}\nnext", 100), "ok[2J\nnext");
        assert_eq!(label("ééé", 3), "é");
        assert_eq!(title("a\nb\tc", 100), "a b c");
    }

    #[test]
    fn text_splits_on_character_boundaries_and_keeps_every_byte() {
        let text = "é".repeat(10);
        let chunks = message_chunks("s", &text, 3);
        assert_eq!(chunks.len(), 10);
        let joined: String = chunks
            .iter()
            .map(|chunk| chunk["update"]["content"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(joined, text);
        assert_eq!(message_chunks("s", "é", 1).len(), 1);
        assert!(message_chunks("s", "", 3).is_empty());
    }

    #[test]
    fn a_prompt_refuses_what_it_cannot_carry() {
        let params = json!({"prompt": [
            {"type": "text", "text": "read"},
            {"type": "resource_link", "uri": "file:///a", "name": "a"},
        ]});
        assert_eq!(prompt_text(&params).unwrap(), "read\n\nfile:///a");
        assert!(prompt_text(&json!({"prompt": [{"type": "image", "data": "x"}]})).is_none());
        assert!(prompt_text(&json!({})).is_none());
    }

    #[test]
    fn a_denied_event_names_tools_that_did_not_run() {
        assert_eq!(
            denied_tools(&event(DENIED_EVENT, "Bash, WebFetch")),
            ["Bash", "WebFetch"]
        );
        assert!(denied_tools(&event(TEXT_EVENT, "Bash, WebFetch")).is_empty());
    }

    #[test]
    fn a_kind_no_update_carries_goes_as_the_extension() {
        assert!(needs_extension(&event("notice", "x")));
        assert!(needs_extension(&event(BLOCKED_EVENT, "refused")));
        assert!(!needs_extension(&event(BLOCKED_EVENT, TURN_CANCELLED)));
        assert!(!needs_extension(&event(TEXT_EVENT, "x")));
    }
}
