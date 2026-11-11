//! Running a harness's own sign-in: the CLI through the spawner, what it prints relayed to the person, and the ends the
//! kind names (cancelled, timed out, refused) answered within two seconds whatever the CLI or the prompter is doing.
//!
//! The sign-in runs on a thread of its own so that the caller can answer at the deadline or on a cancel while a prompter
//! call or a read is still out; the thread lets go of what it was handed when it returns.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    Credentials, ErrorOutput, LoginAnswer, LoginForm, LoginOutcome, LoginPrompter, OpenUrl,
    ProcessExit, ProcessSpawner, READ_LINK_DOWN, READ_TIMED_OUT, SpawnSpec, SpawnedProcess,
};

use super::threads;

/// How often the caller looks at the prompter while the sign-in runs.
const LOOK: Duration = Duration::from_millis(25);
/// How long a cancelled sign-in is given to let go of what it was handed before the caller answers anyway.
const LET_GO: Duration = Duration::from_millis(1500);
/// The largest file a sign-in is read back from.
pub const MAX_FILE: u64 = 1 << 20;

fn gone(why: &str) -> ProviderError {
    ProviderError::new(Status::GUATIAO_ERR_GONE, why)
}

/// The programs a sign-in started, so that the caller can end them from another thread.
#[derive(Default)]
struct Running {
    ended: core::sync::atomic::AtomicBool,
    processes: Mutex<Vec<Arc<Object<dyn SpawnedProcess>>>>,
}

impl Running {
    fn kill_all(&self) {
        self.ended.store(true, core::sync::atomic::Ordering::SeqCst);
        let processes: Vec<_> = self
            .processes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for process in processes {
            let _ = process.kill();
        }
    }
}

/// What a sign-in works with: the spawner, the prompter and the deadline.
pub struct Ctx {
    spawner: Arc<Object<dyn ProcessSpawner>>,
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
        if self
            .running
            .ended
            .load(core::sync::atomic::Ordering::SeqCst)
            || self.prompter.cancelled().unwrap_or(false)
        {
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
    /// As [`open_url`](Self::open_url).
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

    /// Starts `spec` on the spawner, with its standard error kept apart. The program is ended with the sign-in.
    ///
    /// # Errors
    ///
    /// The sign-in has ended, or the spawner refused.
    pub fn spawn(&self, spec: SpawnSpec) -> Result<Child, ProviderError> {
        self.check()?;
        let process = Arc::new(
            self.spawner
                .spawn(spec.error_output(ErrorOutput::Separate))?,
        );
        self.running
            .processes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&process));
        if self
            .running
            .ended
            .load(core::sync::atomic::Ordering::SeqCst)
        {
            let _ = process.kill();
        }
        Ok(Child {
            process,
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
        Ok(self.spawner.read_file(path, MAX_FILE)?.expose().to_text())
    }
}

/// A program a sign-in started.
pub struct Child {
    process: Arc<Object<dyn SpawnedProcess>>,
    out: Vec<u8>,
    err: Vec<u8>,
}

/// Removes terminal colour and hyperlink sequences from a line.
#[must_use]
pub fn clean(line: &str) -> String {
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                clean.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut last = ' ';
                for c in chars.by_ref() {
                    if c == '\u{7}' || (last == '\u{1b}' && c == '\\') {
                        break;
                    }
                    last = c;
                }
            }
            _ => {}
        }
    }
    clean
}

impl Child {
    fn drain(&mut self, error: bool, wait_ms: u64, lines: &mut Vec<String>) {
        let mut chunk = [0u8; 4096];
        loop {
            let read = if error {
                self.process.read_error(&mut chunk, wait_ms)
            } else {
                self.process.read(&mut chunk, wait_ms)
            };
            let n = match read {
                Ok(n) if n > 0 => usize::try_from(n).unwrap_or(0),
                Ok(READ_TIMED_OUT | READ_LINK_DOWN) | Ok(_) | Err(_) => return,
            };
            let buffer = if error { &mut self.err } else { &mut self.out };
            buffer.extend_from_slice(&chunk[..n]);
            while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                lines.push(clean(&String::from_utf8_lossy(&line)));
            }
            if buffer.len() > (1 << 16) {
                buffer.clear();
            }
        }
    }

    /// Runs it to its end, handing each line it prints (standard error's too) to `on_line` as it comes, and answers how
    /// it ended.
    ///
    /// # Errors
    ///
    /// The sign-in was cancelled or ran out of time (the program is then ended), or `on_line` refused.
    pub fn run(
        &mut self,
        ctx: &Ctx,
        on_line: impl FnMut(&str) -> Result<(), ProviderError>,
    ) -> Result<ProcessExit, ProviderError> {
        self.run_while(|| ctx.check(), on_line)
    }

    /// As [`run`](Self::run), until `proceed` refuses.
    ///
    /// # Errors
    ///
    /// What `proceed` or `on_line` refused.
    pub fn run_while(
        &mut self,
        mut proceed: impl FnMut() -> Result<(), ProviderError>,
        mut on_line: impl FnMut(&str) -> Result<(), ProviderError>,
    ) -> Result<ProcessExit, ProviderError> {
        loop {
            proceed()?;
            let mut lines = Vec::new();
            self.drain(false, 20, &mut lines);
            self.drain(true, 20, &mut lines);
            for line in lines {
                on_line(&line)?;
            }
            let exit = self.process.wait(0)?;
            if !exit.running {
                let mut lines = Vec::new();
                self.drain(false, 0, &mut lines);
                self.drain(true, 0, &mut lines);
                for line in lines {
                    on_line(&line)?;
                }
                return Ok(exit);
            }
        }
    }
}

/// Runs `spec` on `spawner` to its end, reading its output away, and ends it when it has not finished in `limit`.
///
/// # Errors
///
/// The spawner's refusal; `GUATIAO_ERR_INTERNAL` when the program did not finish in time.
pub fn run_to_end(
    spawner: &Object<dyn ProcessSpawner>,
    spec: SpawnSpec,
    limit: Duration,
) -> Result<ProcessExit, ProviderError> {
    let process = Arc::new(spawner.spawn(spec.error_output(ErrorOutput::Separate))?);
    let mut child = Child {
        process: Arc::clone(&process),
        out: Vec::new(),
        err: Vec::new(),
    };
    let deadline = Instant::now() + limit;
    let outcome = child.run_while(
        || {
            if Instant::now() >= deadline {
                Err(ProviderError::new(
                    Status::GUATIAO_ERR_INTERNAL,
                    "the program did not finish in time",
                ))
            } else {
                Ok(())
            }
        },
        |_| Ok(()),
    );
    let _ = process.kill();
    outcome
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
    let ctx = Ctx {
        spawner: Arc::new(spawner),
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
    threads::track(thread);
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
