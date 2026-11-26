//! Properties of the decision over generated pools: whatever the strategy, a member that cannot serve is never chosen, and each
//! strategy's order holds.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

mod support;

use coqui_acp_api::{Coverage, Status};
use coqui_acp_pool::{
    Ask, Member, Memory, Pool, Reason, Settings, Standing, Strategy as Pick, Usage, decide,
};
use proptest::prelude::*;
use support::{NOW, report};

#[derive(Debug, Clone)]
struct Spec {
    priority: i32,
    cap: Option<u32>,
    active: u32,
    used: Option<u64>,
    exhausted: bool,
    unavailable: bool,
    group: u8,
}

fn spec() -> impl Strategy<Value = Spec> {
    (
        -3i32..4,
        proptest::option::of(0u32..3),
        0u32..4,
        proptest::option::of(0u64..=1000),
        any::<bool>(),
        any::<bool>(),
        0u8..3,
    )
        .prop_map(
            |(priority, cap, active, used, exhausted, unavailable, group)| Spec {
                priority,
                cap,
                active,
                used,
                exhausted,
                unavailable,
                group,
            },
        )
}

fn build(specs: &[Spec], strategy: Pick) -> (Pool<()>, Vec<Standing>) {
    let mut pool = Pool::new("p").strategy(strategy).settings(Settings::new());
    let mut standings = Vec::new();
    for (at, each) in specs.iter().enumerate() {
        let mut member = Member::new(format!("m{at}"), ())
            .priority(each.priority)
            .group(format!("g{}", each.group));
        if let Some(cap) = each.cap {
            member = member.max_concurrent(cap);
        }
        pool = pool.member(member);
        let mut standing = Standing::new().active(each.active);
        if each.unavailable {
            standing = standing.unavailable("down");
        }
        if each.exhausted {
            standing = standing.usage(Usage::Known(report(
                Status::Exhausted,
                Coverage::All,
                Some(NOW + 60),
                None,
            )));
        } else if let Some(used) = each.used {
            standing = standing.usage(Usage::Known(report(
                Status::Ok,
                Coverage::All,
                Some(NOW + 60),
                Some((used, 1000)),
            )));
        }
        standings.push(standing);
    }
    (pool, standings)
}

/// Whether a member can serve, as the rules say them.
fn can_serve(each: &Spec) -> bool {
    !each.unavailable
        && each.cap.is_none_or(|cap| each.active < cap)
        && !each.exhausted
        && each.used.is_none_or(|used| used < 1000)
}

fn strategies() -> impl Strategy<Value = coqui_acp_pool::Strategy> {
    prop_oneof![
        Just(coqui_acp_pool::Strategy::Priority),
        Just(coqui_acp_pool::Strategy::Headroom),
        Just(coqui_acp_pool::Strategy::RoundRobin),
    ]
}

proptest! {
    #[test]
    fn a_member_that_cannot_serve_is_never_chosen_and_one_that_can_is_always_found(
        specs in proptest::collection::vec(spec(), 0..8),
        strategy in strategies(),
    ) {
        let (pool, standings) = build(&specs, strategy);
        let result = decide(&pool, &mut Memory::new(), &standings, &Ask::new(NOW));
        let any = specs.iter().any(can_serve);
        match result {
            Ok(chosen) => {
                prop_assert!(any);
                prop_assert!(can_serve(&specs[chosen.index]));
                prop_assert_eq!(chosen.skipped.len(), specs.iter().filter(|each| !can_serve(each)).count());
            }
            Err(none) => {
                prop_assert!(!any);
                prop_assert_eq!(none.reasons.len(), specs.len());
                prop_assert!(none.reasons.iter().all(|each| !matches!(&each.reason, Reason::Unavailable(why) if why == "nothing holds it back")));
            }
        }
    }

    #[test]
    fn priority_takes_the_highest_priority_and_the_earliest_of_equals(
        specs in proptest::collection::vec(spec(), 1..8),
    ) {
        let (pool, standings) = build(&specs, Pick::Priority);
        if let Ok(chosen) = decide(&pool, &mut Memory::new(), &standings, &Ask::new(NOW)) {
            let best = specs.iter().filter(|each| can_serve(each)).map(|each| each.priority).max().expect("one serves");
            prop_assert_eq!(specs[chosen.index].priority, best);
            let earliest = specs.iter().position(|each| can_serve(each) && each.priority == best).expect("one");
            prop_assert_eq!(chosen.index, earliest);
        }
    }

    #[test]
    fn headroom_takes_the_least_used_counting_unknown_as_the_setting(
        specs in proptest::collection::vec(spec(), 1..8),
    ) {
        let (pool, standings) = build(&specs, Pick::Headroom);
        if let Ok(chosen) = decide(&pool, &mut Memory::new(), &standings, &Ask::new(NOW)) {
            let used = |each: &Spec| each.used.unwrap_or(500);
            let least = specs.iter().filter(|each| can_serve(each)).map(used).min().expect("one serves");
            prop_assert_eq!(used(&specs[chosen.index]), least);
        }
    }

    #[test]
    fn round_robin_visits_every_member_that_can_serve_once_a_round(
        specs in proptest::collection::vec(spec(), 1..8),
    ) {
        let (pool, standings) = build(&specs, Pick::RoundRobin);
        let serving: Vec<usize> = (0..specs.len()).filter(|at| can_serve(&specs[*at])).collect();
        prop_assume!(!serving.is_empty());
        let mut memory = Memory::new();
        let mut seen: Vec<usize> = (0..serving.len())
            .map(|_| decide(&pool, &mut memory, &standings, &Ask::new(NOW)).expect("one serves").index)
            .collect();
        seen.sort_unstable();
        prop_assert_eq!(seen, serving);
    }

    #[test]
    fn a_move_stays_in_the_group_while_a_member_of_it_can_serve(
        specs in proptest::collection::vec(spec(), 2..8),
        strategy in strategies(),
    ) {
        let (pool, standings) = build(&specs, strategy);
        let ask = Ask::new(NOW).current("m0").stick(false);
        if let Ok(chosen) = decide(&pool, &mut Memory::new(), &standings, &ask) {
            let group_serves = specs.iter().any(|each| can_serve(each) && each.group == specs[0].group);
            prop_assert_eq!(specs[chosen.index].group == specs[0].group, group_serves);
            prop_assert_eq!(chosen.same_group, group_serves);
        }
    }

    #[test]
    fn a_session_whose_member_can_serve_stays_unless_it_does_not_stick(
        specs in proptest::collection::vec(spec(), 1..8),
        strategy in prop_oneof![Just(Pick::Priority), Just(Pick::Headroom)],
    ) {
        let (pool, standings) = build(&specs, strategy);
        let ask = Ask::new(NOW).current("m0");
        let result = decide(&pool, &mut Memory::new(), &standings, &ask);
        if can_serve(&specs[0]) {
            let chosen = result.expect("its own member serves");
            prop_assert_eq!(chosen.index, 0);
            prop_assert!(!chosen.moved);
        } else if let Ok(chosen) = result {
            prop_assert!(chosen.moved);
        }
    }
}
