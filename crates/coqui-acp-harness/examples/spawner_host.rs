//! A program the spawner's tests start.
//!
//! - `spawner_host echo`: writes `err` to standard error, copies standard input to standard output,
//!   and exits with code 3 at the end of its input.
//! - `spawner_host tree`: a host. It starts, through `LocalSpawner`, a shell that starts a process in
//!   its group (and on Linux one in a session of its own), passes the shell's lines on (`pid N`,
//!   `escaped N`, `ready`), then waits to be killed, so a test can see the tree end with its host.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::print_stdout,
    reason = "an example names what it uses from std and talks on its own streams"
)]

use std::io::{BufRead, Read, Write};

use coqui_acp_harness::LocalSpawner;
use coqui_acp_harness_kind::{ProcessSpawner, SpawnSpec};

/// A shell that starts a process that outlives it unless its tree is ended, and says its id.
pub fn tree() -> SpawnSpec {
    if cfg!(windows) {
        SpawnSpec::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(
                "$p = Start-Process -PassThru -WindowStyle Hidden -FilePath ping.exe \
                 -ArgumentList '-n','300','127.0.0.1'; \
                 Write-Output ('pid ' + $p.Id); Write-Output ready; Start-Sleep 300",
            )
    } else {
        SpawnSpec::new("/bin/sh").arg("-c").arg(
            "sleep 300 & echo \"pid $!\"; \
             if command -v setsid >/dev/null 2>&1; then setsid sleep 300 & echo \"escaped $!\"; fi; \
             echo ready; wait",
        )
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("echo") => {
            eprintln!("err");
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            let mut out = std::io::stdout();
            let _ = out.write_all(&input);
            let _ = out.flush();
            std::process::exit(3);
        }
        Some("tree") => {
            let process = LocalSpawner::new().spawn(tree()).expect("the tree starts");
            let mut lines = Vec::new();
            let mut chunk = [0u8; 256];
            loop {
                let n = process.read(&mut chunk, 30_000).expect("read");
                if n <= 0 {
                    break;
                }
                lines.extend_from_slice(&chunk[..usize::try_from(n).expect("a count")]);
                if lines.windows(5).any(|w| w == b"ready") {
                    break;
                }
            }
            for line in lines.as_slice().lines().map_while(Result::ok) {
                println!("{}", line.trim());
            }
            let _ = std::io::stdout().flush();
            // Held until this process is killed: its tree must end with it.
            let _keep = process;
            loop {
                std::thread::park();
            }
        }
        _ => {
            eprintln!("spawner_host echo|tree");
            std::process::exit(2);
        }
    }
}
