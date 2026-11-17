//! The contract suites behind a command line, for the tests that drive them as a program would:
//! `check harness --library FILE [OPTIONS]` and `check account --library FILE [OPTIONS]`, each also taking
//! `--source SOURCE_LIBRARY --source-config JSON` in place of a library. Exits 0 when the provider keeps every rule, 1 with
//! each rule it broke, 2 when it could not be loaded or the command line is wrong. A program that people run is the host's.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs,
    reason = "an example names what it uses from std and reports on its own streams"
)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use coqui_acp_api::check::{AccountCheck, Again, HarnessCheck, run_account, run_harness};

#[derive(Debug, Parser)]
#[command(name = "check", version)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Debug, Subcommand)]
enum Mode {
    /// Run the `acp_harness` contract.
    Harness(HarnessCheck),
    /// Run the `account_provider` contract.
    Account(AccountCheck),
}

fn main() -> ExitCode {
    let outcome = match Cli::parse().command {
        Mode::Harness(mut args) => {
            args.again = std::env::current_exe().ok().map(|program| Again {
                program,
                args: std::env::args_os().skip(1).collect(),
            });
            run_harness(&args)
        }
        Mode::Account(args) => run_account(&args),
    };
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(why) => {
            eprintln!("check: {why}");
            ExitCode::from(2)
        }
    }
}
