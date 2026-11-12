use core::time::Duration;
use std::path::PathBuf;

use coqui_acp::secret::{DEFAULT_LIFETIME, Limits};

/// Every limit of the host side, with its default. A default is the safe choice; a looser one is configuration and is listed by
/// [`Settings::non_defaults`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Settings {
    /// What a form may hold (questions, choices, schema and value sizes). The extension's defaults.
    pub limits: Limits,
    /// Secrets one session keeps at once. Default 32.
    pub max_handles: usize,
    /// How long a handle lives after it was made. Default 1 hour.
    pub handle_lifetime: Duration,
    /// How long the person has to answer a request or a show. Default 240 s, the extension's own.
    pub answer_lifetime: Duration,
    /// How long a request id is remembered. Default 330 s: the verifier's default maximum lifetime (5 min) plus its skew (30 s); a
    /// verifier configured with more needs this raised with it.
    pub request_memory: Duration,
    /// The message of a prompt, in bytes. Default 2048.
    pub max_message_bytes: usize,
    /// The plain values of a show, as JSON text, in bytes. Default 64 KiB.
    pub max_values_bytes: usize,
    /// A command, as the person is shown it, in bytes. Default 8192.
    pub max_command_bytes: usize,
    /// How long a command may run. Default 60 s.
    pub command_timeout: Duration,
    /// Output kept of each stream of a command, in bytes. Default 64 KiB.
    pub max_output_bytes: usize,
    /// The mode of a file written for a use when the call names none (Unix). Default `0o600`.
    pub file_mode: u32,
    /// The widest mode a call may ask for (Unix); a mode with a bit outside it is refused. Default `0o700`.
    pub max_file_mode: u32,
    /// Directories besides the session's roots a secret may be written to, such as a private temporary area. Default none. A file
    /// there is not under the agent's roots, so its tools cannot read it back.
    pub write_dirs: Vec<PathBuf>,
    /// The environment variables a command inherits from the host; every other one is removed. Default `PATH`, `HOME`, the
    /// temporary-directory and locale variables, and Windows' `SystemRoot`, `ComSpec`, `PATHEXT`, `USERPROFILE`.
    pub inherit_env: Vec<String>,
    /// Variable names a secret may not be put in, because they change how a program is found or started: a name, or a prefix ending
    /// in `*`, compared without regard to case. A small default of the mechanism's own; the person adds to it for the programs they
    /// run, and nothing here claims to be complete.
    pub denied_variables: Vec<String>,
    /// The shortest piece of a secret's base64 form that scrubbing looks for inside other text, in characters. Default 8: shorter
    /// pieces would match unrelated output. The whole value is always looked for, in every form.
    pub scrub_min_piece: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: Limits::new(),
            max_handles: 32,
            handle_lifetime: Duration::from_secs(3600),
            answer_lifetime: DEFAULT_LIFETIME,
            request_memory: Duration::from_secs(330),
            max_message_bytes: 2048,
            max_values_bytes: 64 * 1024,
            max_command_bytes: 8192,
            command_timeout: Duration::from_secs(60),
            max_output_bytes: 64 * 1024,
            file_mode: 0o600,
            max_file_mode: 0o700,
            write_dirs: Vec::new(),
            inherit_env: [
                "PATH",
                "HOME",
                "LANG",
                "LC_ALL",
                "TERM",
                "TMPDIR",
                "TEMP",
                "TMP",
                "SystemRoot",
                "ComSpec",
                "PATHEXT",
                "USERPROFILE",
            ]
            .map(String::from)
            .to_vec(),
            denied_variables: [
                "PATH",
                "PATHEXT",
                "COMSPEC",
                "IFS",
                "ENV",
                "BASH_ENV",
                "HOME",
                "USERPROFILE",
                "TMPDIR",
                "TEMP",
                "TMP",
                "SYSTEMROOT",
                "LD_*",
                "DYLD_*",
            ]
            .map(String::from)
            .to_vec(),
            scrub_min_piece: 8,
        }
    }
}

/// One setting that is looser than its default.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NonDefault {
    /// The setting's name.
    pub setting: &'static str,
    /// Its value, as text.
    pub value: String,
    /// Its default, as text.
    pub default: String,
}

impl Settings {
    /// Every setting looser than its default: a limit raised, a time lengthened, a mode widened, a directory added, an
    /// environment variable passed on, a scrubbing piece shortened.
    #[must_use]
    pub fn non_defaults(&self) -> Vec<NonDefault> {
        let base = Self::default();
        let mut out = Vec::new();
        let mut note = |setting: &'static str, loose: bool, value: String, default: String| {
            if loose {
                out.push(NonDefault {
                    setting,
                    value,
                    default,
                });
            }
        };
        let (l, d) = (&self.limits, &base.limits);
        note(
            "limits",
            l.fields > d.fields
                || l.options > d.options
                || l.schema_bytes > d.schema_bytes
                || l.name_bytes > d.name_bytes
                || l.value_bytes > d.value_bytes,
            format!("{l:?}"),
            format!("{d:?}"),
        );
        note(
            "max_handles",
            self.max_handles > base.max_handles,
            self.max_handles.to_string(),
            base.max_handles.to_string(),
        );
        note(
            "handle_lifetime",
            self.handle_lifetime > base.handle_lifetime,
            format!("{:?}", self.handle_lifetime),
            format!("{:?}", base.handle_lifetime),
        );
        note(
            "answer_lifetime",
            self.answer_lifetime > base.answer_lifetime,
            format!("{:?}", self.answer_lifetime),
            format!("{:?}", base.answer_lifetime),
        );
        note(
            "max_message_bytes",
            self.max_message_bytes > base.max_message_bytes,
            self.max_message_bytes.to_string(),
            base.max_message_bytes.to_string(),
        );
        note(
            "max_values_bytes",
            self.max_values_bytes > base.max_values_bytes,
            self.max_values_bytes.to_string(),
            base.max_values_bytes.to_string(),
        );
        note(
            "max_command_bytes",
            self.max_command_bytes > base.max_command_bytes,
            self.max_command_bytes.to_string(),
            base.max_command_bytes.to_string(),
        );
        note(
            "command_timeout",
            self.command_timeout > base.command_timeout,
            format!("{:?}", self.command_timeout),
            format!("{:?}", base.command_timeout),
        );
        note(
            "max_output_bytes",
            self.max_output_bytes > base.max_output_bytes,
            self.max_output_bytes.to_string(),
            base.max_output_bytes.to_string(),
        );
        note(
            "file_mode",
            self.file_mode & !base.file_mode != 0,
            format!("{:o}", self.file_mode),
            format!("{:o}", base.file_mode),
        );
        note(
            "max_file_mode",
            self.max_file_mode & !base.max_file_mode != 0,
            format!("{:o}", self.max_file_mode),
            format!("{:o}", base.max_file_mode),
        );
        note(
            "write_dirs",
            !self.write_dirs.is_empty(),
            format!("{:?}", self.write_dirs),
            "[]".into(),
        );
        let extra: Vec<&String> = self
            .inherit_env
            .iter()
            .filter(|name| !base.inherit_env.contains(name))
            .collect();
        note(
            "inherit_env",
            !extra.is_empty(),
            format!("{extra:?}"),
            "the default list".into(),
        );
        let dropped: Vec<&String> = base
            .denied_variables
            .iter()
            .filter(|name| !self.denied_variables.contains(name))
            .collect();
        note(
            "denied_variables",
            !dropped.is_empty(),
            format!("without {dropped:?}"),
            "the default list".into(),
        );
        note(
            "scrub_min_piece",
            self.scrub_min_piece < base.scrub_min_piece,
            self.scrub_min_piece.to_string(),
            base.scrub_min_piece.to_string(),
        );
        out
    }
}
