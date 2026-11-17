//! The `pi` agent's command line over `coqui_acp_pi::cli`, for trying it by hand:
//! `cargo run --example serve_pi -- --help`. A program that people run is the host's.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs,
    reason = "an example names what it uses from std"
)]

use std::process::ExitCode;

use clap::Parser;
use coqui_acp_pi::cli;

#[derive(Debug, Parser)]
#[command(name = "serve_pi", version)]
struct Cli {
    #[cfg(feature = "history")]
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: cli::Args,
}

/// What the example does besides serving.
#[cfg(feature = "history")]
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Moves a session in and out of the standard history format.
    History {
        #[command(subcommand)]
        action: cli::History,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    #[cfg(feature = "history")]
    let outcome = match &cli.command {
        Some(Command::History { action }) => cli::run_history(action),
        None => cli::run(&cli.serve),
    };
    #[cfg(not(feature = "history"))]
    let outcome = cli::run(&cli.serve);
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(why) => {
            eprintln!("coqui-acp-pi: {why}");
            ExitCode::FAILURE
        }
    }
}
