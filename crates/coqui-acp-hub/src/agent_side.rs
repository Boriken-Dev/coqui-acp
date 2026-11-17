//! What the hub does with a message the agent sent, and how a turn runs.

use serde_json::{Value, json};

use crate::hub::{Hub, Queued, Route, Running};
use crate::output::{Note, Output};
use crate::protocol::{
    ENDED, INTERNAL, REFUSED, TURN_ENDED, error, fields, notification, request, stamp,
};

impl Hub {
    /// Handles one JSON-RPC message from the agent: a `session/update` (kept and sent to every attached client), a
    /// `session/request_permission` (put to the clients the policy lets answer), another request (put to the client whose
    /// prompt is running), or a response to a prompt or a request a client made.
    pub fn from_agent(&mut self, message: &Value) -> Vec<Output> {
        self.touch();
        let mut out = Vec::new();
        let method = message.get("method").and_then(Value::as_str);
        match (method, message.get("id")) {
            (Some("session/request_permission"), Some(id)) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                self.permission_asked(id.clone(), params, &mut out);
            }
            (Some(method), Some(id)) => self.agent_request(method, id, message, &mut out),
            (Some(_), None) => self.publish(message.clone(), None, &mut out),
            (None, Some(id)) => self.agent_response(id, message, &mut out),
            (None, None) => out.push(Output::Note(Note::Ignored {
                client: None,
                reason: "not a JSON-RPC message".to_owned(),
            })),
        }
        out
    }

    /// The agent's connection ended. Every prompt not yet answered fails, the clients are told, and the session is over:
    /// the host decides what to do with the hub.
    pub fn agent_closed(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        if self.ended {
            return out;
        }
        self.touch();
        self.ended = true;
        let mut waiting: Vec<(crate::client::ClientId, Value)> = Vec::new();
        if let Some(running) = self.running.take() {
            waiting.push((running.client, running.req_id));
        }
        waiting.extend(self.queue.drain(..).map(|q| (q.client, q.req_id)));
        for (client, id) in waiting {
            if self.members.contains_key(&client) {
                out.push(Output::ToClient {
                    client,
                    message: error(&id, INTERNAL, "the agent ended"),
                });
            }
        }
        self.routes.clear();
        self.agent_routes.clear();
        self.pending.clear();
        let message = notification(ENDED, json!({ "sessionId": self.session }));
        self.publish(message, None, &mut out);
        out.push(Output::Note(Note::Ended));
        out
    }

    /// Sends a queued or fresh prompt to the agent and makes it the running turn.
    pub(crate) fn start_prompt(&mut self, queued: Queued, out: &mut Vec<Output>) {
        let agent_id = format!("hub-{}", self.fresh_id());
        let mut message = request(json!(agent_id), "session/prompt", queued.params);
        stamp(
            &mut message,
            fields([
                ("client", json!(queued.client.as_str())),
                ("prompt", json!(queued.prompt)),
            ]),
        );
        self.running = Some(Running {
            agent_id,
            client: queued.client,
            req_id: queued.req_id,
            prompt: queued.prompt,
        });
        out.push(Output::ToAgent(message));
    }

    fn agent_request(&mut self, method: &str, id: &Value, message: &Value, out: &mut Vec<Output>) {
        let target = self
            .running
            .as_ref()
            .map(|r| r.client.clone())
            .filter(|c| self.members.get(c).is_some_and(|m| m.attached));
        let Some(client) = target else {
            out.push(Output::ToAgent(error(
                id,
                REFUSED,
                "no client is attached to answer",
            )));
            return;
        };
        let hub_id = format!("hub-agent-{}", self.fresh_id());
        self.agent_routes.insert(hub_id.clone(), id.clone());
        out.push(Output::ToClient {
            client,
            message: request(
                json!(hub_id),
                method,
                message.get("params").cloned().unwrap_or(Value::Null),
            ),
        });
    }

    fn agent_response(&mut self, id: &Value, message: &Value, out: &mut Vec<Output>) {
        let Some(key) = id.as_str() else {
            out.push(Output::Note(Note::Ignored {
                client: None,
                reason: "a response to nothing the hub sent".to_owned(),
            }));
            return;
        };
        if self.running.as_ref().is_some_and(|r| r.agent_id == key) {
            self.turn_ended(message, out);
        } else if let Some(Route::Client { client, id }) = self.routes.remove(key) {
            if self.members.contains_key(&client) {
                let mut response = message.clone();
                if let Some(object) = response.as_object_mut() {
                    object.insert("id".to_owned(), id);
                }
                out.push(Output::ToClient {
                    client,
                    message: response,
                });
            }
        } else {
            out.push(Output::Note(Note::Ignored {
                client: None,
                reason: "a response to nothing the hub sent".to_owned(),
            }));
        }
    }

    fn turn_ended(&mut self, message: &Value, out: &mut Vec<Output>) {
        let Some(running) = self.running.take() else {
            return;
        };
        if self.members.contains_key(&running.client) {
            let mut response = message.clone();
            if let Some(object) = response.as_object_mut() {
                object.insert("id".to_owned(), running.req_id.clone());
            }
            out.push(Output::ToClient {
                client: running.client.clone(),
                message: response,
            });
        }
        let ended = notification(
            TURN_ENDED,
            json!({
                "sessionId": self.session,
                "prompt": running.prompt,
                "client": running.client.as_str(),
                "stopReason": message.pointer("/result/stopReason"),
                "error": message.get("error"),
            }),
        );
        self.publish(ended, Some(running.client), out);
        if let Some(next) = self.queue.pop_front() {
            self.start_prompt(next, out);
        }
    }
}
