//! Canoa's account-pool policy, moved: which account of a pool takes a turn, with stand-in accounts and stand-in usage. The pool is
//! round-robin with a warm-cache keep, as account pools are.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

mod support;

use core::time::Duration;

use coqui_acp_api::{CacheLifetime, Coverage, Refill, Status};
use coqui_acp_pool::{Ask, Memory, Standing, Strategy, Usage, Why, after, decide, utc};
use support::{NOW, exhausted, pool_of, refused, reporting, take};

fn rr(name: &str, ids: &[&str]) -> coqui_acp_pool::Pool<()> {
    pool_of(name, Strategy::RoundRobin, ids)
}

fn plain(count: usize) -> Vec<Standing> {
    (0..count).map(|_| Standing::new()).collect()
}

fn ask(current: Option<&str>, idle: Option<u64>, lifetime: CacheLifetime) -> Ask<'_> {
    let mut ask = Ask::new(NOW).lifetime(lifetime);
    if let Some(current) = current {
        ask = ask.current(current);
    }
    if let Some(idle) = idle {
        ask = ask.idle(Duration::from_secs(idle));
    }
    ask
}

#[test]
fn sessions_that_start_take_the_accounts_in_turn() {
    let mut memory = Memory::new();
    let pool = rr("pool", &["a", "b", "c"]);
    let standings = plain(3);
    let start = ask(None, None, CacheLifetime::Long);
    let order: Vec<String> = (0..4)
        .map(|_| take(&pool, &mut memory, &standings, &start).member)
        .collect();
    assert_eq!(order, ["a", "b", "c", "a"]);
    // Another pool has its own turn.
    let other = rr("other", &["a", "b", "c"]);
    assert_eq!(take(&other, &mut memory, &standings, &start).member, "a");
}

#[test]
fn a_warm_cache_keeps_its_account_and_a_cold_one_is_the_next_turns() {
    let mut memory = Memory::new();
    let pool = rr("pool", &["a", "b", "c"]);
    let standings = plain(3);
    let first = take(
        &pool,
        &mut memory,
        &standings,
        &ask(None, None, CacheLifetime::Long),
    );
    assert_eq!((first.member.as_str(), first.why), ("a", Why::Start));
    // Within the long cache's hour the session stays, however often others start.
    let warm = ask(Some("a"), Some(59 * 60), CacheLifetime::Long);
    for _ in 0..3 {
        let kept = take(&pool, &mut memory, &standings, &warm);
        assert_eq!(
            (kept.member.as_str(), kept.moved, kept.why),
            ("a", false, Why::Kept)
        );
    }
    // Past it, the turn goes to the next account, which is a move.
    let cold = ask(Some("a"), Some(61 * 60), CacheLifetime::Long);
    let next = take(&pool, &mut memory, &standings, &cold);
    assert_eq!(
        (next.member.as_str(), next.moved, next.why),
        ("b", true, Why::InTurn)
    );
    // The short cache and the agent's own are about five minutes, and a cache that is off is never warm.
    let short = ask(Some("b"), Some(301), CacheLifetime::Short);
    assert_eq!(take(&pool, &mut memory, &standings, &short).member, "c");
    let default = ask(Some("c"), Some(120), CacheLifetime::Default);
    assert_eq!(take(&pool, &mut memory, &standings, &default).member, "c");
    let off = ask(Some("c"), Some(1), CacheLifetime::Off);
    let rotated = take(&pool, &mut memory, &standings, &off);
    assert_eq!((rotated.member.as_str(), rotated.moved), ("a", true));
}

#[test]
fn an_account_out_of_usage_is_left_and_the_session_moves_on_even_with_a_warm_cache() {
    let mut memory = Memory::new();
    let pool = rr("pool", &["a", "b", "c"]);
    let standings = [exhausted(NOW + 600), Standing::new(), Standing::new()];
    // A new session does not start on it.
    let start = take(
        &pool,
        &mut memory,
        &standings,
        &ask(None, None, CacheLifetime::Long),
    );
    assert_eq!(start.member, "b");
    // A session on it, with a cache that is warm, moves and says what ran out.
    let warm = ask(Some("a"), Some(5), CacheLifetime::Long);
    let moved = take(&pool, &mut memory, &standings, &warm);
    assert_eq!(
        (moved.member.as_str(), moved.moved, moved.why),
        ("c", true, Why::Exhausted)
    );
    assert_eq!(moved.left.as_deref(), Some("five_hour is used up"));
    // A turn a limit refused holds an account back as well, until the time it said, or until the client says it is over.
    let two = rr("limited", &["b", "c"]);
    let limited = [Standing::new().limited_until(NOW + 30), Standing::new()];
    let on_b = ask(Some("b"), Some(5), CacheLifetime::Long);
    assert_eq!(take(&two, &mut memory, &limited, &on_b).member, "c");
    let mut over = on_b;
    over.now = NOW + 31;
    assert_eq!(take(&two, &mut memory, &limited, &over).member, "b");
    let untimed_pool = rr("untimed", &["b", "c"]);
    let untimed = [Standing::new().limited_until(i64::MAX), Standing::new()];
    assert_eq!(
        take(&untimed_pool, &mut memory, &untimed, &on_b).member,
        "c"
    );
}

#[test]
fn only_a_bucket_that_covers_the_model_holds_an_account_back() {
    let opus_only = reporting(
        Status::Exhausted,
        Coverage::Only {
            models: vec!["opus".to_owned()],
        },
        Some(NOW + 600),
    );
    let standings = [opus_only, Standing::new()];
    let turn = |model: Option<&'static str>| {
        let mut turn = ask(None, None, CacheLifetime::Long);
        turn.model = model;
        turn
    };
    let pick = |name: &str, model| {
        take(
            &rr(name, &["a", "b"]),
            &mut Memory::new(),
            &standings,
            &turn(model),
        )
        .member
    };
    // The account is out for opus and in for the other model; with no model chosen nothing says it is out.
    assert_eq!(pick("opus", Some("opus")), "b");
    assert_eq!(pick("sonnet", Some("sonnet")), "a");
    assert_eq!(pick("none", None), "a");
    // A bucket whose coverage is not said covers every model, and so does one that covers all of them.
    for coverage in [Coverage::Unknown, Coverage::All] {
        let out = [
            reporting(Status::Exhausted, coverage, Some(NOW + 600)),
            Standing::new(),
        ];
        let chosen = take(
            &rr("any", &["a", "b"]),
            &mut Memory::new(),
            &out,
            &turn(Some("sonnet")),
        );
        assert_eq!(chosen.member, "b");
    }
    // A bucket that is warning, or ok, or that has refilled since the report, leaves the account in.
    for (status, resets_at) in [
        (Status::Warning, Some(NOW + 600)),
        (Status::Ok, None),
        (Status::Exhausted, Some(NOW - 1)),
    ] {
        let reported = [reporting(status, Coverage::All, resets_at), Standing::new()];
        let chosen = take(
            &rr("in", &["a", "b"]),
            &mut Memory::new(),
            &reported,
            &turn(None),
        );
        assert_eq!(chosen.member, "a");
    }
}

#[test]
fn an_account_that_does_not_list_the_model_or_is_held_is_never_chosen() {
    let mut memory = Memory::new();
    let pool = rr("pool", &["a", "b"]);
    let standings = [
        Standing::new().unavailable("it lists no such model"),
        Standing::new(),
    ];
    for _ in 0..3 {
        let chosen = take(
            &pool,
            &mut memory,
            &standings,
            &ask(None, None, CacheLifetime::Long),
        );
        assert_eq!(chosen.member, "b");
    }
}

#[test]
fn with_every_account_out_the_refusal_names_when_the_soonest_allowance_refills() {
    // 2h10m after `NOW`, which is 2027-01-15 08:00 UTC, is 10:10; the other refills a day later.
    let pool = rr("pool", &["a", "b"]);
    let standings = [exhausted(NOW + 7800), exhausted(NOW + 86_400)];
    let why = refused(&pool, &standings, &ask(None, None, CacheLifetime::Long)).message("account");
    assert!(
        why.contains("pool pool") && why.contains("a: five_hour is used up"),
        "{why}"
    );
    assert!(
        why.contains("in 2 h 10 min") && why.contains("2027-01-15 10:10 UTC"),
        "{why}"
    );
    // An allowance that says no time is said too.
    let single = rr("pool", &["a"]);
    let mut untimed = exhausted(0);
    if let Usage::Known(report) = &mut untimed.usage {
        report.buckets[0].refill = Refill::Unknown;
    }
    let why =
        refused(&single, &[untimed], &ask(None, None, CacheLifetime::Long)).message("account");
    assert!(why.contains("nothing says when"), "{why}");
    // When none lists the model the refusal says that, and not that usage is out.
    let none = [Standing::new().unavailable("it lists no such model")];
    let why = refused(&single, &none, &ask(None, None, CacheLifetime::Long)).message("account");
    assert!(
        why.contains("a: it lists no such model") && !why.contains("refills"),
        "{why}"
    );
    let empty = rr("pool", &[]);
    let why = decide(
        &empty,
        &mut Memory::new(),
        &[],
        &ask(None, None, CacheLifetime::Long),
    )
    .expect_err("none")
    .message("account");
    assert!(why.contains("holds no account"), "{why}");
}

#[test]
fn times_are_said_in_utc() {
    assert_eq!(utc(0), "1970-01-01 00:00 UTC");
    assert_eq!(utc(1_800_000_000), "2027-01-15 08:00 UTC");
    assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(utc(-1), "1969-12-31 23:59 UTC");
    assert_eq!(after(0), "in 0 min");
    assert_eq!(after(61), "in 2 min");
    assert_eq!(after(3600), "in 1 h");
}
