//! The `acp_harness` kind: a provider that drives an ACP agent and speaks ACP itself, one JSON-RPC
//! message per channel message.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;

use guatiao::library::{Object, ProviderError};
use guatiao_channel::{Channel, ChannelSink};

use crate::models::{
    HarnessInfo, ModelListing, Requirement, SessionRecordFile, SessionRecordQuery, StartRequest,
};

/// Drives an Agent Client Protocol agent, and serves it as one ACP connection over a channel.
///
/// The host is the ACP client and the provider the agent: each message on the channel, either way,
/// is one whole JSON-RPC 2.0 message as UTF-8 JSON text. The kind never models ACP itself, so a
/// provider in any language implements ACP as the protocol defines it, and the version is agreed by
/// `initialize` on the channel.
///
/// Every method is synchronous; a host calls them from a thread of its own.
#[guatiao::kind(name = "acp_harness")]
pub trait AcpHarness: Send + Sync {
    /// Who the harness is and what its `initialize` will advertise, with nothing started: for a host
    /// listing its harnesses. Cheap, and the same on every call.
    fn describe(&self) -> Result<HarnessInfo, ProviderError>;

    /// Starts one ACP connection: messages the host sends are the client's, and the agent's come back
    /// through `sink`.
    ///
    /// The sink is handed over: the provider destroys it, even when it refuses. It returns once the
    /// channel takes messages; the program it drives may start later, at `initialize` or
    /// `session/new`, and a failure then is answered in ACP before the provider closes.
    ///
    /// It refuses with `GUATIAO_ERR_UNSUPPORTED` when it would refuse any of the request's
    /// requirements ([`refusal`](Self::refusal)), and with `GUATIAO_ERR_BAD_VALUE` a request it cannot
    /// read; it never starts with a requirement left out.
    fn start(
        &self,
        request: StartRequest,
        sink: Object<dyn ChannelSink>,
    ) -> Result<Object<dyn Channel>, ProviderError>;

    /// Why the harness could not hold a connection to `requirement`, started with `environment`, in a
    /// sentence; `None` when it can. A host asks before it starts one, and passes over a harness
    /// that would refuse. The default refuses every requirement, known or not.
    fn refusal(
        &self,
        requirement: Requirement,
        environment: BTreeMap<String, String>,
    ) -> Result<Option<String>, ProviderError> {
        drop((requirement, environment));
        Ok(Some(
            "this harness holds a connection to no requirement".to_owned(),
        ))
    }

    /// The models the account behind `environment` may use, when the harness can say without a model
    /// turn. Never carries a value from `environment`. The default: unknown, and says so.
    fn models(&self, environment: BTreeMap<String, String>) -> Result<ModelListing, ProviderError> {
        drop(environment);
        Ok(ModelListing::unknown(
            "this harness has no way to list models",
        ))
    }

    /// The program the harness starts, a name looked up on `PATH` or a path, so a host can say it
    /// cannot be found before it starts a connection. The default: `None`, and the host does not look.
    fn program(&self) -> Result<Option<String>, ProviderError> {
        Ok(None)
    }

    /// Where the harness keeps its own record of the session `query` names, when it keeps one and the
    /// file is there. The default: none.
    fn session_record(
        &self,
        query: SessionRecordQuery,
    ) -> Result<Option<SessionRecordFile>, ProviderError> {
        drop(query);
        Ok(None)
    }
}
