//! The stdio provider as a library any Guatiao host can load. This file is a `cdylib` and nothing else, as a crate
//! calling `providers!` has to be.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "an example names what it uses from std"
)]

use coqui_acp_api::guatiao::{self, Status};
use coqui_acp_harness::vendors::antigravity::{AntigravityAccounts, AntigravityProvider};
use coqui_acp_harness::vendors::grok::{GrokAccounts, GrokProvider};
use coqui_acp_harness::vendors::opencode::{OpenCodeAccounts, OpenCodeProvider};

// The library's say in being unloaded: busy while a thread of its connections still runs.
extern "C" fn unload() -> Status {
    coqui_acp_harness::provider::threads_done()
}

guatiao::providers!(
    id = "coqui_acp_harness_stdio",
    version = env!("CARGO_PKG_VERSION"),
    providers = [
        GrokProvider,
        GrokAccounts,
        AntigravityProvider,
        AntigravityAccounts,
        OpenCodeProvider,
        OpenCodeAccounts
    ],
    unload = unload
);
