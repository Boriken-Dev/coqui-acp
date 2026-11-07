//! `coqui-acp-claude`: Claude Code as an Agent Client Protocol agent on standard input and output.

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
use coqui_acp_claude::harness::{Settings, serve};
use coqui_acp_claude::{ClaudeAgent, ClaudeLaunch};

/// Serves Claude Code as an Agent Client Protocol agent on standard input and output, in ACP version 1 or 2,
/// whichever the client asks for. Each session runs `claude` in stream-json mode; permission prompts are asked
/// through ACP. Diagnostics go to standard error.
#[derive(Debug, Parser)]
#[command(
    name = "coqui-acp-claude",
    version,
    after_help = "Claude Code reads its own variables (CLAUDE_CONFIG_DIR, \
    ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN and others) from the environment it is given."
)]
struct Cli {
    /// The Claude Code program.
    #[arg(long, env = "COQUI_ACP_CLAUDE_EXECUTABLE", default_value = "claude")]
    executable: OsString,
    /// The model every session starts with, until the client chooses another.
    #[arg(long)]
    model: Option<String>,
    /// The effort every session starts with: low, medium, high, xhigh or max.
    #[arg(long)]
    effort: Option<String>,
    /// The permission mode: default, acceptEdits, plan, dontAsk, bypassPermissions, auto or manual.
    #[arg(long)]
    permission_mode: Option<String>,
    /// Offer the Bypass mode: Claude Code is started allowing a session to switch to `bypassPermissions`.
    #[arg(long)]
    allow_bypass: bool,
    /// The setting sources Claude Code loads, comma-separated, of user, project and local; empty loads none.
    #[arg(long)]
    setting_sources: Option<String>,
    /// Only the MCP servers a session is given, none from the person's or the project's configuration.
    #[arg(long)]
    strict_mcp_config: bool,
    /// The highest ACP version answered.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2))]
    max_acp_version: u8,
    /// Sessions at once.
    #[arg(long, default_value_t = 8)]
    max_sessions: usize,
    /// Do not ask for permission on standard output: what nothing has granted is refused where it is made.
    #[arg(long)]
    no_permission_prompts: bool,
    /// Let a client load plugins through `_meta.claudeCode.options.plugins`; their hooks and servers run unasked.
    #[arg(long)]
    allow_client_plugins: bool,
    /// A directory every session's `cwd` and more directories must lie in, repeated for more; none takes any.
    #[arg(long = "working-root", value_name = "DIRECTORY")]
    working_roots: Vec<PathBuf>,
    /// Refuse a session that a client gives MCP servers.
    #[arg(long)]
    no_client_mcp_servers: bool,
    /// Take a network path (`\\\\host\\share`) as a session's `cwd` or more directories.
    #[arg(long)]
    allow_network_paths: bool,
}

fn settings_of(cli: &Cli) -> Settings {
    let mut settings = Settings::new();
    settings.max_version = cli.max_acp_version;
    settings.max_sessions = cli.max_sessions;
    settings.working_roots.clone_from(&cli.working_roots);
    settings.client_mcp_servers = !cli.no_client_mcp_servers;
    settings.network_paths = cli.allow_network_paths;
    settings.on_notice = Some(Arc::new(|text: &str| eprintln!("coqui-acp-claude: {text}")));
    settings
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let settings = settings_of(&cli);
    let mut defaults = ClaudeLaunch::new(".");
    defaults.executable = cli.executable;
    defaults.model = cli.model;
    defaults.effort = cli.effort;
    defaults.permission_mode = cli.permission_mode;
    defaults.allow_bypass = cli.allow_bypass;
    defaults.setting_sources = cli.setting_sources.map(|sources| {
        sources
            .split(',')
            .filter(|source| !source.is_empty())
            .map(str::to_string)
            .collect()
    });
    defaults.strict_mcp_config = cli.strict_mcp_config;
    defaults.permission_prompts = !cli.no_permission_prompts;
    let agent = ClaudeAgent::new()
        .defaults(defaults)
        .client_plugins(cli.allow_client_plugins);
    match serve(
        Arc::new(agent),
        settings,
        std::io::stdin(),
        std::io::stdout(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coqui-acp-claude: {error}");
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
            "coqui-acp-claude",
            "--working-root",
            "/w",
            "--working-root",
            "/x",
            "--no-client-mcp-servers",
        ]));
        assert_eq!(settings.working_roots.len(), 2);
        assert!(!settings.client_mcp_servers);
        assert!(!settings.network_paths);
    }

    #[test]
    fn plugins_from_a_client_and_the_prompt_flags_are_a_hosts_to_set() {
        let cli = Cli::parse_from(["coqui-acp-claude"]);
        assert!(!cli.allow_client_plugins);
        assert!(!cli.no_permission_prompts);
        let cli = Cli::parse_from([
            "coqui-acp-claude",
            "--allow-client-plugins",
            "--no-permission-prompts",
        ]);
        assert!(cli.allow_client_plugins);
        assert!(cli.no_permission_prompts);
    }
}
