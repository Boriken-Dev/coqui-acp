//! Running a command the person approved, with a secret in one environment variable.

use alloc::sync::Arc;
use core::fmt;
use core::time::Duration;
use std::io::Read;
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
            Self::Shell(text) => format!("(shell) {}", escape(text, false)),
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
                        format!("'{}'", escape(arg, true))
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

fn escape(text: &str, quote: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\'' if quote => out.push_str("'\\''"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out
}

/// What a [`Runner`] is asked to run.
pub struct CommandSpec<'a> {
    /// The command.
    pub command: &'a CommandLine,
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
/// secret, standard input is closed, and a command past its timeout is killed. It kills the child only, not what the child started.
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

impl Runner for ProcessRunner {
    fn run(&self, spec: &CommandSpec<'_>) -> Result<CommandOutput, Error> {
        let mut command = match spec.command {
            CommandLine::Argv(args) => {
                let mut command = Command::new(&args[0]);
                command.args(&args[1..]);
                command
            }
            CommandLine::Shell(text) => shell(text),
        };
        command
            .current_dir(spec.cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in spec.inherit_env {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some((name, value)) = spec.secret {
            let text = core::str::from_utf8(value)
                .map_err(|_| Error::Command("the secret is not text a variable can hold"))?;
            command.env(name, text);
        }
        let mut child = command
            .spawn()
            .map_err(|_| Error::Command("could not be started"))?;
        let (out, out_thread) = drain(
            child
                .stdout
                .take()
                .ok_or(Error::Command("no output pipe"))?,
            spec.max_output,
        );
        let (err, err_thread) = drain(
            child.stderr.take().ok_or(Error::Command("no error pipe"))?,
            spec.max_output,
        );
        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if started.elapsed() >= spec.timeout => {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return Err(Error::Command("could not be waited for")),
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

#[cfg(unix)]
fn shell(text: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(text);
    command
}

#[cfg(windows)]
fn shell(text: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    command.arg("/C").raw_arg(text);
    command
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
