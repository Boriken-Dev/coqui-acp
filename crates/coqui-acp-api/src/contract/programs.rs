//! A spawner's process that runs a real program, for a provider that drives its harness's own command line.
//!
//! The suite's other processes do nothing until they are killed. A provider of a harness that starts a
//! program (a stand-in for it, named by the provider's own settings) is held to the same rules when the
//! spawner starts what it is asked: on this machine, standard error apart, with no terminal. This is the
//! smallest spawner that does so; a host's own is [`ProcessSpawner`](crate::ProcessSpawner)'s to provide.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use guatiao::Status;
use guatiao::library::ProviderError;

use crate::spawner::{
    ErrorOutput, LinkState, ProcessExit, READ_TIMED_OUT, SpawnSpec, SpawnedProcess, TerminalSize,
};

/// What one output stream of the program holds that has not been read.
#[derive(Default)]
struct Pipe {
    state: Mutex<PipeState>,
    arrived: Condvar,
}

#[derive(Default)]
struct PipeState {
    bytes: VecDeque<u8>,
    ended: bool,
}

impl Pipe {
    fn lock(&self) -> MutexGuard<'_, PipeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads `source` to its end into the pipe, on a thread of its own.
    fn fill(self: &Arc<Self>, mut source: impl Read + Send + 'static, ended: Option<Arc<AtomicBool>>) {
        let pipe = Arc::clone(self);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match source.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pipe.lock().bytes.extend(&chunk[..n]);
                        pipe.arrived.notify_all();
                    }
                }
            }
            pipe.lock().ended = true;
            pipe.arrived.notify_all();
            if let Some(ended) = ended {
                ended.store(true, Ordering::SeqCst);
            }
        });
    }

    fn read(&self, dst: &mut [u8], timeout: Duration) -> i64 {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.bytes.is_empty() {
                let n = dst.len().min(state.bytes.len());
                for (slot, byte) in dst.iter_mut().zip(state.bytes.drain(..n)) {
                    *slot = byte;
                }
                return i64::try_from(n).unwrap_or(i64::MAX);
            }
            if state.ended {
                return 0;
            }
            let now = Instant::now();
            if now >= deadline {
                return READ_TIMED_OUT;
            }
            state = self
                .arrived
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// A program the suite started: its standard input and output, and whether it has ended.
pub(super) struct Program {
    child: Mutex<Child>,
    input: Mutex<Option<ChildStdin>>,
    out: Arc<Pipe>,
    err: Arc<Pipe>,
    ended: Arc<AtomicBool>,
}

fn refuse(status: Status, why: &str) -> ProviderError {
    ProviderError::new(status, why)
}

impl Program {
    /// Starts what `spec` names, or answers why it could not be: a missing program is `NOT_FOUND`, a terminal or a
    /// merged standard error `UNSUPPORTED`. `ended` is set when the program is killed, destroyed or has exited.
    pub(super) fn start(spec: &SpawnSpec, ended: Arc<AtomicBool>) -> Result<Self, ProviderError> {
        if spec.terminal.is_some() {
            return Err(refuse(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this spawner gives no terminal",
            ));
        }
        if spec.error_output == ErrorOutput::Merged {
            return Err(refuse(
                Status::GUATIAO_ERR_UNSUPPORTED,
                "this spawner keeps standard error apart",
            ));
        }
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        if spec.clear_environment {
            command.env_clear();
        }
        for name in &spec.remove_environment {
            command.env_remove(name);
        }
        for (name, value) in spec.environment.expose().iter() {
            command.env(name, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if spec.error_output == ErrorOutput::Discarded {
                Stdio::null()
            } else {
                Stdio::piped()
            });
        let mut child = command.spawn().map_err(|error| {
            let status = if error.kind() == std::io::ErrorKind::NotFound {
                Status::GUATIAO_ERR_NOT_FOUND
            } else {
                Status::GUATIAO_ERR_INTERNAL
            };
            refuse(status, &error.kind().to_string())
        })?;
        let out = Arc::new(Pipe::default());
        let err = Arc::new(Pipe::default());
        if let Some(stdout) = child.stdout.take() {
            // A program whose output has ended has ended, for what the suite asks of the programs it started.
            out.fill(stdout, Some(Arc::clone(&ended)));
        } else {
            out.lock().ended = true;
        }
        if let Some(stderr) = child.stderr.take() {
            err.fill(stderr, None);
        } else {
            err.lock().ended = true;
        }
        let input = child.stdin.take();
        let program = Self {
            child: Mutex::new(child),
            input: Mutex::new(input),
            out,
            err,
            ended,
        };
        Ok(program)
    }

    fn child(&self) -> MutexGuard<'_, Child> {
        self.child.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the program has exited, noting it.
    fn exited(&self) -> Option<std::process::ExitStatus> {
        let status = self.child().try_wait().ok().flatten();
        if status.is_some() {
            self.ended.store(true, Ordering::SeqCst);
        }
        status
    }
}

impl SpawnedProcess for Program {
    fn write(&self, bytes: &[u8]) -> Result<(), ProviderError> {
        let mut input = self.input.lock().unwrap_or_else(PoisonError::into_inner);
        let stdin = input
            .as_mut()
            .ok_or_else(|| refuse(Status::GUATIAO_ERR_GONE, "input is closed"))?;
        stdin
            .write_all(bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| refuse(Status::GUATIAO_ERR_GONE, "the program has ended"))
    }

    fn close_input(&self) -> Result<(), ProviderError> {
        drop(self.input.lock().unwrap_or_else(PoisonError::into_inner).take());
        Ok(())
    }

    fn read(&self, dst: &mut [u8], timeout_ms: u64) -> Result<i64, ProviderError> {
        Ok(self.out.read(dst, Duration::from_millis(timeout_ms)))
    }

    fn read_error(&self, dst: &mut [u8], timeout_ms: u64) -> Result<i64, ProviderError> {
        Ok(self.err.read(dst, Duration::from_millis(timeout_ms)))
    }

    fn resize(&self, _size: TerminalSize) -> Result<(), ProviderError> {
        Err(refuse(Status::GUATIAO_ERR_UNSUPPORTED, "no terminal"))
    }

    fn wait(&self, timeout_ms: u64) -> Result<ProcessExit, ProviderError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if let Some(status) = self.exited() {
                return Ok(ProcessExit::ended(
                    status.code().map(i64::from),
                    signal_of(&status),
                ));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(ProcessExit::running());
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    fn kill(&self) -> Result<(), ProviderError> {
        let _ = self.child().kill();
        self.ended.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn link(&self) -> Result<LinkState, ProviderError> {
        Ok(LinkState::up())
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.child().wait();
    }
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i64> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(i64::from)
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<i64> {
    None
}

