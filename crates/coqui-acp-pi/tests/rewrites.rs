//! A path as the model writes it is not the path Pi uses: Pi flattens Unicode spaces, drops a leading `@`, expands `~` and
//! reads a `file:` URL. The gate decides on the path Pi will use, and asks about every path that looks rewritten.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core
)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use coqui_acp_pi::Parser;
use coqui_acp_pi::harness::Mode;
use coqui_acp_pi::harness::event::{Event, Outcome, PermissionRequest};
use coqui_acp_pi::mode::Policy;
use coqui_acp_pi::parse::ALLOW_TOOL;
use serde_json::{Value, json};
use support::work;

type Written = Arc<Mutex<Vec<Value>>>;

fn home() -> PathBuf {
    std::env::temp_dir().join("coqui-acp-pi-fixture-home")
}

fn parser_in(mode: Mode) -> (Parser, Written) {
    let written: Written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    )
    .with_policy(Arc::new(Mutex::new(Policy::new(mode, false))))
    .with_roots(&work(), Vec::new())
    .with_protected(vec![home().join(".pi").join("agent")])
    .with_home(Some(home()));
    (parser, written)
}

fn call(tool: &str, input: &Value, dialog: &str) -> Vec<Value> {
    vec![
        json!({"type": "tool_execution_start", "toolCallId": dialog, "toolName": tool, "args": input}),
        json!({"type": "extension_ui_request", "id": dialog, "method": "select",
               "title": format!("coqui-acp-pi-gate {}", json!({"id": dialog, "tool": tool, "input": input})),
               "options": ["allow", "deny", "plan", "protected"]}),
    ]
}

fn run(parser: &mut Parser, lines: &[Value]) -> Vec<PermissionRequest> {
    lines
        .iter()
        .flat_map(|line| parser.line(line))
        .filter_map(|event| match event {
            Event::Permission(request) => Some(request),
            _ => None,
        })
        .collect()
}

fn answer(written: &Written) -> Option<String> {
    written
        .lock()
        .unwrap()
        .iter()
        .rfind(|line| line["type"] == "extension_ui_response")
        .map(|line| line["value"].as_str().unwrap_or("cancelled").to_string())
}

fn slashed(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `file://` and the path, as a URL.
fn url_of(path: &std::path::Path) -> String {
    let text = slashed(path);
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[test]
fn a_credential_file_is_refused_however_the_path_is_spelled() {
    let ssh = home().join(".ssh").join("id_rsa");
    let spellings: Vec<String> = vec![
        "~/.ssh/id_rsa".into(),
        "@~/.ssh/id_rsa".into(),
        format!("@{}", ssh.display()),
        url_of(&ssh),
        format!("@{}", url_of(&ssh)),
        "~/.ssh/id_rsa\u{a0}".to_string(),
        "~/.git-credentials".into(),
        "~/.config/gh/hosts.yml".into(),
        "~/.azure/accessTokens.json".into(),
        "~/AppData/Roaming/gcloud/credentials.db".into(),
        "~/.cargo/credentials.toml".into(),
        "@~/.pi/agent/models.json".into(),
        "~/.pi/agent/auth.json".into(),
    ];
    for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
        for text in &spellings {
            for tool in ["read", "write"] {
                let (mut parser, written) = parser_in(mode);
                parser.prompting();
                let asked = run(
                    &mut parser,
                    &call(tool, &json!({"path": text, "content": "x"}), "c"),
                );
                assert!(
                    asked.is_empty(),
                    "{mode:?} {tool} {text:?} was asked, not refused"
                );
                assert_eq!(
                    answer(&written).as_deref(),
                    Some("protected"),
                    "{mode:?} {tool} {text:?}"
                );
            }
        }
    }
}

#[test]
fn a_path_pi_rewrites_is_asked_about_even_inside_the_project_and_is_never_remembered() {
    let inside = work().join("notes.txt");
    for text in [
        "@notes.txt".to_string(),
        url_of(&inside),
        format!("@{}", inside.display()),
        "~".to_string(),
    ] {
        for tool in ["read", "write"] {
            let (mut parser, _) = parser_in(Mode::AcceptEdits);
            parser.prompting();
            let mut asked = run(
                &mut parser,
                &call(tool, &json!({"path": text, "content": "x"}), "r"),
            );
            assert_eq!(asked.len(), 1, "{tool} {text:?}");
            let options: Vec<_> = asked[0].options.iter().map(|o| o.id.as_str()).collect();
            assert!(
                !options.contains(&ALLOW_TOOL),
                "{tool} {text:?} offers to remember a rewritten path"
            );
            drop(asked.remove(0));
        }
    }
    // The plain spelling of the same file is not asked about in Accept edits.
    let (mut parser, written) = parser_in(Mode::AcceptEdits);
    parser.prompting();
    assert!(
        run(
            &mut parser,
            &call("write", &json!({"path": "notes.txt", "content": "x"}), "p")
        )
        .is_empty()
    );
    assert_eq!(answer(&written).as_deref(), Some("allow"));
}

#[test]
fn accept_edits_asks_before_writing_where_the_home_directory_runs_things() {
    let startup = "~/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/x.bat";
    for text in [
        "~/.gitconfig",
        "@~/.bashrc",
        "~/.profile",
        startup,
        "~/other.txt",
    ] {
        let (mut parser, _) = parser_in(Mode::AcceptEdits);
        parser.prompting();
        let asked = run(
            &mut parser,
            &call("write", &json!({"path": text, "content": "x"}), "w"),
        );
        assert_eq!(asked.len(), 1, "{text:?} ran unasked");
        drop(asked);
    }
}

#[test]
fn a_network_path_is_asked_about_however_it_is_hidden_and_is_not_opened_to_find_out() {
    for text in [
        "\\\\10.255.255.1\\share\\f",
        "//10.255.255.1/share/f",
        "@\\\\10.255.255.1\\share\\f",
        "@//10.255.255.1/share/f",
        "file://10.255.255.1/share/f",
        "@file://10.255.255.1/share/f",
    ] {
        for tool in ["read", "write"] {
            for mode in [Mode::Manual, Mode::AcceptEdits] {
                let (mut parser, _) = parser_in(mode);
                parser.prompting();
                let started = std::time::Instant::now();
                let asked = run(
                    &mut parser,
                    &call(tool, &json!({"path": text, "content": "x"}), "n"),
                );
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(3),
                    "{text:?} was resolved"
                );
                assert_eq!(
                    asked.len(),
                    1,
                    "{mode:?} {tool} {text:?} was not asked about"
                );
                drop(asked);
            }
        }
    }
}

#[test]
fn a_command_cut_for_its_size_is_never_remembered_nor_answered_from_a_remembered_rule() {
    let (mut parser, written) = parser_in(Mode::Manual);
    parser.prompting();
    let mut first = run(
        &mut parser,
        &call("bash", &json!({"command": "git status"}), "d1"),
    );
    first
        .remove(0)
        .responder
        .answer(Outcome::Selected(ALLOW_TOOL.into()))
        .unwrap();
    written.lock().unwrap().clear();
    // What the gate sends of `git --version` followed by 5000 spaces and `; echo PWNED > pwned.txt`.
    let cut =
        json!({"command": format!("git --version{}", " ".repeat(4096 - 13)), "_cut": ["command"]});
    assert_eq!(Policy::remember("bash", &cut), None);
    let mut asked = run(&mut parser, &call("bash", &cut, "d2"));
    assert_eq!(
        asked.len(),
        1,
        "a cut command was answered from a remembered rule"
    );
    assert!(
        !asked[0].options.iter().any(|o| o.id == ALLOW_TOOL),
        "a cut command can be remembered"
    );
    assert!(
        asked[0]
            .description
            .as_deref()
            .is_some_and(|d| d.contains("cut")),
        "{:?}",
        asked[0].description
    );
    drop(asked.remove(0));
}

/// The path of the credential in each spelling Pi opens to it.
#[cfg(windows)]
fn spellings_of_a_credential() -> Vec<String> {
    let ssh = home().join(".ssh").join("id_rsa");
    let back = ssh.to_string_lossy().into_owned();
    let fwd = back.replace('\\', "/");
    let drive = back[..1].to_string();
    let tail = fwd[2..].to_string(); // `/Users/...`
    let rooted_back = back[2..].to_string(); // `\Users\...`
    let mut list = vec![
        format!("/mnt/{}{tail}", drive.to_lowercase()),
        format!("/MNT/{}{tail}", drive.to_uppercase()),
        format!("/Mnt/{}{tail}", drive.to_lowercase()),
        format!("/CYGDRIVE/{}{tail}", drive.to_lowercase()),
        format!("/cygdrive/{}{tail}", drive.to_uppercase()),
        format!("/Cygdrive/{}{tail}", drive.to_lowercase()),
        format!("file:///{drive}|{tail}"),
        format!("file:///{}|{tail}", drive.to_lowercase()),
        format!("file://{drive}:{tail}"),
        format!("file://{drive}|{tail}"),
        format!("file:///{fwd}"),
        format!("file:///{back}"),
        format!("file://\\{back}"),
        format!("file:///{fwd}?x"),
        format!("file:///{fwd}#f"),
        format!("file://\t/{fwd}"),
        format!("file:///{}", fwd.replace(".ssh", ".s\tsh")),
        format!("file:///{}", fwd.replace("id_rsa", "id_\nrsa")),
        format!("file:///{fwd}\r"),
        format!("\\\\?\\{back}"),
        format!("\\\\.\\{back}"),
        format!("//?/{fwd}"),
        format!("//./{fwd}"),
        format!("\\\\?\\{}", back.to_lowercase()),
        format!("///{}", &tail[1..]),
        format!("\\\\\\{}", &rooted_back[1..]),
    ];
    list.extend(list.clone().into_iter().map(|text| format!("@{text}")));
    list
}

/// What Pi opens as the credential itself, whatever the spelling: refused, never asked.
#[cfg(windows)]
#[test]
fn a_credential_is_refused_in_every_spelling_pi_opens_to_it() {
    for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
        for text in spellings_of_a_credential() {
            for tool in ["read", "write"] {
                let (mut parser, written) = parser_in(mode);
                parser.prompting();
                let asked = run(
                    &mut parser,
                    &call(tool, &json!({"path": text, "content": "x"}), "c"),
                );
                assert!(
                    asked.is_empty(),
                    "{mode:?} {tool} {text:?} was asked, not refused"
                );
                assert_eq!(
                    answer(&written).as_deref(),
                    Some("protected"),
                    "{mode:?} {tool} {text:?}"
                );
            }
        }
    }
}

/// `/\host\share` and `\/host/share` are network paths to Windows as `\\host\share` is.
#[cfg(windows)]
#[test]
fn a_network_path_written_with_mixed_separators_or_as_a_device_is_asked_about() {
    for text in [
        "/\\10.255.255.1\\share\\f",
        "\\/10.255.255.1/share/f",
        "@/\\10.255.255.1/share/f",
        "\\\\?\\UNC\\10.255.255.1\\share\\f",
        "//?/UNC/10.255.255.1/share/f",
        "\\\\.\\pipe\\x",
    ] {
        for tool in ["read", "write"] {
            let (mut parser, _) = parser_in(Mode::AcceptEdits);
            parser.prompting();
            let started = std::time::Instant::now();
            let asked = run(
                &mut parser,
                &call(tool, &json!({"path": text, "content": "x"}), "n"),
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(3),
                "{text:?} was resolved"
            );
            assert_eq!(asked.len(), 1, "{tool} {text:?} was not asked about");
            drop(asked);
        }
    }
}

/// On NTFS `.git::$INDEX_ALLOCATION` is the directory `.git`: a name with a colon is not where its text says.
#[cfg(windows)]
#[test]
fn a_name_with_a_colon_is_refused_in_every_mode_but_bypass() {
    for text in [
        ".git::$INDEX_ALLOCATION/config",
        "sub/.git::$INDEX_ALLOCATION/HEAD",
        ".pi::$INDEX_ALLOCATION/settings.json",
        "notes.txt:hidden",
        "notes.txt::$DATA",
        "@.git::$INDEX_ALLOCATION/config",
    ] {
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
            for tool in ["read", "write", "edit"] {
                let (mut parser, written) = parser_in(mode);
                parser.prompting();
                let asked = run(
                    &mut parser,
                    &call(tool, &json!({"path": text, "content": "x"}), "s"),
                );
                assert!(asked.is_empty(), "{mode:?} {tool} {text:?} was asked");
                assert_eq!(
                    answer(&written).as_deref(),
                    Some("protected"),
                    "{mode:?} {tool} {text:?}"
                );
            }
        }
    }
    let (mut parser, written) = parser_in(Mode::Bypass);
    parser.prompting();
    let _ = run(
        &mut parser,
        &call("write", &json!({"path": "x:y", "content": "x"}), "b"),
    );
    assert_eq!(answer(&written).as_deref(), Some("allow"));
}

fn parser_over(root: &std::path::Path, mode: Mode) -> (Parser, Written) {
    let written: Written = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&written);
    let parser = Parser::new(
        Arc::new(move |value: &Value| {
            sink.lock().unwrap().push(value.clone());
            Ok(())
        }),
        4096,
    )
    .with_policy(Arc::new(Mutex::new(Policy::new(mode, false))))
    .with_roots(root, Vec::new())
    .with_protected(vec![home().join(".pi").join("agent")])
    .with_home(Some(home()));
    (parser, written)
}

/// `grep` passes `--hidden`: from a directory above the credentials it reads them, though no path it names is one.
#[test]
fn a_search_from_a_directory_above_a_credential_is_asked_about_not_answered_by_rule() {
    // The directory holding the fixture home is a working root, as a home directory is for a session that works there.
    let above = home().parent().unwrap().to_path_buf();
    let inputs = [
        ("grep", json!({"pattern": "CANARY"})),
        ("grep", json!({"pattern": ".", "path": "."})),
        (
            "grep",
            json!({"pattern": ".", "path": home().to_string_lossy()}),
        ),
        ("find", json!({"pattern": "*"})),
        ("ls", json!({})),
        ("ls", json!({"path": "."})),
    ];
    for (tool, input) in &inputs {
        for mode in [Mode::Manual, Mode::AcceptEdits] {
            let (mut parser, written) = parser_over(&above, mode);
            parser.prompting();
            let asked = run(&mut parser, &call(tool, input, "g"));
            assert_eq!(asked.len(), 1, "{mode:?} {tool} {input} was answered");
            assert!(answer(&written).is_none(), "{mode:?} {tool} {input}");
            drop(asked);
        }
        let (mut parser, written) = parser_over(&above, Mode::Plan);
        parser.prompting();
        let asked = run(&mut parser, &call(tool, input, "g"));
        assert!(asked.is_empty(), "Plan {tool} {input} was asked");
        assert_eq!(answer(&written).as_deref(), Some("protected"), "{tool}");
    }
    // A search inside a project that holds none is answered by the rule, and `read` names one file.
    let (mut parser, written) = parser_over(&work(), Mode::Manual);
    parser.prompting();
    assert!(run(&mut parser, &call("grep", &json!({"pattern": "x"}), "ok")).is_empty());
    assert_eq!(answer(&written).as_deref(), Some("allow"));
    let (mut parser, written) = parser_over(&above, Mode::Manual);
    parser.prompting();
    let _ = run(
        &mut parser,
        &call("read", &json!({"path": "somefile.txt"}), "rd"),
    );
    assert_eq!(answer(&written).as_deref(), Some("allow"));
}

/// What other tools keep their sign-in in is refused as Pi's own is.
#[test]
fn the_credentials_of_the_persons_other_tools_are_refused() {
    let places = [
        "~/.codex/auth.json",
        "~/.claude/.credentials.json",
        "~/.config/git/credentials",
        "~/.config/gh/hosts.yml",
        "~/.npmrc",
        "~/.pypirc",
        "~/.netrc",
        "~/.docker/config.json",
        "~/.kube/config",
        "~/.aws/credentials",
        "~/.azure/accessTokens.json",
        "~/.config/gcloud/credentials.db",
    ];
    for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
        for text in places {
            for tool in ["read", "write"] {
                let (mut parser, written) = parser_in(mode);
                parser.prompting();
                let asked = run(
                    &mut parser,
                    &call(tool, &json!({"path": text, "content": "x"}), "c"),
                );
                assert!(asked.is_empty(), "{mode:?} {tool} {text} was asked");
                assert_eq!(
                    answer(&written).as_deref(),
                    Some("protected"),
                    "{mode:?} {tool} {text}"
                );
            }
        }
    }
}

/// The directory holding the gate is code Pi loads: the model changes nothing in it.
#[test]
fn nothing_is_written_in_the_directory_that_holds_the_gate() {
    let gate = std::env::temp_dir().join("coqui-acp-pi-fixture-gate");
    for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan] {
        for name in ["gate.ts", "guard.cjs", "other.txt"] {
            let path = gate.join(name);
            let (parser, written) = parser_in(mode);
            let mut parser = parser.with_private(vec![gate.clone()]);
            parser.prompting();
            let asked = run(
                &mut parser,
                &call(
                    "write",
                    &json!({"path": path.to_string_lossy(), "content": "x"}),
                    "g",
                ),
            );
            assert!(asked.is_empty(), "{mode:?} {name} was asked");
            assert_eq!(
                answer(&written).as_deref(),
                Some("protected"),
                "{mode:?} {name}"
            );
        }
    }
}
