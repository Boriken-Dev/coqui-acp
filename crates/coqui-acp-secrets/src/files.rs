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
    /// Whether it is under the session's roots, which the agent's own tools are assumed to read.
    pub(crate) in_roots: bool,
}

/// A directory as it is checked: its real path, and the same without Windows' verbatim prefix, which is what a path typed by
/// someone is compared with before any file is touched.
#[derive(Clone, Debug)]
struct Place {
    real: PathBuf,
    plain: PathBuf,
}

/// The directories a session may put a secret in, and the one its commands start in.
#[derive(Clone, Debug)]
pub(crate) struct Places {
    roots: Vec<Place>,
    write_dirs: Vec<Place>,
}

fn outside() -> Error {
    Error::Path("outside the session's roots")
}

/// `path` without the `\\?\` prefix of a disk path; `None` for any other prefix a path can have on Windows (a network share, a device,
/// an unknown verbatim form). Elsewhere the path as it is.
fn plain(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let mut parts = path.components();
        let first = parts.next()?;
        let disk = match first {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
                _ => return None,
            },
            _ => return Some(path.to_path_buf()),
        };
        let mut out = PathBuf::from(format!("{}:", char::from(disk)));
        out.extend(parts);
        Some(out)
    }
    #[cfg(not(windows))]
    {
        Some(path.to_path_buf())
    }
}

/// A real path without the `\\?\` prefix Windows gives a resolved disk path, as a person writes it and as a program is started.
pub(crate) fn plain_path(path: &Path) -> PathBuf {
    plain(path).unwrap_or_else(|| path.to_path_buf())
}

/// Whether `path` is `base` or below it, compared as text on parts (not case-sensitively on Windows).
fn within(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        lower(path).starts_with(lower(base))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

impl Place {
    fn new(dir: &Path) -> Result<Self, Error> {
        let real = fs::canonicalize(dir).map_err(|_| Error::Path("a root is not a directory"))?;
        let plain = plain(&real).ok_or(Error::Path("a root is on a network or device path"))?;
        Ok(Self { real, plain })
    }
}

impl Places {
    /// Resolves every directory once, so a later check compares real paths.
    pub(crate) fn new(roots: &[PathBuf], write_dirs: &[PathBuf]) -> Result<Self, Error> {
        let roots = roots
            .iter()
            .map(|dir| Place::new(dir))
            .collect::<Result<Vec<_>, _>>()?;
        if roots.is_empty() {
            return Err(Error::Path("a session needs at least one root"));
        }
        Ok(Self {
            roots,
            write_dirs: write_dirs
                .iter()
                .filter_map(|dir| Place::new(dir).ok())
                .collect(),
        })
    }

    /// Whether a real (canonical) path is under a root.
    pub(crate) fn under_roots(&self, real: &Path) -> bool {
        self.roots.iter().any(|root| within(real, &root.real))
    }

    fn classify(&self, real: &Path) -> Option<bool> {
        if self.under_roots(real) {
            Some(true)
        } else if self.write_dirs.iter().any(|dir| within(real, &dir.real)) {
            Some(false)
        } else {
            None
        }
    }

    /// The path as given, made absolute against the first root, as text only: no file is touched. It is refused when it has a part
    /// that is not a plain name (`..`, a network or device prefix), a character that could change how it reads in a prompt, or is not
    /// under a root or a write directory by its text.
    fn lexical(&self, path: &str) -> Result<PathBuf, Error> {
        if path.is_empty() || path.len() > MAX_PATH_BYTES || crate::text::has_hidden(path) {
            return Err(Error::Path("empty, too long or holds a hidden character"));
        }
        // `\\?\C:\x` is `C:\x`; the other verbatim forms and every network or device path are refused below.
        #[cfg(windows)]
        let path = path
            .strip_prefix(r"\\?\")
            .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(path);
        let given = Path::new(path);
        let mut full = if given.is_absolute() {
            PathBuf::new()
        } else {
            self.roots[0].plain.clone()
        };
        for part in given.components() {
            match part {
                Component::Prefix(prefix) => {
                    // Only a drive: no network share, no device path, no other verbatim form.
                    #[cfg(windows)]
                    if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                        return Err(outside());
                    }
                    #[cfg(not(windows))]
                    let _ = prefix;
                    full.push(part.as_os_str());
                }
                Component::RootDir => full.push(part.as_os_str()),
                Component::Normal(name) => full.push(name),
                Component::CurDir => {}
                Component::ParentDir => return Err(outside()),
            }
        }
        let bases = self.roots.iter().chain(&self.write_dirs);
        if !bases.into_iter().any(|base| within(&full, &base.plain)) {
            return Err(outside());
        }
        Ok(full)
    }

    /// `path` resolved through the nearest part that exists: `..` is refused outright, and so is a final part that is a link; the
    /// result must be under a root or a write directory, by its text before any file is touched and by its real path after.
    /// `missing` says how many missing parts it may have (0: the file must exist, 1: only the file itself may be missing).
    pub(crate) fn resolve(&self, path: &str, missing: usize) -> Result<Resolved, Error> {
        let full = self.lexical(path)?;
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
        // The path is shown to the person as it is: a link that leads somewhere with a hidden character in its name is refused.
        if crate::text::has_hidden(&path.to_string_lossy()) {
            return Err(Error::Path("the resolved path holds a hidden character"));
        }
        match self.classify(&path) {
            Some(in_roots) => Ok(Resolved { path, in_roots }),
            None => Err(outside()),
        }
    }

    /// A directory a command may start in: it exists and is under a root.
    pub(crate) fn directory(&self, path: Option<&str>) -> Result<PathBuf, Error> {
        let Some(path) = path else {
            return Ok(self.roots[0].real.clone());
        };
        let resolved = self.resolve(path, 0)?;
        if !resolved.in_roots || !resolved.path.is_dir() {
            return Err(Error::Path("not a directory under the session's roots"));
        }
        Ok(resolved.path)
    }

    fn real_inside(&self, path: &Path) -> bool {
        fs::canonicalize(path).is_ok_and(|real| self.classify(&real).is_some())
    }

    /// Writes `value` to `resolved`. A new file is created exclusively; an existing one is replaced by renaming a new file over it,
    /// never written in place, so a file the target is a hard link of keeps its contents. The new file's real path is checked
    /// before the value is written to it. `mode` is applied where the platform has one.
    pub(crate) fn write(
        &self,
        resolved: &Resolved,
        value: &[u8],
        mode: u32,
        overwrite: bool,
    ) -> Result<(), Error> {
        let exists = fs::symlink_metadata(&resolved.path).is_ok();
        if exists && !overwrite {
            return Err(Error::Path("the file exists and overwrite is off"));
        }
        let target = if exists {
            let name = resolved
                .path
                .file_name()
                .ok_or(Error::Path("no file name"))?
                .to_string_lossy();
            resolved
                .path
                .with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()))
        } else {
            resolved.path.clone()
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options
            .open(&target)
            .map_err(|_| Error::Path("the file could not be created"))?;
        let fail = |target: &Path, error: Error| {
            let _ = fs::remove_file(target);
            error
        };
        // What was opened is where it should be, before the value goes in.
        if !self.real_inside(&target) {
            drop(file);
            return Err(fail(&target, outside()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(fs::Permissions::from_mode(mode));
        }
        if file.write_all(value).and_then(|()| file.flush()).is_err() {
            drop(file);
            return Err(fail(&target, Error::Path("the file could not be written")));
        }
        drop(file);
        if exists && fs::rename(&target, &resolved.path).is_err() {
            return Err(fail(&target, Error::Path("the file could not be replaced")));
        }
        if !self.real_inside(&resolved.path) {
            let _ = fs::remove_file(&resolved.path);
            return Err(outside());
        }
        Ok(())
    }

    /// The contents of a file under the roots, at most `max` bytes. The file is opened first and its place checked after, so a
    /// path swapped in between is caught.
    pub(crate) fn read(&self, path: &str, max: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
        let resolved = self.resolve(path, 0)?;
        if !resolved.in_roots {
            return Err(Error::Path("not a file under the session's roots"));
        }
        let file = fs::File::open(&resolved.path).map_err(|_| Error::Path("unreadable"))?;
        let again = self.resolve(path, 0)?;
        if again != resolved || !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(Error::Path("not a file under the session's roots"));
        }
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
    plain_path(path).display().to_string()
}
