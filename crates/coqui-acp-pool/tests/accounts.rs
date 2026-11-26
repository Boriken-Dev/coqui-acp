//! The pure parts of account pools, the pool checks, the cache and the memory, over stand-in accounts.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

use std::collections::BTreeMap;

use coqui_acp_pool::accounts::{
    AUTOMATIC, Declared, EntryMember, EntryPool, Provides, automatic_name, automatic_standard,
    check_one, listed, listing, misfit, worth_listing,
};
use coqui_acp_pool::{Cache, Known, Memory, PoolIssue, Remembered, Settings, check_pool};

/// Accounts by name: a kind and the standards it provides; and the standards the harness logins provide.
struct Accounts {
    kinds: BTreeMap<&'static str, (&'static str, Vec<&'static str>)>,
    order: Vec<&'static str>,
    logins: Vec<&'static str>,
}

impl Accounts {
    fn new(
        accounts: &[(&'static str, &'static str, &[&'static str])],
        logins: &[&'static str],
    ) -> Self {
        Self {
            kinds: accounts
                .iter()
                .map(|(name, kind, st)| (*name, (*kind, st.to_vec())))
                .collect(),
            order: accounts.iter().map(|(name, ..)| *name).collect(),
            logins: logins.to_vec(),
        }
    }
}

impl Provides for Accounts {
    fn accounts(&self) -> Vec<String> {
        self.order.iter().map(|name| (*name).to_owned()).collect()
    }

    fn kind_of(&self, account: &str) -> Option<(String, Vec<String>)> {
        self.kinds.get(account).map(|(kind, st)| {
            (
                (*kind).to_owned(),
                st.iter().map(|each| (*each).to_owned()).collect(),
            )
        })
    }

    fn logins(&self) -> Vec<String> {
        self.logins.iter().map(|each| (*each).to_owned()).collect()
    }
}

fn world() -> Accounts {
    Accounts::new(
        &[
            ("api-one", "anthropic-api-key", &["anthropic-api"]),
            ("api-two", "anthropic-api-key", &["anthropic-api"]),
            ("login", "claude-login", &["claude-code"]),
            ("openai", "codex-api-key", &["openai-api"]),
        ],
        &["claude-code", "codex"],
    )
}

fn names(accounts: &[String]) -> Vec<&str> {
    accounts.iter().map(String::as_str).collect()
}

#[test]
fn a_pool_holds_accounts_that_provide_its_standard_and_refuses_one_that_does_not() {
    let keys = Declared::new(
        "keys",
        "anthropic-api",
        vec!["api-one".into(), "api-two".into()],
    );
    check_one(&world(), &keys).expect("both provide it");
    let mixed = Declared::new(
        "mixed",
        "anthropic-api",
        vec!["api-one".into(), "login".into()],
    );
    let why = check_one(&world(), &mixed).expect_err("a login is not an API key");
    assert!(
        why.contains("pool mixed")
            && why.contains("login")
            && why.contains("claude-login")
            && why.contains("claude-code")
            && why.contains("anthropic-api"),
        "{why}"
    );
    let none = Accounts::new(&[("x", "pi-api-key", &[])], &[]);
    let why = misfit(&none, "anthropic-api", "x").expect_err("provides none");
    assert!(why.contains("provides none"), "{why}");
    let why = misfit(&world(), "anthropic-api", "ghost").expect_err("no such account");
    assert!(why.contains("ghost is not an account"), "{why}");
}

#[test]
fn the_automatic_local_account_provides_the_standards_of_the_harness_logins() {
    misfit(&world(), "claude-code", "local").expect("a login of Claude Code");
    let why = misfit(&world(), "anthropic-api", "local").expect_err("no login is an API key");
    assert!(
        why.contains("local") && why.contains("harness-login"),
        "{why}"
    );
}

#[test]
fn the_host_makes_an_any_pool_of_each_standard_and_it_follows_the_accounts() {
    let pools = listing(&[], &world());
    let by = |name: &str| pools.iter().find(|pool| pool.name == name).expect("pool");
    assert!(by("any-anthropic-api").automatic);
    assert_eq!(
        names(&by("any-anthropic-api").accounts),
        ["api-one", "api-two"]
    );
    assert_eq!(names(&by("any-claude-code").accounts), ["local", "login"]);
    assert_eq!(names(&by("any-openai-api").accounts), ["openai"]);
    assert_eq!(names(&by("any-codex").accounts), ["local"]);
    // An account that is gone is out of it, and a standard nobody provides has no pool.
    let one = Accounts::new(&[("api-one", "anthropic-api-key", &["anthropic-api"])], &[]);
    let pools = listing(&[], &one);
    assert_eq!(names(&pools[0].accounts), ["api-one"]);
    assert!(pools.iter().all(|pool| pool.name != "any-openai-api"));
}

#[test]
fn an_any_pool_is_listed_only_with_two_accounts_and_a_declared_pool_always() {
    assert!(worth_listing(false, 1) && !worth_listing(true, 1) && worth_listing(true, 2));
    let alone = Accounts::new(&[], &["pi"]);
    assert!(
        listing(&[], &alone)
            .iter()
            .any(|pool| pool.name == "any-pi")
    );
    assert!(listed(&[], &alone).is_empty());
    let own = Declared::new("mine", "anthropic-api", vec!["api-one".into()]);
    let shown: Vec<String> = listed(&[own], &world())
        .into_iter()
        .map(|pool| pool.name)
        .collect();
    assert_eq!(shown[0], "mine");
    assert!(
        shown.contains(&"any-claude-code".to_owned())
            && !shown.contains(&"any-openai-api".to_owned())
    );
}

#[test]
fn automatic_names_round_trip_and_only_a_standard_makes_one() {
    assert_eq!(AUTOMATIC, "any-");
    assert_eq!(automatic_name("codex"), "any-codex");
    assert_eq!(automatic_standard("any-codex"), Some("codex"));
    assert_eq!(automatic_standard("any-Codex"), None);
    assert_eq!(automatic_standard("mine"), None);
}

#[test]
fn an_entry_offers_the_members_it_lists_and_a_grant_for_accounts_reaches_the_rest() {
    let pool = EntryPool::new(
        "any-claude-code",
        "claude-code",
        (true, false),
        vec![
            EntryMember::new("local", true),
            EntryMember::new("login", false),
        ],
    );
    let usable = |any| {
        pool.usable(any)
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(usable(false), ["local"]);
    assert_eq!(usable(true), ["local", "login"]);
    assert!(
        pool.open_to(false),
        "the host's own pool is open to every connection"
    );
    let named = EntryPool::new(
        "keys",
        "anthropic-api",
        (false, true),
        vec![EntryMember::new("a", false)],
    );
    assert_eq!(named.usable(false).count(), 1);
    let unnamed = EntryPool::new(
        "keys",
        "anthropic-api",
        (false, false),
        vec![EntryMember::new("a", false)],
    );
    assert!(!unnamed.open_to(false) && unnamed.open_to(true) && unnamed.usable(false).count() == 0);
}

struct Names {
    accounts: Vec<&'static str>,
}

impl Known for Names {
    fn taken(&self, name: &str) -> bool {
        name == "local" || self.accounts.contains(&name)
    }

    fn member(&self, name: &str) -> bool {
        name == "local" || self.accounts.contains(&name)
    }
}

#[test]
fn a_pool_definition_is_checked_in_order() {
    let known = Names {
        accounts: vec!["one", "two"],
    };
    let settings = Settings::new();
    let check = |name: &str, standard: Option<&str>, members: &[&str]| {
        check_pool(name, standard, members, &known, &settings)
    };
    assert_eq!(
        check("keys", Some("anthropic-api"), &["one", "local"]),
        Ok(())
    );
    assert_eq!(
        check("", None, &[]),
        Err(PoolIssue::InvalidLabel { max: 64 })
    );
    assert!(matches!(
        check("a\u{7}b", None, &[]),
        Err(PoolIssue::InvalidLabel { .. })
    ));
    assert!(matches!(
        check(&"x".repeat(65), None, &[]),
        Err(PoolIssue::InvalidLabel { .. })
    ));
    assert_eq!(
        check("one", None, &[]),
        Err(PoolIssue::NameTaken("one".into()))
    );
    assert_eq!(
        check("local", None, &[]),
        Err(PoolIssue::NameTaken("local".into()))
    );
    assert_eq!(
        check("any-codex", Some("codex"), &[]),
        Err(PoolIssue::AutomaticPool("any-codex".into()))
    );
    // A role has no standard, so the host's pool names do not apply to it.
    assert_eq!(check("any-codex", None, &["one"]), Ok(()));
    assert_eq!(
        check("p", Some("Bad Standard"), &[]),
        Err(PoolIssue::InvalidStandard)
    );
    assert_eq!(
        check("p", None, &["three"]),
        Err(PoolIssue::UnknownMember("three".into()))
    );
    assert_eq!(
        check("p", None, &["one", "two", "one"]),
        Err(PoolIssue::DuplicateMember("one".into()))
    );
    let tight = Settings::new().max_members(1).max_name_chars(3);
    assert_eq!(
        check_pool("p", None, &["one", "two"], &known, &tight),
        Err(PoolIssue::TooManyMembers { max: 1 })
    );
    assert_eq!(
        check_pool("four", None, &[] as &[&str], &known, &tight),
        Err(PoolIssue::InvalidLabel { max: 3 })
    );
    assert!(
        PoolIssue::DuplicateMember("one".into())
            .to_string()
            .contains("one is named twice")
    );
}

#[test]
fn a_cache_entry_stands_for_its_life_and_the_oldest_go_first() {
    let mut cache = Cache::new(60);
    cache.put("a", 1, 100);
    assert_eq!(cache.get("a", 159), Some(&1));
    assert_eq!(cache.get("a", 160), None);
    assert_eq!(cache.get("b", 100), None);
    let mut small = Cache::new(60).max_entries(2);
    small.put("x", 1, 1);
    small.put("y", 2, 2);
    small.put("z", 3, 3);
    assert_eq!(
        (small.len(), small.get("x", 3), small.get("z", 3)),
        (2, None, Some(&3))
    );
    assert_eq!(small.forget(), 2);
    assert!(small.is_empty());
}

#[test]
fn a_shared_cache_and_memory_work_through_a_lock() {
    let shared = coqui_acp_pool::SharedCache::new(10);
    shared.put("k", String::from("v"), 0);
    assert_eq!(shared.get("k", 9).as_deref(), Some("v"));
    assert_eq!(shared.forget(), 1);
    let memory = coqui_acp_pool::SharedMemory::new();
    memory.with(|memory| memory.took("p", "a", 5));
    assert_eq!(
        memory.with(|memory| memory.last("p").map(str::to_owned)),
        Some("a".to_owned())
    );
}

#[test]
fn memory_survives_an_export_and_a_restore() {
    let mut memory = Memory::new();
    memory.took("p", "a", 10);
    memory.touch("p", "b", 20);
    assert_eq!(
        (memory.last("p"), memory.last_used("p", "b")),
        (Some("a"), Some(20))
    );
    let again = Memory::restore(memory.export());
    assert_eq!(again, memory);
    let mut other = Remembered::new("q");
    other.last = Some("z".into());
    assert_eq!(Memory::restore([other]).last("q"), Some("z"));
    memory.forget("p");
    assert_eq!(memory.last("p"), None);
}
