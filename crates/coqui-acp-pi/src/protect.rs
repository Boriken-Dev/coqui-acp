//! The paths the model may not read or change without the person's leave: one list in gitignore syntax.
//!
//! A line is a pattern; a leading `!` allows what an earlier line protected, and the last line that matches a path wins. A
//! pattern that matches a directory covers everything under it. `*` and `?` stand for part of a name, `**` for any number of
//! names, `#` starts a comment. Where a pattern starts says what it is relative to: `~/` the home directory of Pi's
//! environment, a drive (`C:/x`) or `/` the file system, anything else any place (`.env` matches every file of that name, `a/b`
//! every `b` under an `a`). Names are compared whatever their case on Windows.
//!
//! Paths are matched after [`crate::paths`] has worked out the one Pi will open, so a spelling that Pi rewrites reaches the same
//! entry. The crate holds no list of its own in code beyond [`RECOMMENDED`], which a person's list replaces.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::{Component, Path};

/// What this crate recommends protecting, for a person who has not written a list of their own: the places credentials are
/// kept in by Pi and by the tools it is used beside. Written out in a person's list to keep, change or remove any of it.
pub const RECOMMENDED: &[&str] = &[
    "~/.pi/agent/auth.json",
    "~/.pi/agent/models.json",
    "~/.pi/agent/mcp-auth.json",
    "~/.ssh",
    "~/.gnupg",
    "~/.aws",
    "~/.azure",
    "~/.kube",
    "~/.netrc",
    "~/.npmrc",
    "~/.pypirc",
    "~/.git-credentials",
    "~/.config/git/credentials",
    "~/.config/gcloud",
    "~/.config/gh",
    "~/.docker/config.json",
    "~/.cargo/credentials",
    "~/.cargo/credentials.toml",
    "~/.codex/auth.json",
    "~/.claude/.credentials.json",
    "~/AppData/Roaming/gcloud",
    "~/AppData/Roaming/GitHub CLI",
];

/// What a pattern is relative to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Anchor {
    Home,
    Root,
    Anywhere,
}

#[derive(Clone, Debug)]
struct Rule {
    allow: bool,
    anchor: Anchor,
    names: Vec<String>,
}

/// A list of patterns, ready to match paths against.
#[derive(Clone, Debug, Default)]
pub struct Protected {
    rules: Vec<Rule>,
    windows: bool,
}

fn fold(text: &str, windows: bool) -> String {
    if windows {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// The names of a path, drive first on Windows.
fn names_of(path: &Path, windows: bool) -> Vec<String> {
    path.components()
        .filter_map(|part| match part {
            Component::Prefix(prefix) => {
                // `\\?\C:` (what links followed make of a drive) is the drive `C:`.
                let text = prefix.as_os_str().to_string_lossy().into_owned();
                let drive = text
                    .rfind(':')
                    .filter(|at| *at >= 1)
                    .map_or(text.as_str(), |at| &text[at - 1..=at]);
                Some(fold(drive, windows))
            }
            Component::Normal(name) => Some(fold(&name.to_string_lossy(), windows)),
            _ => None,
        })
        .collect()
}

fn glob(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some(('?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

/// Whether `pattern` (names, `**` for any number) is the whole of `names`.
fn fits(pattern: &[String], names: &[String]) -> bool {
    match pattern.split_first() {
        None => names.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=names.len()).any(|skip| fits(rest, &names[skip..]))
        }
        Some((first, rest)) => {
            let (pattern_chars, name_chars): (Vec<char>, Vec<char>) = (
                first.chars().collect(),
                names
                    .first()
                    .map(|n| n.chars().collect())
                    .unwrap_or_default(),
            );
            !names.is_empty() && glob(&pattern_chars, &name_chars) && fits(rest, &names[1..])
        }
    }
}

impl Protected {
    /// The list `lines` write; a blank line and a `#` line are nothing. `windows` compares names whatever their case.
    #[must_use]
    pub fn new<S: AsRef<str>>(lines: impl IntoIterator<Item = S>, windows: bool) -> Self {
        let mut rules = Vec::new();
        for line in lines {
            let mut text = line.as_ref().trim().replace('\\', "/");
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let allow = text.starts_with('!');
            if allow {
                text.remove(0);
            }
            let (anchor, rest) = if let Some(rest) = text.strip_prefix("~/") {
                (Anchor::Home, rest.to_string())
            } else if text.starts_with('/') {
                (Anchor::Root, text.trim_start_matches('/').to_string())
            } else if text.as_bytes().get(1) == Some(&b':')
                && text.as_bytes()[0].is_ascii_alphabetic()
            {
                (Anchor::Root, text.clone())
            } else {
                (Anchor::Anywhere, text.clone())
            };
            let names: Vec<String> = rest
                .split('/')
                .filter(|name| !name.is_empty() && *name != ".")
                .map(|name| fold(name, windows))
                .collect();
            if !names.is_empty() {
                rules.push(Rule {
                    allow,
                    anchor,
                    names,
                });
            }
        }
        Self { rules, windows }
    }

    /// This crate's recommendations, with `extra` lines (the agent directory of a launch, which is not in the home directory)
    /// after them.
    #[must_use]
    pub fn recommended<S: AsRef<str>>(extra: impl IntoIterator<Item = S>, windows: bool) -> Self {
        let lines: Vec<String> = RECOMMENDED
            .iter()
            .map(|line| (*line).to_string())
            .chain(extra.into_iter().map(|line| line.as_ref().to_string()))
            .collect();
        Self::new(lines, windows)
    }

    /// Whether the last line that matches `path` (already worked out; `home` is the directory `~/` means) protects it.
    #[must_use]
    pub fn protects(&self, path: &Path, home: Option<&Path>) -> bool {
        let names = names_of(path, self.windows);
        let home = home.map(|home| names_of(home, self.windows));
        let mut protected = false;
        for rule in &self.rules {
            let hit = (1..=names.len()).any(|end| match rule.anchor {
                Anchor::Anywhere => (0..end).any(|start| fits(&rule.names, &names[start..end])),
                Anchor::Root => fits(&rule.names, &names[..end]),
                Anchor::Home => home.as_ref().is_some_and(|home| {
                    end > home.len()
                        && names[..home.len()] == home[..]
                        && fits(&rule.names, &names[home.len()..end])
                }),
            });
            if hit {
                protected = !rule.allow;
            }
        }
        protected
    }

    /// The paths the list names outright (no wildcard, relative to the home directory or the file system): a search from a
    /// directory above one of them would reach it. A pattern that names a place anywhere is not one.
    #[must_use]
    pub fn places(&self, home: Option<&Path>) -> Vec<std::path::PathBuf> {
        let wild = |name: &String| name.contains(['*', '?']);
        self.rules
            .iter()
            .filter(|rule| !rule.allow && !rule.names.iter().any(wild))
            .filter_map(|rule| {
                let mut path = match rule.anchor {
                    Anchor::Home => home?.to_path_buf(),
                    Anchor::Root if cfg!(windows) => std::path::PathBuf::new(),
                    Anchor::Root => std::path::PathBuf::from("/"),
                    Anchor::Anywhere => return None,
                };
                for (at, name) in rule.names.iter().enumerate() {
                    if at == 0 && rule.anchor == Anchor::Root && name.ends_with(':') {
                        path.push(alloc::format!("{name}\\"));
                    } else {
                        path.push(name);
                    }
                }
                Some(path)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { "C:\\h" } else { "/h" })
    }

    fn at(rest: &str) -> PathBuf {
        let mut path = home();
        for name in rest.split('/') {
            path.push(name);
        }
        path
    }

    fn list(lines: &[&str]) -> Protected {
        Protected::new(lines.iter().copied(), cfg!(windows))
    }

    #[test]
    fn a_pattern_covers_what_is_under_it_and_a_later_line_wins() {
        let rules = list(&["~/.ssh", "!~/.ssh/known_hosts", "~/.ssh/known_hosts2"]);
        assert!(rules.protects(&at(".ssh/id_rsa"), Some(&home())));
        assert!(rules.protects(&at(".ssh"), Some(&home())));
        assert!(!rules.protects(&at(".ssh/known_hosts"), Some(&home())));
        assert!(rules.protects(&at(".ssh/known_hosts2"), Some(&home())));
        assert!(!rules.protects(&at(".sshx/id"), Some(&home())));
        assert!(
            !rules.protects(&at("other/.ssh"), Some(&home())),
            "anchored at the home directory"
        );
    }

    #[test]
    fn a_pattern_without_an_anchor_matches_anywhere_and_wildcards_work() {
        let rules = list(&["# secrets", "", ".env", "keys/*.pem", "**/vault/**/token"]);
        assert!(rules.protects(&at("a/b/.env"), Some(&home())));
        assert!(rules.protects(&at("a/keys/x.pem"), Some(&home())));
        assert!(!rules.protects(&at("a/keys/x.txt"), Some(&home())));
        assert!(rules.protects(&at("p/vault/q/r/token"), Some(&home())));
        assert!(!rules.protects(&at("p/vault/q/r/tokens"), Some(&home())));
    }

    #[test]
    fn what_is_not_in_the_list_is_not_protected() {
        let rules = list(&[]);
        assert!(!rules.protects(&at(".ssh/id_rsa"), Some(&home())));
        let rules = list(&["~/x"]);
        assert!(
            !rules.protects(&at("x"), None),
            "no home directory, no `~/` entry"
        );
    }

    #[test]
    fn the_recommended_list_is_a_list_a_person_can_cut() {
        let all = Protected::recommended(Vec::<String>::new(), cfg!(windows));
        assert!(all.protects(&at(".ssh/id_rsa"), Some(&home())));
        assert!(all.protects(&at(".codex/auth.json"), Some(&home())));
        assert!(all.protects(&at(".pi/agent/models.json"), Some(&home())));
        // The person keeps what they want of it and adds an entry of their own.
        let mine = list(&["~/.codex/auth.json", "~/work/secrets"]);
        assert!(!mine.protects(&at(".ssh/id_rsa"), Some(&home())));
        assert!(mine.protects(&at(".codex/auth.json"), Some(&home())));
        assert!(mine.protects(&at("work/secrets/a"), Some(&home())));
        // Or allows one entry of the recommendation.
        let mut lines: Vec<String> = RECOMMENDED.iter().map(|l| (*l).to_string()).collect();
        lines.push("!~/.ssh/config".to_string());
        let kept = Protected::new(lines, cfg!(windows));
        assert!(!kept.protects(&at(".ssh/config"), Some(&home())));
        assert!(kept.protects(&at(".ssh/id_rsa"), Some(&home())));
    }

    #[test]
    fn the_places_a_list_names_outright_are_found() {
        let rules = list(&["~/.ssh", "~/a/*", "!~/b", ".env"]);
        assert_eq!(rules.places(Some(&home())), vec![at(".ssh")]);
    }
}
