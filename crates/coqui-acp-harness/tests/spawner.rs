//! `LocalSpawner`, called as the kind's object: pipes, exits, and a tree that ends at a kill and with
//! its host; `ThroughSpawner` over it.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::Object;
use coqui_acp_api::{ErrorOutput, ProcessSpawner, READ_TIMED_OUT, SpawnSpec, TerminalSize};
use coqui_acp_harness::{Launch, LocalSpawner, Spawn, ThroughSpawner};

#[path = "../examples/spawner_host.rs"]
#[allow(dead_code, reason = "only its tree is used here")]
mod host;

fn helper() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    let profile = tests
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary is in target/<profile>/deps");
    profile
        .join("examples")
        .join(format!("spawner_host{}", std::env::consts::EXE_SUFFIX))
}

fn spawner() -> Object<dyn ProcessSpawner> {
    LocalSpawner::new().into_object()
}

fn echo(output: ErrorOutput) -> SpawnSpec {
    SpawnSpec::new(helper().to_string_lossy())
        .arg("echo")
        .error_output(output)
}

/// Everything a stream gives until it ends.
fn drain(read: impl Fn(&mut [u8]) -> i64) -> String {
    let mut text = Vec::new();
    let mut chunk = [0u8; 64];
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let n = read(&mut chunk);
        if n == 0 {
            return String::from_utf8_lossy(&text).into_owned();
        }
        assert!(Instant::now() < until, "the stream never ended");
        if n > 0 {
            text.extend_from_slice(&chunk[..usize::try_from(n).expect("a count")]);
        }
    }
}

#[test]
fn a_process_takes_input_and_gives_both_streams_and_its_exit() {
    let process = spawner()
        .spawn(echo(ErrorOutput::Separate))
        .expect("spawned");
    process.write(b"hello\n").expect("written");
    process.close_input().expect("closed");
    assert!(
        process.write(b"more").is_err(),
        "a write after closing went through"
    );
    assert_eq!(drain(|b| process.read(b, 1000).expect("read")), "hello\n");
    assert_eq!(
        drain(|b| process.read_error(b, 1000).expect("read")).trim(),
        "err"
    );
    let exit = process.wait(30_000).expect("waited");
    assert!(!exit.running);
    assert_eq!(exit.code, Some(3));
    assert!(process.link().expect("answered").up);
}

#[test]
fn standard_error_goes_where_the_spec_says() {
    let merged = spawner().spawn(echo(ErrorOutput::Merged)).expect("spawned");
    merged.write(b"hello\n").expect("written");
    merged.close_input().expect("closed");
    let both = drain(|b| merged.read(b, 1000).expect("read"));
    assert!(both.contains("err") && both.contains("hello"), "{both:?}");
    assert_eq!(merged.read_error(&mut [0; 8], 0).expect("read"), 0);

    let quiet = spawner()
        .spawn(echo(ErrorOutput::Discarded))
        .expect("spawned");
    quiet.close_input().expect("closed");
    assert_eq!(drain(|b| quiet.read(b, 1000).expect("read")), "");
    assert_eq!(quiet.read_error(&mut [0; 8], 0).expect("read"), 0);
}

#[test]
fn a_read_with_nothing_to_give_times_out() {
    let process = spawner()
        .spawn(echo(ErrorOutput::Separate))
        .expect("spawned");
    assert_eq!(process.read(&mut [0; 8], 50).expect("read"), READ_TIMED_OUT);
    process.kill().expect("killed");
    process.kill().expect("a second kill is no error");
    assert!(!process.wait(10_000).expect("waited").running);
}

#[test]
fn the_spawner_says_what_it_gives_and_refuses_the_rest() {
    let info = spawner().describe().expect("described");
    assert_eq!(info.os, std::env::consts::OS);
    assert!(info.local && info.separate_error && !info.terminal);
    assert!(info.ends_with_host);
    let Err(refused) =
        spawner().spawn(echo(ErrorOutput::Separate).terminal(TerminalSize::new(24, 80)))
    else {
        panic!("a terminal was given");
    };
    assert_eq!(refused.status, Status::GUATIAO_ERR_UNSUPPORTED);
    let Err(missing) = spawner().spawn(SpawnSpec::new("no-such-program-coqui-acp")) else {
        panic!("a missing program started");
    };
    assert_eq!(missing.status, Status::GUATIAO_ERR_NOT_FOUND);
}

#[test]
fn a_launch_runs_through_a_hosts_spawner() {
    let spawn = ThroughSpawner::new(spawner());
    let launch = Launch::new(helper()).arg("echo");
    let mut spawned = spawn.spawn(&launch).expect("spawned");
    spawned.stdin.write_all(b"through\n").expect("written");
    drop(spawned.stdin);
    let mut text = String::new();
    spawned.stdout.read_to_string(&mut text).expect("read");
    assert_eq!(text, "through\n");
    let until = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        if let Some(exit) = spawned.process.try_wait().expect("asked") {
            break exit;
        }
        assert!(Instant::now() < until, "it never exited");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(exit.code, Some(3));
}

/// Whether process `pid` still runs (a zombie does not).
fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    } else {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rfind(')')
                .and_then(|end| stat[end + 1..].split_whitespace().next())
                .is_some_and(|state| state != "Z"),
            Err(_) if std::path::Path::new("/proc/self").exists() => false,
            Err(_) => Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .is_ok_and(|s| s.success()),
        }
    }
}

fn gone_within(pid: u32, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Reads `pid N` and `escaped N` lines up to `ready`.
fn pids(lines: impl Iterator<Item = String>) -> (Vec<u32>, Vec<u32>) {
    let (mut grouped, mut escaped) = (Vec::new(), Vec::new());
    for line in lines {
        let line = line.trim().to_owned();
        if line == "ready" {
            break;
        }
        if let Some(pid) = line.strip_prefix("pid ") {
            grouped.push(pid.parse().expect("a pid"));
        } else if let Some(pid) = line.strip_prefix("escaped ") {
            escaped.push(pid.parse().expect("a pid"));
        }
    }
    assert!(!grouped.is_empty(), "the tree said no pid");
    (grouped, escaped)
}

fn end(pids: &[u32]) {
    for pid in pids {
        let _ = if cfg!(windows) {
            Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output()
        } else {
            Command::new("kill").args(["-9", &pid.to_string()]).output()
        };
    }
}

#[test]
fn a_kill_ends_the_whole_tree() {
    let process = spawner().spawn(host::tree()).expect("the tree starts");
    let mut said = Vec::new();
    let mut chunk = [0u8; 256];
    while !String::from_utf8_lossy(&said).contains("ready") {
        let n = process.read(&mut chunk, 30_000).expect("read");
        assert!(n > 0, "the tree ended before it was ready");
        said.extend_from_slice(&chunk[..usize::try_from(n).expect("a count")]);
    }
    let (grouped, escaped) = pids(said.as_slice().lines().map_while(Result::ok));
    for pid in grouped.iter().chain(&escaped) {
        assert!(alive(*pid), "{pid} is not running before the kill");
    }
    process.kill().expect("killed");
    for pid in &grouped {
        assert!(
            gone_within(*pid, Duration::from_secs(10)),
            "{pid} outlived the kill"
        );
    }
    // Linux finds the processes that left the group; elsewhere they escape, as the spawner says.
    if cfg!(any(target_os = "linux", target_os = "android")) {
        for pid in &escaped {
            assert!(
                gone_within(*pid, Duration::from_secs(10)),
                "{pid}, in a session of its own, outlived the kill"
            );
        }
    }
    end(&escaped);
}

#[test]
fn the_tree_ends_with_its_host() {
    let mut host = Command::new(helper())
        .arg("tree")
        .stdout(Stdio::piped())
        .spawn()
        .expect("the host starts");
    let out = BufReader::new(host.stdout.take().expect("piped"));
    let (grouped, escaped) = pids(out.lines().map_while(Result::ok));
    for pid in &grouped {
        assert!(alive(*pid), "{pid} is not running before the host ends");
    }
    host.kill().expect("the host is killed");
    let _ = host.wait();
    for pid in &grouped {
        assert!(
            gone_within(*pid, Duration::from_secs(10)),
            "{pid} outlived its host"
        );
    }
    end(&escaped);
}
