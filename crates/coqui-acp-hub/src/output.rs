use serde_json::Value;

use crate::client::ClientId;

/// A permission request from the agent, waiting for an answer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PermissionRequest {
    /// The hub's number for it; the request id clients see is `hub-perm-<id>`.
    pub id: u64,
    /// The agent's `session/request_permission` parameters, untrusted.
    pub params: Value,
    /// The hub's clock (`Hub::poll`) when it arrived.
    pub asked_at: u64,
}

/// Who answered a permission request.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Answerer {
    /// An attached client, by identity.
    Client(ClientId),
    /// The host, through `Hub::decide`.
    Host,
    /// Every attached client the request was put to declined it, so the agent was told it was cancelled.
    Declined,
}

/// Something the host should know that is not a message.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Note {
    /// A client's action was refused by the policy or by a limit.
    Refused {
        /// The client.
        client: ClientId,
        /// What it tried.
        action: String,
        /// Why not.
        reason: String,
    },
    /// A message was not acted on (a late answer, an unknown id, a malformed message).
    Ignored {
        /// The client, when the message came from one.
        client: Option<ClientId>,
        /// Why.
        reason: String,
    },
    /// A permission request was answered, for the host's audit.
    PermissionAnswered {
        /// The request.
        request: u64,
        /// Who answered.
        by: Answerer,
    },
    /// A permission request has waited longer than `Settings::permission_window_ms`. Reported once per request; it stays
    /// pending until someone answers.
    PermissionUnanswered {
        /// The request.
        request: u64,
        /// Milliseconds it has waited.
        waiting_ms: u64,
    },
    /// The agent's connection ended; the session cannot go on.
    Ended,
}

/// What the host does after giving the hub an input: it sends each message over its own link.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Output {
    /// A JSON-RPC message for one attached client.
    ToClient {
        /// The client.
        client: ClientId,
        /// The message.
        message: Value,
    },
    /// A JSON-RPC message for the agent.
    ToAgent(Value),
    /// See `Note`.
    Note(Note),
}
