//! A starter named with `set_starter` starts what `PlainSpawn` is asked to, in place of this process's own child. One test, since
//! the starter is the process's.
#![allow(clippy::std_instead_of_core, clippy::std_instead_of_alloc)]

use std::io::{Cursor, Read as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use coqui_acp_harness::{
    ErrorStream, Exit, Launch, PlainSpawn, Process, Spawn, Spawned, Starter, set_starter,
};

struct Fake;

impl Process for Fake {
    fn kill(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn try_wait(&self) -> std::io::Result<Option<Exit>> {
        Ok(Some(Exit::new(Some(3), None)))
    }
}

struct Counting(Arc<AtomicUsize>);

impl Starter for Counting {
    fn start(&self, launch: &Launch, errors: ErrorStream) -> std::io::Result<Spawned> {
        assert_eq!(errors, ErrorStream::Piped);
        self.0.fetch_add(1, Ordering::SeqCst);
        let said = format!("started {}", launch.program.to_string_lossy());
        Ok(Spawned::new(
            Box::new(std::io::sink()),
            Box::new(Cursor::new(said.into_bytes())),
            None,
            Box::new(Fake),
        ))
    }
}

#[test]
fn a_named_starter_starts_what_plain_spawn_is_asked_to() {
    let count = Arc::new(AtomicUsize::new(0));
    set_starter(Arc::new(Counting(Arc::clone(&count))));
    let spawned = PlainSpawn
        .spawn(&Launch::new("no-such-program-anywhere"))
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let mut said = String::new();
    let mut stdout = spawned.stdout;
    stdout.read_to_string(&mut said).unwrap();
    assert_eq!(said, "started no-such-program-anywhere");
    assert_eq!(
        spawned.process.try_wait().unwrap(),
        Some(Exit::new(Some(3), None))
    );
}
