//! Pi's agent as library code: its arguments as a `clap::Args` type and a [`run`] that serves ACP on standard input and
//! output. A program that people run is the host's; `examples/serve_pi.rs` is one for this crate's tests.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use crate::harness::{Mode, Settings, serve};
use crate::{PiAgent, PiLaunch};

/// Serves Pi as an Agent Client Protocol agent on standard input and output, in ACP version 1 or 2, whichever the client
/// asks for. Each session runs `pi --mode rpc` with a permission gate loaded, so that what the modes allow is asked
/// through ACP. Diagnostics go to standard error.
///
/// Pi reads its own variables (`PI_CODING_AGENT_DIR`, `ANTHROPIC_API_KEY` and others) from the environment it is given.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The Pi program. On Windows npm's shim is `pi.cmd`.
    #[arg(long, env = "COQUI_ACP_PI_EXECUTABLE", default_value = "pi")]
    pub executable: OsString,
    /// A Node executable (an absolute path) that runs Pi's CLI `--script`, instead of an `--executable` on the PATH.
    #[arg(long, env = "COQUI_ACP_PI_NODE", requires = "script")]
    pub node: Option<PathBuf>,
    /// Pi's CLI script (an absolute path), which `--node` runs.
    #[arg(long, env = "COQUI_ACP_PI_SCRIPT", requires = "node")]
    pub script: Option<PathBuf>,
    /// The model every session starts with, as provider/id.
    #[arg(long)]
    pub model: Option<String>,
    /// The thinking level every session starts with, such as low or high.
    #[arg(long)]
    pub effort: Option<String>,
    /// The mode every session starts in, and the loosest a client may choose: manual, accept_edits or plan.
    #[arg(long, value_parser = parse_mode)]
    pub mode: Option<Mode>,
    /// Offer the Bypass mode: never ask.
    #[arg(long)]
    pub allow_bypass: bool,
    /// Ask before a tool that only reads, too, in Manual and Plan modes.
    #[arg(long)]
    pub ask_for_reads: bool,
    /// Load only the permission gate: none of the person's own extensions and no MCP servers.
    #[arg(long)]
    pub isolated: bool,
    /// A protected path pattern in gitignore syntax (`!` allows, a later line wins, `~/` is the home directory), repeated
    /// and in order; any replaces the recommended list.
    #[arg(long = "protected", value_name = "PATTERN", allow_hyphen_values = true)]
    pub protected: Vec<String>,
    /// Let a session work in a directory that holds the places credentials are kept in (a home directory).
    #[arg(long)]
    pub allow_credential_roots: bool,
    /// The highest ACP version answered.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub max_acp_version: u8,
    /// Sessions at once.
    #[arg(long, default_value_t = 8)]
    pub max_sessions: usize,
    /// A directory every session's `cwd` and more directories must lie in, repeated for more; none takes any.
    #[arg(long = "working-root", value_name = "DIRECTORY")]
    pub working_roots: Vec<PathBuf>,
    /// Take a network path (`\\host\share`) as a session's `cwd` or more directories.
    #[arg(long)]
    pub allow_network_paths: bool,
}

fn parse_mode(id: &str) -> Result<Mode, String> {
    Mode::from_id(id).ok_or_else(|| format!("{id} is not a mode"))
}

impl Args {
    /// The settings the harness serves with.
    #[must_use]
    pub fn settings(&self) -> Settings {
        let mut settings = Settings::new();
        settings.max_version = self.max_acp_version;
        settings.max_sessions = self.max_sessions;
        settings.working_roots.clone_from(&self.working_roots);
        settings.network_paths = self.allow_network_paths;
        settings.on_notice = Some(Arc::new(|_, text: &str| {
            eprintln!("coqui-acp-pi: {text}");
        }));
        settings
    }

    /// The agent, with these arguments as what each session starts with.
    #[must_use]
    pub fn agent(&self) -> PiAgent {
        let mut defaults = PiLaunch::new(".");
        defaults.executable.clone_from(&self.executable);
        defaults.node.clone_from(&self.node);
        defaults.script.clone_from(&self.script);
        defaults.model.clone_from(&self.model);
        defaults.effort.clone_from(&self.effort);
        defaults.mode = self.mode;
        defaults.allow_bypass = self.allow_bypass;
        defaults.ask_for_reads = self.ask_for_reads;
        defaults.isolated = self.isolated;
        defaults.allow_credential_roots = self.allow_credential_roots;
        defaults.protected = (!self.protected.is_empty()).then(|| self.protected.clone());
        PiAgent::new().defaults(defaults)
    }
}

/// Serves the agent on standard input and output until the client closes it.
///
/// # Errors
///
/// The connection failed; the message says how.
pub fn run(args: &Args) -> Result<u8, String> {
    serve(
        Arc::new(args.agent()),
        args.settings(),
        std::io::stdin(),
        std::io::stdout(),
    )
    .map(|()| 0)
    .map_err(|error| error.to_string())
}

/// What the agent's history commands do: move a session in and out of the standard history format
/// (`docs/session-history.md`).
#[cfg(feature = "history")]
#[derive(Debug, clap::Subcommand)]
pub enum History {
    /// Writes the session's history to standard output, as it is.
    Export {
        /// Pi's agent directory; by default `PI_CODING_AGENT_DIR`, else `.pi/agent` in the home directory.
        #[arg(long)]
        agent_dir: Option<PathBuf>,
        /// The directory the session worked in.
        cwd: PathBuf,
        /// The session's UUID.
        id: String,
    },
    /// Reads a history from standard input and writes it as a session Pi resumes. Prints what could not be carried
    /// over on standard error; never replaces a session.
    Import {
        /// Pi's agent directory; by default `PI_CODING_AGENT_DIR`, else `.pi/agent` in the home directory.
        #[arg(long)]
        agent_dir: Option<PathBuf>,
        /// The directory the session worked in.
        cwd: PathBuf,
        /// The session's UUID.
        id: String,
        /// Seed from the first line of the history, not from its latest compaction summary.
        #[arg(long)]
        no_summary: bool,
    },
}

/// Runs one history command on standard input and output.
///
/// # Errors
///
/// Pi's agent directory is unknown, or the session or the history could not be read or written.
#[cfg(feature = "history")]
pub fn run_history(action: &History) -> Result<u8, String> {
    use std::io::{Read, Write};

    use crate::history::{DEFAULT_MAX_BYTES, ImportOptions, export_file, import_file};

    let place = |agent_dir: &Option<PathBuf>, cwd: &PathBuf| {
        let mut launch = PiLaunch::new(cwd);
        if let Some(dir) = agent_dir {
            launch
                .env
                .insert("PI_CODING_AGENT_DIR".into(), dir.clone().into());
        }
        launch
            .sessions_dir()
            .ok_or_else(|| "Pi's agent directory is unknown: pass --agent-dir".to_owned())
    };
    match action {
        History::Export { agent_dir, cwd, id } => {
            let directory = place(agent_dir, cwd)?;
            let doc = export_file(&directory, id, DEFAULT_MAX_BYTES)
                .map_err(|error| error.to_string())?;
            let bytes = doc.to_vec().map_err(|error| error.to_string())?;
            std::io::stdout()
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            Ok(0)
        }
        History::Import {
            agent_dir,
            cwd,
            id,
            no_summary,
        } => {
            let directory = place(agent_dir, cwd)?;
            let mut text = Vec::new();
            std::io::stdin()
                .read_to_end(&mut text)
                .map_err(|error| error.to_string())?;
            let doc = coqui_acp_history::Document::from_slice(&text)
                .map_err(|error| error.to_string())?;
            let options = ImportOptions::default().use_summary(!no_summary);
            let imported = import_file(&directory, cwd, id, &doc, &options)
                .map_err(|error| error.to_string())?;
            let lost: Vec<String> = imported
                .report
                .losses()
                .iter()
                .map(|loss| format!("{}:{}", loss.kind, loss.count))
                .collect();
            eprintln!("imported; lost: [{}]", lost.join(", "));
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Args;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn what_a_host_narrows_on_the_command_line_reaches_the_settings() {
        let settings = Cli::parse_from(["pi", "--working-root", "/w", "--working-root", "/x"])
            .args
            .settings();
        assert_eq!(settings.working_roots.len(), 2);
        assert!(!settings.network_paths);
    }
}
