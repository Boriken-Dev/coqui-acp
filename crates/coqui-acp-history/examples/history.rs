//! The history commands behind a command line, for the tests that drive them as a program would: `list`, `show`, `convert`.
//! A program that people run is the host's, over `coqui_acp_history::cli`.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs,
    reason = "an example names what it uses from std"
)]

use std::process::ExitCode;

use clap::Parser;
use coqui_acp_history::cli::{Command, run};

#[derive(Debug, Parser)]
#[command(
    name = "history",
    version,
    about = "Look at and move Agent Client Protocol session histories"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn main() -> ExitCode {
    match run(&Cli::parse().command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
