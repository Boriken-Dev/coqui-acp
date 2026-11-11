//! `coqui-acp-pi`: Pi as an Agent Client Protocol agent on standard input and output.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use coqui_acp_pi::harness::{Mode, Settings, serve};
use coqui_acp_pi::{PiAgent, PiLaunch};

/// Serves Pi as an Agent Client Protocol agent on standard input and output, in ACP version 1 or 2, whichever the client
/// asks for. Each session runs `pi --mode rpc` with a permission gate loaded, so that what the modes allow is asked
/// through ACP. Diagnostics go to standard error.
#[derive(Debug, Parser)]
#[command(
    name = "coqui-acp-pi",
    version,
    after_help = "Pi reads its own variables (PI_CODING_AGENT_DIR, ANTHROPIC_API_KEY and \
    others) from the environment it is given."
)]
struct Cli {
    /// The Pi program. On Windows npm's shim is `pi.cmd`.
    #[arg(long, env = "COQUI_ACP_PI_EXECUTABLE", default_value = "pi")]
    executable: OsString,
    /// The model every session starts with, as provider/id.
    #[arg(long)]
    model: Option<String>,
    /// The thinking level every session starts with, such as low or high.
    #[arg(long)]
    effort: Option<String>,
    /// The mode every session starts in, and the loosest a client may choose: manual, accept_edits or plan.
    #[arg(long, value_parser = parse_mode)]
    mode: Option<Mode>,
    /// Offer the Bypass mode: never ask.
    #[arg(long)]
    allow_bypass: bool,
    /// Ask before a tool that only reads, too, in Manual mode.
    #[arg(long)]
    ask_for_reads: bool,
    /// The highest ACP version answered.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2))]
    max_acp_version: u8,
    /// Sessions at once.
    #[arg(long, default_value_t = 8)]
    max_sessions: usize,
    /// A directory every session's `cwd` and more directories must lie in, repeated for more; none takes any.
    #[arg(long = "working-root", value_name = "DIRECTORY")]
    working_roots: Vec<PathBuf>,
    /// Take a network path (`\\host\share`) as a session's `cwd` or more directories.
    #[arg(long)]
    allow_network_paths: bool,
}

fn parse_mode(id: &str) -> Result<Mode, String> {
    Mode::from_id(id).ok_or_else(|| format!("{id} is not a mode"))
}

fn settings_of(cli: &Cli) -> Settings {
    let mut settings = Settings::new();
    settings.max_version = cli.max_acp_version;
    settings.max_sessions = cli.max_sessions;
    settings.working_roots.clone_from(&cli.working_roots);
    settings.network_paths = cli.allow_network_paths;
    settings.on_notice = Some(Arc::new(|text: &str| eprintln!("coqui-acp-pi: {text}")));
    settings
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let settings = settings_of(&cli);
    let mut defaults = PiLaunch::new(".");
    defaults.executable = cli.executable;
    defaults.model = cli.model;
    defaults.effort = cli.effort;
    defaults.mode = cli.mode;
    defaults.allow_bypass = cli.allow_bypass;
    defaults.ask_for_reads = cli.ask_for_reads;
    let agent = PiAgent::new().defaults(defaults);
    match serve(
        Arc::new(agent),
        settings,
        std::io::stdin(),
        std::io::stdout(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coqui-acp-pi: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, settings_of};

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn what_a_host_narrows_on_the_command_line_reaches_the_settings() {
        let settings = settings_of(&Cli::parse_from([
            "coqui-acp-pi",
            "--working-root",
            "/w",
            "--working-root",
            "/x",
        ]));
        assert_eq!(settings.working_roots.len(), 2);
        assert!(!settings.network_paths);
    }
}
