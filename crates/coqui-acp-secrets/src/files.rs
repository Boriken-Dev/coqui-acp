//! Where a secret may be written to or read from: under the session's roots, or a directory the settings name, never anywhere else.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use zeroize::Zeroizing;

use crate::Error;

const MAX_PATH_BYTES: usize = 4096;

/// A path that passed the rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The path with every existing part resolved (links followed), so it is the file the write will make.
    pub(crate) path: PathBuf,
    /// Whether it is under the session's roots, which the agent's own tools can read.
    pub(crate) in_roots: bool,
}

/// The directories a session may put a secret in, and the one its commands start in.
#[derive(Clone, Debug)]
pub(crate) struct Places {
    roots: Vec<PathBuf>,
    write_dirs: Vec<PathBuf>,
}

fn outside() -> Error {
    Error::Path("outside the session's roots")
}

impl Places {
    /// Resolves every directory once, so a later check compares real paths.
    pub(crate) fn new(roots: &[PathBuf], write_dirs: &[PathBuf]) -> Result<Self, Error> {
        let canon = |dirs: &[PathBuf]| -> Result<Vec<PathBuf>, Error> {
            dirs.iter()
                .map(|dir| {
                    fs::canonicalize(dir).map_err(|_| Error::Path("a root is not a directory"))
                })
                .collect()
        };
        let roots = canon(roots)?;
        if roots.is_empty() {
            return Err(Error::Path("a session needs at least one root"));
        }
        Ok(Self {
            roots,
            write_dirs: write_dirs
                .iter()
                .filter_map(|dir| fs::canonicalize(dir).ok())
                .collect(),
        })
    }

    fn classify(&self, canonical: &Path) -> Option<bool> {
        if self.roots.iter().any(|root| canonical.starts_with(root)) {
            Some(true)
        } else if self.write_dirs.iter().any(|dir| canonical.starts_with(dir)) {
            Some(false)
        } else {
            None
        }
    }

    /// `path` made absolute against the first root and resolved through the nearest part that exists. `..` is refused outright, and
    /// so is a final part that is a link; the result must be under a root or a write directory. `parents` says how many missing
    /// parts it may have (0: the file must exist, 1: only the file itself may be missing).
    pub(crate) fn resolve(&self, path: &str, missing: usize) -> Result<Resolved, Error> {
        // The person reads this path in a prompt: nothing that could rewrite how it looks.
        if path.is_empty()
            || path.len() > MAX_PATH_BYTES
            || path.chars().any(|c| {
                c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
        {
            return Err(Error::Path("empty, too long or holds a control character"));
        }
        let given = Path::new(path);
        // Read on the path as given: after a join onto a verbatim root (Windows) `/` and `..` would no longer be separators.
        let mut full = if given.is_absolute() {
            PathBuf::new()
        } else {
            self.roots[0].clone()
        };
        for part in given.components() {
            match part {
                Component::Prefix(prefix) if !prefix.kind().is_verbatim() => {
                    full.push(part.as_os_str());
                }
                Component::RootDir => full.push(part.as_os_str()),
                Component::Normal(name) => full.push(name),
                Component::CurDir => {}
                _ => return Err(outside()),
            }
        }
        if fs::symlink_metadata(&full).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::Path("the file is a link"));
        }
        let mut near = full.as_path();
        let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
        while fs::symlink_metadata(near).is_err() {
            let (Some(parent), Some(name)) = (near.parent(), near.file_name()) else {
                return Err(outside());
            };
            tail.push(name);
            near = parent;
        }
        if tail.len() > missing {
            return Err(Error::Path("the file's directory does not exist"));
        }
        let mut path = fs::canonicalize(near).map_err(|_| outside())?;
        path.extend(tail.iter().rev());
        match self.classify(&path) {
            Some(in_roots) => Ok(Resolved { path, in_roots }),
            None => Err(outside()),
        }
    }

    /// A directory a command may start in: it exists and is under a root.
    pub(crate) fn directory(&self, path: Option<&str>) -> Result<PathBuf, Error> {
        let Some(path) = path else {
            return Ok(self.roots[0].clone());
        };
        let resolved = self.resolve(path, 0)?;
        if !resolved.in_roots || !resolved.path.is_dir() {
            return Err(Error::Path("not a directory under the session's roots"));
        }
        Ok(resolved.path)
    }

    fn still_inside(&self, path: &Path) -> bool {
        fs::canonicalize(path).is_ok_and(|real| self.classify(&real).is_some())
    }

    /// Writes `value` to `resolved`: made new unless `overwrite`, never through a link, with `mode` where the platform has one.
    /// The place is checked again once the file exists, and a file that turns out to be elsewhere is removed.
    pub(crate) fn write(
        &self,
        resolved: &Resolved,
        value: &[u8],
        mode: u32,
        overwrite: bool,
    ) -> Result<(), Error> {
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&resolved.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Path("the file exists and overwrite is off")
            } else {
                Error::Path("the file could not be created")
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(fs::Permissions::from_mode(mode));
        }
        let written = file.write_all(value).and_then(|()| file.flush());
        drop(file);
        if written.is_err() || !self.still_inside(&resolved.path) {
            let _ = fs::remove_file(&resolved.path);
            return Err(if written.is_err() {
                Error::Path("the file could not be written")
            } else {
                outside()
            });
        }
        Ok(())
    }

    /// The contents of a file under the roots, at most `max` bytes.
    pub(crate) fn read(&self, path: &str, max: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
        let resolved = self.resolve(path, 0)?;
        if !resolved.in_roots || !resolved.path.is_file() {
            return Err(Error::Path("not a file under the session's roots"));
        }
        let file = fs::File::open(&resolved.path).map_err(|_| Error::Path("unreadable"))?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Path("unreadable"))?;
        if bytes.len() > max {
            return Err(Error::Limit(
                "the file is longer than a secret value may be",
            ));
        }
        Ok(bytes)
    }
}

/// A path as the person reads it: no verbatim prefix Windows adds to a resolved one.
pub(crate) fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\")
        .filter(|rest| !rest.starts_with("UNC"))
        .map_or(text.clone(), str::to_owned)
}
