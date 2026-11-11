//! Where a path a tool call names really is, as Pi 1.5.2 works it out.
//!
//! Pi does not use the text as written: its tools call `resolveToCwd` (`dist/core/tools/path-utils.js`, with
//! `dist/utils/paths.js`), which turns Unicode spaces into spaces, drops one leading `@`, on Windows reads `/c/...`,
//! `/mnt/c/...` and `/cygdrive/c/...` as drive paths, expands `~` and `~/`, reads a `file://` URL as the path it names, and
//! only then resolves the result against the working directory. A decision made on the text as written (is it inside the
//! project? is it a credential file?) is wrong for every spelling that rewrite changes, so the decision is made on this.
//!
//! This is a port of Pi 1.5.2's rewrite, kept to that version: `tools/record_harnesses/pi/path_table.mjs` prints what a
//! Pi makes of a list of inputs, and the tests hold this to the table it printed. A path that merely *looks* like it takes a
//! rewrite ([`indirect`]) is asked about whatever this finds, in case a Pi changes it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::{Component, Path, PathBuf};

/// The characters Pi turns into a plain space (`UNICODE_SPACES`).
fn unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// What a path text names once Pi has worked it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The absolute path, with `.` and `..` resolved by how they are written (links are not followed here).
    pub path: PathBuf,
    /// Whether it is a network or device path (`\\host\share`, `//host/share`): not to be opened to find out more.
    pub network: bool,
    /// Whether this could not say where it is (a home directory unknown, a `file:` URL Pi would refuse or one this does not
    /// read): the text is to be treated as unknown.
    pub unknown: bool,
    /// Whether, on Windows, a name in it holds a `:` (an alternate data stream: `.git::$INDEX_ALLOCATION` is the directory
    /// `.git`), so that it is not the place its text seems to be.
    pub stream: bool,
}

/// `%XX` escapes decoded, `None` for a bad escape or one that hides a separator (which Pi refuses).
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = text.get(at + 1..at + 3)?;
            let byte = u8::from_str_radix(pair, 16).ok()?;
            if matches!(byte, b'/' | b'\\') {
                return None;
            }
            out.push(byte);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A `/c/x`, `/mnt/c/x` or `/cygdrive/c/x` as the drive path Pi makes of it on Windows.
fn windows_shell_path(text: &str) -> Option<String> {
    if !text.starts_with('/') || text.starts_with("//") || text.contains('\\') {
        return None;
    }
    // Pi matches the two prefixes whatever their case.
    let after_prefix = |prefix: &str| {
        text.get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &text[prefix.len()..])
    };
    let rest = after_prefix("/mnt/")
        .or_else(|| after_prefix("/cygdrive/"))
        .unwrap_or(&text[1..]);
    let mut chars = rest.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    let after = chars.as_str();
    if !(after.is_empty() || after.starts_with('/')) {
        return None;
    }
    let suffix = after.strip_prefix('/').unwrap_or("").replace('/', "\\");
    Some(alloc::format!("{}:\\{suffix}", drive.to_ascii_uppercase()))
}

/// A `file://` URL as the path Pi reads from it; `None` where Pi would refuse it. A host that is not the machine's makes a
/// network path on Windows. The text is read as URL parsing reads it: ASCII tab and newline are removed wherever they are,
/// control characters and spaces at either end are cut, a backslash is a slash, the query and the fragment are not part of
/// the path, and (Windows) a drive is `C:` or `C|`, in the host's place as well as in the path's.
fn file_url(text: &str, windows: bool) -> Option<String> {
    let cleaned: String = text
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let rest = cleaned.strip_prefix("file://")?;
    let rest = rest[..rest.find(['?', '#']).unwrap_or(rest.len())].replace('\\', "/");
    let (host, path) = rest
        .find('/')
        .map_or((rest.as_str(), ""), |at| rest.split_at(at));
    let drive_host = host.len() == 2
        && host.as_bytes()[0].is_ascii_alphabetic()
        && matches!(host.as_bytes()[1], b':' | b'|');
    let (host, mut path) = if windows && drive_host {
        ("", alloc::format!("/{}:{path}", &host[..1]))
    } else {
        (host, path.to_string())
    };
    let bytes = path.as_bytes();
    if windows
        && bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b'|'
        && (bytes.len() == 3 || bytes[3] == b'/')
    {
        path.replace_range(2..3, ":");
    }
    let path = percent_decode(&path)?;
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        if windows {
            let drive = path.strip_prefix('/')?;
            let mut chars = drive.chars();
            let letter = chars.next().filter(char::is_ascii_alphabetic)?;
            let after = chars.as_str().strip_prefix(':')?;
            if !(after.is_empty() || after.starts_with('/')) {
                return None;
            }
            Some(alloc::format!(
                "{letter}:{}",
                if after.is_empty() {
                    "\\".to_string()
                } else {
                    after.replace('/', "\\")
                }
            ))
        } else {
            Some(path)
        }
    } else if windows {
        Some(alloc::format!("\\\\{host}{}", path.replace('/', "\\")))
    } else {
        None
    }
}

/// `path` with `.` dropped and `..` taken against what comes before it, by how it is written. Built from text, since a
/// directory called like a drive (`C:`) must stay a name and not replace what precedes it.
#[must_use]
pub fn lexical(path: &Path) -> PathBuf {
    let mut head = String::new();
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => head.push_str(&prefix.as_os_str().to_string_lossy()),
            Component::RootDir => head.push(std::path::MAIN_SEPARATOR),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
        }
    }
    let separator = std::path::MAIN_SEPARATOR.to_string();
    PathBuf::from(alloc::format!("{head}{}", parts.join(&separator)))
}

/// Whether the text names a drive with no root (`C:foo`): relative to that drive's own directory, which this does not know.
fn drive_relative(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && !matches!(chars.next(), Some('/' | '\\') | None)
}

/// Pi 1.5.2's `resolveToCwd(text, cwd)`, with `home` for `~` and `windows` for the Windows-only rewrite and URL reading.
#[must_use]
pub fn resolve(text: &str, cwd: &Path, home: Option<&Path>, windows: bool) -> Resolved {
    let spaced: String = text
        .chars()
        .map(|c| if unicode_space(c) { ' ' } else { c })
        .collect();
    rewrite(
        spaced.strip_prefix('@').unwrap_or(&spaced),
        cwd,
        home,
        windows,
    )
}

/// What Pi 1.5.2 makes of a directory a setting or an environment variable names (`PI_CODING_AGENT_DIR`): the same rewrite
/// as a tool's path (`~`, a Windows shell path, a `file:` URL) but for the Unicode spaces and the leading `@`, which only
/// a tool's path loses.
#[must_use]
pub fn setting(text: &str, cwd: &Path, home: Option<&Path>, windows: bool) -> Resolved {
    rewrite(text, cwd, home, windows)
}

fn rewrite(text: &str, cwd: &Path, home: Option<&Path>, windows: bool) -> Resolved {
    let mut unknown = false;
    let mut s = text.to_string();
    if windows && let Some(drive) = windows_shell_path(&s) {
        s = drive;
    }
    if s == "~" || s.starts_with("~/") || (windows && s.starts_with("~\\")) {
        match home {
            Some(home) => {
                let tail = s.get(2..).unwrap_or("");
                s = if s == "~" {
                    home.to_string_lossy().into_owned()
                } else {
                    home.join(tail).to_string_lossy().into_owned()
                };
            }
            None => unknown = true,
        }
    }
    if s.starts_with("file://") {
        match file_url(&s, windows) {
            Some(path) => s = path,
            None => unknown = true,
        }
    }
    if windows && drive_relative(&s) {
        unknown = true;
    }
    if windows {
        s = windows_form(&s);
    }
    let network = if windows {
        s.starts_with("\\\\")
    } else {
        s.starts_with("\\\\") || s.starts_with("//")
    };
    let path = if network {
        PathBuf::from(&s)
    } else {
        lexical(&cwd.join(&s))
    };
    let stream = windows && hides_a_stream(&path.to_string_lossy());
    Resolved {
        path,
        network,
        unknown,
        stream,
    }
}

fn separator(c: char) -> bool {
    matches!(c, '/' | '\\')
}

/// A path on Windows, written the way the system reads it. A network path starts with exactly two separators of either kind
/// and is written `\\host\share`; three or more are one, the root of the current drive; `\\?\C:\x`, `\\.\C:\x` and `//?/C:/x`
/// name the file `C:\x` and are written so; `\\?\UNC\host\share` and every other device path are network paths.
fn windows_form(text: &str) -> String {
    let mut chars = text.chars();
    let (first, second, third) = (chars.next(), chars.next(), chars.next());
    if !(first.is_some_and(separator) && second.is_some_and(separator)) {
        return text.to_string();
    }
    if third.is_some_and(separator) {
        return alloc::format!("\\{}", text.trim_start_matches(separator));
    }
    let rest = &text[2..];
    if let Some(device) = rest
        .strip_prefix(['?', '.'])
        .and_then(|after| after.strip_prefix(separator))
    {
        let mut letters = device.chars();
        let drive = letters.next().filter(char::is_ascii_alphabetic);
        if drive.is_some() && letters.next() == Some(':') {
            match letters.next() {
                None => return alloc::format!("{}\\", &device[..2]),
                Some(c) if separator(c) => return device.to_string(),
                Some(_) => {}
            }
        }
    }
    alloc::format!("\\\\{}", rest.replace('/', "\\"))
}

/// Whether a Windows path has a `:` in a name, which the file system reads as an alternate data stream
/// (`.git::$INDEX_ALLOCATION` is the directory `.git`): any colon but the one after the drive letter.
fn hides_a_stream(path: &str) -> bool {
    if path.starts_with("\\\\") {
        return false;
    }
    let bytes = path.as_bytes();
    let rest = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        &path[2..]
    } else {
        path
    };
    rest.contains(':')
}

/// Whether the text looks as if Pi rewrites it: it starts with `~` or `@`, holds a `file:` URL, or (on Windows) names a
/// drive without a root (`C:foo`, relative to that drive's own directory). Asked about always, never allowed unasked or
/// remembered, so that a Pi that rewrites more than this port knows is not trusted with it.
#[must_use]
pub fn indirect(text: &str, windows: bool) -> bool {
    let lower = text.to_ascii_lowercase();
    let spaced = text.trim_start_matches(unicode_space);
    let drive_relative = windows && drive_relative(text);
    text.starts_with(['~', '@'])
        || spaced.starts_with(['~', '@'])
        || lower.contains("file:")
        || drive_relative
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:\\fakehome"
        } else {
            "/fakehome"
        })
    }

    fn cwd() -> PathBuf {
        PathBuf::from(if cfg!(windows) { "C:\\proj" } else { "/proj" })
    }

    fn at(text: &str, windows: bool) -> PathBuf {
        resolve(text, &cwd(), Some(&home()), windows).path
    }

    fn rooted(rest: &str) -> PathBuf {
        let base = if cfg!(windows) { "C:\\" } else { "/" };
        Path::new(base).join(rest)
    }

    #[test]
    fn a_plain_path_is_against_the_directory_and_dots_are_resolved() {
        assert_eq!(at("notes.txt", false), cwd().join("notes.txt"));
        assert_eq!(at("sub/../notes.txt", false), cwd().join("notes.txt"));
        assert_eq!(at("./a/./b.txt", false), cwd().join("a").join("b.txt"));
        assert_eq!(at("../outside.txt", false), rooted("outside.txt"));
    }

    #[test]
    fn the_unicode_spaces_pi_flattens_are_spaces() {
        assert_eq!(at("a\u{2003}b.txt", false), cwd().join("a b.txt"));
        assert_eq!(at("a\u{a0}b", false), cwd().join("a b"));
        // And a leading one does not hide a `~`: it is a space then, so a name inside the directory.
        assert_eq!(at("\u{3000}~/x", false), cwd().join(" ~").join("x"));
    }

    #[test]
    fn one_leading_at_is_dropped() {
        assert_eq!(at("@notes.txt", false), cwd().join("notes.txt"));
        assert_eq!(at("@@x", false), cwd().join("@x"));
        assert_eq!(
            at("@~/.pi/agent/models.json", false),
            home().join(".pi").join("agent").join("models.json")
        );
    }

    #[test]
    fn a_tilde_is_the_home_directory_but_a_tilde_name_is_not() {
        assert_eq!(at("~", false), home());
        assert_eq!(
            at("~/.ssh/id_rsa", false),
            home().join(".ssh").join("id_rsa")
        );
        assert_eq!(at("~user/x", false), cwd().join("~user").join("x"));
        assert!(resolve("~/x", &cwd(), None, false).unknown);
    }

    #[test]
    fn a_file_url_is_the_path_it_names() {
        if cfg!(windows) {
            assert_eq!(
                at("file:///C:/fakehome/.ssh/id_rsa", true),
                home().join(".ssh").join("id_rsa")
            );
            assert_eq!(at("file://localhost/C:/x", true), PathBuf::from("C:\\x"));
            assert_eq!(
                at("file:///C:/with%20space/x", true),
                PathBuf::from("C:\\with space\\x")
            );
            assert_eq!(at("@file:///C:/x", true), PathBuf::from("C:\\x"));
        } else {
            assert_eq!(
                at("file:///etc/passwd", false),
                PathBuf::from("/etc/passwd")
            );
            assert_eq!(at("file:///a%20b/c", false), PathBuf::from("/a b/c"));
        }
        // Encoded separators are refused by Pi; a malformed escape too.
        assert!(resolve("file:///a%2Fb", &cwd(), None, false).unknown);
        assert!(resolve("file:///a%zz", &cwd(), None, false).unknown);
        // Not a URL Pi reads: a name inside the directory.
        assert_eq!(at("file:notes.txt", false), cwd().join("file:notes.txt"));
    }

    #[test]
    fn a_network_path_is_found_by_how_it_is_written_whatever_hides_it() {
        for text in [
            "\\\\host\\share\\f",
            "//host/share/f",
            "@\\\\host\\share\\f",
            "@//host/share/f",
            "file://host/share/f",
            "@file://host/share/f",
        ] {
            let found = resolve(text, &cwd(), Some(&home()), true);
            assert!(found.network, "{text}");
        }
        assert!(!resolve("notes.txt", &cwd(), Some(&home()), true).network);
        // On another system Pi cannot read a URL naming a host.
        assert!(resolve("file://host/share/f", &cwd(), Some(&home()), false).unknown);
    }

    #[test]
    fn windows_shell_paths_are_drive_paths_on_windows_only() {
        for (text, wanted) in [
            ("/c/Windows/win.ini", "C:\\Windows\\win.ini"),
            ("/C/Windows/win.ini", "C:\\Windows\\win.ini"),
            ("/mnt/c/Windows/win.ini", "C:\\Windows\\win.ini"),
            ("/cygdrive/c/Windows/win.ini", "C:\\Windows\\win.ini"),
            ("/c", "C:\\"),
        ] {
            assert_eq!(windows_shell_path(text).as_deref(), Some(wanted), "{text}");
        }
        for text in ["/cx/y", "/c\\y", "//c/x", "c/x", "/mnt/cc/x"] {
            assert_eq!(windows_shell_path(text), None, "{text}");
        }
    }

    #[test]
    fn a_shell_path_prefix_is_matched_whatever_its_case() {
        for text in [
            "/MNT/c/x",
            "/Mnt/C/x",
            "/CYGDRIVE/c/x",
            "/Cygdrive/c/x",
            "/cygdrive/C/x",
        ] {
            assert_eq!(windows_shell_path(text).as_deref(), Some("C:\\x"), "{text}");
        }
    }

    #[test]
    fn a_file_url_is_cleaned_as_url_parsing_cleans_it() {
        for (text, wanted) in [
            ("file:///C|/Data/x", "C:\\Data\\x"),
            ("file://C:/Data/x", "C:\\Data\\x"),
            ("file://C|/Data/x", "C:\\Data\\x"),
            ("file://\t/C:/Data/x", "C:\\Data\\x"),
            ("file:///C:/Da\tta/x", "C:\\Data\\x"),
            ("file:///C:/Data/x\r", "C:\\Data\\x"),
            ("file:///C:/Data/x?q#f", "C:\\Data\\x"),
            ("file:///C:\\Data\\x", "C:\\Data\\x"),
            ("file://\\C:\\Data\\x", "C:\\Data\\x"),
            ("file://localhost\\C:\\x", "C:\\x"),
        ] {
            assert_eq!(file_url(text, true).as_deref(), Some(wanted), "{text:?}");
        }
        // Pi refuses these.
        for text in [
            "file:///C%7c/Data/x",
            "file:///C||/x",
            "file:///\\host\\share\\f",
        ] {
            assert_eq!(file_url(text, true), None, "{text:?}");
        }
    }

    #[test]
    fn a_windows_path_is_written_the_way_the_system_reads_it() {
        for (text, wanted) in [
            ("\\\\?\\C:\\Data\\x", "C:\\Data\\x"),
            ("\\\\.\\C:\\x", "C:\\x"),
            ("//?/C:/Data/x", "C:/Data/x"),
            ("\\\\?\\C:", "C:\\"),
            ("/\\host\\share", "\\\\host\\share"),
            ("\\/host/share", "\\\\host\\share"),
            ("//host/share", "\\\\host\\share"),
            ("\\\\?\\UNC\\h\\s", "\\\\?\\UNC\\h\\s"),
            ("//?/UNC/h/s", "\\\\?\\UNC\\h\\s"),
            ("\\\\.\\pipe\\x", "\\\\.\\pipe\\x"),
            ("///a/b", "\\a/b"),
            ("\\\\\\a\\b", "\\a\\b"),
            ("C:\\x", "C:\\x"),
            ("/x", "/x"),
        ] {
            assert_eq!(windows_form(text), wanted, "{text:?}");
        }
    }

    #[test]
    fn only_the_colon_after_the_drive_is_not_a_stream() {
        for text in [
            "C:\\proj\\.git::$INDEX_ALLOCATION\\config",
            "C:\\a:b",
            "sub\\a:b",
        ] {
            assert!(hides_a_stream(text), "{text}");
        }
        for text in ["C:\\proj\\a.txt", "\\\\host\\share\\a:b", "C:\\", "a.txt"] {
            assert!(!hides_a_stream(text), "{text}");
        }
    }

    #[test]
    fn what_looks_rewritten_is_indirect() {
        for text in [
            "~",
            "~/x",
            "@x",
            "\u{a0}~/x",
            "file:///x",
            "x file:///y",
            "FILE:///x",
        ] {
            assert!(indirect(text, false), "{text}");
        }
        assert!(indirect("C:foo", true));
        assert!(!indirect("C:foo", false));
        for text in [
            "notes.txt",
            "a/b.txt",
            "C:\\x",
            "C:/x",
            "../x",
            "profile.txt",
        ] {
            assert!(!indirect(text, true), "{text}");
        }
    }
}

/// What Pi 1.5.2 itself made of a list of inputs, printed by `tools/record_harnesses/pi/path_table.mjs` on Windows with
/// the directory `C:\proj` and the home `C:\fakehome`. This port is held to it.
#[cfg(all(test, windows))]
mod pi_table {
    use super::*;

    const TABLE: &[(&str, &str)] = &[
        ("notes.txt", "C:\\proj\\notes.txt"),
        ("sub/../notes.txt", "C:\\proj\\notes.txt"),
        ("../outside.txt", "C:\\outside.txt"),
        ("./a/./b.txt", "C:\\proj\\a\\b.txt"),
        ("~", "C:\\fakehome"),
        ("~/.ssh/id_rsa", "C:\\fakehome\\.ssh\\id_rsa"),
        ("~\\.ssh\\id_rsa", "C:\\fakehome\\.ssh\\id_rsa"),
        ("~user/x", "C:\\proj\\~user\\x"),
        ("@notes.txt", "C:\\proj\\notes.txt"),
        (
            "@~/.pi/agent/models.json",
            "C:\\fakehome\\.pi\\agent\\models.json",
        ),
        ("@@x", "C:\\proj\\@x"),
        ("@C:\\fakehome\\.ssh\\id_rsa", "C:\\fakehome\\.ssh\\id_rsa"),
        ("@\\\\host\\share\\f", "\\\\host\\share\\f"),
        ("@//host/share/f", "\\\\host\\share\\f"),
        ("//host/share/f", "\\\\host\\share\\f"),
        ("\\\\host\\share\\f", "\\\\host\\share\\f"),
        ("\u{a0}~/.ssh/id_rsa", "C:\\proj\\ ~\\.ssh\\id_rsa"),
        ("~/.ssh/id_rsa\u{a0}", "C:\\fakehome\\.ssh\\id_rsa "),
        ("a\u{2003}b.txt", "C:\\proj\\a b.txt"),
        ("\u{3000}~/x", "C:\\proj\\ ~\\x"),
        ("/c/Windows/win.ini", "C:\\Windows\\win.ini"),
        ("/C/Windows/win.ini", "C:\\Windows\\win.ini"),
        ("/mnt/c/Windows/win.ini", "C:\\Windows\\win.ini"),
        ("/cygdrive/c/Windows/win.ini", "C:\\Windows\\win.ini"),
        ("/c", "C:\\"),
        ("/cx/y", "C:\\cx\\y"),
        ("/c\\y", "C:\\c\\y"),
        (
            "file:///C:/fakehome/.ssh/id_rsa",
            "C:\\fakehome\\.ssh\\id_rsa",
        ),
        ("file://host/share/f", "\\\\host\\share\\f"),
        ("file://localhost/C:/x", "C:\\x"),
        ("file:///C:/with%20space/x", "C:\\with space\\x"),
        ("file:notes.txt", "C:\\proj\\file:notes.txt"),
        ("FILE:///C:/x", "C:\\proj\\FILE:\\C:\\x"),
        ("@file:///C:/x", "C:\\x"),
        ("C:\\x\\..\\y", "C:\\y"),
        ("\\x", "C:\\x"),
        ("C:\\Data\\..\\Windows", "C:\\Windows"),
        // The prefixes of a shell path are matched whatever their case.
        ("/MNT/C/Data/x", "C:\\Data\\x"),
        ("/Mnt/c/Data/x", "C:\\Data\\x"),
        ("/mnt/C/Data/x", "C:\\Data\\x"),
        ("/CYGDRIVE/c/Data/x", "C:\\Data\\x"),
        ("/cygdrive/C/Data/x", "C:\\Data\\x"),
        ("/Cygdrive/c/x", "C:\\x"),
        // A URL is read as URL parsing reads it.
        ("file:///C|/Data/x", "C:\\Data\\x"),
        ("file:///c|/Data/x", "c:\\Data\\x"),
        ("file://C:/Data/x", "C:\\Data\\x"),
        ("file://C|/Data/x", "C:\\Data\\x"),
        ("file://c:/Data/x", "c:\\Data\\x"),
        ("file:///C:\\Data\\x", "C:\\Data\\x"),
        ("file://\\C:\\Data\\x", "C:\\Data\\x"),
        ("file://localhost\\C:\\x", "C:\\x"),
        ("file:///C:/Da\tta/x", "C:\\Data\\x"),
        ("file:///C:/Da\nta/x", "C:\\Data\\x"),
        ("file://\t/C:/Data/x", "C:\\Data\\x"),
        ("file:///\tC:/Data/x", "C:\\Data\\x"),
        ("file:///C:/Data/x\r", "C:\\Data\\x"),
        ("file:///C:/Data/x ", "C:\\Data\\x"),
        ("file:///C:/Data/x?q", "C:\\Data\\x"),
        ("file:///C:/Data/x#f", "C:\\Data\\x"),
        ("file:///C:/Data/x?a#b", "C:\\Data\\x"),
        ("file:///C:/Data/%2e%2e/x", "C:\\x"),
        ("file:///C:/Data/x%3f", "C:\\Data\\x?"),
        ("fi\tle:///C:/Data/x", "C:\\proj\\fi\tle:\\C:\\Data\\x"),
        ("file:\t///C:/Data/x", "C:\\proj\\file:\t\\C:\\Data\\x"),
        ("file://host.example/share/f", "\\\\host.example\\share\\f"),
        // Mixed separators make a network path; three or more are the root of the drive.
        ("/\\host\\share\\f", "\\\\host\\share\\f"),
        ("\\/host/share/f", "\\\\host\\share\\f"),
        ("/\\host/share", "\\\\host\\share"),
        ("///host/share/f", "C:\\host\\share\\f"),
        ("\\\\\\host\\share\\f", "C:\\host\\share\\f"),
        ("\\//host/share", "C:\\host\\share"),
        ("////host/share", "C:\\host\\share"),
        // Names that are colons are streams of a file, not different names.
        (
            ".git::$INDEX_ALLOCATION/config",
            "C:\\proj\\.git::$INDEX_ALLOCATION\\config",
        ),
        ("C:\\a\\b:c", "C:\\a\\b:c"),
    ];

    /// Pi writes a verbatim or device path as it was given (`\\?\C:\x`); it names the file `C:\x`, which this port decides on.
    const VERBATIM: &[(&str, &str)] = &[
        ("\\\\?\\C:\\Data\\x", "C:\\Data\\x"),
        ("\\\\.\\C:\\Data\\x", "C:\\Data\\x"),
        ("//?/C:/Data/x", "C:\\Data\\x"),
        ("//./C:/Data/x", "C:\\Data\\x"),
        ("\\\\?\\c:\\users\\x", "c:\\users\\x"),
        ("\\\\?\\C:\\a\\..\\b", "C:\\b"),
        ("\\\\?\\C:", "C:\\"),
        ("\\\\.\\C:", "C:\\"),
        ("\\\\?\\UNC\\host\\share\\f", "\\\\?\\UNC\\host\\share\\f"),
        ("//?/UNC/host/share/f", "\\\\?\\UNC\\host\\share\\f"),
        ("\\\\.\\pipe\\x", "\\\\.\\pipe\\x"),
        ("\\\\?\\Volume{abc}\\x", "\\\\?\\Volume{abc}\\x"),
    ];

    #[test]
    fn a_verbatim_or_device_path_to_a_drive_is_that_drive_and_any_other_is_a_network_path() {
        let (cwd, home) = (PathBuf::from("C:\\proj"), PathBuf::from("C:\\fakehome"));
        for (input, wanted) in VERBATIM {
            let found = resolve(input, &cwd, Some(&home), true);
            assert_eq!(
                found.path.to_string_lossy().replace('/', "\\"),
                *wanted,
                "{input:?}"
            );
            assert_eq!(found.network, wanted.starts_with("\\\\"), "{input:?}");
        }
    }

    #[test]
    fn a_name_with_a_colon_is_a_stream() {
        let (cwd, home) = (PathBuf::from("C:\\proj"), PathBuf::from("C:\\fakehome"));
        for text in [
            ".git::$INDEX_ALLOCATION/config",
            "sub/.git::$INDEX_ALLOCATION/HEAD",
            ".pi::$INDEX_ALLOCATION/settings.json",
            "notes.txt:hidden",
            "C:\\proj\\a:b",
            "@.git::$INDEX_ALLOCATION/config",
        ] {
            assert!(resolve(text, &cwd, Some(&home), true).stream, "{text}");
        }
        for text in ["notes.txt", "C:\\proj\\a.txt", "//host/share/a:b", "C:/x/y"] {
            assert!(!resolve(text, &cwd, Some(&home), true).stream, "{text}");
        }
    }

    #[test]
    fn the_port_makes_of_each_input_what_pi_made() {
        let (cwd, home) = (PathBuf::from("C:\\proj"), PathBuf::from("C:\\fakehome"));
        for (input, wanted) in TABLE {
            let found = resolve(input, &cwd, Some(&home), true);
            assert_eq!(
                found.path.to_string_lossy().replace('/', "\\"),
                *wanted,
                "{input:?}"
            );
        }
    }
}
