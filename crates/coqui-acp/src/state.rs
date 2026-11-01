//! The turn state machine: updates in, events out. No I/O, no clock, no thread.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use agent_client_protocol::schema::v1::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCallStatus, ToolKind,
};
use serde_json::Value;

use crate::event::Event;

/// Text is flushed at this size even mid-message, so it cannot grow without limit.
const TEXT_LIMIT: usize = 64 * 1024;

/// What one input produced.
#[derive(Debug, Default)]
pub(crate) struct Step {
    pub(crate) events: Vec<Event>,
    /// The session must end: a restricted tool ran without asking.
    pub(crate) abort: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Turn {
    refused: Vec<String>,
    text: String,
    kinds: BTreeMap<String, String>,
    finished: bool,
    ending: bool,
}

pub(crate) fn kind_name(kind: &ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        _ => "other",
    }
}

impl Turn {
    pub(crate) fn new(refused: Vec<String>) -> Self {
        Self {
            refused,
            ..Self::default()
        }
    }

    pub(crate) fn begin(&mut self) {
        self.finished = false;
    }

    pub(crate) const fn finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn is_refused(&self, kind: &str) -> bool {
        self.refused.iter().any(|refused| refused == kind)
    }

    /// Records what the agent says a tool is. A refused kind stays, whatever a later message claims, since the agent
    /// chooses what it says and only the first claim could have been weighed.
    pub(crate) fn announce(&mut self, id: &str, kind: &str) {
        let refused = self
            .kinds
            .get(id)
            .is_some_and(|known| self.is_refused(known));
        if !refused {
            self.kinds.insert(id.to_string(), kind.to_string());
        }
    }

    pub(crate) fn kind_of(&self, id: &str) -> Option<String> {
        self.kinds.get(id).cloned()
    }

    /// Text gathered so far, which must come out before anything that follows it.
    pub(crate) fn flush(&mut self) -> Option<Event> {
        if self.text.is_empty() {
            None
        } else {
            Some(Event::Text(core::mem::take(&mut self.text)))
        }
    }

    /// Marks the end of a turn for an event that ends one.
    pub(crate) fn note(&mut self, event: &Event) {
        if matches!(event, Event::Completed { .. } | Event::Failed(_)) {
            self.finished = true;
        }
    }

    /// One `session/update` notification's parameters. What cannot be read is ignored: an agent
    /// may be newer than this library.
    pub(crate) fn update(&mut self, params: &Value) -> Step {
        let mut step = Step::default();
        let Ok(notification) = serde_json::from_value::<SessionNotification>(params.clone()) else {
            return step;
        };
        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    self.text.push_str(&text.text);
                    if self.text.len() >= TEXT_LIMIT {
                        step.events.extend(self.flush());
                    }
                }
            }
            SessionUpdate::ToolCall(call) => {
                let kind = kind_name(&call.kind);
                let id = call.tool_call_id.0.to_string();
                self.announce(&id, kind);
                let known = self.kind_of(&id).unwrap_or_else(|| kind.to_string());
                step.events.extend(self.flush());
                step.events.push(Event::Tool {
                    id: id.clone(),
                    title: call.title,
                    name: call.name,
                    kind: kind.to_string(),
                });
                self.check_running(&known, call.status, &mut step);
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let id = update.tool_call_id.0.to_string();
                if let Some(kind) = update.fields.kind.as_ref() {
                    self.announce(&id, kind_name(kind));
                }
                if let (Some(kind), Some(status)) = (self.kind_of(&id), update.fields.status) {
                    self.check_running(&kind, status, &mut step);
                }
            }
            SessionUpdate::UsageUpdate(usage) => {
                if let Some(cost) = usage.cost {
                    step.events.extend(self.flush());
                    step.events.push(Event::Cost {
                        amount: cost.amount,
                        currency: cost.currency,
                    });
                }
            }
            _ => {}
        }
        step
    }

    /// The agent's ending of a v1 turn, by the stop reason of its `session/prompt` answer.
    pub(crate) fn stopped(&mut self, stop: Option<&str>) -> Event {
        match stop {
            Some(stop @ ("end_turn" | "max_tokens" | "max_turn_requests")) => Event::Completed {
                stop: stop.to_string(),
            },
            Some("refusal") => Event::Failed("the agent refused to continue".to_string()),
            Some("cancelled") => Event::Blocked("the turn was cancelled".to_string()),
            Some(other) => Event::Failed(format!("the agent ended the turn with {other}")),
            None => Event::Failed("the agent ended the turn with no stop reason".to_string()),
        }
    }

    /// A refused kind is never given leave, so one that runs ends the session, whether it asked or not.
    fn check_running(&mut self, kind: &str, status: ToolCallStatus, step: &mut Step) {
        if !self.is_refused(kind)
            || !matches!(
                status,
                ToolCallStatus::InProgress | ToolCallStatus::Completed
            )
            || self.ending
        {
            return;
        }
        self.ending = true;
        step.events.push(Event::Failed(format!(
            "the agent ran a {kind} tool without leave; the session was ended"
        )));
        step.abort = true;
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn chunk(text: &str) -> Value {
        json!({"sessionId": "s", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}})
    }

    fn tool(id: &str, kind: &str, status: &str) -> Value {
        json!({"sessionId": "s", "update": {"sessionUpdate": "tool_call", "toolCallId": id, "title": "T", "kind": kind, "status": status}})
    }

    #[test]
    fn text_gathers_and_comes_out_before_a_tool() {
        let mut turn = Turn::new(vec![]);
        assert!(turn.update(&chunk("hel")).events.is_empty());
        assert!(turn.update(&chunk("lo")).events.is_empty());
        let step = turn.update(&tool("1", "read", "pending"));
        assert_eq!(step.events[0], Event::Text("hello".to_string()));
        assert!(matches!(step.events[1], Event::Tool { .. }));
    }

    #[test]
    fn text_is_flushed_at_its_limit() {
        let mut turn = Turn::new(vec![]);
        let big = "x".repeat(TEXT_LIMIT);
        assert_eq!(turn.update(&chunk(&big)).events.len(), 1);
    }

    #[test]
    fn a_refused_tool_that_ran_unasked_ends_the_session_once() {
        let mut turn = Turn::new(vec!["execute".to_string()]);
        assert!(!turn.update(&tool("1", "execute", "pending")).abort);
        let step = turn.update(&tool("2", "execute", "in_progress"));
        assert!(step.abort);
        assert!(!turn.update(&tool("3", "execute", "completed")).abort);
    }

    fn update(id: &str, fields: &Value) -> Value {
        let mut update = json!({"sessionUpdate": "tool_call_update", "toolCallId": id});
        for (key, value) in fields.as_object().unwrap() {
            update[key] = value.clone();
        }
        json!({"sessionId": "s", "update": update})
    }

    #[test]
    fn a_refused_kind_that_ran_after_being_refused_ends_the_session() {
        let mut turn = Turn::new(vec!["edit".to_string()]);
        assert!(!turn.update(&tool("1", "edit", "pending")).abort);
        assert!(
            turn.update(&update("1", &json!({"status": "in_progress"})))
                .abort
        );
    }

    #[test]
    fn a_refused_kind_that_failed_after_being_refused_ends_nothing() {
        let mut turn = Turn::new(vec!["edit".to_string()]);
        assert!(!turn.update(&tool("1", "edit", "pending")).abort);
        assert!(
            !turn
                .update(&update("1", &json!({"status": "failed"})))
                .abort
        );
    }

    #[test]
    fn a_refused_kind_stays_refused_whatever_a_later_message_says() {
        let mut turn = Turn::new(vec!["execute".to_string()]);
        assert!(!turn.update(&tool("1", "execute", "pending")).abort);
        let step = turn.update(&update(
            "1",
            &json!({"kind": "read", "status": "in_progress"}),
        ));
        assert!(step.abort);
        assert_eq!(turn.kind_of("1").as_deref(), Some("execute"));

        let mut turn = Turn::new(vec!["execute".to_string()]);
        turn.announce("2", "execute");
        assert!(turn.update(&tool("2", "read", "in_progress")).abort);
    }

    #[test]
    fn a_kind_that_becomes_refused_counts_from_then_on() {
        let mut turn = Turn::new(vec!["execute".to_string()]);
        assert!(!turn.update(&tool("1", "read", "in_progress")).abort);
        let step = turn.update(&update(
            "1",
            &json!({"kind": "execute", "status": "in_progress"}),
        ));
        assert!(step.abort);
    }

    #[test]
    fn stop_reasons_map_to_events() {
        let mut turn = Turn::new(vec![]);
        assert_eq!(
            turn.stopped(Some("end_turn")),
            Event::Completed {
                stop: "end_turn".to_string()
            }
        );
        assert!(matches!(turn.stopped(Some("refusal")), Event::Failed(_)));
        assert!(matches!(turn.stopped(Some("cancelled")), Event::Blocked(_)));
        assert!(matches!(turn.stopped(Some("what")), Event::Failed(_)));
        assert!(matches!(turn.stopped(None), Event::Failed(_)));
    }

    fn any_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i32>().prop_map(|n| json!(n)),
            "[a-z_]{0,12}".prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                proptest::collection::btree_map(
                    prop_oneof![
                        Just("sessionUpdate".to_string()),
                        Just("sessionId".to_string()),
                        Just("update".to_string()),
                        "[a-z]{1,8}"
                    ],
                    inner,
                    0..6
                )
                .prop_map(|map| Value::Object(map.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn arbitrary_updates_never_panic(updates in proptest::collection::vec(any_json(), 0..24)) {
            let mut turn = Turn::new(vec!["execute".to_string()]);
            for update in &updates {
                let _ = turn.update(update);
            }
            let _ = turn.flush();
        }
    }
}
