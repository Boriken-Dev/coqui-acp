//! A host: loads the provider a Guatiao source offers and serves it as an ACP agent on standard input and
//! output, which is how any client starts an ACP agent. Standard error says what went wrong.
//!
//! `provider-python-host <source file> <provider id> <directory of manifests> <python path>...`

use std::path::Path;
use std::process::ExitCode;

use coqui_acp::lines;
use coqui_acp_provider::guatiao::library::{ImportSearch, Registry};
use coqui_acp_provider::guatiao::{List, Map};
use coqui_acp_provider::{AcpFacade, AgentProvider};

fn serve() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut next = |what: &str| args.next().ok_or(format!("no {what} given"));
    let source = next("source file")?;
    let id = next("provider id")?;
    let manifests = next("directory of manifests")?;
    let mut dirs = List::new();
    dirs.push(manifests.as_str());
    let mut paths = List::new();
    for path in args {
        paths.push(path.as_str());
    }
    let mut config = Map::new();
    config.set("dirs", dirs);
    config.set("paths", paths);

    let mut registry = Registry::new("provider-python-host", env!("CARGO_PKG_VERSION"));
    // On Windows the source imports python3.dll, which is found through PATH.
    registry.set_import_search(ImportSearch::Path);
    registry
        .add_source_file(Path::new(&source), &config)
        .map_err(|error| format!("the source was refused: {error}"))?;
    let changed = registry.refresh();
    if !changed.refused.is_empty() {
        return Err(format!("a library was refused: {:?}", changed.refused));
    }
    let offer = registry
        .offer::<dyn AgentProvider>(&id)
        .ok_or(format!("nothing offers {id}"))?
        .map_err(|mismatch| format!("{id} is not this kind: {mismatch:?}"))?;
    let provider = offer
        .shared()
        .ok_or(format!("{id} needs a configuration"))?;

    let (transport, _diagnostics) = lines::over_io(
        std::io::stdin(),
        std::io::stdout(),
        0,
        lines::DEFAULT_MAX_LINE,
    )
    .map_err(|error| format!("the transport did not start: {error}"))?;
    AcpFacade::new(provider)
        .run(transport)
        .map_err(|error| error.to_string())
}

fn main() -> ExitCode {
    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("provider-python-host: {why}");
            ExitCode::FAILURE
        }
    }
}
