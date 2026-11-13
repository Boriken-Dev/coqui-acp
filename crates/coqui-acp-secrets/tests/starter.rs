//! A starter named with `set_starter` starts what `ProcessRunner` runs, in place of this process's own child. One test, since the
//! starter is the process's.
#![cfg(unix)]
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coqui_acp_secrets::{
    CommandLine, CommandSpec, Launch, Process, ProcessRunner, Runner, Started, Starter, set_starter,
};

#[derive(Default)]
struct Seen {
    calls: Vec<&'static str>,
    program: String,
    args: Vec<String>,
    names: Vec<String>,
    cwd: String,
}

struct Fake(Arc<Mutex<Seen>>);

impl Process for Fake {
    fn try_wait(&mut self) -> std::io::Result<Option<Option<i32>>> {
        Ok(Some(Some(7)))
    }

    fn end_group_after_exit(&mut self) {
        self.0.lock().unwrap().calls.push("end_group_after_exit");
    }

    fn end(&mut self) {
        self.0.lock().unwrap().calls.push("end");
    }
}

struct Recording(Arc<Mutex<Seen>>);

impl Starter for Recording {
    fn start(&self, launch: &Launch) -> std::io::Result<Started> {
        let mut seen = self.0.lock().unwrap();
        seen.program = launch.program.to_string_lossy().into_owned();
        seen.args = launch
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        seen.names = launch
            .env
            .iter()
            .map(|(n, _)| n.to_string_lossy().into_owned())
            .collect();
        seen.cwd = launch.cwd.to_string_lossy().into_owned();
        assert!(
            !format!("{launch:?}").contains("s3cret"),
            "the secret is not in its Debug"
        );
        Ok(Started {
            process: Box::new(Fake(Arc::clone(&self.0))),
            stdout: Box::new(Cursor::new(b"out".to_vec())),
            stderr: Box::new(Cursor::new(Vec::new())),
        })
    }
}

#[test]
fn a_named_starter_starts_what_the_runner_runs_and_ends_what_is_left_after_it() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    set_starter(Arc::new(Recording(Arc::clone(&seen))));
    let command = CommandLine::Argv(vec!["tool".into(), "a".into()]);
    let output = ProcessRunner
        .run(&CommandSpec {
            command: &command,
            program: Path::new("/usr/bin/tool"),
            path: None,
            cwd: Path::new("/"),
            secret: Some(("TOKEN", b"s3cret")),
            timeout: Duration::from_secs(5),
            max_output: 100,
            inherit_env: &[],
        })
        .expect("it runs");
    assert_eq!(output.status, Some(7));
    assert_eq!(output.stdout.as_slice(), b"out");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.program, "/usr/bin/tool");
    assert_eq!(seen.args, ["a"]);
    assert_eq!(seen.cwd, "/");
    assert!(seen.names.contains(&"TOKEN".to_owned()));
    assert_eq!(seen.calls, ["end_group_after_exit"]);
}
