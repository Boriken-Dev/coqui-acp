//! What a Pi session is started with.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use coqui_acp_harness::{Launch, Mode};

/// How a session begins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionStart {
    /// A new session.
    #[default]
    New,
    /// An earlier session, picked up where it ended; its history is not replayed.
    Resume(String),
    /// An earlier session, its history replayed to the sink first.
    Load(String),
}

/// Whether `id` can be a Pi session id: letters, digits and dashes, 1 to 128. Pi matches a partial id, so the id Pi
/// answers with is compared with the one asked for as well.
#[must_use]
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// What a session of Pi is started with. Everything but the environment becomes arguments of `pi --mode rpc`, or a file in
/// the launch's private directory; none of the person's text is an argument.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct PiLaunch {
    /// The program: `pi`, or the path of one. On Windows npm's shim is `pi.cmd`.
    pub executable: OsString,
    /// The directory Pi works in: the process's own, since Pi has no other way to be given one.
    pub cwd: PathBuf,
    /// A new session, or an earlier one.
    pub session: SessionStart,
    /// The model every session starts on, as `provider/id`; Pi's own when absent.
    pub model: Option<String>,
    /// The thinking level every session starts at; the model's own when absent.
    pub effort: Option<String>,
    /// Text appended to Pi's system prompt (`--append-system-prompt`, from a file made for the launch).
    pub system_prompt: Option<String>,
    /// More directories the session works in. Pi has no such setting: they are where a diff may read a file's old content.
    pub additional_directories: Vec<PathBuf>,
    /// The directories a file's old content may be read from for a diff; empty takes `cwd` and the more directories.
    pub working_roots: Vec<PathBuf>,
    /// The mode a session starts in, and the loosest a client may choose; Manual when absent.
    pub mode: Option<Mode>,
    /// Whether the Bypass mode is offered.
    pub allow_bypass: bool,
    /// The modes a client may choose, besides the mode and every stricter one. Bypass needs `allow_bypass`; Auto is never
    /// offered.
    pub modes: Option<Vec<Mode>>,
    /// Whether Manual mode asks even before a tool that only reads.
    pub ask_for_reads: bool,
    /// The directory the private one holding the permission gate is made in; the system's temporary directory when absent.
    pub gate_dir: Option<PathBuf>,
    /// Arguments of the caller's own, after the others, unchecked.
    pub extra_args: Vec<OsString>,
    /// Variables Pi is started with.
    pub env: BTreeMap<OsString, OsString>,
    /// Variables removed from what Pi inherits.
    pub remove_env: Vec<OsString>,
    /// Whether Pi inherits nothing.
    pub clear_env: bool,
}

impl fmt::Debug for PiLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The environment may hold a credential: only its names are shown.
        f.debug_struct("PiLaunch")
            .field("executable", &self.executable)
            .field("cwd", &self.cwd)
            .field("session", &self.session)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("mode", &self.mode)
            .field("allow_bypass", &self.allow_bypass)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl PiLaunch {
    /// A launch of `pi` in `cwd`, with everything else left to Pi and the defaults.
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            executable: "pi".into(),
            cwd: cwd.into(),
            session: SessionStart::New,
            model: None,
            effort: None,
            system_prompt: None,
            additional_directories: Vec::new(),
            working_roots: Vec::new(),
            mode: None,
            allow_bypass: false,
            modes: None,
            ask_for_reads: false,
            gate_dir: None,
            extra_args: Vec::new(),
            env: BTreeMap::new(),
            remove_env: Vec::new(),
            clear_env: false,
        }
    }

    /// The mode a session starts in.
    #[must_use]
    pub fn start_mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::Manual)
    }

    /// The command: `pi --mode rpc`, with the gate when there is one (`-e`), the session to open, the model and the thinking
    /// level, the system prompt's file, and the caller's own arguments.
    #[must_use]
    pub fn launch(&self, extension: Option<&Path>, prompt: Option<&Path>) -> Launch {
        let mut launch = Launch::new(self.executable.clone())
            .args(["--mode", "rpc"])
            .cwd(&self.cwd)
            .clear_env(self.clear_env);
        if let Some(extension) = extension {
            launch = launch.arg("-e").arg(extension);
        }
        match &self.session {
            SessionStart::New => {}
            SessionStart::Resume(id) | SessionStart::Load(id) => {
                launch = launch.arg("--session").arg(id);
            }
        }
        if let Some(model) = &self.model {
            launch = launch.arg("--model").arg(model);
        }
        if let Some(effort) = &self.effort {
            launch = launch.arg("--thinking").arg(effort);
        }
        if let Some(prompt) = prompt {
            launch = launch.arg("--append-system-prompt").arg(prompt);
        }
        launch = launch.args(self.extra_args.iter());
        launch.env.clone_from(&self.env);
        launch.remove_env.clone_from(&self.remove_env);
        launch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_plain() {
        assert!(valid_session_id("01a1ca97-a3ac-7698-8959-be54d841fc56"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../x"));
        assert!(!valid_session_id("a b"));
    }

    #[test]
    fn the_command_line_holds_no_text_of_the_person() {
        let mut launch = PiLaunch::new("/w");
        launch.session = SessionStart::Resume("abc-1".into());
        launch.model = Some("anthropic/claude".into());
        launch.effort = Some("high".into());
        let command = launch.launch(Some(Path::new("/g/gate.ts")), Some(Path::new("/g/p.md")));
        let args: Vec<_> = command
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--mode",
                "rpc",
                "-e",
                "/g/gate.ts",
                "--session",
                "abc-1",
                "--model",
                "anthropic/claude",
                "--thinking",
                "high",
                "--append-system-prompt",
                "/g/p.md"
            ]
        );
    }
}
