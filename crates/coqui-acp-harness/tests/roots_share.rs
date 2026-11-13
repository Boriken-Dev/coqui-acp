//! A folder on a share (`\\server\share\work`) is a working directory like any other. The share is this machine's own
//! administrative share, so nothing leaves it.
#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "a test names what it uses from std"
)]

use std::path::Path;

use coqui_acp_harness::acp::directory_admitted;

/// A folder on a share is held to the roots by its parts, whichever case and separator it is written with, with `.` and
/// `..` refused, and a device namespace never.
#[cfg(windows)]
#[test]
fn a_folder_on_a_share_is_a_working_directory_held_to_the_roots() {
    use std::path::PathBuf;

    let on_share = |path: &Path| {
        let text = path.to_string_lossy();
        let (drive, rest) = text.split_at(2);
        let shared = PathBuf::from(format!(r"\\localhost\{}${rest}", &drive[..1]));
        assert!(shared.is_dir(), "{} does not answer", shared.display());
        shared
    };
    let base = std::env::temp_dir().join(format!("coqui-acp-roots-{}", std::process::id()));
    let inside = base.join("project").join("src");
    let beside = base.join("other");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&beside).unwrap();
    let root = on_share(&base.join("project"));
    let roots = vec![root.clone()];
    let text = |path: &Path| path.to_string_lossy().into_owned();
    for taken in [
        text(&root),
        text(&on_share(&inside)),
        text(&on_share(&inside)).to_uppercase(),
        text(&on_share(&inside)).replace('\\', "/"),
    ] {
        assert_eq!(
            directory_admitted(Path::new(&taken), &roots, false),
            Ok(()),
            "{taken}"
        );
    }
    for refused in [
        text(&on_share(&beside)),
        format!(r"{}\..\..\other", root.display()),
        format!(r"{}\.\src", root.display()),
        r"\\localhost\C$".to_owned(),
        r"\\.\pipe\x".to_owned(),
        r"\\?\UNC\localhost\C$\x".to_owned(),
        "//./pipe/x".to_owned(),
        text(&inside),
    ] {
        assert!(
            directory_admitted(Path::new(&refused), &roots, false).is_err(),
            "{refused}"
        );
    }
    // With no roots a share is still no device and has no `..`.
    assert_eq!(directory_admitted(&on_share(&inside), &[], false), Ok(()));
    for refused in [r"\\.\pipe\x", r"\\localhost\C$\x\..\y"] {
        assert!(
            directory_admitted(Path::new(refused), &[], false).is_err(),
            "{refused}"
        );
    }
    // What a harness takes from a request outside these two fields is still held to the network rule.
    assert!(coqui_acp_harness::acp::path_admitted(&on_share(&inside), &[], false).is_err());
    assert_eq!(
        coqui_acp_harness::acp::path_admitted(&on_share(&inside), &[], true),
        Ok(())
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Off Windows nothing about a working directory changes: a path that opens with two separators is refused as before.
#[cfg(not(windows))]
#[test]
fn off_windows_a_double_separator_path_is_refused_as_before() {
    assert!(directory_admitted(Path::new("//host/share/x"), &[], false).is_err());
    assert_eq!(directory_admitted(Path::new("/tmp"), &[], false), Ok(()));
}
