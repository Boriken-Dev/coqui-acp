//! Running a harness's own sign-in on top of the shared supervisor: the CLI through the spawner, what it prints relayed to
//! the person, and the ends the kind names answered by [`crate::login::supervise`].

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    Credentials, LoginAnswer, LoginForm, LoginOutcome, LoginPrompter, OpenUrl, ProcessSpawner,
};

use crate::ThroughSpawner;
use crate::account::Files;
use crate::cli::{Cli, Next, plain};
use crate::login::{Job, asked, supervise};
use crate::process::{Exit, Launch};

/// How long a program that closed its output is given to exit.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// What a sign-in's work reaches: the spawner, the prompter and the supervisor's job.
pub struct Ctx<'a> {
    spawn: ThroughSpawner,
    files: Files,
    prompter: &'a Object<dyn LoginPrompter>,
    job: &'a Job,
}

impl Ctx<'_> {
    /// Refuses once the supervisor has answered.
    ///
    /// # Errors
    ///
    /// `GUATIAO_ERR_GONE`.
    pub fn check(&self) -> Result<(), ProviderError> {
        self.job.check()
    }

    /// Asks the person to open a page.
    ///
    /// # Errors
    ///
    /// The person cancelled.
    pub fn open_url(&self, open: OpenUrl) -> Result<(), ProviderError> {
        asked(self.prompter.open_url(open))
    }

    /// Asks the person to fill a form.
    ///
    /// # Errors
    ///
    /// The person cancelled.
    pub fn ask(&self, form: LoginForm) -> Result<LoginAnswer, ProviderError> {
        asked(self.prompter.ask(form))
    }

    /// Starts `launch` on the spawner; the supervisor ends it with the sign-in.
    ///
    /// # Errors
    ///
    /// The sign-in has ended, or the program could not be started.
    pub fn spawn(&self, launch: &Launch) -> Result<Child, ProviderError> {
        Ok(Child {
            cli: self.job.start(&self.spawn, launch)?,
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
    /// The sign-in was ended, or `on_line` refused.
    pub fn run(
        &mut self,
        ctx: &Ctx<'_>,
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

/// Runs `work` as a sign-in under the shared supervisor.
///
/// # Errors
///
/// What `work` refused; `GUATIAO_ERR_GONE` with `login cancelled` or `login timed out`.
pub fn run(
    spawner: Object<dyn ProcessSpawner>,
    prompter: Object<dyn LoginPrompter>,
    timeout_ms: u64,
    work: impl FnOnce(&Ctx<'_>) -> Result<LoginOutcome, ProviderError> + Send + 'static,
) -> Result<LoginOutcome, ProviderError> {
    supervise(
        Arc::new(spawner),
        prompter,
        timeout_ms,
        Box::new(move |spawner, prompter, job| {
            let ctx = Ctx {
                files: Files::new(Arc::clone(spawner)),
                spawn: ThroughSpawner::shared(Arc::clone(spawner)),
                prompter,
                job,
            };
            work(&ctx)
        }),
    )
}

/// A credentials map holding one entry.
#[must_use]
pub fn one(key: &str, value: &str) -> Credentials {
    let mut credentials = Credentials::new();
    credentials.insert(key, value.to_string());
    credentials
}
