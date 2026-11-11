//! Running a harness's own sign-in: the CLI through the spawner, what it prints relayed to the person, and the ends the
//! kind names (cancelled, timed out, refused) answered within two seconds whatever the CLI or the prompter is doing.
//!
//! The sign-in runs on a thread of its own so that the caller can answer at the deadline or on a cancel while a prompter
//! call or a read is still out; the thread lets go of what it was handed when it returns.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    Credentials, LoginAnswer, LoginForm, LoginOutcome, LoginPrompter, OpenUrl, ProcessSpawner,
};

use crate::ThroughSpawner;
use crate::account::Files;
use crate::cli::{Cli, Next, plain};
use crate::process::{Exit, Launch};
use crate::provider::track;

/// How often the caller looks at the prompter while the sign-in runs.
const LOOK: Duration = Duration::from_millis(25);
/// How long a cancelled sign-in is given to let go of what it was handed before the caller answers anyway.
const LET_GO: Duration = Duration::from_millis(1500);
/// How long a program that closed its output is given to exit.
const EXIT_WAIT: Duration = Duration::from_secs(5);

fn gone(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_GONE, why)
}

/// The programs a sign-in started, so that the caller can end them from another thread.
#[derive(Default)]
struct Running {
    ended: AtomicBool,
    programs: Mutex<Vec<Arc<Cli>>>,
}

impl Running {
    fn kill_all(&self) {
        self.ended.store(true, Ordering::SeqCst);
        let programs: Vec<_> = self
            .programs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for program in programs {
            program.kill();
        }
    }
}

/// What a sign-in works with: the spawner, the prompter and the deadline.
pub struct Ctx {
    spawn: ThroughSpawner,
    files: Files,
    prompter: Arc<Object<dyn LoginPrompter>>,
    deadline: Instant,
    running: Arc<Running>,
}

impl Ctx {
    /// Refuses once the sign-in has been cancelled or has run out of time.
    ///
    /// # Errors
    ///
    /// `GUATIAO_ERR_GONE`, `login cancelled` or `login timed out`.
    pub fn check(&self) -> Result<(), ProviderError> {
        if Instant::now() >= self.deadline {
            return Err(gone("login timed out"));
        }
        if self.running.ended.load(Ordering::SeqCst) || self.prompter.cancelled().unwrap_or(false) {
            return Err(gone("login cancelled"));
        }
        Ok(())
    }

    fn mapped<T>(&self, answer: Result<T, ProviderError>) -> Result<T, ProviderError> {
        match answer {
            Ok(value) => {
                self.check()?;
                Ok(value)
            }
            Err(error) if error.status == Status::GUATIAO_ERR_GONE => {
                Err(gone(if Instant::now() >= self.deadline {
                    "login timed out"
                } else {
                    "login cancelled"
                }))
            }
            Err(error) => Err(error),
        }
    }

    /// Asks the person to open a page.
    ///
    /// # Errors
    ///
    /// The person cancelled, or the sign-in ran out of time.
    pub fn open_url(&self, open: OpenUrl) -> Result<(), ProviderError> {
        self.mapped(self.prompter.open_url(open))
    }

    /// Asks the person to fill a form.
    ///
    /// # Errors
    ///
    /// As [`open_url`](Self::open_url).
    pub fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        self.mapped(self.prompter.ask(form))
    }

    /// Starts `launch` on the spawner. The program is ended with the sign-in.
    ///
    /// # Errors
    ///
    /// The sign-in has ended, or the program could not be started.
    pub fn spawn(&self, launch: &Launch) -> Result<Child, ProviderError> {
        self.check()?;
        let cli = Cli::start(&self.spawn, launch).map_err(|error| {
            let status = if error.kind() == io::ErrorKind::NotFound {
                Status::GUATIAO_ERR_NOT_FOUND
            } else {
                Status::GUATIAO_ERR_INTERNAL
            };
            ProviderError::new(status, "the sign-in's program could not be started")
        })?;
        self.running
            .programs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&cli));
        if self.running.ended.load(Ordering::SeqCst) {
            cli.kill();
        }
        Ok(Child {
            cli,
            out: Vec::new(),
            err: Vec::new(),
        })
    }

    /// The file `path` in the sign-in's directory.
    ///
    /// # Errors
    ///
    /// The spawner's refusal; `GUATIAO_ERR_NOT_FOUND` for a file that is not there.
    pub fn read_file(&self, path: &str) -> Result<String, ProviderError> {
        self.files
            .read(path)?
            .ok_or_else(|| ProviderError::new(Status::GUATIAO_ERR_NOT_FOUND, "no such file"))
    }
}

/// A program a sign-in started, whose output is read a line at a time.
pub struct Child {
    cli: Arc<Cli>,
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Child {
    fn lines(buffer: &mut Vec<u8>, bytes: &[u8], lines: &mut Vec<String>) {
        buffer.extend_from_slice(bytes);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            lines.push(plain(
                String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']),
            ));
        }
        if buffer.len() > (1 << 16) {
            buffer.clear();
        }
    }

    /// Runs it to its end, handing each line it prints (standard error's too, without terminal colour) to `on_line` as
    /// it comes, and answers how it ended.
    ///
    /// # Errors
    ///
    /// The sign-in was cancelled or ran out of time, or `on_line` refused; the program is ended then.
    pub fn run(
        &mut self,
        ctx: &Ctx,
        mut on_line: impl FnMut(&str) -> Result<(), ProviderError>,
    ) -> Result<Exit, ProviderError> {
        loop {
            ctx.check()?;
            let mut lines = Vec::new();
            let ended = match self.cli.next(Duration::from_millis(40)) {
                Next::Piece(piece) => {
                    let buffer = if piece.error {
                        &mut self.err
                    } else {
                        &mut self.out
                    };
                    Self::lines(buffer, &piece.bytes, &mut lines);
                    false
                }
                Next::Idle => false,
                Next::Ended => true,
            };
            for line in lines {
                on_line(&line)?;
            }
            if ended {
                break;
            }
        }
        self.cli.wait(EXIT_WAIT).ok_or_else(|| {
            ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "the program did not exit")
        })
    }
}

/// Runs `work` as a sign-in on a thread of its own and answers its outcome, or a cancel or a timeout the moment they
/// come.
///
/// # Errors
///
/// What `work` refused; `GUATIAO_ERR_GONE` with `login cancelled` or `login timed out`; `GUATIAO_ERR_INTERNAL` when the
/// thread could not start or died.
pub fn run(
    spawner: Object<dyn ProcessSpawner>,
    prompter: Object<dyn LoginPrompter>,
    timeout_ms: u64,
    work: impl FnOnce(&Ctx) -> Result<LoginOutcome, ProviderError> + Send + 'static,
) -> Result<LoginOutcome, ProviderError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let running = Arc::new(Running::default());
    let prompter = Arc::new(prompter);
    let spawner = Arc::new(spawner);
    let ctx = Ctx {
        files: Files::new(Arc::clone(&spawner)),
        spawn: ThroughSpawner::shared(spawner),
        prompter: Arc::clone(&prompter),
        deadline,
        running: Arc::clone(&running),
    };
    let killer = Arc::clone(&running);
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("coqui-acp-login".into())
        .spawn(move || {
            let outcome = work(&ctx);
            ctx.running.kill_all();
            drop(ctx);
            let _ = tx.send(outcome);
        })
        .map_err(|_| ProviderError::new(Status::GUATIAO_ERR_INTERNAL, "no thread to sign in on"))?;
    track(thread);
    loop {
        match rx.recv_timeout(LOOK) {
            Ok(outcome) => return outcome,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ProviderError::new(
                    Status::GUATIAO_ERR_INTERNAL,
                    "the sign-in failed",
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= deadline {
            killer.kill_all();
            return Err(gone("login timed out"));
        }
        if prompter.cancelled().unwrap_or(false) {
            killer.kill_all();
            // Given a moment to let go of what it was handed, then answered whatever it is doing.
            let _ = rx.recv_timeout(LET_GO);
            return Err(gone("login cancelled"));
        }
    }
}

/// A credentials map holding one entry.
#[must_use]
pub fn one(key: &str, value: &str) -> Credentials {
    let mut credentials = Credentials::new();
    credentials.insert(key, value.to_string());
    credentials
}
