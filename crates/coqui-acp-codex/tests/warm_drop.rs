//! `drop_all` stops the Codex app servers kept ahead. In a test binary of its own, as it stops every pool of the process.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::time::{Duration, Instant};

use coqui_acp_codex::harness::{WarmPolicy, drop_all};
use coqui_acp_codex::{CodexAgent, CodexSettings};
use support::{Scratch, replaying};

#[test]
fn drop_all_stops_every_app_server_kept_ahead() {
    let scratch = Scratch::new("drop");
    let mut defaults = replaying(None, &scratch.0);
    defaults
        .env
        .insert("CODEX_HOME".into(), scratch.0.clone().into());
    let mut settings = CodexSettings::new();
    settings.warm = WarmPolicy::new(2, Duration::from_secs(60));
    let agent = CodexAgent::new().defaults(defaults).settings(settings);
    agent.prewarm();
    let limit = Instant::now() + Duration::from_secs(20);
    while agent.warm_ready() < 2 {
        assert!(Instant::now() < limit, "two app servers become warm");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop_all();
    assert_eq!(agent.warm_ready(), 0);
}
