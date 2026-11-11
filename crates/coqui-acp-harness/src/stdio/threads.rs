//! The threads this provider started, which a library has to be able to wait for before it is unloaded.

use alloc::vec::Vec;
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;

use coqui_acp_api::guatiao::Status;

static THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// Keeps `thread` to be joined, and joins the ones that finished.
pub fn track(thread: JoinHandle<()>) {
    let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
    let (done, running): (Vec<_>, Vec<_>) = threads.drain(..).partition(JoinHandle::is_finished);
    *threads = running;
    for finished in done {
        let _ = finished.join();
    }
    threads.push(thread);
}

/// What a library's `unload` hook answers: `GUATIAO_ERR_BUSY` while a thread of a connection or a sign-in still runs,
/// else the threads are joined and it is `GUATIAO_OK`.
#[must_use]
pub fn threads_done() -> Status {
    let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
    if threads.iter().any(|t| !t.is_finished()) {
        return Status::GUATIAO_ERR_BUSY;
    }
    for thread in threads.drain(..) {
        let _ = thread.join();
    }
    Status::GUATIAO_OK
}
