//! A plain process ends with everything it started.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::Read;
use std::time::{Duration, Instant};

use coqui_acp_harness::{Launch, PlainSpawn, Spawn};

/// The shell's child inherits its output, so killing the shell alone would
/// leave the output open, and the reader waiting, until the child exits.
#[test]
fn a_killed_process_ends_with_everything_it_started() {
    let launch = if cfg!(windows) {
        Launch::new("cmd.exe").args(["/d", "/c", "ping -n 30 127.0.0.1 >nul & echo done"])
    } else {
        Launch::new("/bin/sh").args(["-c", "sleep 30; echo done"])
    };
    let spawned = PlainSpawn.spawn(&launch).unwrap();
    let mut stdout = spawned.stdout;
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = sender.send(output);
    });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(spawned.process.try_wait().unwrap(), None, "it runs");
    let started = Instant::now();
    spawned.process.kill().unwrap();
    let output = received
        .recv_timeout(Duration::from_secs(15))
        .expect("the child outlived the kill and held the output open");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(!output.contains("done"), "{output}");
    assert!(
        spawned.process.kill().is_ok(),
        "killing an ended process is no error"
    );
}

#[test]
fn an_exit_is_told_with_its_code() {
    let launch = if cfg!(windows) {
        Launch::new("cmd.exe").args(["/d", "/c", "exit 3"])
    } else {
        Launch::new("/bin/sh").args(["-c", "exit 3"])
    };
    let spawned = PlainSpawn.spawn(&launch).unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    let exit = loop {
        if let Some(exit) = spawned.process.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < until, "it never exited");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(exit.code, Some(3));
    assert_eq!(exit.to_string(), "exited with code 3");
    spawned.process.kill().unwrap();
}

#[test]
fn a_launchs_debug_never_shows_an_environment_value() {
    let launch = Launch::new("claude").env("ANTHROPIC_API_KEY", "sk-secret");
    let shown = format!("{launch:?}");
    assert!(
        shown.contains("ANTHROPIC_API_KEY") && !shown.contains("sk-secret"),
        "{shown}"
    );
}

#[test]
fn a_killed_process_reports_its_end_and_a_second_kill_does_nothing() {
    let launch = if cfg!(windows) {
        Launch::new("cmd.exe").args(["/d", "/c", "ping -n 30 127.0.0.1 >nul"])
    } else {
        Launch::new("/bin/sh").args(["-c", "sleep 30"])
    };
    let spawned = PlainSpawn.spawn(&launch).unwrap();
    spawned.process.kill().unwrap();
    assert!(
        spawned.process.try_wait().unwrap().is_some(),
        "a collected process has ended"
    );
    spawned.process.kill().unwrap();
}

#[test]
fn a_process_dropped_without_a_kill_ends_with_everything_it_started() {
    let launch = if cfg!(windows) {
        Launch::new("cmd.exe").args(["/d", "/c", "ping -n 30 127.0.0.1 >nul & echo done"])
    } else {
        Launch::new("/bin/sh").args(["-c", "sleep 30; echo done"])
    };
    let spawned = PlainSpawn.spawn(&launch).unwrap();
    let mut stdout = spawned.stdout;
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = sender.send(output);
    });
    drop(spawned.process);
    drop(spawned.stdin);
    let output = received
        .recv_timeout(Duration::from_secs(15))
        .expect("the process outlived its handle");
    assert!(!output.contains("done"), "{output}");
}

#[test]
fn any_wait_for_an_exit_is_taken() {
    struct Silent;
    impl coqui_acp_harness::Process for Silent {
        fn kill(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn try_wait(&self) -> std::io::Result<Option<coqui_acp_harness::Exit>> {
            Err(std::io::Error::other("no status"))
        }
    }
    assert_eq!(
        coqui_acp_harness::exit_text(&Silent, std::time::Duration::MAX),
        "closed its output"
    );
}
