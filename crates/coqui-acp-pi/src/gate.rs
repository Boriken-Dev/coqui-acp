//! The permission gate: a Pi extension this crate ships as text and loads with `-e`.
//!
//! Pi runs every tool unasked. The gate puts each tool call to this program as an extension dialog (`select`), so what a
//! mode allows is decided here, and a call that needs leave becomes an ACP permission request. It is written to a private
//! directory made for one launch and removed with it; nothing is written to Pi's own configuration.

use alloc::string::String;
use std::io;
use std::path::{Path, PathBuf};

/// The extension's source: TypeScript, which Pi loads itself.
pub const GATE_SOURCE: &str = include_str!("gate.ts");

/// What a dialog's title starts with when it is the gate's.
pub(crate) const PREFIX: &str = "coqui-acp-pi-gate ";

/// The files one launch of Pi is given, in a directory of its own that is removed when this is dropped.
#[derive(Debug)]
pub struct Gate {
    dir: PathBuf,
    extension: PathBuf,
    prompt: Option<PathBuf>,
}

impl Gate {
    /// Writes the extension, and the text to append to Pi's system prompt when there is some, to a new directory under
    /// `parent` (the system's temporary directory when `None`). On Unix the directory is private to the user.
    ///
    /// # Errors
    ///
    /// The directory or a file could not be written.
    pub fn write(parent: Option<&Path>, system_prompt: Option<&str>) -> io::Result<Self> {
        let base = parent.map_or_else(std::env::temp_dir, Path::to_path_buf);
        let dir = base.join(alloc::format!("coqui-acp-pi-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        let gate = Self {
            extension: dir.join("gate.ts"),
            prompt: system_prompt.map(|_| dir.join("system-prompt.md")),
            dir,
        };
        std::fs::write(&gate.extension, GATE_SOURCE)?;
        if let (Some(path), Some(text)) = (&gate.prompt, system_prompt) {
            std::fs::write(path, text)?;
        }
        Ok(gate)
    }

    /// The extension file, for `-e`.
    #[must_use]
    pub fn extension(&self) -> &Path {
        &self.extension
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
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The call a gate dialog is about: the id of the tool call Pi gives the extension, and the tool.
pub(crate) fn call_of(title: &str) -> Option<(Option<String>, String)> {
    let call: serde_json::Value = serde_json::from_str(title.strip_prefix(PREFIX)?).ok()?;
    let id = call
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(String::from);
    let tool = call.get("tool")?.as_str()?.to_string();
    Some((id, tool))
}
