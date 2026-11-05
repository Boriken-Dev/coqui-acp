//! Starting a harness, and ending it with everything it started.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Instant;

/// What to start: a program, its arguments, its working directory and its environment.
///
/// `Debug` names the environment's variables and never shows their values, which may be credentials.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Launch {
    /// The program, found on `PATH` when it names no directory.
    pub program: OsString,
    /// Its arguments, each passed as one argument with no shell between.
    pub args: Vec<OsString>,
    /// Its working directory; the caller's when `None`.
    pub cwd: Option<PathBuf>,
    /// Variables set over the inherited environment, or over an empty one with `clear_env`.
    pub env: BTreeMap<OsString, OsString>,
    /// Variables removed from the inherited environment.
    pub remove_env: Vec<OsString>,
    /// Start from an empty environment rather than this process's.
    pub clear_env: bool,
}

impl Launch {
    /// A launch of `program` with no arguments, in the caller's directory and environment.
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// Adds one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Adds arguments.
    #[must_use]
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets one variable.
    #[must_use]
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    /// Starts from an empty environment.
    #[must_use]
    pub const fn clear_env(mut self, clear: bool) -> Self {
        self.clear_env = clear;
        self
    }

    /// The command line as one text, for messages: the program and each argument, quoted when it holds a space.
    #[must_use]
    pub fn display(&self) -> String {
        let mut text = quoted(&self.program);
        for arg in &self.args {
            text.push(' ');
            text.push_str(&quoted(arg));
        }
        text
    }
}

fn quoted(word: &OsStr) -> String {
    let word = word.to_string_lossy();
    if word.is_empty() || word.contains(' ') {
        alloc::format!("\"{word}\"")
    } else {
        word.into_owned()
    }
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("remove_env", &self.remove_env)
            .field("clear_env", &self.clear_env)
            .finish()
    }
}

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Exit {
    /// Its exit code, when it exited by itself.
    pub code: Option<i32>,
    /// The signal that ended it, on Unix.
    pub signal: Option<i32>,
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(code), _) => write!(f, "exited with code {code}"),
            (None, Some(signal)) => write!(f, "was ended by signal {signal}"),
            (None, None) => f.write_str("exited"),
        }
    }
}

/// A started process, as far as the harness needs it after its pipes are taken.
pub trait Process: Send + Sync {
    /// Ends the process and everything it started. Ending one that has ended is no error.
    ///
    /// # Errors
    ///
    /// The operating system refused.
    fn kill(&self) -> io::Result<()>;

    /// How it ended, or `None` while it runs.
    ///
    /// # Errors
    ///
    /// The operating system could not tell.
    fn try_wait(&self) -> io::Result<Option<Exit>>;
}

/// How `process` ended, for a message, waiting up to `wait` for it to say: `exited with code 3`, `was ended by signal 9`,
/// or `closed its output` when it does not.
#[must_use]
pub fn exit_text(process: &dyn Process, wait: Duration) -> String {
    let until = Instant::now() + wait;
    loop {
        match process.try_wait() {
            Ok(Some(exit)) => return exit.to_string(),
            Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
            _ => return "closed its output".into(),
        }
    }
}

/// A started process and its pipes. Standard error is `None` when the spawner keeps it.
#[non_exhaustive]
pub struct Spawned {
    /// Its standard input.
    pub stdin: Box<dyn Write + Send>,
    /// Its standard output.
    pub stdout: Box<dyn Read + Send>,
    /// Its standard error.
    pub stderr: Option<Box<dyn Read + Send>>,
    /// The process itself.
    pub process: Box<dyn Process>,
}

impl Spawned {
    /// A spawned process from its parts.
    #[must_use]
    pub fn new(
        stdin: Box<dyn Write + Send>,
        stdout: Box<dyn Read + Send>,
        stderr: Option<Box<dyn Read + Send>>,
        process: Box<dyn Process>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            process,
        }
    }
}

impl fmt::Debug for Spawned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spawned").finish_non_exhaustive()
    }
}

/// Starts processes. Implement it to run a harness in a container, over SSH or under a terminal; [`PlainSpawn`] runs
/// it here.
pub trait Spawn: Send + Sync {
    /// Starts `launch` with its standard input and output piped.
    ///
    /// # Errors
    ///
    /// The process could not be started.
    fn spawn(&self, launch: &Launch) -> io::Result<Spawned>;
}

/// Starts a plain process whose [`Process::kill`] ends the whole tree it started: on Windows every process of a job
/// object of its own, on Unix its process group. Standard error is piped.
///
/// On Windows a process the harness starts in the instant before it joins its job is outside it. On Unix a process
/// that leaves the group escapes it. Dropping the process kills it as `kill` does.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainSpawn;

impl Spawn for PlainSpawn {
    fn spawn(&self, launch: &Launch) -> io::Result<Spawned> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if launch.clear_env {
            command.env_clear();
        }
        for name in &launch.remove_env {
            command.env_remove(name);
        }
        command.envs(&launch.env);
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match contain(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let taken = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(stdin), Some(stdout), stderr) = taken else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("the process's pipes were not opened"));
        };
        Ok(Spawned {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: stderr.map(|stderr| Box::new(stderr) as Box<dyn Read + Send>),
            process: Box::new(PlainProcess {
                child: Mutex::new(Running {
                    child,
                    collected: None,
                }),
                #[cfg(windows)]
                job: Mutex::new(Some(job)),
            }),
        })
    }
}

struct PlainProcess {
    child: Mutex<Running>,
    /// Holds the process and everything it starts; closing it ends them all.
    #[cfg(windows)]
    job: Mutex<Option<win32job::Job>>,
}

struct Running {
    child: Child,
    /// How it ended, once it has been collected: from then on its id, and its group's, may be another process's, so
    /// nothing is sent to them again.
    collected: Option<Exit>,
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    Exit {
        code: status.code(),
        signal,
    }
}

impl Process for PlainProcess {
    fn kill(&self) -> io::Result<()> {
        let mut running = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        if running.collected.is_some() {
            return Ok(());
        }
        #[cfg(windows)]
        drop(
            self.job
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        #[cfg(unix)]
        if let Some(group) = i32::try_from(running.child.id())
            .ok()
            .filter(|pid| *pid > 1)
            .and_then(rustix::process::Pid::from_raw)
        {
            // The process is not collected before this, so its group id is still its own.
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        match running.child.kill() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
            Err(error) => return Err(error),
        }
        let status = running.child.wait()?;
        running.collected = Some(exit_of(status));
        Ok(())
    }

    fn try_wait(&self) -> io::Result<Option<Exit>> {
        let mut running = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(exit) = running.collected {
            return Ok(Some(exit));
        }
        #[cfg(unix)]
        {
            let _ = &mut running;
            peek(running.child.id())
        }
        #[cfg(not(unix))]
        Ok(running.child.try_wait()?.map(exit_of))
    }
}

impl Drop for PlainProcess {
    /// A process nobody kept ends with everything it started, as on Windows, where closing its job does it.
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// How the process ended, leaving it uncollected: until it is collected its id, and its group's, stay its own.
#[cfg(unix)]
fn peek(pid: u32) -> io::Result<Option<Exit>> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("not a process id"))?;
    let status = waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?;
    Ok(status.map(|status| Exit {
        code: status.exit_status(),
        signal: status.terminating_signal(),
    }))
}

/// Puts a process just started in a job object of its own that ends everything in it when its last handle closes.
#[cfg(windows)]
fn contain(child: &Child) -> io::Result<win32job::Job> {
    use std::os::windows::io::AsRawHandle;
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&limits).map_err(io::Error::other)?;
    job.assign_process(child.as_raw_handle() as isize)
        .map_err(io::Error::other)?;
    Ok(job)
}
