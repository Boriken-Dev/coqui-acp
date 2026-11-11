//! `within_roots` on a path a model chose: a network or device path is refused by how it is written, before anything
//! resolves it, since resolving `\host\share` opens a connection to the host.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use coqui_acp_harness::acp::within_roots;

/// Runs `within_roots` on a thread, so that a path that makes it wait fails the test instead of hanging it.
fn answered(path: &str, roots: Vec<PathBuf>) -> Option<bool> {
    let (sender, received) = mpsc::channel();
    let path = PathBuf::from(path);
    std::thread::spawn(move || {
        let _ = sender.send(within_roots(&path, &roots));
    });
    received.recv_timeout(Duration::from_secs(2)).ok()
}

#[test]
fn a_network_or_device_path_is_outside_every_root_and_is_never_resolved() {
    let root = std::env::temp_dir();
    for path in [
        r"\10.255.255.1\share\file.txt",
        r"\?\UNC\10.255.255.1\share\file.txt",
        r"\.\pipe\x",
        "//10.255.255.1/share/file.txt",
    ] {
        assert_eq!(
            answered(path, vec![root.clone()]),
            Some(false),
            "{path} was not refused at once"
        );
    }
}

#[test]
fn an_ordinary_path_inside_a_root_still_is() {
    let root = std::env::temp_dir();
    assert_eq!(
        answered(
            root.join("a").join("b.txt").to_str().unwrap(),
            vec![root.clone()]
        ),
        Some(true)
    );
    assert!(!within_roots(Path::new("/definitely/elsewhere"), &[root]));
}
