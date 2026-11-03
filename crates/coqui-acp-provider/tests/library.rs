//! The provider written once, in Rust: a real library the host loads, served by the facade, used by the
//! real client. `examples/echo_provider.rs` is that library, and `cargo test` builds it.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;
use std::time::Duration;

use coqui_acp::{Client, Event, Exchange};
use coqui_acp_provider::guatiao::library::Registry;
use coqui_acp_provider::{AcpFacade, AgentProvider};

const WAIT: Duration = Duration::from_secs(20);

fn library() -> PathBuf {
    let tests = std::env::current_exe().expect("the test knows where it is");
    let profile = tests
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary is in target/<profile>/deps");
    profile.join("examples").join(format!(
        "{}echo_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

/// The registry stays alive with the test: what it offered points into the loaded file.
fn loaded() -> (Registry, AcpFacade) {
    let mut registry = Registry::new("coqui-acp-provider-tests", "1.0");
    registry.load_file(&library()).unwrap_or_else(|error| {
        panic!("the library loads, once `cargo test` or `cargo build --examples` built it: {error}")
    });
    let offer = registry
        .offer::<dyn AgentProvider>("echo_provider")
        .expect("the library offers it")
        .expect("its table is this crate's kind");
    let provider = offer.shared().expect("the provider is its one instance");
    (registry, AcpFacade::new(provider))
}

fn cwd() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

#[test]
fn a_provider_in_a_library_answers_initialize() {
    let (_registry, facade) = loaded();
    let (agent, client) = coqui_acp::duplex();
    facade.serve(agent).expect("a thread starts");
    let answers = Exchange::new()
        .initialize_timeout(Some(WAIT))
        .request_timeout(Some(WAIT))
        .run(
            client,
            vec![("session/load".to_owned(), serde_json::json!({}))],
        )
        .expect("initialize is answered");
    assert_eq!(answers.len(), 1);
}

#[test]
fn a_provider_in_a_library_answers_prompts_and_resumes() {
    let (_registry, facade) = loaded();
    let (agent, client) = coqui_acp::duplex();
    let served = facade.serve(agent).expect("a thread starts");
    let session = Client::builder(cwd())
        .prompt("hello")
        .connect(client)
        .expect("a session opens");
    assert_eq!(next_text(&session), "echo: hello");
    session.send("again").unwrap();
    assert_eq!(next_text(&session), "echo: again");
    session.close();
    served.join().expect("no panic").expect("nothing left over");

    let (agent, client) = coqui_acp::duplex();
    facade.serve(agent).expect("a thread starts");
    let resumed = Client::builder(cwd())
        .resume("echo-1")
        .prompt("back")
        .connect(client)
        .expect("the provider knows its own identity");
    assert_eq!(next_text(&resumed), "echo: back");
    resumed.close();

    let (agent, client) = coqui_acp::duplex();
    facade.serve(agent).expect("a thread starts");
    let Err(refused) = Client::builder(cwd())
        .resume("someone-elses")
        .connect(client)
    else {
        panic!("the provider refuses it");
    };
    assert!(refused.to_string().contains("no such session"), "{refused}");
}

/// The text of the next turn, which must complete.
fn next_text(session: &coqui_acp::Session) -> String {
    let mut text = String::new();
    loop {
        match session.next_event(WAIT).expect("the turn ends") {
            Event::Text(part) => text.push_str(&part),
            Event::Completed { .. } => return text,
            Event::Failed(why) => panic!("the turn failed: {why}"),
            _ => {}
        }
    }
}
