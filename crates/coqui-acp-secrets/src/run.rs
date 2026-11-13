//! Running a command the person approved, with a secret in one environment variable.

use alloc::sync::Arc;
use core::fmt;
use core::time::Duration;
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;
use zeroize::Zeroizing;

use crate::Error;

/// A command as an agent names it: an argument vector run directly, or text run by the platform's shell (`sh -c`, `cmd /C`).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandLine {
    /// The program and its arguments, no shell between.
    Argv(Vec<String>),
    /// Text the shell reads, so it can expand the variable that holds the secret.
    Shell(String),
}

impl CommandLine {
    /// Reads the `command` argument of a tool call: a string (shell) or an array of strings (argv).
    ///
    /// # Errors
    /// [`Error::Arguments`] for anything else, an empty command or a NUL; [`Error::Limit`] over `max_bytes`.
    pub fn from_value(value: &Value, max_bytes: usize) -> Result<Self, Error> {
        let line = match value {
            Value::String(text) => Self::Shell(text.clone()),
            Value::Array(items) => Self::Argv(
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(Error::Arguments("command: every argument is a string"))?,
            ),
            _ => {
                return Err(Error::Arguments(
                    "command: a string for the shell or an array of strings",
                ));
            }
        };
        let (empty, size, nul) = match &line {
            Self::Shell(text) => (text.trim().is_empty(), text.len(), text.contains('\0')),
            Self::Argv(args) => (
                args.first().is_none_or(String::is_empty),
                args.iter().map(|arg| arg.len() + 1).sum(),
                args.iter().any(|arg| arg.contains('\0')),
            ),
        };
        if empty || nul {
            return Err(Error::Arguments("command: empty, or holds a NUL"));
        }
        if size > max_bytes {
            return Err(Error::Limit(
                "the command is longer than the settings allow",
            ));
        }
        Ok(line)
    }

    /// The command as the person is shown it, in full on one line: an argument with anything but plain characters is single-quoted,
    /// and a control character is written as an escape, so the text cannot hide a line break.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Shell(text) => format!("(shell) {}", crate::text::escape(text, false)),
            Self::Argv(args) => args
                .iter()
                .map(|arg| {
                    let plain = !arg.is_empty()
                        && arg
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
                    if plain {
                        arg.clone()
                    } else {
                        format!("'{}'", crate::text::escape(arg, true))
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// What a [`Runner`] is asked to run.
pub struct CommandSpec<'a> {
    /// The command.
    pub command: &'a CommandLine,
    /// The program that is started: for an argument vector its resolved first element (on a search path that excludes the session's
    /// roots), for shell text the shell, by full path. What the person approved.
    pub program: &'a Path,
    /// The `PATH` the command gets: the host's, without a relative entry or one under the session's roots. `None` gives none.
    pub path: Option<&'a OsStr>,
    /// The directory it starts in: under the session's roots.
    pub cwd: &'a Path,
    /// The variable that holds the secret and the secret's bytes (valid UTF-8, no NUL), when the command is a use.
    pub secret: Option<(&'a str, &'a [u8])>,
    /// The longest it may run.
    pub timeout: Duration,
    /// The most bytes kept of each output stream.
    pub max_output: usize,
    /// The only variables of the host's environment the command is given.
    pub inherit_env: &'a [String],
}

impl fmt::Debug for CommandSpec<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("command", &self.command.display())
            .field("cwd", &self.cwd)
            .field("secret_variable", &self.secret.map(|(name, _)| name))
            .finish_non_exhaustive()
    }
}

/// What a command did.
pub struct CommandOutput {
    /// The exit status, `None` when it was ended by a signal or by the timeout.
    pub status: Option<i32>,
    /// Standard output, at most the spec's limit.
    pub stdout: Zeroizing<Vec<u8>>,
    /// Standard error, at most the spec's limit.
    pub stderr: Zeroizing<Vec<u8>>,
    /// Whether a stream was cut at the limit.
    pub truncated: bool,
    /// Whether the command was ended for taking too long.
    pub timed_out: bool,
}

impl fmt::Debug for CommandOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandOutput")
            .field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("truncated", &self.truncated)
            .field("timed_out", &self.timed_out)
            .finish()
    }
}

/// Runs a command for a host: in its own sandbox, terminal or remote runner. The output is scrubbed by the caller.
pub trait Runner: Send + Sync {
    /// Runs the command to its end or its timeout.
    ///
    /// # Errors
    /// [`Error::Command`] when it could not be started.
    fn run(&self, spec: &CommandSpec<'_>) -> Result<CommandOutput, Error>;
}

/// Runs the command as a child process of the host: the environment is cleared except [`CommandSpec::inherit_env`] and the one
/// secret, standard input is closed, and when the command ends, or runs past its timeout, everything it started is ended with it (a
/// process group on Unix, a job object on Windows), so nothing it left behind keeps the secret in its environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessRunner;

#[derive(Default)]
struct Capture {
    bytes: Zeroizing<Vec<u8>>,
    cut: bool,
}

fn drain(
    mut stream: impl Read + Send + 'static,
    max: usize,
) -> (Arc<Mutex<Capture>>, std::thread::JoinHandle<()>) {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let sink = Arc::clone(&capture);
    let handle = std::thread::spawn(move || {
        let mut buffer = Zeroizing::new([0u8; 4096]);
        while let Ok(read) = stream.read(&mut *buffer) {
            if read == 0 {
                break;
            }
            let Ok(mut held) = sink.lock() else { break };
            let room = max.saturating_sub(held.bytes.len());
            if read > room {
                held.cut = true;
            }
            let take = read.min(room);
            held.bytes.extend_from_slice(&buffer[..take]);
        }
        buffer.fill(0);
    });
    (capture, handle)
}

fn take(capture: &Arc<Mutex<Capture>>) -> (Zeroizing<Vec<u8>>, bool) {
    capture
        .lock()
        .map_or((Zeroizing::new(Vec::new()), false), |mut held| {
            (core::mem::take(&mut held.bytes), held.cut)
        })
}

/// A command that has been started, as far as [`ProcessRunner`] needs it: whether it has ended, and its end with everything it
/// started.
pub trait Process: Send {
    /// How it stands: `None` while it runs, else its exit code (`None` for a signal's end).
    ///
    /// # Errors
    /// The system could not say.
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>>;

    /// Ends what the command left running, once it has ended: the process group it led, which is still its own while the
    /// command is not collected.
    fn end_group_after_exit(&mut self);

    /// Ends the command and everything it started, and collects it.
    fn end(&mut self);
}

/// What a [`Starter`] gives back: the process and its two output pipes.
pub struct Started {
    /// The process.
    pub process: Box<dyn Process>,
    /// Its standard output.
    pub stdout: Box<dyn Read + Send>,
    /// Its standard error.
    pub stderr: Box<dyn Read + Send>,
}

impl fmt::Debug for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Started").finish_non_exhaustive()
    }
}

/// What to start: the whole of the command's environment is `env`, its standard input is closed, and it leads a process group.
pub struct Launch {
    /// The program, by full path.
    pub program: std::path::PathBuf,
    /// Its arguments.
    pub args: Vec<std::ffi::OsString>,
    /// The directory it starts in.
    pub cwd: std::path::PathBuf,
    /// Its whole environment, nothing else inherited. Holds the secret: not to be logged.
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

/// Starts what [`ProcessRunner`] runs, in place of this process's own child, once [`set_starter`] has named one: for a host that
/// cannot wait on its own children, as one inside an application whose process handling collects every child that ends cannot.
/// It starts the process in a group of its own with standard input closed and both outputs piped, and keeps it uncollected until
/// [`Process::try_wait`] says it ended and [`Process::end`] or the end of the group has been done. Used on Unix only.
pub trait Starter: Send + Sync {
    /// Starts `launch`.
    ///
    /// # Errors
    /// The process could not be started.
    fn start(&self, launch: &Launch) -> io::Result<Started>;
}

static STARTER: Mutex<Option<Arc<dyn Starter>>> = Mutex::new(None);

/// Has [`ProcessRunner`] start commands through `starter` from now on.
pub fn set_starter(starter: Arc<dyn Starter>) {
    *STARTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(starter);
}

impl fmt::Debug for Launch {
    /// Without its environment, which holds the secret.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("program", &self.program)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// A child of this process.
struct Own(std::process::Child);

impl Process for Own {
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok(self.0.try_wait()?.map(|status| status.code()))
    }

    fn end_group_after_exit(&mut self) {
        #[cfg(unix)]
        end_group(&self.0);
    }

    fn end(&mut self) {
        end_tree(&mut self.0);
    }
}

/// The environment a command is given: the variables its spec lets through, its path, and its secret.
fn environment(
    spec: &CommandSpec<'_>,
) -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>, Error> {
    let mut env = Vec::new();
    for name in spec.inherit_env {
        if name.eq_ignore_ascii_case("PATH") {
            continue;
        }
        if let Some(value) = std::env::var_os(name) {
            env.push((name.into(), value));
        }
    }
    if let Some(path) = spec.path {
        env.push(("PATH".into(), path.to_owned()));
    }
    // Windows would look in the current directory for a program before it looks on the path.
    env.push(("NoDefaultCurrentDirectoryInExePath".into(), "1".into()));
    if let Some((name, value)) = spec.secret {
        let text = core::str::from_utf8(value)
            .map_err(|_| Error::Command("the secret is not text a variable can hold"))?;
        env.push((name.into(), text.into()));
    }
    Ok(env)
}

/// A child of this process and its two output pipes.
type OwnChild = (
    std::process::Child,
    Box<dyn Read + Send>,
    Box<dyn Read + Send>,
);

/// Starts the command as a child of this process.
fn start_own(spec: &CommandSpec<'_>) -> Result<OwnChild, Error> {
    let mut command = Command::new(spec.program);
    match spec.command {
        CommandLine::Argv(args) => {
            command.args(&args[1..]);
        }
        CommandLine::Shell(text) => shell_args(&mut command, text),
    }
    command
        .current_dir(spec.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.envs(environment(spec)?);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|_| Error::Command("could not be started"))?;
    drop(command);
    let stdout = child
        .stdout
        .take()
        .ok_or(Error::Command("no output pipe"))?;
    let stderr = child.stderr.take().ok_or(Error::Command("no error pipe"))?;
    Ok((child, Box::new(stdout), Box::new(stderr)))
}

impl Runner for ProcessRunner {
    fn run(&self, spec: &CommandSpec<'_>) -> Result<CommandOutput, Error> {
        #[cfg(unix)]
        let named = STARTER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        #[cfg(not(unix))]
        let named: Option<Arc<dyn Starter>> = None;
        #[cfg(windows)]
        let mut job = None;
        let Started {
            mut process,
            stdout,
            stderr,
        } = if let Some(starter) = named {
            let args: Vec<std::ffi::OsString> = match spec.command {
                CommandLine::Argv(args) => args[1..].iter().map(Into::into).collect(),
                CommandLine::Shell(text) => vec!["-c".into(), text.into()],
            };
            starter
                .start(&Launch {
                    program: spec.program.to_path_buf(),
                    args,
                    cwd: spec.cwd.to_path_buf(),
                    env: environment(spec)?,
                })
                .map_err(|_| Error::Command("could not be started"))?
        } else {
            let (child, stdout, stderr) = start_own(spec)?;
            #[cfg(windows)]
            {
                job = contain(&child);
            }
            Started {
                process: Box::new(Own(child)),
                stdout,
                stderr,
            }
        };
        let (out, out_thread) = drain(stdout, spec.max_output);
        let (err, err_thread) = drain(stderr, spec.max_output);
        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match process.try_wait() {
                Ok(Some(code)) => {
                    // The command is over; whatever it left running is ended, still with the secret in its environment.
                    process.end_group_after_exit();
                    break code;
                }
                Ok(None) if started.elapsed() >= spec.timeout => {
                    timed_out = true;
                    process.end();
                    #[cfg(windows)]
                    drop(job.take());
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    process.end();
                    return Err(Error::Command("could not be waited for"));
                }
            }
        };
        // Something the command started may hold a pipe open for ever: give the readers a moment, then take what they have.
        let grace = Instant::now();
        while !(out_thread.is_finished() && err_thread.is_finished())
            && grace.elapsed() < Duration::from_millis(500)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let ((stdout, cut_out), (stderr, cut_err)) = (take(&out), take(&err));
        Ok(CommandOutput {
            status,
            stdout,
            stderr,
            truncated: cut_out || cut_err,
            timed_out,
        })
    }
}

/// Ends the process group the child led.
#[cfg(unix)]
fn end_group(child: &std::process::Child) {
    if let Some(group) = i32::try_from(child.id())
        .ok()
        .filter(|pid| *pid > 1)
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

/// Ends the child and everything it started: on Unix its process group (it leads one), on Windows the job it was put in is closed
/// by the caller, and the child itself is killed here.
fn end_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    end_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// Puts the child in a job object that ends everything in it when it is closed.
#[cfg(windows)]
fn contain(child: &std::process::Child) -> Option<win32job::Job> {
    use std::os::windows::io::AsRawHandle;
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&limits).ok()?;
    job.assign_process(child.as_raw_handle() as isize).ok()?;
    Some(job)
}

/// The shell the platform runs shell text with, by full path, so no program in the working directory takes its place.
pub(crate) fn shell_program() -> Result<std::path::PathBuf, Error> {
    #[cfg(unix)]
    let shell = std::path::PathBuf::from("/bin/sh");
    #[cfg(windows)]
    let shell = {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::path::PathBuf::from(root)
            .join("System32")
            .join("cmd.exe")
    };
    if shell.is_file() {
        Ok(shell)
    } else {
        Err(Error::Command("the platform's shell was not found"))
    }
}

#[cfg(unix)]
fn shell_args(command: &mut Command, text: &str) {
    command.arg("-c").arg(text);
}

#[cfg(windows)]
fn shell_args(command: &mut Command, text: &str) {
    use std::os::windows::process::CommandExt;
    // `/D`: no AutoRun command from the registry runs first, with the secret in its environment.
    command.arg("/D").arg("/C").raw_arg(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_shown_on_one_line_with_its_control_characters_escaped() {
        let line = CommandLine::Argv(vec!["curl".into(), "-H".into(), "X: it's\nhidden".into()]);
        assert_eq!(line.display(), "curl -H 'X: it'\\''s\\nhidden'");
        let shell = CommandLine::Shell("echo a\r\nrm -rf x".into());
        assert_eq!(shell.display(), "(shell) echo a\\r\\nrm -rf x");
    }

    #[test]
    fn a_bad_command_is_refused() {
        for bad in [
            serde_json::json!(""),
            serde_json::json!([]),
            serde_json::json!(["a\u{0}b"]),
            serde_json::json!(3),
            serde_json::json!([1]),
        ] {
            assert!(CommandLine::from_value(&bad, 100).is_err(), "{bad}");
        }
        let long = serde_json::json!("x".repeat(101));
        assert!(matches!(
            CommandLine::from_value(&long, 100),
            Err(Error::Limit(_))
        ));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    #[test]
    fn the_windows_shell_starts_without_autorun() {
        let mut command = std::process::Command::new("cmd");
        super::shell_args(&mut command, "echo hi");
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["/D", "/C", "echo hi"]);
    }
}
