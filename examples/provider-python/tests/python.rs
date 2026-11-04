//! The provider written once, in Python: `echo/acp_echo.py`, loaded through Guatiao's source of Python
//! libraries by a host serving it over standard input and output, with the real `coqui_acp::Client` at the
//! other end of the pipes.
//!
//! The source is a `cdylib` built against a Python, so these tests build it, from Cargo's own checkout of
//! Guatiao at the workspace's revision. Where build.rs found no Python the one test left is ignored, and a run
//! counts it as not run.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

#[cfg(not(python_ready))]
#[test]
#[ignore = "NOT RUN: build.rs found no Python to embed, and said why in a warning"]
fn the_python_provider_needs_a_python() {
    panic!("build.rs found no Python to embed: set COQUI_ACP_TEST_PYTHON to one");
}

#[cfg(python_ready)]
mod with_python {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use coqui_acp::{Client, Event, Exchange, RpcFailure, Session, lines};
    use serde_json::{Value, json};

    const WAIT: Duration = Duration::from_secs(60);

    fn guatiao() -> PathBuf {
        PathBuf::from(env!("COQUI_ACP_TEST_GUATIAO"))
    }

    fn python() -> &'static str {
        env!("COQUI_ACP_TEST_PYTHON_EXE")
    }

    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn kinds_crate() -> PathBuf {
        manifest_dir().join("../../crates/coqui-acp-provider")
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("a directory under the test's own");
        for entry in std::fs::read_dir(from).expect("a readable checkout") {
            let entry = entry.expect("a readable entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("a file type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("a file copies");
            }
        }
    }

    /// Builds the source once per test binary. A copy, since a build writes `Cargo.lock` beside the manifest
    /// and Cargo's checkout of a dependency is not this test's to write in. The copy is named after the
    /// checkout's directory, which Cargo names by revision, so another pin builds from a copy of its own.
    fn source() -> &'static Path {
        static SOURCE: OnceLock<PathBuf> = OnceLock::new();
        SOURCE.get_or_init(|| {
            let checkout = guatiao();
            let revision = checkout
                .file_name()
                .expect("Cargo's checkout is a directory of its own");
            let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
                .join("guatiao-source-py")
                .join(revision);
            let copy = scratch.join("guatiao");
            if !copy.join("Cargo.toml").is_file() {
                copy_tree(&guatiao().join("crates"), &copy.join("crates"));
                std::fs::copy(guatiao().join("Cargo.toml"), copy.join("Cargo.toml"))
                    .expect("the root manifest copies");
            }
            let manifest = copy.join("crates/guatiao-source-py/Cargo.toml");
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let built = Command::new(cargo)
                .arg("build")
                .arg("--manifest-path")
                .arg(&manifest)
                .arg("--target-dir")
                .arg(scratch.join("target"))
                .env("PYO3_PYTHON", python())
                .output()
                .expect("cargo runs");
            assert!(
                built.status.success(),
                "the Python source did not build:\n{}",
                String::from_utf8_lossy(&built.stderr)
            );
            scratch.join("target/debug").join(format!(
                "{}guatiao_source_py{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ))
        })
    }

    /// A host serving the Python provider. Dropping it kills what is left.
    struct Host(Child);

    impl Drop for Host {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    impl Host {
        fn start() -> (Self, lines::IoLines) {
            let mut command = Command::new(env!("CARGO_BIN_EXE_provider-python-host"));
            command
                .arg(source())
                .arg("acp_echo_echo")
                .arg(manifest_dir().join("echo"))
                .arg(guatiao().join("bindings/python/guatiao/src"))
                .arg(kinds_crate().join("python"))
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if cfg!(windows) {
                // Where the source finds python3.dll, and through it the interpreter.
                let path = std::env::var_os("PATH").unwrap_or_default();
                let mut dirs = vec![PathBuf::from(env!("COQUI_ACP_TEST_PYTHON_PREFIX"))];
                dirs.extend(std::env::split_paths(&path));
                command.env("PATH", std::env::join_paths(dirs).expect("paths join"));
            }
            let mut child = command.spawn().expect("the host starts");
            let stdout = child.stdout.take().expect("piped");
            let stdin = child.stdin.take().expect("piped");
            let (transport, _diagnostics) =
                lines::over_io(stdout, stdin, 20, lines::DEFAULT_MAX_LINE).expect("threads start");
            (Self(child), transport)
        }

        /// The host's exit code, within the time a test waits.
        fn wait(&mut self) -> Option<i32> {
            let deadline = Instant::now() + WAIT;
            loop {
                if let Some(status) = self.0.try_wait().expect("the host can be asked") {
                    return status.code();
                }
                assert!(Instant::now() < deadline, "the host did not exit");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn cwd() -> String {
        std::env::temp_dir().to_string_lossy().into_owned()
    }

    /// The events up to and including whatever ends the turn.
    fn turn(session: &Session) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let event = session
                .next_event(WAIT)
                .unwrap_or_else(|| panic!("the turn did not end: {events:?}"));
            let ended = matches!(
                event,
                Event::Completed { .. } | Event::Failed(_) | Event::Cancelled
            );
            events.push(event);
            if ended {
                return events;
            }
        }
    }

    fn text(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_python_provider_answers_initialize() {
        let (_host, transport) = Host::start();
        let answers = Exchange::new()
            .initialize_timeout(Some(WAIT))
            .request_timeout(Some(WAIT))
            .run(transport, vec![("session/load".to_owned(), json!({}))])
            .expect("initialize is answered, through a provider written in Python");
        assert!(
            matches!(&answers[0], Err(RpcFailure { code: -32601, .. })),
            "{answers:?}"
        );
    }

    #[test]
    fn a_python_provider_answers_a_prompt_and_the_next_and_the_host_ends_with_the_client() {
        let (mut host, transport) = Host::start();
        let session = Client::builder(cwd())
            .prompt("hello")
            .connect(transport)
            .expect("a session opens");
        let events = turn(&session);
        assert_eq!(text(&events), "echo: hello");
        assert!(
            matches!(events.last(), Some(Event::Completed { .. })),
            "{events:?}"
        );
        session.send("again").unwrap();
        assert_eq!(text(&turn(&session)), "echo: again");
        session.close();
        drop(session);
        assert_eq!(host.wait(), Some(0), "the host ends cleanly");
    }

    #[test]
    fn what_a_client_asks_for_reaches_python_whole() {
        let (_host, transport) = Host::start();
        let session = Client::builder(cwd())
            .meta(
                coqui_acp_provider::LAUNCH_META_KEY,
                json!({
                    "context": ["first"],
                    "model": "m",
                    "effort": "high",
                    "meta": {"nested": [1, true, null, "x"]},
                }),
            )
            .prompt("show")
            .connect(transport)
            .expect("a session opens");
        let events = turn(&session);
        let seen: Value = serde_json::from_str(&text(&events)).expect("the provider wrote JSON");
        assert_eq!(seen["prompt"], "show");
        assert_eq!(seen["working_directory"], json!(cwd()));
        assert_eq!(seen["context"], json!(["first"]));
        assert_eq!(seen["model"], "m");
        assert_eq!(seen["effort"], "high");
        assert_eq!(seen["meta"], json!({"nested": [1, true, null, "x"]}));
        session.close();
    }

    #[test]
    fn a_python_provider_uses_tools_and_is_cancelled_and_fails_in_its_own_words() {
        let (_host, transport) = Host::start();
        let session = Client::builder(cwd())
            .prompt("tool")
            .connect(transport)
            .expect("a session opens");
        let events = turn(&session);
        assert!(
            events.iter().any(|event| matches!(
                event,
                Event::Tool { title, name, kind, .. }
                    if title == "Read a file" && name.as_deref() == Some("Read") && kind == "other"
            )),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Cost { .. })),
            "{events:?}"
        );

        session.send("wait").unwrap();
        // Time for the message to reach the provider, whose answer would otherwise follow the cancel.
        std::thread::sleep(Duration::from_millis(300));
        session.cancel().unwrap();
        let events = turn(&session);
        assert_eq!(text(&events), "waiting");
        assert!(
            !matches!(
                events.last(),
                Some(Event::Completed { .. } | Event::Failed(_))
            ),
            "{events:?}"
        );

        session.send("raise").unwrap();
        let events = turn(&session);
        assert!(
            matches!(events.last(), Some(Event::Failed(why)) if why.contains("the message was refused")),
            "{events:?}"
        );

        session.send("fail").unwrap();
        let events = turn(&session);
        assert!(
            matches!(events.last(), Some(Event::Failed(why)) if why.contains("the harness exited with 3")),
            "{events:?}"
        );
        session.close();
    }

    #[test]
    fn a_python_provider_resumes_what_it_issued_and_refuses_the_rest() {
        let (_host, transport) = Host::start();
        let resumed = Client::builder(cwd())
            .resume("python-echo-9")
            .prompt("back")
            .connect(transport)
            .expect("the provider knows its own identity");
        assert_eq!(text(&turn(&resumed)), "echo: back");
        resumed.close();

        let (_host, transport) = Host::start();
        let Err(refused) = Client::builder(cwd())
            .resume("someone-elses")
            .connect(transport)
        else {
            panic!("the provider refuses it");
        };
        assert!(refused.to_string().contains("no such session"), "{refused}");
    }

    #[test]
    fn the_committed_python_kinds_are_what_the_description_writes() {
        let checked = Command::new(python())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONPATH", guatiao().join("bindings/python/guatiao/src"))
            .arg(kinds_crate().join("python/write_kinds.py"))
            .arg("--check")
            .output()
            .expect("Python runs");
        assert!(
            checked.status.success(),
            "regenerate them: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
}
