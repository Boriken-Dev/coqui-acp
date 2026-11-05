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

struct Stuck(std::sync::mpsc::Receiver<()>);

impl std::io::Write for Stuck {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.recv();
        Err(std::io::Error::other(format!(
            "{} bytes refused",
            bytes.len()
        )))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_write_never_waits_for_a_harness_that_has_stopped_reading() {
    let (release, stuck) = std::sync::mpsc::channel();
    let writer = coqui_acp_harness::line_writer(Box::new(Stuck(stuck)), 2, "test").unwrap();
    let started = std::time::Instant::now();
    let results: Vec<bool> = (0..5)
        .map(|number| writer(&serde_json::json!({"n": number})).is_ok())
        .collect();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(results[0], "{results:?}");
    assert!(!results[4], "past the bound it is refused: {results:?}");
    drop(release);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while writer(&serde_json::json!({})).is_ok() {
        assert!(std::time::Instant::now() < until, "a broken pipe is told");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn lines_are_written_whole_and_in_order() {
    #[derive(Clone, Default)]
    struct Kept(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Kept {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let kept = Kept::default();
    let writer = coqui_acp_harness::line_writer(Box::new(kept.clone()), 64, "test").unwrap();
    for number in 0..32 {
        writer(&serde_json::json!({"n": number})).unwrap();
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while kept
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        < 32
    {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let text = String::from_utf8(kept.0.lock().unwrap().clone()).unwrap();
    for (number, line) in text.lines().enumerate() {
        assert_eq!(line, format!("{{\"n\":{number}}}"));
    }
}

struct Exits(
    std::sync::atomic::AtomicUsize,
    std::sync::atomic::AtomicUsize,
);

impl coqui_acp_harness::Process for Exits {
    fn kill(&self) -> std::io::Result<()> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn try_wait(&self) -> std::io::Result<Option<coqui_acp_harness::Exit>> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Ok(None)
        } else {
            Err(std::io::Error::other("its status cannot be read"))
        }
    }
}

struct ExitsWith(
    std::sync::atomic::AtomicUsize,
    std::sync::atomic::AtomicUsize,
);

impl coqui_acp_harness::Process for ExitsWith {
    fn kill(&self) -> std::io::Result<()> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn try_wait(&self) -> std::io::Result<Option<coqui_acp_harness::Exit>> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Ok(None)
        } else {
            // What a real exit reports, from a real process that has just ended.
            let plain = coqui_acp_harness::PlainSpawn;
            let spawned = coqui_acp_harness::Spawn::spawn(&plain, &quick()).unwrap();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Some(exit) = spawned.process.try_wait()? {
                    return Ok(Some(exit));
                }
                assert!(std::time::Instant::now() < until);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

fn quick() -> coqui_acp_harness::Launch {
    if cfg!(windows) {
        coqui_acp_harness::Launch::new("cmd").arg("/c").arg("exit")
    } else {
        coqui_acp_harness::Launch::new("sh").arg("-c").arg("exit 0")
    }
}

#[test]
fn an_exit_with_its_output_still_open_ends_the_session_once() {
    let process = std::sync::Arc::new(ExitsWith(
        std::sync::atomic::AtomicUsize::new(0),
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let ended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&ended);
    let done = coqui_acp_harness::ending_once();
    coqui_acp_harness::watch_exit(
        std::sync::Arc::clone(&process) as std::sync::Arc<dyn coqui_acp_harness::Process>,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(50),
        std::sync::Arc::clone(&done),
        move || {
            counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        },
    )
    .unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while ended.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(std::time::Instant::now() < until, "the end never came");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        process.1.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "what is left of it is ended"
    );
    assert!(done.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn a_status_that_cannot_be_read_stops_the_watch_and_ends_nothing() {
    let process = std::sync::Arc::new(Exits(
        std::sync::atomic::AtomicUsize::new(0),
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let done = coqui_acp_harness::ending_once();
    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saying = std::sync::Arc::clone(&ended);
    coqui_acp_harness::watch_exit(
        std::sync::Arc::clone(&process) as std::sync::Arc<dyn coqui_acp_harness::Process>,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(5),
        done,
        move || saying.store(true, std::sync::atomic::Ordering::SeqCst),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!ended.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(process.1.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn a_launchs_debug_shows_no_argument_value() {
    let launch = coqui_acp_harness::Launch::new("codex").arg("--config=api_key=\"hunter2\"");
    let shown = format!("{launch:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(shown.contains("codex"), "{shown}");
}
