//! What each mode lets Pi do without asking.
//!
//! Pi has no permission system of its own, so the modes are this crate's: the permission gate puts every tool call, with
//! the input it will run on, to the translator, which allows it, refuses it as a plan or as a path it keeps from the model,
//! or asks the person. Pi has no automatic reviewer, so [`Mode::Auto`] is never offered.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::{Component, Path, PathBuf};

use coqui_acp_harness::acp::within_roots;

use crate::paths;
use coqui_acp_harness::mode;
pub use coqui_acp_harness::mode::Mode;
use serde_json::Value;

/// The tools that only read.
pub const READ_ONLY: [&str; 4] = ["read", "grep", "find", "ls"];
/// The tools that change files.
pub const EDITS: [&str; 2] = ["edit", "write"];
/// The tools that run a command.
pub const COMMANDS: [&str; 2] = ["bash", "powershell"];

/// What is done with one tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// It runs.
    Allow,
    /// The person is asked.
    Ask,
    /// It is refused: the session is only to plan.
    Plan,
    /// It is refused: the path holds credentials, which the model does not get.
    Protected,
}

/// Where the session works and what Pi keeps its credentials and code in.
#[derive(Clone, Debug)]
pub struct Scope {
    cwd: PathBuf,
    roots: Vec<PathBuf>,
    agent_dirs: Vec<PathBuf>,
    private: Vec<PathBuf>,
    home: Option<PathBuf>,
    windows: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Self::new(Path::new("."), Vec::new())
    }
}

/// `path` with the links of its longest existing part followed and the rest kept as written, so that a file not made yet
/// is placed where it would land.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut existing = path.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            return rest.iter().rev().fold(real, |whole, part| whole.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn same(a: &std::ffi::OsStr, b: &str) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether `path` is `base` or lies inside it; on Windows names are compared whatever their case.
fn under(path: &Path, base: &Path, windows: bool) -> bool {
    if !windows {
        return path.starts_with(base);
    }
    let fold = |p: &Path| -> Vec<String> {
        p.components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (path, base) = (fold(path), fold(base));
    path.len() >= base.len() && path[..base.len()] == base[..]
}

fn has_component(path: &Path, names: &[&str]) -> bool {
    path.components().any(|part| match part {
        Component::Normal(name) => names.iter().any(|wanted| same(name, wanted)),
        _ => false,
    })
}

/// Where in a person's home credentials are kept: directories and single files.
const SECRET_HOME: [&str; 21] = [
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".azure",
    ".netrc",
    ".npmrc",
    ".git-credentials",
    ".config/gcloud",
    ".config/gh",
    ".docker/config.json",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".pypirc",
    ".codex/auth.json",
    ".claude/.credentials.json",
    ".config/git/credentials",
    ".config/gh/hosts.yml",
    ".kube/config",
    "AppData/Roaming/gcloud",
    "AppData/Roaming/GitHub CLI",
];

impl Scope {
    /// A scope in `cwd`, whose files a call may use freely when they lie in `roots` (`cwd` alone when empty). The home
    /// directory is the process's (`USERPROFILE` on Windows, `HOME` elsewhere): [`Scope::with_home`] names another.
    #[must_use]
    pub fn new(cwd: &Path, roots: Vec<PathBuf>) -> Self {
        let roots = if roots.is_empty() {
            vec![cwd.to_path_buf()]
        } else {
            roots
        };
        let home =
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        Self {
            cwd: cwd.to_path_buf(),
            roots,
            agent_dirs: Vec::new(),
            private: Vec::new(),
            home,
            windows: cfg!(windows),
        }
    }

    /// Pi's agent directories: where it keeps its credentials, settings and the code it loads.
    #[must_use]
    pub fn with_agent_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.agent_dirs = dirs;
        self
    }

    /// The translator's own directories (the one holding the gate): code Pi loads, which the model never changes.
    #[must_use]
    pub fn with_private(mut self, dirs: Vec<PathBuf>) -> Self {
        self.private = dirs;
        self
    }

    /// Whether the path lies in one of the translator's own directories.
    #[must_use]
    pub fn private(&self, text: &str) -> bool {
        let found = self.pi(text);
        if found.network || found.unknown {
            return false;
        }
        let path = resolved(&found.path);
        self.private
            .iter()
            .any(|dir| under(&path, &resolved(dir), self.windows))
    }

    /// The home directory Pi expands `~` to: what `os.homedir()` says for the environment Pi is started with.
    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// The directory relative paths are against.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn pi(&self, text: &str) -> paths::Resolved {
        paths::resolve(text, &self.cwd, self.home.as_deref(), self.windows)
    }

    /// Whether the path is written, or hidden by Pi's own rewriting, as a network or device path (UNC, `//host/share`,
    /// `file://host/share`, any of these after `@`), which nothing here resolves: resolving it would open a connection to its
    /// host.
    #[must_use]
    pub fn network(&self, text: &str) -> bool {
        self.pi(text).network
    }

    /// Whether the text looks as if Pi rewrites it (`~`, `@`, a `file:` URL, `C:foo`) or could not be worked out: asked about
    /// always, never allowed unasked or remembered.
    #[must_use]
    pub fn indirect(&self, text: &str) -> bool {
        paths::indirect(text, self.windows) || self.pi(text).unknown
    }

    /// Whether the path, as Pi will resolve it, lies in the session's directories.
    #[must_use]
    pub fn inside(&self, text: &str) -> bool {
        let found = self.pi(text);
        !found.network && !found.unknown && within_roots(&found.path, &self.roots)
    }

    /// Whether the path holds credentials: Pi's `auth.json`, `models.json` (which can name a command that yields a key) and
    /// `mcp-auth.json` in an agent directory, and what a person keeps secrets in under their home (`.ssh`, `.aws`,
    /// `.gnupg`, `.kube`, `.azure`, `gcloud`, `gh`, `.netrc`, `.npmrc`, `.git-credentials`, Cargo's and Docker's).
    #[must_use]
    pub fn secret(&self, text: &str) -> bool {
        let found = self.pi(text);
        if found.network || found.unknown {
            return false;
        }
        let path = resolved(&found.path);
        let credentials = ["auth.json", "models.json", "mcp-auth.json"];
        if self.agent_dirs.iter().any(|dir| {
            path.starts_with(resolved(dir))
                && path
                    .file_name()
                    .is_some_and(|name| credentials.iter().any(|wanted| same(name, wanted)))
        }) {
            return true;
        }
        let Some(home) = &self.home else {
            return false;
        };
        let home = resolved(home);
        SECRET_HOME.iter().any(|name| {
            path.starts_with(home.join(name)) || resolved(&found.path).starts_with(home.join(name))
        })
    }

    /// Whether a name in the path holds a `:` on Windows, which the file system reads as a stream of the file before it
    /// (`.git::$INDEX_ALLOCATION` is the directory `.git`): the path is not where its text says, and is refused.
    #[must_use]
    pub fn stream(&self, text: &str) -> bool {
        self.pi(text).stream
    }

    /// Where credentials are kept: the files Pi keeps its own in, and what a person keeps secrets in under their home.
    fn secret_places(&self) -> Vec<PathBuf> {
        let mut places = Vec::new();
        for dir in &self.agent_dirs {
            for name in ["auth.json", "models.json", "mcp-auth.json"] {
                places.push(resolved(&dir.join(name)));
            }
        }
        if let Some(home) = &self.home {
            let home = resolved(home);
            places.extend(SECRET_HOME.iter().map(|name| home.join(name)));
        }
        places
    }

    /// Whether a search that starts at `root` (the working directory when absent) would reach a credential: the root holds
    /// one of the places credentials are kept, a directory above it. `grep` passes `--hidden`, so a search from the
    /// person's home would read `.ssh` unasked although no path it names is a credential.
    #[must_use]
    pub fn reaches_secret(&self, root: Option<&str>) -> bool {
        let start = match root {
            None => resolved(&self.cwd),
            Some(text) => {
                let found = self.pi(text);
                if found.network || found.unknown {
                    return false;
                }
                resolved(&found.path)
            }
        };
        self.secret_places()
            .iter()
            .any(|place| under(place, &start, self.windows))
    }

    /// Whether Pi loads code or configuration from the path, so that a file written there runs later with no question: its
    /// agent directories and `~/.pi`, any `.pi` or `.git` directory (hooks), and what holds credentials.
    #[must_use]
    pub fn loaded(&self, text: &str) -> bool {
        let found = self.pi(text);
        if found.network || found.unknown {
            return false;
        }
        let path = resolved(&found.path);
        self.secret(text)
            || has_component(&path, &[".pi", ".git"])
            || has_component(&found.path, &[".pi", ".git"])
            || self
                .agent_dirs
                .iter()
                .any(|dir| path.starts_with(resolved(dir)))
            || self
                .home
                .as_ref()
                .is_some_and(|home| path.starts_with(resolved(&home.join(".pi"))))
    }
}

/// The text of a tool input's `field`, when it is whole text.
fn text<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(Value::as_str)
}

/// Whether the input says its `field` was cut by the gate for its size.
fn cut(input: &Value, field: &str) -> bool {
    input
        .get("_cut")
        .and_then(Value::as_array)
        .is_some_and(|fields| fields.iter().any(|name| name == field))
}

/// The program a plain command starts, when remembering "this program" says what it will do: a command with no
/// separator, substitution, redirection, variable, line break or assignment before its first word.
fn program_of(command: &str) -> Option<&str> {
    let command = command.trim();
    if command.is_empty()
        || command.chars().any(|c| {
            matches!(
                c,
                ';' | '&' | '|' | '`' | '$' | '(' | ')' | '<' | '>' | '\n' | '\r' | '%'
            )
        })
    {
        return None;
    }
    let first = command.split_whitespace().next()?;
    (!first.contains('=')).then_some(first)
}

/// What the person can be asked to allow for the rest of the session in place of this one call, and how it is said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remember {
    /// The key it is kept under.
    pub key: String,
    /// The option's label.
    pub label: String,
}

/// The mode in use and what the person allowed for the rest of the session.
#[derive(Clone, Debug)]
pub struct Policy {
    mode: Mode,
    always: BTreeSet<String>,
    ask_for_reads: bool,
}

impl Policy {
    /// A policy in `mode`; with `ask_for_reads` even a tool that only reads is asked about in Manual and Plan.
    #[must_use]
    pub const fn new(mode: Mode, ask_for_reads: bool) -> Self {
        Self {
            mode,
            always: BTreeSet::new(),
            ask_for_reads,
        }
    }

    /// The mode in use.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Puts the calls that follow in `mode`.
    pub const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Allows what `key` (from [`Policy::remember`]) names for the rest of the session, in every mode but Plan.
    pub fn allow_key(&mut self, key: &str) {
        self.always.insert(key.to_string());
    }

    /// What the person may allow in place of this call, when it is something that can be said exactly: a plain command by
    /// its program, an edit by its tool (inside the session's directories), any other tool by its name. Not a tool that
    /// only reads, and not a compound command.
    #[must_use]
    pub fn remember(tool: &str, input: &Value) -> Option<Remember> {
        if READ_ONLY.contains(&tool) {
            return None;
        }
        if COMMANDS.contains(&tool) {
            // A command cut for its size cannot be said exactly: what follows the cut could be anything.
            if cut(input, "command") {
                return None;
            }
            let program = program_of(text(input, "command")?)?;
            return Some(Remember {
                key: format!("cmd:{program}"),
                label: format!(
                    "Always allow `{program} …` in this session: every `{program}` command, including ones that run other programs"
                ),
            });
        }
        if EDITS.contains(&tool) {
            return Some(Remember {
                key: format!("edit:{tool}"),
                label: format!(
                    "Always allow `{tool}` inside the working directories in this session"
                ),
            });
        }
        Some(Remember {
            key: format!("tool:{tool}"),
            label: format!("Always allow the tool `{tool}` in this session"),
        })
    }

    fn remembered(&self, tool: &str, input: &Value) -> bool {
        Self::remember(tool, input).is_some_and(|r| self.always.contains(&r.key))
    }

    /// What to do with a call of `tool` on `input`.
    ///
    /// | Mode | read-only tools | `edit`, `write` | commands and any other tool |
    /// | --- | --- | --- | --- |
    /// | Manual | run inside the directories (asked with `ask_for_reads`) | asked | asked |
    /// | Accept edits | run inside the directories | run inside the directories, away from what Pi loads | asked |
    /// | Plan | as Manual | refused as a plan | refused as a plan |
    /// | Bypass | run | run | run |
    ///
    /// Outside the directories a read is asked; a credential file is refused, read or changed, in every mode but Bypass. A tool the person
    /// allowed for the session is allowed by the rule it was allowed under, except in Plan; an edit stays inside the
    /// directories and away from what Pi loads.
    #[must_use]
    pub fn decide(&self, tool: &str, input: &Value, scope: &Scope) -> Decision {
        if self.mode == Mode::Bypass {
            return Decision::Allow;
        }
        // A path that hides a stream of a file is where it says it is not, for every tool that takes a path.
        if (READ_ONLY.contains(&tool) || EDITS.contains(&tool))
            && text(input, "path").is_some_and(|path| !cut(input, "path") && scope.stream(path))
        {
            return Decision::Protected;
        }
        if READ_ONLY.contains(&tool) {
            let path = text(input, "path");
            // A search from a directory above a credential finds it whatever its own path says; `read` names one file.
            if tool != "read"
                && !path.is_some_and(|path| {
                    cut(input, "path") || scope.network(path) || scope.secret(path)
                })
                && scope.reaches_secret(path)
            {
                return if self.mode == Mode::Plan {
                    Decision::Protected
                } else {
                    Decision::Ask
                };
            }
            if let Some(path) = path {
                if cut(input, "path") {
                    return Decision::Ask;
                }
                if scope.network(path) {
                    return Decision::Ask;
                }
                if scope.secret(path) {
                    return Decision::Protected;
                }
                if scope.indirect(path) || !scope.inside(path) {
                    return Decision::Ask;
                }
            } else if input.get("path").is_some_and(|value| !value.is_null()) {
                return Decision::Ask;
            }
            return match self.mode {
                Mode::Manual | Mode::Plan if self.ask_for_reads => Decision::Ask,
                _ => Decision::Allow,
            };
        }
        // A credential file is not changed by the model in any mode but Bypass, as it is not read; nor is the directory that
        // holds the gate, which Pi loads.
        if EDITS.contains(&tool)
            && text(input, "path").is_some_and(|path| {
                !cut(input, "path")
                    && !scope.network(path)
                    && (scope.secret(path) || scope.private(path))
            })
        {
            return Decision::Protected;
        }
        if self.mode == Mode::Plan {
            return Decision::Plan;
        }
        if EDITS.contains(&tool) {
            let Some(path) = text(input, "path").filter(|_| !cut(input, "path")) else {
                return Decision::Ask;
            };
            if scope.network(path) {
                return Decision::Ask;
            }
            let free = scope.inside(path) && !scope.indirect(path) && !scope.loaded(path);
            return if free && (self.mode == Mode::AcceptEdits || self.remembered(tool, input)) {
                Decision::Allow
            } else {
                Decision::Ask
            };
        }
        if self.remembered(tool, input) {
            Decision::Allow
        } else {
            Decision::Ask
        }
    }
}

/// The modes a client may choose: the host's own mode and every stricter one, and those the host named besides. Bypass
/// only when allowed, and never Auto, which Pi has nothing to back.
#[must_use]
pub fn offered(loosest: Option<Mode>, named: &[Mode], allow_bypass: bool) -> Vec<Mode> {
    let mut modes = mode::widened(&mode::up_to(loosest, allow_bypass), named, allow_bypass);
    modes.retain(|mode| *mode != Mode::Auto);
    modes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope::new(
            &std::env::temp_dir().join("coqui-acp-pi-mode-test"),
            Vec::new(),
        )
    }

    #[test]
    fn each_mode_decides_as_the_table_says() {
        let s = scope();
        let in_mode = |mode| Policy::new(mode, false);
        let read = json!({"path": "a.txt"});
        let write = json!({"path": "a.txt", "content": "x"});
        let bash = json!({"command": "ls"});
        assert_eq!(
            in_mode(Mode::Manual).decide("read", &read, &s),
            Decision::Allow
        );
        assert_eq!(
            in_mode(Mode::Manual).decide("write", &write, &s),
            Decision::Ask
        );
        assert_eq!(
            in_mode(Mode::Manual).decide("bash", &bash, &s),
            Decision::Ask
        );
        assert_eq!(
            in_mode(Mode::AcceptEdits).decide("edit", &write, &s),
            Decision::Allow
        );
        assert_eq!(
            in_mode(Mode::AcceptEdits).decide("bash", &bash, &s),
            Decision::Ask
        );
        assert_eq!(
            in_mode(Mode::AcceptEdits).decide("mcp__x__y", &bash, &s),
            Decision::Ask
        );
        assert_eq!(
            in_mode(Mode::Plan).decide("grep", &json!({"pattern": "x"}), &s),
            Decision::Allow
        );
        assert_eq!(
            in_mode(Mode::Plan).decide("write", &write, &s),
            Decision::Plan
        );
        assert_eq!(
            in_mode(Mode::Plan).decide("bash", &bash, &s),
            Decision::Plan
        );
        assert_eq!(
            in_mode(Mode::Bypass).decide("bash", &bash, &s),
            Decision::Allow
        );
        assert_eq!(
            Policy::new(Mode::Manual, true).decide("read", &read, &s),
            Decision::Ask
        );
    }

    #[test]
    fn what_the_person_allowed_is_allowed_by_its_own_rule_and_not_in_plan() {
        let s = scope();
        let mut policy = Policy::new(Mode::Manual, false);
        let git = json!({"command": "git status"});
        policy.allow_key(&Policy::remember("bash", &git).unwrap().key);
        assert_eq!(
            policy.decide("bash", &json!({"command": "git log"}), &s),
            Decision::Allow
        );
        assert_eq!(
            policy.decide("bash", &json!({"command": "rm -rf x"}), &s),
            Decision::Ask
        );
        assert_eq!(
            policy.decide("bash", &json!({"command": "git log; rm x"}), &s),
            Decision::Ask
        );
        policy.set_mode(Mode::Plan);
        assert_eq!(policy.decide("bash", &git, &s), Decision::Plan);
    }

    #[test]
    fn auto_is_never_offered() {
        let modes = offered(Some(Mode::Auto), &[Mode::Auto], true);
        assert!(!modes.contains(&Mode::Auto));
        assert!(modes.contains(&Mode::Bypass));
        assert_eq!(offered(None, &[], false), vec![Mode::Plan]);
    }

    #[test]
    fn a_command_is_remembered_by_its_program_only_when_it_is_one() {
        fn program(command: &str) -> Option<&str> {
            program_of(command)
        }
        assert_eq!(program("git status"), Some("git"));
        assert_eq!(program("  cargo test -p x=y"), Some("cargo"));
        for compound in [
            "a; b", "a && b", "a | b", "a $(b)", "a `b`", "a > f", "A=1 a", "", "a\nb",
        ] {
            assert_eq!(program(compound), None, "{compound:?}");
        }
    }
}
