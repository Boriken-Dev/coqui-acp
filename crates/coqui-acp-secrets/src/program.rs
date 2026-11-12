//! Which program a command starts: found on a search path that cannot lead into the session's roots, so what the person approves
//! is the program that runs, and a file the agent planted in its working directory does not take the place of a system tool.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::files::{Places, plain_path};
use crate::run::{CommandLine, shell_program};

/// What is started for a command.
pub(crate) struct Launch {
    /// The program, by full path, as it was found: a link is started under its own name.
    pub(crate) program: PathBuf,
    /// What `program` is a link to, when that is another file: shown, never started.
    pub(crate) target: Option<PathBuf>,
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

/// The files a program name can stand for in one directory, in the order Windows would take them: a name with no extension of the
/// executable kind is tried with each `PATHEXT` extension before it is tried bare (`dart` is `dart.bat`, not the script beside it).
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let base = dir.join(name);
    #[cfg(windows)]
    {
        let extensions = extensions();
        let ends_so = |ext: &String| {
            name.to_ascii_lowercase()
                .ends_with(&ext.to_ascii_lowercase())
        };
        if extensions.iter().any(ends_so) {
            return vec![base];
        }
        let mut all: Vec<PathBuf> = extensions
            .iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .collect();
        all.push(base);
        all
    }
    #[cfg(not(windows))]
    vec![base]
}

/// Whether `file` is something to start: a file, or on Windows an app execution alias (a reparse point `metadata` cannot follow).
fn startable(file: &Path) -> bool {
    if std::fs::metadata(file).is_ok_and(|meta| meta.is_file()) {
        return true;
    }
    cfg!(windows)
        && std::fs::metadata(file).is_err()
        && std::fs::symlink_metadata(file).is_ok_and(|meta| !meta.is_dir())
}

fn is_path(name: &str) -> bool {
    name.contains('/') || (cfg!(windows) && (name.contains('\\') || name.contains(':')))
}

/// The file `name` stands for in `dirs`, in order, as it was found, and the file it is a link to when that is another file.
fn resolve_in(dirs: &[PathBuf], name: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    let found = dirs
        .iter()
        .flat_map(|dir| candidates(dir, name))
        .find(|c| startable(c))?;
    Some(with_target(found))
}

/// `found` and, when it is a link to another file, that file: both without Windows' verbatim prefix.
fn with_target(found: PathBuf) -> (PathBuf, Option<PathBuf>) {
    let target = std::fs::canonicalize(&found)
        .ok()
        .map(|real| plain_path(&real));
    let same = |a: &Path, b: &Path| {
        if cfg!(windows) {
            a.to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy())
        } else {
            a == b
        }
    };
    let target = target.filter(|target| !same(target, &found));
    (found, target)
}

/// Resolves `line` for a command that starts in `cwd`.
///
/// # Errors
/// [`Error::Command`] when the program is not found on the safe path, or is named by a path to something that is not a file.
pub(crate) fn plan(line: &CommandLine, cwd: &Path, places: &Places) -> Result<Launch, Error> {
    let dirs = safe_dirs(places);
    let path = std::env::join_paths(&dirs).unwrap_or_default();
    let (program, target) = match line {
        CommandLine::Shell(_) => (shell_program()?, None),
        CommandLine::Argv(args) => {
            let name = args[0].as_str();
            if crate::text::has_hidden(name) {
                return Err(Error::Command("the program name holds a hidden character"));
            }
            let resolved = if is_path(name) {
                // A path is the agent's choice, and is shown to the person as the file it is.
                let named = Path::new(name);
                let base = if named.is_absolute() {
                    PathBuf::new()
                } else {
                    cwd.to_path_buf()
                };
                let dir = std::fs::canonicalize(base.join(named.parent().unwrap_or(Path::new(""))))
                    .map(|real| plain_path(&real))
                    .map_err(|_| Error::Command("the program's directory cannot be resolved"))?;
                let file = named.file_name().and_then(|n| n.to_str()).unwrap_or("");
                candidates(&dir, file)
                    .into_iter()
                    .find(|c| startable(c))
                    .map(with_target)
            } else {
                resolve_in(&dirs, name)
            };
            resolved.ok_or(Error::Command(
                "the program was not found on a search path outside the session's roots",
            ))?
        }
    };
    // What the person is shown has nothing hidden in it.
    for shown in std::iter::once(&program).chain(target.iter()) {
        if crate::text::has_hidden(&shown.to_string_lossy()) {
            return Err(Error::Command(
                "the program's path holds a hidden character",
            ));
        }
    }
    Ok(Launch {
        program,
        target,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "coqui-acp-secrets-prog-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(std::fs::canonicalize(dir).unwrap())
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_name_takes_its_pathext_file_before_the_extensionless_one() {
        // `dart`, `flutter` and `code` are `.bat` or `.cmd` files beside an extensionless script that cannot be started.
        let dir = Tmp::new("pathext");
        std::fs::write(dir.0.join("dart"), "#!/bin/sh").unwrap();
        std::fs::write(dir.0.join("dart.bat"), "@echo off").unwrap();
        let (program, _) = resolve_in(&[dir.0.clone()], "dart").unwrap();
        assert_eq!(
            program
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_lowercase(),
            "dart.bat"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_started_under_its_own_name_and_its_target_is_shown() {
        // `xzcat` is `xz` run under another name, and `cargo` is `rustup`: the program must start as the name it was found by.
        let dir = Tmp::new("links");
        std::fs::write(dir.0.join("xz"), "x").unwrap();
        std::os::unix::fs::symlink(dir.0.join("xz"), dir.0.join("xzcat")).unwrap();
        let (program, target) = resolve_in(&[dir.0.clone()], "xzcat").unwrap();
        assert_eq!(program.file_name().unwrap(), "xzcat");
        assert_eq!(target.unwrap().file_name().unwrap(), "xz");
        let (program, target) = resolve_in(&[dir.0.clone()], "xz").unwrap();
        assert_eq!(program.file_name().unwrap(), "xz");
        assert!(target.is_none());
    }
}
