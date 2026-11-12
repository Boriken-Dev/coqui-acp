//! Which program a command starts: found on a search path that cannot lead into the session's roots, so what the person approves
//! is the program that runs, and a file the agent planted in its working directory does not take the place of a system tool.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::files::{Places, plain_path};
use crate::run::{CommandLine, shell_program};

/// What is started for a command.
pub(crate) struct Launch {
    /// The program, by full path.
    pub(crate) program: PathBuf,
    /// The `PATH` the command gets.
    pub(crate) path: OsString,
}

/// The host's `PATH` without a relative entry and without one under the session's roots.
fn safe_dirs(places: &Places) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .filter(|real| !places.under_roots(real))
        .map(|real| plain_path(&real))
        .collect()
}

#[cfg(windows)]
fn extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The files a program name can stand for in one directory.
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let base = dir.join(name);
    #[cfg(windows)]
    {
        let mut all = vec![base.clone()];
        if base.extension().is_none() {
            all.extend(
                extensions()
                    .iter()
                    .map(|ext| dir.join(format!("{name}{ext}"))),
            );
        }
        all
    }
    #[cfg(not(windows))]
    vec![base]
}

fn is_path(name: &str) -> bool {
    name.contains('/') || (cfg!(windows) && (name.contains('\\') || name.contains(':')))
}

/// Resolves `line` for a command that starts in `cwd`.
///
/// # Errors
/// [`Error::Command`] when the program is not found on the safe path, or is named by a path to something that is not a file.
pub(crate) fn plan(line: &CommandLine, cwd: &Path, places: &Places) -> Result<Launch, Error> {
    let dirs = safe_dirs(places);
    let path = std::env::join_paths(&dirs).unwrap_or_default();
    let program = match line {
        CommandLine::Shell(_) => shell_program()?,
        CommandLine::Argv(args) => {
            let name = args[0].as_str();
            if crate::text::has_hidden(name) {
                return Err(Error::Command("the program name holds a hidden character"));
            }
            let found = if is_path(name) {
                // A path is the agent's choice, and is shown to the person as the file it is.
                let named = Path::new(name);
                let base = if named.is_absolute() {
                    PathBuf::new()
                } else {
                    cwd.to_path_buf()
                };
                let dir = base.join(named.parent().unwrap_or(Path::new("")));
                let file = named.file_name().and_then(|n| n.to_str()).unwrap_or("");
                candidates(&dir, file).into_iter().find(|c| c.is_file())
            } else {
                dirs.iter()
                    .flat_map(|dir| candidates(dir, name))
                    .find(|c| c.is_file())
            };
            let found = found.ok_or(Error::Command(
                "the program was not found on a search path outside the session's roots",
            ))?;
            plain_path(
                &std::fs::canonicalize(found)
                    .map_err(|_| Error::Command("the program cannot be resolved"))?,
            )
        }
    };
    Ok(Launch { program, path })
}
