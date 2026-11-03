//! The agent process: lines out through a bounded queue, lines in through a queue to a thread of their own, since a write to an
//! agent that has stopped reading never returns.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Lines waiting for the network. A full queue stops the reader, so the agent feels back-pressure on its pipe.
const QUEUED_LINES: usize = 256;
/// Bytes waiting for the network, past which the reader stops as it does for a full queue. One line may always wait.
const QUEUED_BYTES: usize = 64 * 1024 * 1024;
/// Lines waiting for the agent. A full queue stops the relay reading, so the peer feels back-pressure.
const LINES_IN: usize = 8;
/// How often a wait looks up.
const POLL: Duration = Duration::from_millis(10);

/// A line the agent wrote, or why its output ended.
#[derive(Debug)]
pub(crate) enum Output {
    Line(Vec<u8>),
    TooLong,
}

/// Why a line did not go to the agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteEnd {
    Ended,
    Stuck,
    Stopped,
}

/// A running agent. Dropping it ends the process.
#[derive(Debug)]
pub struct Agent {
    child: Mutex<Child>,
    input: SyncSender<Vec<u8>>,
    output: Mutex<Receiver<Output>>,
    queued: Arc<AtomicUsize>,
    write_limit: Duration,
    stopped: AtomicBool,
    guard: Mutex<Option<Guard>>,
}

/// A helper process waiting to end this agent if this process goes first.
#[derive(Debug)]
struct Guard {
    process: Child,
    told: ChildStdin,
}

/// What a helper started by [`Agent::guarded`] does: waits on `from_bridge`, its standard input, and ends the process tree of `agent`
/// unless a byte arrives first. A pipe that ends with no byte means the bridge went, however it went.
pub fn watch(agent: u32, mut from_bridge: impl Read) {
    if from_bridge.read_exact(&mut [0_u8]).is_err() {
        end_tree(agent);
    }
}

/// Ends a process and what it started. A failure here only means less was ended than asked: `stop` still ends the child itself.
fn end_tree(pid: u32) {
    #[cfg(windows)]
    let ended = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();
    #[cfg(not(windows))]
    let ended = Command::new("kill")
        .args(["-s", "KILL", "--", &format!("-{pid}")])
        .output();
    drop(ended);
}

impl Agent {
    /// Starts `program`. Its standard error stays on this process's, where a person can read it.
    ///
    /// # Errors
    ///
    /// The operating system could not start it.
    pub fn spawn(program: &OsString, args: &[OsString], max_line: usize) -> io::Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Its own group, so `stop` can reach what it starts: an adapter is usually a launcher around the program that does the work.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no standard input"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no standard output"))?;
        let (input, lines_in) = mpsc::sync_channel::<Vec<u8>>(LINES_IN);
        thread::Builder::new()
            .name("coqui-acp-bridge-agent-input".to_owned())
            .spawn(move || {
                while let Ok(line) = lines_in.recv() {
                    if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
                        return;
                    }
                }
            })?;
        let (lines, output) = mpsc::sync_channel(QUEUED_LINES);
        let queued = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&queued);
        thread::Builder::new()
            .name("coqui-acp-bridge-agent".to_owned())
            .spawn(move || {
                let limit = u64::try_from(max_line)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = Vec::new();
                    match (&mut reader).take(limit).read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) if line.len() > max_line && line.last() != Some(&b'\n') => {
                            let _ = lines.send(Output::TooLong);
                            return;
                        }
                        Ok(_) => {
                            while matches!(line.last(), Some(b'\n' | b'\r')) {
                                line.pop();
                            }
                            if line.is_empty() {
                                continue;
                            }
                            // The count has one other holder for as long as anyone can take a line.
                            while counted.load(Ordering::SeqCst) > 0
                                && counted.load(Ordering::SeqCst).saturating_add(line.len())
                                    > QUEUED_BYTES
                            {
                                if Arc::strong_count(&counted) == 1 {
                                    return;
                                }
                                thread::sleep(POLL);
                            }
                            counted.fetch_add(line.len(), Ordering::SeqCst);
                            if lines.send(Output::Line(line)).is_err() {
                                return;
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            child: Mutex::new(child),
            input,
            output: Mutex::new(output),
            queued,
            write_limit: Duration::from_secs(30),
            stopped: AtomicBool::new(false),
            guard: Mutex::new(None),
        })
    }

    /// Starts `program args... PID`, which has to call [`watch`] with this agent's id and its standard input, so this agent does not
    /// outlive this process, whatever ends it and whether or not the agent reads its input. It is apart from a terminal's Ctrl-C,
    /// which has to leave it standing to do this.
    ///
    /// # Errors
    ///
    /// The operating system could not start it, and this agent has been ended.
    pub fn guarded(self, program: &OsStr, args: &[&str]) -> io::Result<Self> {
        let pid = self.pid();
        let mut command = Command::new(program);
        command
            .args(args)
            .arg(pid.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        // CREATE_NO_WINDOW: a console of its own, so the bridge's console closing or its Ctrl-C does not end it too.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        let mut process = command.spawn()?;
        let told = process
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no standard input"))?;
        *self.guard.lock().unwrap_or_else(PoisonError::into_inner) = Some(Guard { process, told });
        Ok(self)
    }

    /// How long a line may wait on an agent that has stopped reading, after which the link ends as
    /// [`LinkEnd::AgentStuck`](crate::relay::LinkEnd::AgentStuck). Default 30 seconds.
    #[must_use]
    pub const fn with_write_limit(mut self, limit: Duration) -> Self {
        self.write_limit = limit;
        self
    }

    pub(crate) fn write_line(&self, message: &[u8], stop: &AtomicBool) -> Result<(), WriteEnd> {
        let mut line = Vec::with_capacity(message.len().saturating_add(1));
        line.extend_from_slice(message);
        line.push(b'\n');
        let until = Instant::now() + self.write_limit;
        loop {
            match self.input.try_send(line) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => return Err(WriteEnd::Ended),
                Err(TrySendError::Full(back)) => {
                    line = back;
                    if stop.load(Ordering::SeqCst) {
                        return Err(WriteEnd::Stopped);
                    }
                    if Instant::now() >= until {
                        return Err(WriteEnd::Stuck);
                    }
                    thread::sleep(POLL);
                }
            }
        }
    }

    pub(crate) fn next_output(&self, wait: Duration) -> Result<Output, RecvTimeoutError> {
        let output = self.output.lock().unwrap_or_else(PoisonError::into_inner);
        let next = output.recv_timeout(wait)?;
        if let Output::Line(line) = &next {
            self.queued.fetch_sub(line.len(), Ordering::SeqCst);
        }
        Ok(next)
    }

    /// The process's id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .id()
    }

    /// The id of the helper [`Self::guarded`] started, until it has been dismissed.
    #[must_use]
    pub fn guard_pid(&self) -> Option<u32> {
        self.guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|guard| guard.process.id())
    }

    /// Whether the process has ended, and how.
    ///
    /// # Errors
    ///
    /// The operating system could not say.
    pub fn try_status(&self) -> io::Result<Option<ExitStatus>> {
        self.child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_wait()
    }

    /// Ends the process and collects its status. Only this process: one it started itself is not reached.
    ///
    /// # Errors
    ///
    /// The operating system could not end it.
    pub fn stop(&self) -> io::Result<ExitStatus> {
        let status = self.end_process()?;
        // Once it has been reaped its id is free to be someone else's, so the helper has to be told not to look for it.
        if let Some(mut guard) = self
            .guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = guard.told.write_all(b"x");
            drop(guard.told);
            let _ = guard.process.wait();
        }
        Ok(status)
    }

    fn end_process(&self) -> io::Result<ExitStatus> {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        // Before looking at the child: once it has been reaped its id is free to be someone else's.
        if !self.stopped.swap(true, Ordering::SeqCst) {
            end_tree(child.id());
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        child.kill()?;
        child.wait()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
