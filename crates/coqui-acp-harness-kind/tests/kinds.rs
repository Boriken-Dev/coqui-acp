//! The contract other programs are built against: the kinds' hashes and description, and the
//! requirement names a harness matches on.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    clippy::alloc_instead_of_core,
    reason = "a test names what it uses from std"
)]

use std::path::PathBuf;

use coqui_acp_harness_kind::{
    AcpHarnessVtable, ChannelSinkVtable, ChannelVtable, NO_COMMANDS, ONLY_SKILLS, READ_ONLY,
    RESUME_WIDER, SANDBOXED_SCRIPTS,
};
use serde_json::Value;

fn file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// The harness kind and the channel kinds it starts, as one document, so a provider in another
/// language has every kind its `start` names.
fn rendered() -> String {
    let document = format!(
        "{{\"kinds\":[{},{},{}]}}",
        AcpHarnessVtable::description(),
        ChannelVtable::description(),
        ChannelSinkVtable::description()
    );
    let value: Value = serde_json::from_str(&document).expect("a kind's description is JSON");
    let mut text = serde_json::to_string_pretty(&value).expect("written");
    text.push('\n');
    text
}

/// Regenerate with `COQUI_ACP_HARNESS_KIND_WRITE_KINDS=1 cargo test -p coqui-acp-harness-kind --test kinds`.
#[test]
fn the_committed_description_is_what_this_build_describes() {
    let path = file("include/coqui_acp_harness_kind.kinds.json");
    let fresh = rendered();
    if std::env::var_os("COQUI_ACP_HARNESS_KIND_WRITE_KINDS").is_some() {
        std::fs::write(&path, &fresh).expect("the description is written");
    }
    let committed = std::fs::read_to_string(&path).expect("the kind description is committed");
    assert!(
        committed == fresh,
        "regenerate include/coqui_acp_harness_kind.kinds.json"
    );
}

/// Changing a slot, a model's name or a kind's name breaks every built provider. This names the
/// numbers, so doing that is a decision.
#[test]
fn the_kinds_hashes_are_pinned() {
    assert_eq!(AcpHarnessVtable::FLOOR_HASH, HARNESS_HASH);
    assert_eq!(ChannelVtable::FLOOR_HASH, CHANNEL_HASH);
    assert_eq!(ChannelSinkVtable::FLOOR_HASH, SINK_HASH);
}

const HARNESS_HASH: u32 = 2194037834;
const CHANNEL_HASH: u32 = 2843269355;
const SINK_HASH: u32 = 3528315804;

#[test]
fn the_api_header_says_those_hashes() {
    let header = std::fs::read_to_string(file("AGENTS.md")).expect("the header ships");
    for hash in [HARNESS_HASH, CHANNEL_HASH, SINK_HASH] {
        assert!(header.contains(&hash.to_string()), "AGENTS.md lacks {hash}");
    }
}

#[test]
fn the_python_kinds_carry_those_hashes() {
    let module = std::fs::read_to_string(file("python/coqui_acp_harness_kinds.py"))
        .expect("the Python kinds are committed");
    for (kind, hash) in [
        ("acp_harness", HARNESS_HASH),
        ("channel", CHANNEL_HASH),
        ("channel_sink", SINK_HASH),
    ] {
        assert!(
            module.contains(&format!("kind=\"{kind}\", floor_hash={hash}")),
            "python/coqui_acp_harness_kinds.py lacks {kind} at {hash}: regenerate it"
        );
    }
}

/// A harness matches these as text, in every language: renaming one breaks every harness.
#[test]
fn the_requirement_names_are_pinned() {
    assert_eq!(
        [
            READ_ONLY,
            NO_COMMANDS,
            RESUME_WIDER,
            ONLY_SKILLS,
            SANDBOXED_SCRIPTS
        ],
        [
            "read_only",
            "no_commands",
            "resume_wider",
            "only_skills",
            "sandboxed_scripts"
        ]
    );
}
