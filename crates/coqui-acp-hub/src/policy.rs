use crate::client::Client;
use crate::output::PermissionRequest;

/// What a client asks the hub to do.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Action<'a> {
    /// Connect at all (`Hub::connect`).
    Attach,
    /// Send a prompt into the session.
    Prompt,
    /// Cancel the running turn.
    Cancel,
    /// Make any other request of the agent (`session/set_mode`, `session/set_config_option`, an extension's), by method.
    Request {
        /// The request's method.
        method: &'a str,
    },
    /// Answer this permission request. Asked when the request is sent to the client and again when it answers.
    Answer {
        /// The agent's request.
        request: &'a PermissionRequest,
    },
}

/// A policy's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Go ahead.
    Allow,
    /// Refuse, saying why; the reason reaches the client.
    Deny(String),
}

/// Who may do what, decided by the host. The hub knows only the identity it was given.
pub trait Policy: Send + Sync {
    /// Judges one action of one client.
    fn check(&self, client: &Client, action: &Action<'_>) -> Verdict;
}

impl<F> Policy for F
where
    F: Fn(&Client, &Action<'_>) -> Verdict + Send + Sync,
{
    fn check(&self, client: &Client, action: &Action<'_>) -> Verdict {
        self(client, action)
    }
}

/// Every client may do everything: the policy for a host that has already decided who connects.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl Policy for AllowAll {
    fn check(&self, _: &Client, _: &Action<'_>) -> Verdict {
        Verdict::Allow
    }
}
