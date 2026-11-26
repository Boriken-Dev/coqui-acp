//! The strategies, the caps, unknown usage, stop points, holds and the order a session moves in.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

mod support;

use core::time::Duration;

use coqui_acp_api::{CacheLifetime, Coverage, Status};
use coqui_acp_pool::{
    Ask, Fit, Hold, Member, Memory, Pool, Reason, Settings, Standing, Strategy, Usage, Why, decide,
    fits,
};
use support::{NOW, exhausted, pool_of, refused, report, take, used};

fn start() -> Ask<'static> {
    Ask::new(NOW)
}

fn on(member: &str) -> Ask<'_> {
    Ask::new(NOW)
        .current(member)
        .idle(Duration::from_secs(5))
        .lifetime(CacheLifetime::Long)
}

fn plain(count: usize) -> Vec<Standing> {
    (0..count).map(|_| Standing::new()).collect()
}

#[test]
fn priority_is_the_default_and_with_no_priorities_it_is_the_listed_order() {
    assert_eq!(Pool::<()>::new("p").strategy, Strategy::Priority);
    let pool = pool_of("p", Strategy::Priority, &["a", "b", "c"]);
    let mut memory = Memory::new();
    for _ in 0..3 {
        assert_eq!(take(&pool, &mut memory, &plain(3), &start()).member, "a");
    }
}

#[test]
fn priority_takes_the_highest_first_and_equal_ones_in_the_listed_order() {
    let pool = Pool::new("p")
        .member(Member::new("low", ()).priority(-1))
        .member(Member::new("first", ()).priority(5))
        .member(Member::new("zero", ()))
        .member(Member::new("second", ()).priority(5));
    let mut memory = Memory::new();
    assert_eq!(
        take(&pool, &mut memory, &plain(4), &start()).member,
        "first"
    );
    // With the first out of usage the next of equal priority goes, then the unprioritised.
    let mut standings = plain(4);
    standings[1] = exhausted(NOW + 60);
    assert_eq!(
        take(&pool, &mut memory, &standings, &start()).member,
        "second"
    );
    standings[3] = exhausted(NOW + 60);
    assert_eq!(
        take(&pool, &mut memory, &standings, &start()).member,
        "zero"
    );
}

#[test]
fn a_session_stays_on_its_member_while_it_can_serve_and_a_new_one_takes_the_earlier() {
    let pool = pool_of("p", Strategy::Priority, &["a", "b"]);
    let mut memory = Memory::new();
    // `a` has refilled; the session on `b` stays, and a new session takes `a`.
    let kept = take(&pool, &mut memory, &plain(2), &on("b"));
    assert_eq!(
        (kept.member.as_str(), kept.moved, kept.why),
        ("b", false, Why::Kept)
    );
    assert_eq!(take(&pool, &mut memory, &plain(2), &start()).member, "a");
    // A session that does not stick takes the strategy's choice again.
    let loose = take(&pool, &mut memory, &plain(2), &on("b").stick(false));
    assert_eq!(
        (loose.member.as_str(), loose.moved, loose.why),
        ("a", true, Why::InTurn)
    );
    // When its member runs out it moves, saying why.
    let standings = [Standing::new(), exhausted(NOW + 60)];
    let moved = take(&pool, &mut memory, &standings, &on("b"));
    assert_eq!((moved.member.as_str(), moved.why), ("a", Why::Exhausted));
    assert_eq!(moved.left.as_deref(), Some("five_hour is used up"));
}

#[test]
fn headroom_takes_the_member_with_the_least_used() {
    let pool = pool_of("p", Strategy::Headroom, &["a", "b", "c"]);
    let standings = [used(700), used(200), used(450)];
    let chosen = take(&pool, &mut Memory::new(), &standings, &start());
    assert_eq!(chosen.member, "b");
    assert!(chosen.usage_known);
}

#[test]
fn headroom_counts_unknown_usage_as_five_hundred_permille_by_setting() {
    let pool = pool_of(
        "p",
        Strategy::Headroom,
        &["known_high", "unknown", "known_low"],
    );
    let standings = [used(600), Standing::new(), used(400)];
    let mut memory = Memory::new();
    assert_eq!(
        take(&pool, &mut memory, &standings, &start()).member,
        "known_low"
    );
    // Without the low one, unknown (500) beats 600 and is not marked known.
    let standings = [used(600), Standing::new(), used(900)];
    let chosen = take(&pool, &mut memory, &standings, &start());
    assert_eq!(
        (chosen.member.as_str(), chosen.usage_known),
        ("unknown", false)
    );
    // At 300 the unknown member beats a 400 one; at 700 it loses to it.
    let low = pool.clone().settings(Settings::new().unknown_permille(300));
    let standings = [used(600), Standing::new(), used(400)];
    assert_eq!(
        take(&low, &mut memory, &standings, &start()).member,
        "unknown"
    );
    let high = pool.settings(Settings::new().unknown_permille(700));
    assert_eq!(
        take(&high, &mut memory, &standings, &start()).member,
        "known_low"
    );
}

#[test]
fn headroom_breaks_ties_by_active_runs_then_least_recently_used_then_priority_then_order() {
    let pool = Pool::new("p")
        .strategy(Strategy::Headroom)
        .member(Member::new("a", ()))
        .member(Member::new("b", ()))
        .member(Member::new("c", ()).priority(9));
    let mut memory = Memory::new();
    // Equal use: the one with fewest runs.
    let standings = [
        used(100).active(2),
        used(100).active(1),
        used(100).active(1),
    ];
    // `b` and `c` tie on runs; neither was used, so priority takes `c`.
    assert_eq!(take(&pool, &mut memory, &standings, &start()).member, "c");
    // Least recently used comes before priority.
    let standings = [
        used(100).last_used(NOW - 10),
        used(100).last_used(NOW - 500),
        used(100).last_used(NOW - 5),
    ];
    assert_eq!(take(&pool, &mut memory, &standings, &start()).member, "b");
    // Never used counts as least recent; then the listed order decides between equals.
    let plain_pool = pool_of("q", Strategy::Headroom, &["a", "b"]);
    assert_eq!(
        take(
            &plain_pool,
            &mut Memory::new(),
            &[used(100), used(100)],
            &start()
        )
        .member,
        "a"
    );
}

#[test]
fn headroom_remembers_what_it_gave_so_equal_members_take_turns() {
    let pool = pool_of("p", Strategy::Headroom, &["a", "b"]);
    let mut memory = Memory::new();
    let standings = [used(100), used(100)];
    let order: Vec<String> = (0..4)
        .map(|at| {
            let mut ask = start();
            ask.now = NOW + at;
            take(&pool, &mut memory, &standings, &ask).member
        })
        .collect();
    assert_eq!(order, ["a", "b", "a", "b"]);
}

#[test]
fn the_most_used_bucket_that_covers_the_model_is_the_member_use() {
    let pool = pool_of("p", Strategy::Headroom, &["a", "b"]);
    let mut two = report(Status::Ok, Coverage::All, Some(NOW + 10), Some((100, 1000)));
    let mut weekly = report(
        Status::Ok,
        Coverage::All,
        Some(NOW + 100),
        Some((800, 1000)),
    )
    .buckets;
    weekly[0].id = "weekly".into();
    two.buckets.append(&mut weekly);
    let a = Standing::new().usage(Usage::Known(two));
    // `a` reads 800 and `b` 500: `b` has more headroom.
    let b = used(500);
    assert_eq!(
        take(&pool, &mut Memory::new(), &[a, b], &start()).member,
        "b"
    );
}

#[test]
fn a_bucket_that_names_other_models_is_no_usage_for_the_asked_one() {
    let pool = pool_of("p", Strategy::Headroom, &["a", "b"]);
    let mut other = report(
        Status::Ok,
        Coverage::Only {
            models: vec!["opus".into()],
        },
        None,
        Some((990, 1000)),
    );
    other.buckets[0].id = "opus_only".into();
    let a = Standing::new().usage(Usage::Known(other));
    let mut ask = start();
    ask.model = Some("sonnet");
    // `a` has nothing for sonnet: unknown, 500, so `b` at 600 loses to it.
    let chosen = take(&pool, &mut Memory::new(), &[a, used(600)], &ask);
    assert_eq!((chosen.member.as_str(), chosen.usage_known), ("a", false));
}

#[test]
fn round_robin_takes_each_member_once_a_round_and_skips_those_that_cannot_serve() {
    let pool = pool_of("p", Strategy::RoundRobin, &["a", "b", "c"]);
    let mut memory = Memory::new();
    let mut standings = plain(3);
    let first: Vec<String> = (0..3)
        .map(|_| take(&pool, &mut memory, &standings, &start()).member)
        .collect();
    assert_eq!(first, ["a", "b", "c"]);
    standings[1] = exhausted(NOW + 60);
    let next: Vec<String> = (0..3)
        .map(|_| take(&pool, &mut memory, &standings, &start()).member)
        .collect();
    assert_eq!(next, ["a", "c", "a"]);
}

#[test]
fn a_member_at_its_cap_of_runs_is_left_and_the_pool_default_applies_to_the_rest() {
    let pool = Pool::new("p")
        .member(Member::new("capped", ()).max_concurrent(1))
        .member(Member::new("open", ()));
    let mut memory = Memory::new();
    let standings = [Standing::new().active(1), Standing::new().active(50)];
    let chosen = take(&pool, &mut memory, &standings, &start());
    assert_eq!(chosen.member, "open");
    assert!(
        chosen
            .skipped
            .iter()
            .any(|skipped| skipped.member == "capped"
                && matches!(skipped.reason, Reason::AtCapacity { active: 1, max: 1 })),
        "{:?}",
        chosen.skipped
    );
    // A cap in the settings holds the members that name none.
    let capped = pool.settings(Settings::new().max_concurrent(Some(2)));
    let standings = [Standing::new().active(0), Standing::new().active(2)];
    assert_eq!(
        take(&capped, &mut memory, &standings, &start()).member,
        "capped"
    );
    let standings = [Standing::new().active(1), Standing::new().active(2)];
    let why = refused(&capped, &standings, &start()).message("member");
    assert!(
        why.contains("capped: 1 of 1 runs are in use") && why.contains("open: 2 of 2"),
        "{why}"
    );
    assert!(!why.contains("refills"), "{why}");
}

#[test]
fn a_session_on_a_member_at_its_cap_counts_only_the_other_runs() {
    // The host reports runs other than the asking session's, so a session alone on its member is not held out of it.
    let pool = Pool::new("p").member(Member::new("only", ()).max_concurrent(1));
    let chosen = take(
        &pool,
        &mut Memory::new(),
        &[Standing::new().active(0)],
        &on("only"),
    );
    assert_eq!(chosen.why, Why::Kept);
}

#[test]
fn a_member_stops_at_its_own_stop_point_while_the_window_is_open() {
    let pool = Pool::new("p")
        .member(Member::new("careful", ()).stop_permille(800))
        .member(Member::new("plain", ()));
    let mut memory = Memory::new();
    let standings = [used(850), used(850)];
    let chosen = take(&pool, &mut memory, &standings, &start());
    assert_eq!(chosen.member, "plain");
    let skipped = &chosen.skipped[0];
    assert_eq!(
        skipped.reason.text(),
        "five_hour is at 850 permille, its stop point is 800"
    );
    // Just under it, it serves; a pool-wide stop point applies to members that name none.
    assert_eq!(
        take(&pool, &mut memory, &[used(799), used(850)], &start()).member,
        "careful"
    );
    let all = pool.settings(Settings::new().stop_permille(900));
    let why = refused(&all, &[used(900), used(900)], &start()).message("member");
    assert!(
        why.contains("plain: five_hour is at 900 permille")
            || why.contains("plain: five_hour is used up"),
        "{why}"
    );
    // A full bucket stops with the default, and one whose window ended is a refilled one.
    let default = pool_of("d", Strategy::Priority, &["a", "b"]);
    assert_eq!(
        take(
            &default,
            &mut Memory::new(),
            &[used(1000), used(10)],
            &start()
        )
        .member,
        "b"
    );
    let ended = Standing::new().usage(Usage::Known(report(
        Status::Ok,
        Coverage::All,
        Some(NOW - 1),
        Some((1000, 1000)),
    )));
    assert_eq!(
        take(&default, &mut Memory::new(), &[ended, used(10)], &start()).member,
        "a"
    );
}

#[test]
fn unknown_usage_leaves_a_member_usable_and_is_marked() {
    let pool = pool_of("p", Strategy::Priority, &["a"]);
    let chosen = take(&pool, &mut Memory::new(), &[Standing::new()], &start());
    assert!(!chosen.usage_known && !chosen.usage_pending);
    // Pending is usable too, and says the report is still to come.
    let pending = Standing::new().usage(Usage::Pending);
    let chosen = take(&pool, &mut Memory::new(), &[pending], &start());
    assert!(!chosen.usage_known && chosen.usage_pending);
    // A report with a bucket that covers it makes it known.
    assert!(take(&pool, &mut Memory::new(), &[used(10)], &start()).usage_known);
}

#[test]
fn a_hold_keeps_a_member_back_until_it_lets_go() {
    let pool = pool_of("p", Strategy::Priority, &["a", "b"]);
    let held = Standing::new().hold(Hold::new("spending limit").until(NOW + 600));
    let mut memory = Memory::new();
    assert_eq!(
        take(
            &pool,
            &mut memory,
            &[held.clone(), Standing::new()],
            &start()
        )
        .member,
        "b"
    );
    let mut later = start();
    later.now = NOW + 601;
    assert_eq!(
        take(&pool, &mut memory, &[held.clone(), Standing::new()], &later).member,
        "a"
    );
    // With nothing else, the refusal says when, as for an allowance.
    let one = pool_of("p", Strategy::Priority, &["a"]);
    let none = refused(&one, &[held], &start());
    assert_eq!(none.soonest, Some(NOW + 600));
    assert!(none.message("member").contains("a: spending limit"));
    let endless = Standing::new().hold(Hold::new("stopped"));
    assert_eq!(refused(&one, &[endless], &start()).soonest, None);
}

#[test]
fn a_move_prefers_the_group_of_the_member_it_leaves_whatever_the_order() {
    let pool = Pool::new("p")
        .member(Member::new("codex", ()).group("codex").priority(9))
        .member(Member::new("claude_a", ()).group("claude"))
        .member(Member::new("claude_b", ()).group("claude"));
    let mut memory = Memory::new();
    let standings = [Standing::new(), exhausted(NOW + 60), Standing::new()];
    let moved = take(&pool, &mut memory, &standings, &on("claude_a"));
    assert_eq!(
        (moved.member.as_str(), moved.same_group),
        ("claude_b", true)
    );
    // With the whole group out, another group takes it, and says so.
    let standings = [Standing::new(), exhausted(NOW + 60), exhausted(NOW + 60)];
    let moved = take(&pool, &mut memory, &standings, &on("claude_a"));
    assert_eq!((moved.member.as_str(), moved.same_group), ("codex", false));
    // A new session has no group to prefer.
    assert_eq!(
        take(&pool, &mut memory, &plain(3), &start()).member,
        "codex"
    );
}

#[test]
fn the_group_preference_holds_under_each_strategy() {
    for strategy in [Strategy::Priority, Strategy::Headroom, Strategy::RoundRobin] {
        let pool = Pool::new("p")
            .strategy(strategy)
            .member(Member::new("x", ()).group("one").priority(1))
            .member(Member::new("y", ()).group("two").priority(9))
            .member(Member::new("z", ()).group("one"));
        let standings = [exhausted(NOW + 60), Standing::new(), Standing::new()];
        let moved = take(&pool, &mut Memory::new(), &standings, &on("x"));
        assert_eq!(moved.member, "z", "{strategy}");
    }
}

#[test]
fn a_current_member_the_pool_does_not_hold_is_a_start() {
    // A current member the pool does not hold is a session that starts.
    let pool = pool_of("p", Strategy::Priority, &["a"]);
    let chosen = take(&pool, &mut Memory::new(), &plain(1), &on("gone"));
    assert_eq!(chosen.why, Why::Start);
}

#[test]
fn the_refusal_lists_each_member_in_order_with_its_reason_and_the_soonest_refill() {
    let pool = pool_of("p", Strategy::Priority, &["a", "b", "c"]);
    let standings = [
        Standing::new().unavailable("not signed in"),
        exhausted(NOW + 5000),
        exhausted(NOW + 100),
    ];
    let none = refused(&pool, &standings, &start());
    let members: Vec<&str> = none
        .reasons
        .iter()
        .map(|each| each.member.as_str())
        .collect();
    assert_eq!(members, ["a", "b", "c"]);
    assert_eq!(none.soonest, Some(NOW + 100));
    assert!(matches!(none.reasons[0].reason, Reason::Unavailable(_)));
    assert_eq!(none.to_string(), none.message("member"));
}

#[test]
fn a_standing_missing_from_the_list_counts_as_a_member_with_nothing_against_it() {
    let pool = pool_of("p", Strategy::Priority, &["a", "b"]);
    assert_eq!(take(&pool, &mut Memory::new(), &[], &start()).member, "a");
    let chosen = decide(&pool, &mut Memory::new(), &[exhausted(NOW + 1)], &start()).expect("b");
    assert_eq!(chosen.member, "b");
}

#[test]
fn needs_are_checked_against_the_capabilities_a_model_reports() {
    assert_eq!(fits(&["chat"], &["chat", "tool_use"]), Fit::Yes);
    assert_eq!(
        fits(&["chat", "embed"], &["chat"]),
        Fit::No(vec!["embed".to_owned()])
    );
    assert_eq!(fits(&["chat"], &[] as &[&str]), Fit::Unreported);
    assert!(Fit::Unreported.able() && !Fit::No(vec![]).able());
    assert_eq!(fits(&[] as &[&str], &[] as &[&str]), Fit::Yes);
}

#[test]
fn a_changed_setting_is_visible() {
    assert!(Settings::new().non_defaults().is_empty());
    let changed = Settings::new()
        .unknown_permille(300)
        .max_concurrent(Some(2));
    let names: Vec<String> = changed
        .non_defaults()
        .into_iter()
        .map(|each| each.setting)
        .collect();
    assert_eq!(names, ["unknown_permille", "max_concurrent"]);
    assert_eq!("round_robin".parse::<Strategy>(), Ok(Strategy::RoundRobin));
    assert!("fastest".parse::<Strategy>().is_err());
    assert_eq!(Strategy::Headroom.to_string(), "headroom");
}
