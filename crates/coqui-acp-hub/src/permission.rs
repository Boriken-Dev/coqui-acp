//! Permission requests: sent to every client the policy lets answer, closed for the rest when one answers.

use alloc::collections::BTreeSet;

use serde_json::{Value, json};

use crate::client::ClientId;
use crate::error::HubError;
use crate::hub::{Hub, Pending};
use crate::output::{Answerer, Note, Output, PermissionRequest};
use crate::policy::Action;
use crate::protocol::{PERMISSION_CLOSED, fields, notification, request, result, stamp};

pub(crate) const ID_PREFIX: &str = "hub-perm-";

/// Whether `result` is a permission outcome: cancelled, or one of the options the request offered (when it lists any).
fn valid_outcome(params: &Value, result: &Value) -> bool {
    let Some(outcome) = result.get("outcome") else {
        return false;
    };
    match outcome.get("outcome").and_then(Value::as_str) {
        Some("cancelled") => true,
        Some("selected") => {
            let Some(chosen) = outcome.get("optionId").and_then(Value::as_str) else {
                return false;
            };
            match params.get("options").and_then(Value::as_array) {
                Some(options) => options
                    .iter()
                    .any(|o| o.get("optionId").and_then(Value::as_str) == Some(chosen)),
                None => true,
            }
        }
        _ => false,
    }
}

fn is_cancelled(result: &Value) -> bool {
    result.pointer("/outcome/outcome").and_then(Value::as_str) == Some("cancelled")
}

impl Hub {
    fn permission_message(&self, request_id: u64, params: &Value) -> Value {
        let mut message = request(
            json!(format!("{ID_PREFIX}{request_id}")),
            "session/request_permission",
            params.clone(),
        );
        stamp(&mut message, fields([("request", json!(request_id))]));
        message
    }

    /// The agent asked: send it to every attached client the policy lets answer.
    pub(crate) fn permission_asked(
        &mut self,
        agent_id: Value,
        params: Value,
        out: &mut Vec<Output>,
    ) {
        let id = self.fresh_id();
        let request = PermissionRequest {
            id,
            params,
            asked_at: self.now,
        };
        let mut addressed = BTreeSet::new();
        for (client, member) in &self.members {
            if member.attached
                && self
                    .policy
                    .check(&member.client, &Action::Answer { request: &request })
                    == crate::policy::Verdict::Allow
            {
                addressed.insert(client.clone());
                out.push(Output::ToClient {
                    client: client.clone(),
                    message: self.permission_message(id, &request.params),
                });
            }
        }
        self.pending.insert(
            id,
            Pending {
                request,
                agent_id,
                addressed,
                withdrawn: BTreeSet::new(),
                flagged: false,
            },
        );
    }

    /// A client has just attached: it is asked what is still open, if the policy lets it answer.
    pub(crate) fn permissions_for(&mut self, client: &ClientId, out: &mut Vec<Output>) {
        let Some(member) = self.members.get(client) else {
            return;
        };
        let ids: Vec<u64> = self.pending.keys().copied().collect();
        for id in ids {
            let Some(pending) = self.pending.get(&id) else {
                continue;
            };
            if self.policy.check(
                &member.client,
                &Action::Answer {
                    request: &pending.request,
                },
            ) != crate::policy::Verdict::Allow
            {
                continue;
            }
            let message = self.permission_message(id, &pending.request.params);
            if let Some(pending) = self.pending.get_mut(&id) {
                pending.addressed.insert(client.clone());
                pending.withdrawn.remove(client);
            }
            out.push(Output::ToClient {
                client: client.clone(),
                message,
            });
        }
    }

    /// A client's response to one of the hub's `session/request_permission` requests.
    pub(crate) fn permission_response(
        &mut self,
        client: &ClientId,
        id: &str,
        message: &Value,
        out: &mut Vec<Output>,
    ) {
        let ignored = |reason: &str| {
            Output::Note(Note::Ignored {
                client: Some(client.clone()),
                reason: reason.to_owned(),
            })
        };
        let Some(number) = id
            .strip_prefix(ID_PREFIX)
            .and_then(|n| n.parse::<u64>().ok())
        else {
            out.push(ignored("not a request the hub sent"));
            return;
        };
        let Some(pending) = self.pending.get(&number) else {
            out.push(ignored("the permission request was already answered"));
            return;
        };
        if !pending.addressed.contains(client) {
            out.push(ignored("the permission request was not put to this client"));
            return;
        }
        if let Err(reason) = self.allowed(
            client,
            &Action::Answer {
                request: &pending.request,
            },
        ) {
            out.push(Output::Note(Note::Refused {
                client: client.clone(),
                action: "answer".to_owned(),
                reason,
            }));
            return;
        }
        let Some(answer) = message.get("result") else {
            // An error response is this client declining to answer.
            self.withdraw(number, client, out);
            return;
        };
        if !valid_outcome(&pending.request.params, answer) {
            out.push(ignored("not an outcome the permission request offers"));
            return;
        }
        if is_cancelled(answer) {
            self.withdraw(number, client, out);
        } else {
            self.resolve(
                number,
                answer.clone(),
                Answerer::Client(client.clone()),
                out,
            );
        }
    }

    /// Answers a pending request as the host. The result is the agent's `RequestPermissionResponse`.
    ///
    /// # Errors
    /// `HubError::UnknownRequest`, or `HubError::InvalidAnswer` when the outcome is not one the request offers.
    pub fn decide(&mut self, request: u64, result: Value) -> Result<Vec<Output>, HubError> {
        let pending = self
            .pending
            .get(&request)
            .ok_or(HubError::UnknownRequest(request))?;
        if !valid_outcome(&pending.request.params, &result) {
            return Err(HubError::InvalidAnswer);
        }
        self.touch();
        let mut out = Vec::new();
        self.resolve(request, result, Answerer::Host, &mut out);
        Ok(out)
    }

    fn withdraw(&mut self, number: u64, client: &ClientId, out: &mut Vec<Output>) {
        if let Some(pending) = self.pending.get_mut(&number) {
            pending.withdrawn.insert(client.clone());
        }
        self.settle_withdrawn(number, out);
    }

    /// When every attached client the request was put to has declined it, the agent is told it was cancelled. Nobody
    /// attached is not a refusal: the request waits.
    pub(crate) fn settle_withdrawn(&mut self, number: u64, out: &mut Vec<Output>) {
        let Some(pending) = self.pending.get(&number) else {
            return;
        };
        let mut eligible = self
            .members
            .iter()
            .filter(|(id, m)| m.attached && pending.addressed.contains(*id))
            .map(|(id, _)| id)
            .peekable();
        if eligible.peek().is_none() || !eligible.all(|id| pending.withdrawn.contains(id)) {
            return;
        }
        self.resolve(
            number,
            json!({ "outcome": { "outcome": "cancelled" } }),
            Answerer::Declined,
            out,
        );
    }

    fn resolve(&mut self, number: u64, answer: Value, by: Answerer, out: &mut Vec<Output>) {
        let Some(pending) = self.pending.remove(&number) else {
            return;
        };
        out.push(Output::ToAgent(result(&pending.agent_id, answer.clone())));
        let who = match &by {
            Answerer::Client(client) => json!(client.as_str()),
            Answerer::Host => json!("host"),
            _ => Value::Null,
        };
        let origin = if let Answerer::Client(client) = &by {
            Some(client.clone())
        } else {
            None
        };
        let message = notification(
            PERMISSION_CLOSED,
            json!({ "sessionId": self.session, "requestId": format!("{ID_PREFIX}{number}"), "by": who, "outcome": answer.get("outcome") }),
        );
        self.publish(message, origin, out);
        out.push(Output::Note(Note::PermissionAnswered {
            request: number,
            by,
        }));
    }
}
