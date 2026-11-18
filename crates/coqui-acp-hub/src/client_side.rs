//! What the hub does with a message a client sent.

use serde_json::{Value, json};

use crate::client::{ClientId, Cursor};
use crate::error::HubError;
use crate::hub::{Hub, Queued, Route};
use crate::log::Gap;
use crate::output::{Note, Output};
use crate::permission::ID_PREFIX;
use crate::policy::Action;
use crate::protocol::{
    INVALID_PARAMS, META_KEY, REFUSED, error, error_with, fields, notification, own_meta, request,
    result, stamp,
};

impl Hub {
    /// Handles one JSON-RPC message from a connected client: its requests, notifications and responses to the requests
    /// the hub sent it. Answers, refusals and everything else to send come back as outputs.
    ///
    /// A client attaches with `session/load` (replay from `_meta["dev.boriken.coqui.acp/hub"].cursor`, or from the start
    /// when it names none) or `session/resume` (live only, unless it names a cursor). Until then only `initialize` and
    /// `authenticate` are answered.
    ///
    /// # Errors
    /// `HubError::UnknownClient` when the client was never connected (or was disconnected).
    pub fn from_client(
        &mut self,
        client: &ClientId,
        message: &Value,
    ) -> Result<Vec<Output>, HubError> {
        if !self.members.contains_key(client) {
            return Err(HubError::UnknownClient(client.clone()));
        }
        self.touch();
        let mut out = Vec::new();
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id");
        match (method, id) {
            (Some(method), Some(id)) => self.client_request(client, method, id, message, &mut out),
            (Some(method), None) => self.client_notification(client, method, &mut out),
            (None, Some(id)) => self.client_response(client, id, message, &mut out),
            (None, None) => out.push(self.ignored(client, "not a JSON-RPC message")),
        }
        Ok(out)
    }

    fn ignored(&self, client: &ClientId, reason: &str) -> Output {
        Output::Note(Note::Ignored {
            client: Some(client.clone()),
            reason: reason.to_owned(),
        })
    }

    fn reply(client: &ClientId, message: Value) -> Output {
        Output::ToClient {
            client: client.clone(),
            message,
        }
    }

    fn refuse(client: &ClientId, id: &Value, action: &str, reason: String, out: &mut Vec<Output>) {
        out.push(Self::reply(client, error(id, REFUSED, &reason)));
        out.push(Output::Note(Note::Refused {
            client: client.clone(),
            action: action.to_owned(),
            reason,
        }));
    }

    fn is_attached(&self, client: &ClientId) -> bool {
        self.members.get(client).is_some_and(|m| m.attached)
    }

    fn client_request(
        &mut self,
        client: &ClientId,
        method: &str,
        id: &Value,
        message: &Value,
        out: &mut Vec<Output>,
    ) {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => {
                let mut init = self.initialize.clone();
                if let Some(caps) = init.get_mut("agentCapabilities").and_then(Value::as_object_mut) {
                    caps.insert("loadSession".to_owned(), json!(true));
                    let session = caps.entry("sessionCapabilities").or_insert_with(|| json!({}));
                    if let Some(session) = session.as_object_mut() {
                        session.entry("resume").or_insert_with(|| json!({}));
                    }
                }
                out.push(Self::reply(client, result(id, init)));
            }
            "authenticate" => out.push(Self::reply(client, result(id, json!({})))),
            "session/new" => out.push(Self::reply(
                client,
                error(id, INVALID_PARAMS, "this connection serves one running session: attach with session/load or session/resume"),
            )),
            "session/load" | "session/resume" => self.attach(client, method, id, message, out),
            _ if self.ended => out.push(Self::reply(client, error(id, REFUSED, "the session has ended"))),
            _ if !self.is_attached(client) => {
                out.push(Self::reply(client, error(id, REFUSED, "not attached: send session/load or session/resume first")));
            }
            "session/prompt" => self.prompt(client, id, params, out),
            _ => {
                if let Err(reason) = self.allowed(client, &Action::Request { method }) {
                    Self::refuse(client, id, method, reason, out);
                    return;
                }
                let agent_id = format!("hub-{}", self.fresh_id());
                self.routes.insert(agent_id.clone(), Route::Client { client: client.clone(), id: id.clone() });
                out.push(Output::ToAgent(request(json!(agent_id), method, params)));
            }
        }
    }

    fn attach(
        &mut self,
        client: &ClientId,
        method: &str,
        id: &Value,
        message: &Value,
        out: &mut Vec<Output>,
    ) {
        if message.pointer("/params/sessionId").and_then(Value::as_str)
            != Some(self.session.as_str())
        {
            out.push(Self::reply(
                client,
                error(id, INVALID_PARAMS, "this connection serves another session"),
            ));
            return;
        }
        let cursor = match own_meta(message).and_then(|m| m.get("cursor")) {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_str().and_then(|c| c.parse::<Cursor>().ok()) {
                Some(cursor) => Some(cursor),
                None => {
                    out.push(Self::reply(
                        client,
                        error_with(
                            id,
                            INVALID_PARAMS,
                            "the cursor is not one this session issued",
                            json!({ "reason": "bad_cursor" }),
                        ),
                    ));
                    return;
                }
            },
        };
        let after = match (method == "session/load", cursor) {
            (_, Some(cursor)) => cursor.0,
            (true, None) => 0,
            (false, None) => self.log.head().0,
        };
        if let Some(gap) = self.log.gap(after) {
            let (message, data) = match gap {
                Gap::Ahead { head } => (
                    "the cursor is ahead of this session's log: the log it came from is gone",
                    json!({ "reason": "cursor_ahead", "head": head.to_string() }),
                ),
                Gap::Dropped { first } => (
                    "the cursor is older than the log kept: what followed it is gone",
                    json!({ "reason": "cursor_expired", "oldest": first.to_string(), "head": self.log.head().to_string() }),
                ),
            };
            out.push(Self::reply(
                client,
                error_with(id, INVALID_PARAMS, message, data),
            ));
            return;
        }
        let truncated = self.log.truncated_after(after);
        let was_attached = self.is_attached(client);
        if let Some(member) = self.members.get_mut(client) {
            member.attached = true;
        }
        let replay: Vec<Value> = self.log.after(after).map(|e| e.message.clone()).collect();
        out.extend(
            replay
                .into_iter()
                .map(|message| Self::reply(client, message)),
        );
        out.push(Self::reply(
            client,
            result(id, json!({ "_meta": { META_KEY: { "cursor": self.log.head().to_string(), "truncated": truncated } } })),
        ));
        self.permissions_for(client, out);
        if !was_attached {
            self.presence(out);
        }
    }

    fn prompt(&mut self, client: &ClientId, id: &Value, params: Value, out: &mut Vec<Output>) {
        if params.get("sessionId").and_then(Value::as_str) != Some(self.session.as_str()) {
            out.push(Self::reply(
                client,
                error(id, INVALID_PARAMS, "this connection serves another session"),
            ));
            return;
        }
        if let Err(reason) = self.allowed(client, &Action::Prompt) {
            Self::refuse(client, id, "prompt", reason, out);
            return;
        }
        if self.running.is_some() && self.queue.len() >= self.settings.max_queued_prompts {
            Self::refuse(
                client,
                id,
                "prompt",
                "the prompt queue is full".to_owned(),
                out,
            );
            return;
        }
        let prompt = self.next_prompt;
        self.next_prompt += 1;
        let who = self.members.get(client).map(|m| m.client.clone());
        let blocks = params
            .get("prompt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for block in blocks {
            let mut echo = notification(
                "session/update",
                json!({ "sessionId": self.session, "update": { "sessionUpdate": "user_message_chunk", "content": block } }),
            );
            stamp(
                &mut echo,
                fields([
                    ("client", json!(client.as_str())),
                    ("label", json!(who.as_ref().map(|c| c.label.as_str()))),
                    ("prompt", json!(prompt)),
                ]),
            );
            self.publish(echo, Some(client.clone()), out);
        }
        let queued = Queued {
            client: client.clone(),
            req_id: id.clone(),
            params,
            prompt,
        };
        if self.running.is_none() {
            self.start_prompt(queued, out);
        } else {
            self.queue.push_back(queued);
        }
    }

    fn client_notification(&mut self, client: &ClientId, method: &str, out: &mut Vec<Output>) {
        if method != "session/cancel" {
            out.push(self.ignored(client, "a notification the hub does not forward"));
            return;
        }
        if !self.is_attached(client) {
            out.push(self.ignored(client, "not attached"));
            return;
        }
        if let Err(reason) = self.allowed(client, &Action::Cancel) {
            out.push(Output::Note(Note::Refused {
                client: client.clone(),
                action: "cancel".to_owned(),
                reason,
            }));
            return;
        }
        if self.running.is_some() {
            out.push(Output::ToAgent(notification(
                "session/cancel",
                json!({ "sessionId": self.session }),
            )));
        }
        if self.settings.cancel_drops_queue {
            while let Some(queued) = self.queue.pop_front() {
                if self.members.contains_key(&queued.client) {
                    out.push(Self::reply(
                        &queued.client,
                        result(&queued.req_id, json!({ "stopReason": "cancelled" })),
                    ));
                }
            }
        }
    }

    fn client_response(
        &mut self,
        client: &ClientId,
        id: &Value,
        message: &Value,
        out: &mut Vec<Output>,
    ) {
        let Some(id) = id.as_str() else {
            out.push(self.ignored(client, "not a request the hub sent"));
            return;
        };
        if id.starts_with(ID_PREFIX) {
            self.permission_response(client, id, message, out);
        } else if let Some(agent_id) = self.agent_routes.remove(id) {
            let mut response = message.clone();
            if let Some(object) = response.as_object_mut() {
                object.insert("id".to_owned(), agent_id);
            }
            out.push(Output::ToAgent(response));
        } else {
            out.push(self.ignored(client, "not a request the hub sent"));
        }
    }
}
