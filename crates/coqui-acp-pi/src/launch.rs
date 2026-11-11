//! What a Pi session is started with.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use coqui_acp_harness::acp::within_roots;
use coqui_acp_harness::{Launch, Mode};

use crate::{Error, paths};

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
    /// The program: `pi`, or the path of one. On Windows a bare `pi` is looked for in `PATH` (see [`PiLaunch::command`]).
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
    /// More directories the session works in. Pi has no such setting: calls may use files there freely, as in `cwd`.
    pub additional_directories: Vec<PathBuf>,
    /// The directories calls may use freely; empty takes `cwd` and the more directories.
    pub working_roots: Vec<PathBuf>,
    /// The mode a session starts in, and the loosest a client may choose; Manual when absent.
    pub mode: Option<Mode>,
    /// Whether the Bypass mode is offered. A launch whose `mode` is Bypass needs it.
    pub allow_bypass: bool,
    /// The modes a client may choose, besides the mode and every stricter one. Bypass needs `allow_bypass`; Auto is never
    /// offered.
    pub modes: Option<Vec<Mode>>,
    /// Whether Manual and Plan ask even before a tool that only reads.
    pub ask_for_reads: bool,
    /// Whether a working directory may hold the places credentials are kept (the person's home, say, with its `.ssh`), which a
    /// session is otherwise refused. A search from there (`grep` passes `--hidden`) reads them.
    pub allow_credential_roots: bool,
    /// Whether Pi loads only the gate: no extension of the person's and no built-in one, MCP included, which another
    /// extension could use to change a call after the gate approved it.
    pub isolated: bool,
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

/// Whether a `PATH` entry is one a program may be taken from: a directory written in full, not on the network.
fn plain_entry(entry: &Path) -> bool {
    let text = entry.to_string_lossy();
    entry.is_absolute() && !text.starts_with("\\\\") && !text.starts_with("//")
}

/// Where a program called `name` is, looking in the directories of `path` for each of `extensions` in turn. A relative
/// entry, a network one, and one in `keep_out` (the session's own directories) are not looked in. `None` when `name` has a
/// directory or an extension already, or nothing is found.
fn find_in(
    name: &OsStr,
    path: &OsStr,
    extensions: &[&str],
    keep_out: &[PathBuf],
) -> Option<PathBuf> {
    let text = name.to_str()?;
    if text.contains(['/', '\\']) || Path::new(text).extension().is_some() {
        return None;
    }
    std::env::split_paths(path)
        .filter(|dir| plain_entry(dir))
        // `..` is resolved by how it is written before the entry is judged: `C:\tools\..\proj` is the project.
        .map(|dir| paths::lexical(&dir))
        .filter(|dir| !within_roots(dir, keep_out))
        .find_map(|dir| {
            extensions
                .iter()
                .map(|extension| dir.join(format!("{text}.{extension}")))
                .find(|candidate| candidate.is_file())
        })
}

/// The script an npm shim (`pi.cmd`) runs, from its text: the `.js` path written under the shim's own directory
/// (`%dp0%`).
fn shim_script(shim: &Path, text: &str) -> Option<PathBuf> {
    let dir = shim.parent()?;
    text.split('"')
        .filter_map(|quoted| quoted.strip_prefix("%dp0%\\"))
        .find(|relative| relative.to_ascii_lowercase().ends_with(".js") && !relative.contains('%'))
        .map(|relative| dir.join(relative.replace('\\', "/")))
        .filter(|script| script.is_file())
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
            allow_credential_roots: false,
            isolated: false,
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

    /// The directories calls may use freely.
    #[must_use]
    pub fn roots(&self) -> Vec<PathBuf> {
        if self.working_roots.is_empty() {
            let mut roots = vec![self.cwd.clone()];
            roots.extend(self.additional_directories.iter().cloned());
            roots
        } else {
            self.working_roots.clone()
        }
    }

    fn named(&self, name: &str) -> Option<PathBuf> {
        self.env
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
            .map(|(_, value)| PathBuf::from(value))
            .or_else(|| std::env::var_os(name).map(PathBuf::from))
    }

    /// The home directory Pi expands `~` to: `USERPROFILE` (Windows) or `HOME` of the launch's environment, else the
    /// process's.
    #[must_use]
    pub fn home(&self) -> Option<PathBuf> {
        self.named(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
    }

    /// Pi's agent directories: the launch's `PI_CODING_AGENT_DIR`, else the process's, else `~/.pi/agent`. Where Pi keeps
    /// its credentials and the code it loads.
    #[must_use]
    pub fn agent_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(dir) = self
            .named("PI_CODING_AGENT_DIR")
            .filter(|dir| !dir.as_os_str().is_empty())
        {
            // Pi expands `~` in it, and reads a relative one against the directory it works in.
            let home = self.home();
            let found = paths::setting(
                &dir.to_string_lossy(),
                &self.cwd,
                home.as_deref(),
                cfg!(windows),
            );
            dirs.push(found.path);
        } else if let Some(home) = self.home() {
            dirs.push(home.join(".pi").join("agent"));
        }
        dirs
    }

    /// The first working directory that holds a place credentials are kept in (Pi's `auth.json` and `models.json`, the
    /// person's `.ssh` and the like), when the launch does not allow it.
    #[must_use]
    pub fn credential_root(&self) -> Option<PathBuf> {
        if self.allow_credential_roots {
            return None;
        }
        let scope = crate::mode::Scope::new(&self.cwd, self.roots())
            .with_agent_dirs(self.agent_dirs())
            .with_home(self.home());
        self.roots()
            .into_iter()
            .chain(core::iter::once(self.cwd.clone()))
            .find(|root| scope.reaches_secret(Some(&root.to_string_lossy())))
    }

    fn path_value(&self) -> Option<OsString> {
        self.env
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("PATH"))
    }

    /// The program to start, and the arguments that come before Pi's own.
    ///
    /// On any system but Windows this is `executable`. On Windows nothing is started through `cmd.exe` and nothing is found
    /// by a search that includes the project: a bare `pi` is looked for in `PATH` (absolute entries outside the session's
    /// directories) as `pi.exe`, then `pi.cmd`. npm's `pi.cmd` is not run: its script (`cli.js`) is read from it and
    /// started with an absolute `node.exe`, the one beside the shim or the first on `PATH` outside the session's
    /// directories.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] when, on Windows, `pi` or `node` is not to be found that way.
    pub fn command(&self) -> Result<(OsString, Vec<OsString>), Error> {
        if !cfg!(windows) {
            return Ok((self.executable.clone(), Vec::new()));
        }
        let keep_out = self.roots();
        let path = self.path_value().unwrap_or_default();
        let found = find_in(&self.executable, &path, &["exe", "cmd"], &keep_out)
            .or_else(|| {
                // A path or a name with an extension is the host's own choice: used as written.
                let written = self.executable.to_string_lossy();
                (written.contains(['/', '\\']) || Path::new(&self.executable).extension().is_some())
                    .then(|| PathBuf::from(&self.executable))
            })
            .ok_or_else(|| Error::Refused("Pi was not found on PATH".into()))?;
        let is_shim = found.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });
        if !is_shim {
            return Ok((found.into_os_string(), Vec::new()));
        }
        let script = std::fs::read_to_string(&found)
            .ok()
            .and_then(|text| shim_script(&found, &text))
            .ok_or_else(|| {
                Error::Refused(
                    "that Pi shim names no script this crate can start without cmd.exe".into(),
                )
            })?;
        let beside = found.with_file_name("node.exe");
        let node = if beside.is_file() {
            beside
        } else {
            find_in(OsStr::new("node"), &path, &["exe"], &keep_out).ok_or_else(|| {
                Error::Refused(
                    "node.exe was not found on PATH outside the session's directories".into(),
                )
            })?
        };
        Ok((node.into_os_string(), vec![script.into_os_string()]))
    }

    /// Loads `guard` into a Pi that runs on Node, ahead of whatever `NODE_OPTIONS` the launch already has.
    fn preload(&self, launch: &mut Launch, guard: &Path) {
        // Forward slashes: NODE_OPTIONS reads a backslash as an escape inside quotes.
        let written = guard.to_string_lossy().replace('\\', "/");
        let require = if written.contains([' ', '"']) {
            format!("--require \"{}\"", written.replace('"', "\\\""))
        } else {
            format!("--require {written}")
        };
        let before = (!self.clear_env || self.env.keys().any(|k| k == "NODE_OPTIONS"))
            .then(|| self.named("NODE_OPTIONS"))
            .flatten();
        let options = match before {
            Some(before) if !before.as_os_str().is_empty() => {
                format!("{require} {}", before.to_string_lossy())
            }
            _ => require,
        };
        launch.env.insert("NODE_OPTIONS".into(), options.into());
        launch.remove_env.retain(|name| name != "NODE_OPTIONS");
        let keep_out: Vec<String> = {
            let mut dirs = self.roots();
            dirs.push(self.cwd.clone());
            dirs.iter()
                .map(|dir| dir.to_string_lossy().into_owned())
                .collect()
        };
        launch.env.insert(
            "COQUI_ACP_PI_KEEP_OUT".into(),
            serde_json::to_string(&keep_out).unwrap_or_default().into(),
        );
    }

    /// The command of Pi with no arguments of its own yet: the program, its leading arguments, the environment.
    ///
    /// # Errors
    ///
    /// As [`PiLaunch::command`].
    pub fn base(&self) -> Result<Launch, Error> {
        let (program, leading) = self.command()?;
        let mut launch = Launch::new(program).args(leading).clear_env(self.clear_env);
        launch.env.clone_from(&self.env);
        launch.remove_env.clone_from(&self.remove_env);
        if cfg!(windows) {
            // Programs Pi starts (a shell, say) must not be found in the project either.
            launch.remove_env.retain(|name| {
                !name
                    .to_string_lossy()
                    .eq_ignore_ascii_case("NoDefaultCurrentDirectoryInExePath")
            });
            launch
                .env
                .insert("NoDefaultCurrentDirectoryInExePath".into(), "1".into());
            // The program search of Pi's own spawner (cross-spawn, `which`) tries the working directory first and honours a
            // script extension (`.JS`) in PATHEXT: a project's `npm.js` would run. Only what the system runs as a program stays.
            launch
                .env
                .retain(|name, _| !name.to_string_lossy().eq_ignore_ascii_case("PATHEXT"));
            launch
                .remove_env
                .retain(|name| !name.to_string_lossy().eq_ignore_ascii_case("PATHEXT"));
            launch
                .env
                .insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        }
        Ok(launch)
    }

    /// The command: `pi --mode rpc`, with the gate when there is one (`-e`), the session to open, the model and the thinking
    /// level, the system prompt's file, the arguments that keep the project's own resources out (`--no-approve`; with
    /// `isolated`, `--no-extensions` and `--no-mcp` too), and the caller's own arguments.
    ///
    /// # Errors
    ///
    /// As [`PiLaunch::command`].
    pub fn launch(&self, extension: Option<&Path>, prompt: Option<&Path>) -> Result<Launch, Error> {
        self.launch_guarded(extension, prompt, None)
    }

    /// As [`PiLaunch::launch`], and with `guard` (the Node preload `guard.cjs`) loaded into Pi: `NODE_OPTIONS` gets
    /// `--require` of it, and `COQUI_ACP_PI_KEEP_OUT` names the session's directories, which no program search of Pi's may
    /// look in. The preload only acts in a Pi that runs on Node.
    ///
    /// # Errors
    ///
    /// As [`PiLaunch::command`].
    pub fn launch_guarded(
        &self,
        extension: Option<&Path>,
        prompt: Option<&Path>,
        guard: Option<&Path>,
    ) -> Result<Launch, Error> {
        let mut launch = self
            .base()?
            .args(["--mode", "rpc", "--no-approve"])
            .cwd(&self.cwd);
        if self.isolated {
            launch = launch.args(["--no-extensions", "--no-mcp"]);
        }
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
        if let Some(guard) = guard {
            self.preload(&mut launch, guard);
        }
        Ok(launch.args(self.extra_args.iter()))
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
        launch.executable = std::env::current_exe().unwrap().into();
        launch.session = SessionStart::Resume("abc-1".into());
        launch.model = Some("anthropic/claude".into());
        launch.effort = Some("high".into());
        let command = launch
            .launch(Some(Path::new("/g/gate.ts")), Some(Path::new("/g/p.md")))
            .unwrap();
        let args: Vec<_> = command
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args[..],
            [
                "--mode",
                "rpc",
                "--no-approve",
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
            ][..]
        );
        launch.isolated = true;
        let args = launch.launch(None, None).unwrap().args;
        assert!(
            args.iter().any(|a| a == "--no-extensions") && args.iter().any(|a| a == "--no-mcp")
        );
    }

    #[test]
    fn exe_comes_before_cmd_and_entries_that_are_not_to_be_trusted_are_skipped() {
        let base = std::env::temp_dir().join(format!("coqui-acp-pi-find-{}", std::process::id()));
        let (bin, project) = (base.join("bin"), base.join("project"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(bin.join("pi.cmd"), "").unwrap();
        std::fs::write(project.join("pi.exe"), "").unwrap();
        let keep_out = vec![project.clone()];
        let path = std::env::join_paths([&project, Path::new("."), Path::new(""), &bin]).unwrap();
        let find = |name: &str| find_in(OsStr::new(name), &path, &["exe", "cmd"], &keep_out);
        // The project's `pi.exe`, a relative entry and an empty one are passed over.
        assert_eq!(find("pi"), Some(bin.join("pi.cmd")));
        std::fs::write(bin.join("pi.exe"), "").unwrap();
        assert_eq!(find("pi"), Some(bin.join("pi.exe")));
        assert_eq!(find("pi.cmd"), None);
        assert_eq!(find("./pi"), None);
        // A `..` that leads back into the project does not hide it.
        let sneaky = bin.join("..").join("project");
        let path = std::env::join_paths([&sneaky, &bin]).unwrap();
        let find = |name: &str| find_in(OsStr::new(name), &path, &["exe", "cmd"], &keep_out);
        assert_eq!(
            find("pi"),
            Some(bin.join("pi.exe")),
            "an entry that resolves into the project was searched"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_shim_names_its_script_under_its_own_directory() {
        let base = std::env::temp_dir().join(format!("coqui-acp-pi-shim-{}", std::process::id()));
        let script = base.join("node_modules").join("pkg").join("cli.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        let shim = base.join("pi.cmd");
        let text = "IF EXIST \"%dp0%\\node.exe\" (\r\n) ELSE (\r\n)\r\n\"%_prog%\"  \"%dp0%\\node_modules\\pkg\\cli.js\" %*\r\n";
        assert_eq!(shim_script(&shim, text), Some(script));
        assert_eq!(shim_script(&shim, "\"%dp0%\\missing.js\""), None);
        assert_eq!(shim_script(&shim, "\"%dp0%\\%EVIL%\\cli.js\""), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
