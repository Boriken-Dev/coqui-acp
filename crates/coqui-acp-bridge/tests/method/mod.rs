//! The proof method a run of these tests uses: `COQUI_ACP_TEST_METHOD`, `noise` or `webcrypto`, else the default method. The
//! suite runs once per method.
#![allow(dead_code)]

use std::path::Path;

use coqui_agentic_peer::auth::{Identity, Method};

pub fn method() -> Method {
    std::env::var("COQUI_ACP_TEST_METHOD")
        .ok()
        .filter(|name| !name.is_empty())
        .map_or(Method::DEFAULT, |name| {
            name.parse()
                .expect("COQUI_ACP_TEST_METHOD is noise or webcrypto")
        })
}

pub fn name() -> &'static str {
    method().name()
}

/// The key at `path`, made of this run's method when absent.
pub fn identity(path: &Path) -> Identity {
    Identity::open_or_create(path, method()).unwrap()
}
