//! `drop_all` reaches the warm processes of every pool in the process, in a process of its own so that no other test's pool
//! is stopped with them.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use coqui_acp_harness::{Launch, Maker, WarmKey, WarmPolicy, WarmPool, Warmed, drop_all};

struct Toy(Arc<AtomicBool>);

impl Warmed for Toy {
    fn ready(&self) -> bool {
        !self.0.load(Ordering::SeqCst)
    }
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn drop_all_stops_the_warm_processes_of_every_pool() {
    let policy = WarmPolicy::new(1, Duration::from_secs(60));
    let key = WarmKey::new(&policy, &Launch::new("agent"));
    let stops = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let pools = [WarmPool::new(), WarmPool::new()];
    for (pool, stop) in pools.iter().zip(&stops) {
        let stop = Arc::clone(stop);
        let make: Maker<Toy> = Arc::new(move || Some(Toy(Arc::clone(&stop))));
        pool.fill(&key, &policy, &make);
    }
    let limit = Instant::now() + Duration::from_secs(10);
    while pools.iter().any(|pool| pool.ready(&key) == 0) {
        assert!(Instant::now() < limit, "the processes start");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop_all();
    assert!(stops.iter().all(|stop| stop.load(Ordering::SeqCst)));
    assert!(pools.iter().all(|pool| pool.ready(&key) == 0));
}
