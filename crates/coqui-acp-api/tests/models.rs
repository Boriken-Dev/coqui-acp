//! The rules the models carry: requirements read with their values, environments that never print,
//! and working roots compared on a real file system.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::{Path, PathBuf};

use coqui_acp_api::guatiao::{FromValue, List, Secret, Status, ToValue, Value};
use coqui_acp_api::{
    ANSWER_ONLY, DENIED_PATHS, Environment, FILES_UNDER, NETWORK, NO_COMMANDS, Narrowing,
    ONLY_SKILLS, PathRefusal, READ_ONLY, Requirement, RequirementError, SANDBOXED_SCRIPTS,
    SessionQuery, SpawnSpec, StartRequest, WorkingRoots,
};

fn list(items: &[&str]) -> Value {
    let mut list = List::new();
    for item in items {
        list.push(*item);
    }
    Value::from(list)
}

fn absolute(name: &str) -> String {
    if cfg!(windows) {
        format!(r"C:\{name}")
    } else {
        format!("/{name}")
    }
}

#[test]
fn a_requirement_is_read_with_its_value() {
    assert_eq!(
        Requirement::new(READ_ONLY).narrowing(),
        Ok(Narrowing::ReadOnly)
    );
    assert_eq!(
        Requirement::new(ANSWER_ONLY).narrowing(),
        Ok(Narrowing::AnswerOnly)
    );
    let work = absolute("work");
    assert_eq!(
        Requirement::with_value(FILES_UNDER, list(&[&work])).narrowing(),
        Ok(Narrowing::FilesUnder(vec![work.clone()]))
    );
    assert_eq!(
        Requirement::with_value(FILES_UNDER, list(&[])).narrowing(),
        Ok(Narrowing::FilesUnder(Vec::new())),
        "an empty list allows no file to be written"
    );
    assert_eq!(
        Requirement::with_value(NETWORK, list(&["api.example.com:443", "*.example.org"]))
            .narrowing(),
        Ok(Narrowing::Network(vec![
            "api.example.com:443".into(),
            "*.example.org".into()
        ]))
    );
    assert_eq!(
        Requirement::with_value(SANDBOXED_SCRIPTS, list(&["python3"])).narrowing(),
        Ok(Narrowing::SandboxedScripts(vec!["python3".into()]))
    );
    assert_eq!(
        Requirement::from(Narrowing::OnlySkills(vec!["pdf".into()])),
        Requirement::with_value(ONLY_SKILLS, list(&["pdf"]))
    );
}

#[test]
fn a_known_name_with_a_value_it_does_not_take_is_a_bad_value() {
    let bad = |requirement: Requirement| {
        let error = requirement.narrowing().expect_err("refused");
        assert_eq!(error.status(), Status::GUATIAO_ERR_BAD_VALUE, "{error}");
    };
    bad(Requirement::new(FILES_UNDER));
    bad(Requirement::with_value(FILES_UNDER, "/work"));
    bad(Requirement::with_value(FILES_UNDER, list(&["relative"])));
    bad(Requirement::with_value(
        DENIED_PATHS,
        list(&["/work/../etc"]),
    ));
    bad(Requirement::with_value(
        NETWORK,
        list(&["https://example.com"]),
    ));
    bad(Requirement::with_value(NETWORK, list(&[""])));
    bad(Requirement::with_value(READ_ONLY, true));
    bad(Requirement::with_value(SANDBOXED_SCRIPTS, list(&[""])));
    assert_eq!(
        Requirement::with_value(READ_ONLY, Value::null()).narrowing(),
        Ok(Narrowing::ReadOnly),
        "null is no value"
    );
}

#[test]
fn an_unknown_name_is_unsupported_and_a_repeated_one_a_bad_value() {
    let unknown = Requirement::new("max_cost")
        .narrowing()
        .expect_err("refused");
    assert_eq!(unknown.status(), Status::GUATIAO_ERR_UNSUPPORTED);
    assert_eq!(
        Narrowing::read_all(&[Requirement::new(NO_COMMANDS), Requirement::new(NO_COMMANDS)]),
        Err(RequirementError::Repeated {
            name: NO_COMMANDS.into()
        })
    );
    assert_eq!(
        Narrowing::read_all(&[Requirement::new(READ_ONLY), Requirement::new(NO_COMMANDS)]),
        Ok(vec![Narrowing::ReadOnly, Narrowing::NoCommands])
    );
}

#[test]
fn an_environment_never_prints_a_value() {
    let environment: Environment = [("API_KEY", "sk-very-secret")].into_iter().collect();
    let request = StartRequest::builder()
        .environment(environment.clone())
        .requirement(Requirement::new(READ_ONLY))
        .build();
    let query = SessionQuery::new("0f8fad5b-d9cb-469f-a165-70867728950e", "/")
        .environment(environment.clone());
    let spec = SpawnSpec::new("claude").environment(environment.clone());
    let printed = format!(
        "{environment:?} {request:?} {query:?} {spec:?} {:?}",
        Secret::new(environment.clone())
    );
    assert!(!printed.contains("sk-very-secret"), "{printed}");
    assert!(
        printed.contains("API_KEY"),
        "the names still show: {printed}"
    );
    // It still crosses whole: a secret is marked by its schema, not withheld from the provider.
    let crossed = StartRequest::from_value(&request.to_value().expect("written")).expect("read");
    assert_eq!(
        crossed.environment.expose().get("API_KEY"),
        Some("sk-very-secret")
    );
}

/// A directory of its own for one test, removed when it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let temp = std::fs::canonicalize(std::env::temp_dir()).expect("a temporary directory");
        let dir = temp.join(format!("acp-api-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("made");
        Self(dir)
    }

    fn path(&self, rest: &str) -> String {
        let path = self.0.join(rest);
        let text = path.to_string_lossy().into_owned();
        text.strip_prefix(r"\\?\")
            .map_or(text.clone(), str::to_owned)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("a symbolic link is made");
}

/// A junction: Windows lets anyone make one, unlike a symbolic link.
#[cfg(windows)]
fn link(target: &Path, link: &Path) {
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("cmd runs");
    assert!(made.status.success(), "a junction is made: {made:?}");
}

#[test]
fn a_working_root_holds_on_a_real_file_system() {
    let scratch = Scratch::new("roots");
    std::fs::create_dir_all(scratch.0.join("root/inside")).expect("made");
    std::fs::create_dir_all(scratch.0.join("outside")).expect("made");
    link(&scratch.0.join("outside"), &scratch.0.join("root/link"));
    let roots = WorkingRoots::new(vec![scratch.path("root")]);
    assert_eq!(roots.admits(&scratch.path("root")), Ok(()));
    assert_eq!(roots.admits(&scratch.path("root/inside")), Ok(()));
    assert_eq!(roots.admits(&scratch.path("root/not-yet/made")), Ok(()));
    assert_eq!(
        roots.admits(&format!("{}-beside", scratch.path("root"))),
        Err(PathRefusal::Outside)
    );
    let separator = std::path::MAIN_SEPARATOR;
    assert_eq!(
        roots.admits(&format!(
            "{}{separator}..{separator}outside",
            scratch.path("root")
        )),
        Err(PathRefusal::NotPlainAbsolute)
    );
    assert_eq!(
        roots.admits(&scratch.path("root/link")),
        Err(PathRefusal::LinkLeadsOutside)
    );
    assert_eq!(
        roots.admits(&scratch.path("root/link/below")),
        Err(PathRefusal::LinkLeadsOutside)
    );
    assert_eq!(
        roots
            .clone()
            .follow_links(false)
            .admits(&scratch.path("root/link")),
        Ok(()),
        "as written, the link is inside"
    );
    if cfg!(windows) {
        assert_eq!(roots.admits(&scratch.path("root").to_uppercase()), Ok(()));
        assert_eq!(
            roots.admits(&scratch.path("root").replace('\\', "/")),
            Ok(())
        );
    } else {
        assert_eq!(
            roots.admits(&scratch.path("root").to_uppercase()),
            Err(PathRefusal::Outside)
        );
    }
    std::fs::remove_dir(scratch.0.join("root/link"))
        .or_else(|_| std::fs::remove_file(scratch.0.join("root/link")))
        .expect("the link goes");
}

#[test]
fn a_root_that_is_a_link_admits_where_it_leads() {
    let scratch = Scratch::new("linked-root");
    std::fs::create_dir_all(scratch.0.join("real/inside")).expect("made");
    link(&scratch.0.join("real"), &scratch.0.join("alias"));
    let roots = WorkingRoots::new(vec![scratch.path("alias")]);
    assert_eq!(roots.admits(&scratch.path("alias/inside")), Ok(()));
    assert_eq!(
        roots.admits(&scratch.path("real/inside")),
        Err(PathRefusal::Outside),
        "as written it is outside"
    );
    std::fs::remove_dir(scratch.0.join("alias"))
        .or_else(|_| std::fs::remove_file(scratch.0.join("alias")))
        .expect("the link goes");
}
