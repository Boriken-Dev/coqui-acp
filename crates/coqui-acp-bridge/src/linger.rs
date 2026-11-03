//! Keeping an agent through a reconnect: when a link ends its agent waits for the same proven peer, for a time, with what it writes
//! meanwhile held in a bounded buffer. What the buffer has to drop is said on the next link, since nothing here replays silently.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coqui_web::Channel;

use crate::agent::{Agent, Output};
use crate::relay::{LinkEnd, relay_holding};

/// The method of the one message the bridge writes itself: a notification, first on a link, with `lines` and `bytes` it dropped while
/// nobody was connected. ACP has no message for this, and has clients ignore an unknown notification whose method starts with `_`.
pub const OUTPUT_LOST: &str = "_dev.boriken.coqui.acp/output_lost";

/// How often a kept agent's watcher looks up from its output.
const POLL: Duration = Duration::from_millis(50);

/// What a [`Keeper`] keeps, and for how long.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Settings {
    /// How long an agent waits once its link has ended.
    pub keep: Duration,
    /// Agents kept for one peer. The bridge reads no ACP, so it cannot tell which agent holds the session a peer wants: past one,
    /// a peer can be given the wrong one. Past this the peer's oldest ends. Default 1.
    pub per_peer: NonZeroUsize,
    /// Agents kept for everyone together, past which the oldest ends. Default 4.
    pub total: NonZeroUsize,
    /// Bytes of a kept agent's output to hold, past which the oldest lines are dropped and said to be. Default 16 MiB.
    pub buffer: usize,
    /// How long a new connection waits for a link of the same peer to end, so that a reload faster than the old link's end still
    /// finds its agent. It decides what a second connection beside a live link waits. Default 500 ms.
    pub settle: Duration,
}

impl Settings {
    /// Keeps for `keep`, with the defaults above.
    #[must_use]
    pub const fn new(keep: Duration) -> Self {
        Self {
            keep,
            per_peer: NonZeroUsize::MIN,
            total: NonZeroUsize::new(4).expect("four is not zero"),
            buffer: 16 * 1024 * 1024,
            settle: Duration::from_millis(500),
        }
    }
}

/// Output held for the next link: always the newest lines in order, with a count of what went before them.
#[derive(Debug)]
pub(crate) struct Backlog {
    limit: usize,
    held: usize,
    lines: VecDeque<Vec<u8>>,
    lost_lines: u64,
    lost_bytes: u64,
}

impl Backlog {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            limit,
            held: 0,
            lines: VecDeque::new(),
            lost_lines: 0,
            lost_bytes: 0,
        }
    }

    /// Holds a line. The newest are the ones kept, since those hold the answer that ends a turn; a line over the limit by itself goes
    /// with all before it, so what is held stays unbroken.
    pub(crate) fn push(&mut self, line: Vec<u8>) {
        if line.len() > self.limit {
            while self.drop_oldest() {}
            self.note_lost(&line);
            return;
        }
        self.held += line.len();
        self.lines.push_back(line);
        while self.held > self.limit {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) -> bool {
        let Some(line) = self.lines.pop_front() else {
            return false;
        };
        self.held -= line.len();
        self.note_lost(&line);
        true
    }

    fn note_lost(&mut self, line: &[u8]) {
        self.lost_lines = self.lost_lines.saturating_add(1);
        self.lost_bytes = self
            .lost_bytes
            .saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
    }

    /// What goes on the wire next: the notice of what was lost, then the lines.
    pub(crate) fn next(&self) -> Option<Cow<'_, [u8]>> {
        if self.lost_lines > 0 {
            let notice = format!(
                r#"{{"jsonrpc":"2.0","method":"{OUTPUT_LOST}","params":{{"lines":{},"bytes":{}}}}}"#,
                self.lost_lines, self.lost_bytes
            );
            return Some(Cow::Owned(notice.into_bytes()));
        }
        self.lines
            .front()
            .map(|line| Cow::Borrowed(line.as_slice()))
    }

    /// What [`Self::next`] gave has been sent.
    pub(crate) fn sent(&mut self) {
        if self.lost_lines > 0 {
            self.lost_lines = 0;
            self.lost_bytes = 0;
        } else if let Some(line) = self.lines.pop_front() {
            self.held -= line.len();
        }
    }
}

struct Kept {
    agent: Agent,
    backlog: Backlog,
}

struct Parked {
    id: u64,
    peer: String,
    leave: Arc<AtomicBool>,
    watcher: JoinHandle<Option<Kept>>,
}

#[derive(Default)]
struct State {
    /// Oldest first.
    parked: Vec<Parked>,
    /// Links up, by peer, counted from their arrival until their agent is dealt with.
    up: BTreeMap<String, usize>,
    next: u64,
}

struct Inner {
    settings: Settings,
    report: Box<dyn Fn(&str) + Send + Sync>,
    state: Mutex<State>,
    changed: Condvar,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The agents waiting for their peers. Clones share them. Each is ended by its own time, by making room, or by being dropped with its
/// [`Agent`]; one left behind by this process ending is what [`Agent::guarded`] is for.
#[derive(Clone)]
pub struct Keeper(Arc<Inner>);

impl core::fmt::Debug for Keeper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Keeper")
            .field("settings", &self.0.settings)
            .finish_non_exhaustive()
    }
}

impl Keeper {
    /// Keeps by `settings`. `report` hears, in a sentence, what became of each agent.
    pub fn new(settings: Settings, report: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Inner {
            settings,
            report: Box::new(report),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }))
    }

    /// A new connection from `peer`, proven. Waits up to [`Settings::settle`] while a link of this peer is up, so it finds what that
    /// link leaves. Hold what it returns until the connection's agent has been dealt with.
    #[must_use]
    pub fn arrive(&self, peer: &str) -> Arrival {
        let until = Instant::now().checked_add(self.0.settings.settle);
        let mut state = self.0.state();
        while state.up.get(peer).is_some_and(|links| *links > 0) {
            let left = until.map_or(Duration::ZERO, |until| {
                until.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                break;
            }
            state = self
                .0
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *state.up.entry(peer.to_owned()).or_default() += 1;
        Arrival {
            keeper: self.clone(),
            peer: peer.to_owned(),
        }
    }

    /// The oldest agent kept for `peer` that is still running.
    fn take(&self, peer: &str) -> Option<Kept> {
        loop {
            let parked = {
                let mut state = self.0.state();
                let found = state.parked.iter().position(|parked| parked.peer == peer)?;
                state.parked.remove(found)
            };
            parked.leave.store(true, Ordering::SeqCst);
            if let Ok(Some(kept)) = parked.watcher.join()
                && matches!(kept.agent.try_status(), Ok(None))
            {
                return Some(kept);
            }
        }
    }

    /// Keeps an agent for `peer`, ending the oldest where a bound is reached.
    fn park(&self, peer: &str, kept: Kept) {
        let settings = self.0.settings;
        let mut evicted = Vec::new();
        {
            let mut state = self.0.state();
            while state
                .parked
                .iter()
                .filter(|parked| parked.peer == peer)
                .count()
                >= settings.per_peer.get()
            {
                if let Some(oldest) = state.parked.iter().position(|parked| parked.peer == peer) {
                    evicted.push(state.parked.remove(oldest));
                }
            }
            while state.parked.len() >= settings.total.get() {
                evicted.push(state.parked.remove(0));
            }
            state.next += 1;
            let id = state.next;
            let leave = Arc::new(AtomicBool::new(false));
            let keeper = self.clone();
            let (told, owner) = (Arc::clone(&leave), peer.to_owned());
            // A closed process table is the only way this fails, and the closure's drop then ends the agent.
            let until = Instant::now()
                .checked_add(settings.keep)
                .unwrap_or_else(Instant::now);
            let watcher = thread::Builder::new()
                .name("coqui-acp-bridge-kept".to_owned())
                .spawn(move || keeper.watch(id, &owner, kept, &told, until));
            match watcher {
                Ok(watcher) => state.parked.push(Parked {
                    id,
                    peer: peer.to_owned(),
                    leave,
                    watcher,
                }),
                Err(error) => {
                    (self.0.report)(&format!("{peer}'s agent could not be kept: {error}"))
                }
            }
        }
        self.0.changed.notify_all();
        for parked in evicted {
            parked.leave.store(true, Ordering::SeqCst);
            drop(parked.watcher.join());
            (self.0.report)(&format!(
                "{}'s kept agent was ended to make room",
                parked.peer
            ));
        }
    }

    /// Takes a kept agent's output into its buffer until it is wanted, or has to go. Whoever removed the entry is waiting for it.
    fn watch(
        &self,
        id: u64,
        peer: &str,
        mut kept: Kept,
        leave: &AtomicBool,
        until: Instant,
    ) -> Option<Kept> {
        let why = loop {
            if leave.load(Ordering::SeqCst) {
                return Some(kept);
            }
            if Instant::now() >= until {
                break "its time ran out";
            }
            match kept.agent.next_output(POLL) {
                Ok(Output::Line(line)) => kept.backlog.push(line),
                Ok(Output::TooLong) => break "it wrote a line over the limit",
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break "it ended",
            }
        };
        let mine = {
            let mut state = self.0.state();
            let found = state.parked.iter().position(|parked| parked.id == id);
            found.map(|found| state.parked.remove(found))
        };
        if mine.is_none() {
            return Some(kept);
        }
        drop(kept);
        (self.0.report)(&format!("{peer}'s kept agent is gone: {why}"));
        None
    }
}

/// A connection a [`Keeper`] knows of. Dropping it says this connection's agent has been dealt with.
#[derive(Debug)]
pub struct Arrival {
    keeper: Keeper,
    peer: String,
}

impl Arrival {
    /// Relays over `channel` until the link ends, with the agent kept for this peer or else one from `start`. What was kept for this
    /// peer goes first. Afterwards the agent is kept again, or ended if the link's end leaves nothing to come back to.
    ///
    /// # Errors
    ///
    /// `start` failed.
    pub fn serve<C: Channel + ?Sized>(
        &self,
        channel: &C,
        start: impl FnOnce() -> io::Result<Agent>,
    ) -> io::Result<LinkEnd> {
        let inner = &self.keeper.0;
        let mut kept = match self.keeper.take(&self.peer) {
            Some(kept) => {
                (inner.report)(&format!(
                    "{} got its agent back, {} lines of its output lost",
                    self.peer, kept.backlog.lost_lines
                ));
                kept
            }
            None => Kept {
                agent: start()?,
                backlog: Backlog::new(inner.settings.buffer),
            },
        };
        let end = relay_holding(channel, &kept.agent, &mut kept.backlog);
        if end.keeps_agent() {
            self.keeper.park(&self.peer, kept);
        } else {
            let _ = kept.agent.stop();
        }
        Ok(end)
    }
}

impl Drop for Arrival {
    fn drop(&mut self) {
        let inner = &self.keeper.0;
        {
            let mut state = inner.state();
            if let Some(links) = state.up.get_mut(&self.peer) {
                *links -= 1;
                if *links == 0 {
                    state.up.remove(&self.peer);
                }
            }
        }
        inner.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(backlog: &mut Backlog) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(message) = backlog.next() {
            out.push(message.into_owned());
            backlog.sent();
        }
        out
    }

    #[test]
    fn what_fits_comes_back_in_order_with_no_notice() {
        let mut backlog = Backlog::new(10);
        backlog.push(b"one".to_vec());
        backlog.push(b"two".to_vec());
        assert_eq!(drain(&mut backlog), [b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn past_the_limit_the_oldest_go_and_the_notice_comes_first_with_their_count() {
        let mut backlog = Backlog::new(6);
        for line in [&b"aaa"[..], b"bbb", b"cc", b"d"] {
            backlog.push(line.to_vec());
        }
        let out = drain(&mut backlog);
        let notice: serde_json::Value = serde_json::from_slice(&out[0]).unwrap();
        assert_eq!(notice["jsonrpc"], "2.0");
        assert_eq!(notice["method"], OUTPUT_LOST);
        assert!(notice.get("id").is_none(), "a notification has no id");
        assert_eq!(notice["params"]["lines"], 1);
        assert_eq!(notice["params"]["bytes"], 3);
        assert_eq!(out[1..], [b"bbb".to_vec(), b"cc".to_vec(), b"d".to_vec()]);
    }

    #[test]
    fn a_line_over_the_limit_goes_with_all_before_it_so_what_is_held_is_unbroken() {
        let mut backlog = Backlog::new(4);
        backlog.push(b"aa".to_vec());
        backlog.push(b"toolong".to_vec());
        backlog.push(b"bb".to_vec());
        let out = drain(&mut backlog);
        let notice: serde_json::Value = serde_json::from_slice(&out[0]).unwrap();
        assert_eq!(notice["params"]["lines"], 2);
        assert_eq!(notice["params"]["bytes"], 9);
        assert_eq!(out[1..], [b"bb".to_vec()]);
    }

    #[test]
    fn a_notice_not_yet_sent_stays_until_it_is() {
        let mut backlog = Backlog::new(1);
        backlog.push(b"gone".to_vec());
        let first = backlog.next().unwrap().into_owned();
        assert_eq!(backlog.next().unwrap().into_owned(), first);
        backlog.sent();
        assert!(backlog.next().is_none());
    }

    #[test]
    fn with_no_room_every_line_is_counted_and_none_held() {
        let mut backlog = Backlog::new(0);
        backlog.push(b"a".to_vec());
        backlog.push(b"bc".to_vec());
        let out = drain(&mut backlog);
        assert_eq!(out.len(), 1);
        let notice: serde_json::Value = serde_json::from_slice(&out[0]).unwrap();
        assert_eq!(notice["params"]["lines"], 2);
        assert_eq!(notice["params"]["bytes"], 3);
    }

    #[test]
    fn the_defaults_keep_one_agent_a_peer_and_four_in_all() {
        let settings = Settings::new(Duration::from_secs(1));
        assert_eq!(settings.per_peer.get(), 1);
        assert_eq!(settings.total.get(), 4);
    }
}
