//! The library's commands behind a command line, for the tests that drive them as a program would: `keygen`, `pair-accept`,
//! `pair-dial`, `serve`, `dial`, `forward`, `grant`, `discover`, `mcp-config`, `mcp-shim` and the agent guard. A program that
//! people run is the host's, not this library's.
#![allow(
    clippy::large_enum_variant,
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    missing_docs
)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use coqui_acp_bridge::helper::Helper;
use coqui_acp_bridge::standing::DEFAULT_NAME;
use coqui_acp_bridge::{agent, dial, discover, forward, grant, keygen, mcp_shim, pairing, serve};
use coqui_agentic_peer::auth::Method;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Show a PIN and wait for one peer to pair with it.
    PairAccept(pairing::PairAccept),
    /// Pair with a waiting `pair-accept`, using its PIN.
    PairDial(pairing::PairDial),
    /// Create an identity file and print its peer id.
    Keygen {
        path: PathBuf,
        #[arg(long, value_name = "METHOD")]
        method: Option<Method>,
    },
    /// Wait for peers, prove each, and give it an agent of its own.
    Serve(serve::Serve),
    /// Connect out to a peer, for an agent behind NAT.
    Dial(dial::Dial),
    /// Forward TCP over a link.
    Forward(forward::Forward),
    /// Let a paired peer open a protocol, or with `--revoke` no longer.
    Grant {
        peer: String,
        protocol: String,
        #[arg(long)]
        revoke: bool,
    },
    /// Search this machine and the link for agents.
    Discover {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// The stdio MCP server an offer declares, or with `--peer` the one `mcp-config` prints.
    McpShim {
        #[arg(long)]
        peer: Option<String>,
    },
    /// Print the `mcpServers` entry that gives an MCP client a peer's tools.
    McpConfig {
        #[arg(long)]
        peer: String,
        #[arg(long, default_value = DEFAULT_NAME)]
        name: String,
    },
    /// Waits on standard input, and ends the agent's process group if it closes with nothing said.
    AgentGuard { pid: u32 },
}

fn run(cli: Cli) -> Result<u8, String> {
    let helper = Helper::current()?;
    match cli.command {
        Mode::Keygen { path, method } => {
            println!("{}", keygen::create(&path, method)?);
            Ok(0)
        }
        Mode::PairAccept(args) => pairing::accept(&args),
        Mode::PairDial(args) => pairing::join(&args),
        Mode::Serve(mut args) => {
            args.helper = Some(helper);
            serve::run(&args)
        }
        Mode::Dial(args) => dial::run(&args),
        Mode::Forward(args) => forward::run(&args),
        Mode::Grant {
            peer,
            protocol,
            revoke,
        } => grant::run(None, &peer, &protocol, revoke),
        Mode::Discover { seconds } => discover::run(seconds),
        Mode::McpShim { peer } => mcp_shim::run(peer.as_deref()),
        Mode::McpConfig { peer, name } => {
            mcp_shim::config(&helper, None, &peer, &name).map(|config| {
                println!("{config}");
                0
            })
        }
        Mode::AgentGuard { pid } => {
            agent::watch(pid, std::io::stdin().lock());
            Ok(0)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(why) => {
            eprintln!("coqui-acp-bridge: {why}");
            ExitCode::from(2)
        }
    }
}
