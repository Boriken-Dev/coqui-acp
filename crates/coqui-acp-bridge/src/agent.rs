//! The agent process: lines out through a bounded queue, lines in through its standard input.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, PoisonError};
use std::thread;

/// Lines waiting for the network. A full queue stops the reader, so the agent feels back-pressure on its pipe.
const QUEUED_LINES: usize = 256;

/// A line the agent wrote, or why its output ended.
#[derive(Debug)]
pub(crate) enum Output {
    Line(Vec<u8>),
    TooLong,
}

/// A running agent. Dropping it ends the process.
#[derive(Debug)]
pub struct Agent {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    output: Mutex<Receiver<Output>>,
}

impl Agent {
    /// Starts `program`. Its standard error stays on this process's, where a person can read it.
    ///
    /// # Errors
    ///
    /// The operating system could not start it.
    pub fn spawn(program: &OsString, args: &[OsString], max_line: usize) -> io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no standard input"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no standard output"))?;
        let (lines, output) = mpsc::sync_channel(QUEUED_LINES);
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
                            if !line.is_empty() && lines.send(Output::Line(line)).is_err() {
                                return;
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            output: Mutex::new(output),
        })
    }

    pub(crate) fn write_line(&self, message: &[u8]) -> io::Result<()> {
        let mut stdin = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
        stdin.write_all(message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    pub(crate) fn output(&self) -> std::sync::MutexGuard<'_, Receiver<Output>> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Ends the process and collects its status.
    ///
    /// # Errors
    ///
    /// The operating system could not end it.
    pub fn stop(&self) -> io::Result<ExitStatus> {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
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
