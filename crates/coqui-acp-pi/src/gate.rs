//! The permission gate: a Pi extension this crate ships as text and loads with `-e`.
//!
//! Pi runs every tool unasked. The gate puts each tool call, with the input it will run on, to this program as an extension
//! dialog (`select`), so what a mode allows is decided here, and a call that needs leave becomes an ACP permission
//! request. It is written to a private directory made for one launch and removed with it; nothing is written to Pi's own
//! configuration. It registers a command whose name holds a nonce of the launch, which the session looks for before it
//! trusts Pi: a Pi that did not load the gate is refused.

use alloc::string::String;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The extension's source: TypeScript, which Pi loads itself. `__NONCE__` stands where a launch's nonce goes.
pub const GATE_SOURCE: &str = include_str!("gate.ts");

/// The Node preload that keeps the project out of every program search Pi makes.
pub const GUARD_SOURCE: &str = include_str!("guard.cjs");

/// What a dialog's title starts with when it is the gate's.
pub(crate) const PREFIX: &str = "coqui-acp-pi-gate ";

/// What the gate's command is called, before the nonce.
const COMMAND_PREFIX: &str = "coqui-acp-pi-gate-";

/// The files one launch of Pi is given, in a directory of its own that is removed when this is dropped.
#[derive(Debug)]
pub struct Gate {
    dir: PathBuf,
    extension: PathBuf,
    guard: PathBuf,
    prompt: Option<PathBuf>,
    nonce: String,
}

/// Makes `dir` closed to everyone but the person on Windows, where a new directory inherits its parent's access.
#[cfg(windows)]
fn close_to_others(dir: &Path) -> io::Result<()> {
    let user = std::env::var("USERNAME").map_err(|_| io::Error::other("no user name"))?;
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let who = if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    };
    let done = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{who}:(OI)(CI)F"))
        .output()?;
    if done.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "the gate's directory could not be closed to other users",
        ))
    }
}

#[cfg(not(windows))]
fn close_to_others(_dir: &Path) -> io::Result<()> {
    Ok(())
}

impl Gate {
    /// Writes the extension, and the text to append to Pi's system prompt when there is some, to a new directory under
    /// `parent` (the system's temporary directory when `None`). The directory is made by this call alone (one that is
    /// there already is an error), closed to every other user, and the extension carries a nonce of this launch.
    ///
    /// # Errors
    ///
    /// The directory or a file could not be written, or the directory could not be closed to others.
    pub fn write(parent: Option<&Path>, system_prompt: Option<&str>) -> io::Result<Self> {
        let gate = Self::make(parent, system_prompt)?;
        std::fs::write(
            &gate.extension,
            GATE_SOURCE.replace("__NONCE__", &gate.nonce),
        )?;
        std::fs::write(&gate.guard, GUARD_SOURCE)?;
        if let (Some(path), Some(text)) = (&gate.prompt, system_prompt) {
            std::fs::write(path, text)?;
        }
        Ok(gate)
    }

    /// The directory, made exclusively and closed to others, and the paths in it.
    fn make(parent: Option<&Path>, system_prompt: Option<&str>) -> io::Result<Self> {
        let base = parent.map_or_else(std::env::temp_dir, Path::to_path_buf);
        std::fs::create_dir_all(&base)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let dir = base.join(alloc::format!("coqui-acp-pi-{}", uuid::Uuid::new_v4()));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        // Not recursive: the last directory is made here, or the call fails.
        builder.create(&dir)?;
        let gate = Self {
            extension: dir.join("gate.ts"),
            guard: dir.join("guard.cjs"),
            prompt: system_prompt.map(|_| dir.join("system-prompt.md")),
            dir,
            nonce,
        };
        close_to_others(&gate.dir)?;
        Ok(gate)
    }

    /// Writes the guard alone, to a new directory made as [`Gate::write`] makes its own: for a Pi that runs no tool (the
    /// listing of its models), which still must not find a program in the project.
    ///
    /// # Errors
    ///
    /// As [`Gate::write`].
    pub fn guard_only(parent: Option<&Path>) -> io::Result<Self> {
        let gate = Self::make(parent, None)?;
        std::fs::write(&gate.guard, GUARD_SOURCE)?;
        Ok(gate)
    }

    /// The extension file, for `-e`.
    #[must_use]
    pub fn extension(&self) -> &Path {
        &self.extension
    }

    /// The Node preload (`guard.cjs`), for `--require`.
    #[must_use]
    pub fn guard(&self) -> &Path {
        &self.guard
    }

    /// The file holding the system prompt to append, for `--append-system-prompt`.
    #[must_use]
    pub fn prompt(&self) -> Option<&Path> {
        self.prompt.as_deref()
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The command the gate registers in Pi, whose being listed shows that Pi loaded it.
    #[must_use]
    pub fn command(&self) -> String {
        alloc::format!("{COMMAND_PREFIX}{}", self.nonce)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        // Windows refuses to remove a directory while the process that used it is still ending.
        for _ in 0..40 {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => return,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(core::time::Duration::from_millis(50)),
            }
        }
    }
}

/// The call a gate dialog is about.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    /// The id of the tool call Pi gives the extension.
    pub id: Option<String>,
    /// The tool.
    pub tool: String,
    /// The input the call will run on, as the gate saw it (long text cut, which `_cut` names).
    pub input: Value,
}

pub(crate) fn call_of(title: &str) -> Option<Call> {
    let call: Value = serde_json::from_str(title.strip_prefix(PREFIX)?).ok()?;
    Some(Call {
        id: call.get("id").and_then(Value::as_str).map(String::from),
        tool: call.get("tool")?.as_str()?.to_string(),
        input: call.get("input").cloned().unwrap_or(Value::Null),
    })
}
