//! Whether Pi has a login, without starting it, asking a service or showing what it holds.
//!
//! Pi keeps what `/login` signed in with in `auth.json` of its agent directory (`PI_CODING_AGENT_DIR`, else `.pi/agent` below
//! the user's home), and reads a provider's key from that provider's own variable. A login is there when `auth.json` holds an
//! entry or when one of [`KEY_VARIABLES`] is set and not empty. Pi keeps no login anywhere
//! else, so a file that is not there is signed out.

use std::path::PathBuf;

use coqui_acp_api::{Environment, SignedInCheck};
use coqui_acp_harness::signed_in;

use crate::accounts::KEY_VARIABLES;

/// The file Pi keeps its logins in, in the agent directory.
const AUTH_FILE: &str = "auth.json";

/// Whether Pi has a login for the agent directory and the variables `environment` gives, over this process's.
#[must_use]
pub fn check(environment: &Environment) -> SignedInCheck {
    let home = signed_in::home(environment);
    let named = signed_in::value(environment, "PI_CODING_AGENT_DIR").map(|text| {
        // Pi reads a leading `~` as the home.
        match (text.strip_prefix('~'), &home) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
                home.join(rest.trim_start_matches(['/', '\\']))
            }
            _ => PathBuf::from(text),
        }
    });
    let dir = named.or_else(|| home.map(|home| home.join(".pi").join("agent")));
    signed_in::answer_for_file(
        environment,
        KEY_VARIABLES,
        dir.map(|dir| dir.join(AUTH_FILE)).as_deref(),
        "Pi's auth.json",
    )
}
