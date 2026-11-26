//! The switchboard over stand-in seams: assigning a session, moving one at a turn's end, and what the ledger is given.

#![allow(
    clippy::std_instead_of_core,
    clippy::std_instead_of_alloc,
    reason = "tests run on std"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use coqui_acp_api::{Coverage, Status};
use coqui_acp_history::{LossReport, TurnRecord};
use coqui_acp_pool::{
    Ask, AtTurn, Availability, Basics, Carried, Carry, DEFAULT_CONTINUATION, Ended, Hold, Holds,
    HowEnded, Ledger, Member, Memory, Occupancy, Pool, Resume, Started, Starter, Strategy,
    Switchboard, Usage, UsageSource, Why,
};
use serde_json::json;

mod support;
use support::{NOW, report};

/// Stand-in seams driven by tables.
#[derive(Default)]
struct World {
    unavailable: BTreeMap<String, String>,
    limited: BTreeMap<String, i64>,
    cannot_carry: BTreeSet<String>,
    usage: BTreeMap<String, Usage>,
    holds: BTreeMap<String, Vec<Hold>>,
    active: BTreeMap<String, u32>,
    refuses_start: BTreeSet<String>,
    refuses_carry: BTreeSet<String>,
    carried: RefCell<Option<Carried>>,
    log: RefCell<Vec<String>>,
    records: RefCell<Vec<TurnRecord>>,
}

impl World {
    fn carried(&self) -> Carried {
        self.carried
            .borrow()
            .clone()
            .unwrap_or_else(|| Carried::new(LossReport::new(), true))
    }
}

impl Availability<()> for World {
    fn availability(&self, member: &Member<()>, _ask: &Ask<'_>) -> Basics {
        let mut basics = Basics::new();
        if let Some(why) = self.unavailable.get(&member.id) {
            basics = basics.unavailable(why.clone());
        }
        if let Some(until) = self.limited.get(&member.id) {
            basics = basics.limited_until(*until);
        }
        if self.cannot_carry.contains(&member.id) {
            basics = basics.cannot_carry();
        }
        basics
    }
}

impl UsageSource<()> for World {
    fn report(&self, member: &Member<()>) -> Usage {
        self.usage.get(&member.id).cloned().unwrap_or_default()
    }
}

impl Occupancy<()> for World {
    fn active(&self, member: &Member<()>, _excluding: Option<&str>) -> u32 {
        self.active.get(&member.id).copied().unwrap_or(0)
    }

    fn last_used(&self, _member: &Member<()>) -> Option<i64> {
        None
    }
}

impl Holds<()> for World {
    fn holds(&self, member: &Member<()>, _now: i64) -> Vec<Hold> {
        self.holds.get(&member.id).cloned().unwrap_or_default()
    }
}

impl Starter<()> for World {
    fn start(&self, member: &Member<()>, _ask: &Ask<'_>) -> Result<Started, String> {
        self.log.borrow_mut().push(format!("start {}", member.id));
        if self.refuses_start.contains(&member.id) {
            return Err("the program would not start".to_owned());
        }
        Ok(Started::new("s1"))
    }

    fn carry(&self, session: &str, from: &Member<()>, to: &Member<()>) -> Carry {
        self.log
            .borrow_mut()
            .push(format!("carry {session} {} -> {}", from.id, to.id));
        if self.refuses_carry.contains(&to.id) {
            return Carry::Refused("it cannot import".to_owned());
        }
        if from.group == to.group {
            Carry::Whole(self.carried())
        } else {
            Carry::Replayed(self.carried())
        }
    }

    fn resume(&self, session: &str, to: &Member<()>, resume: &Resume) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(format!("resume {session} {} {resume:?}", to.id));
        Ok(())
    }
}

impl Ledger for World {
    fn record(&self, record: &TurnRecord) {
        self.records.borrow_mut().push(record.clone());
    }
}

fn pool(strategy: Strategy) -> Pool<()> {
    Pool::new("planner")
        .strategy(strategy)
        .member(Member::new("claude_a", ()).group("claude"))
        .member(Member::new("claude_b", ()).group("claude"))
        .member(Member::new("codex", ()).group("codex"))
}

fn board<'a>(
    pool: &'a Pool<()>,
    memory: &'a RefCell<Memory>,
    world: &'a World,
) -> Switchboard<'a, ()> {
    Switchboard::new(pool, memory, world, world)
        .usage(world)
        .occupancy(world)
        .holds(world)
        .ledger(world)
}

fn full(until: i64) -> Usage {
    Usage::Known(report(
        Status::Exhausted,
        Coverage::Unknown,
        Some(until),
        None,
    ))
}

#[test]
fn a_new_session_starts_on_the_first_member_that_can_and_the_ledger_hears_of_it() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.usage.insert("claude_a".into(), full(NOW + 600));
    let mut record = TurnRecord::new(0);
    record.role = Some("planner".into());
    record.agent = Some("Claude".into());
    let assigned = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW).model("opus"), record)
        .expect("a member starts");
    assert_eq!(
        (assigned.chosen.member.as_str(), assigned.chosen.why),
        ("claude_b", Why::Start)
    );
    assert_eq!(assigned.started.session, "s1");
    let records = world.records.borrow();
    assert_eq!(records.len(), 1);
    let served = &records[0];
    assert_eq!(served.pool.as_deref(), Some("planner"));
    assert_eq!(served.member.as_deref(), Some("claude_b"));
    assert_eq!(served.harness.as_deref(), Some("claude"));
    assert_eq!(served.model.as_deref(), Some("opus"));
    assert_eq!(served.role.as_deref(), Some("planner"));
    assert_eq!(served.agent.as_deref(), Some("Claude"));
    assert_eq!(served.why.as_deref(), Some("start"));
    assert_eq!(served.session.as_deref(), Some("s1"));
    assert_eq!(served.usage_known, Some(false));
    assert_eq!(served.at, NOW);
}

#[test]
fn a_member_that_fails_to_start_is_left_out_and_the_next_is_tried_once_each() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.refuses_start.insert("claude_a".into());
    world.refuses_start.insert("claude_b".into());
    let assigned = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW), TurnRecord::default())
        .expect("codex starts");
    assert_eq!(assigned.chosen.member, "codex");
    assert_eq!(
        *world.log.borrow(),
        ["start claude_a", "start claude_b", "start codex"]
    );
    // With every member failing, none is left, and the refusal says why for each.
    world.refuses_start.insert("codex".into());
    let none = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW), TurnRecord::default())
        .expect_err("none starts");
    let said = none.to_string();
    assert!(
        said.contains("claude_a: could not start: the program would not start"),
        "{said}"
    );
    assert_eq!(
        world.records.borrow().len(),
        1,
        "a failed start is not a served turn"
    );
}

#[test]
fn nothing_available_is_refused_with_each_reason() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world
        .unavailable
        .insert("claude_a".into(), "not signed in".into());
    world.usage.insert("claude_b".into(), full(NOW + 7800));
    world.holds.insert(
        "codex".into(),
        vec![Hold::new("spending limit").until(NOW + 60)],
    );
    let none = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW), TurnRecord::default())
        .expect_err("none");
    let said = none.0.message("member");
    assert!(
        said.contains("claude_a: not signed in") && said.contains("codex: spending limit"),
        "{said}"
    );
    assert_eq!(none.0.soonest, Some(NOW + 60));
}

#[test]
fn resolve_answers_without_starting_or_moving_the_cursor() {
    let pool = pool(Strategy::RoundRobin);
    let memory = RefCell::new(Memory::new());
    let world = World::default();
    let switchboard = board(&pool, &memory, &world);
    for _ in 0..3 {
        assert_eq!(
            switchboard.resolve(&Ask::new(NOW)).expect("one").member,
            "claude_a"
        );
    }
    assert!(world.log.borrow().is_empty());
    assert_eq!(memory.borrow().last("planner"), None);
}

#[test]
fn a_limit_moves_the_session_within_its_harness_first_and_sends_the_continuation() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let world = World::default();
    let mut record = TurnRecord::new(NOW);
    record.tokens = Some(coqui_acp_history::Tokens::new());
    let ended = Ended::new(
        "s1",
        "claude_a",
        HowEnded::Limit {
            until: Some(NOW + 3600),
        },
        NOW,
    )
    .resume_text("Carry on.")
    .record(record);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("the session moves");
    };
    assert_eq!(
        (moved.from.as_str(), moved.chosen.member.as_str()),
        ("claude_a", "claude_b")
    );
    assert_eq!(moved.chosen.why, Why::Exhausted);
    assert!(!moved.replayed && moved.chosen.same_group);
    assert_eq!(moved.resume, Resume::Continue("Carry on.".to_owned()));
    assert_eq!(
        *world.log.borrow(),
        [
            "carry s1 claude_a -> claude_b",
            "resume s1 claude_b Continue(\"Carry on.\")"
        ]
    );
    let records = world.records.borrow();
    let served = &records[0];
    assert_eq!(served.moved_from.as_deref(), Some("claude_a"));
    assert!(
        served
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.moved_on && outcome.limit)
    );
    assert!(
        served.tokens.is_some(),
        "what the host knew of the turn is kept"
    );
}

#[test]
fn another_harness_takes_it_only_when_none_of_the_first_can_and_it_is_a_replay() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world
        .unavailable
        .insert("claude_b".into(), "model not listed".into());
    let ended = Ended::new("s1", "claude_a", HowEnded::Limit { until: None }, NOW);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("it moves");
    };
    assert_eq!(moved.chosen.member, "codex");
    assert!(moved.replayed && !moved.chosen.same_group);
    // The same board, asked first, says it would leave the harness, so a host that asks the person can.
    let next = board(&pool, &memory, &world)
        .choose_next(&ended)
        .expect("some")
        .expect("a move");
    assert!(!next.same_group);
}

#[test]
fn a_move_never_happens_mid_turn_a_refill_or_a_finished_turn_leaves_the_session_alone() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let world = World::default();
    // The session is on `codex` though earlier members have refilled: a turn that ends well leaves it there.
    let ended = Ended::new("s1", "codex", HowEnded::Finished, NOW);
    assert_eq!(
        board(&pool, &memory, &world).at_turn(&ended),
        AtTurn::Stayed
    );
    // So does a turn that ended for another reason, even with the member used up.
    let mut spent = World::default();
    spent.usage.insert("codex".into(), full(NOW + 60));
    let other = Ended::new("s1", "codex", HowEnded::Other, NOW);
    assert_eq!(
        board(&pool, &memory, &spent).at_turn(&other),
        AtTurn::Stayed
    );
    assert!(world.log.borrow().is_empty() && spent.log.borrow().is_empty());
}

#[test]
fn a_turn_that_ended_well_with_the_bucket_now_used_up_moves_the_idle_session_and_sends_nothing() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.usage.insert("claude_a".into(), full(NOW + 600));
    let ended = Ended::new("s1", "claude_a", HowEnded::Finished, NOW);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("it moves");
    };
    assert_eq!(
        (moved.chosen.member.as_str(), &moved.resume),
        ("claude_b", &Resume::Nothing)
    );
    assert_eq!(*world.log.borrow(), ["carry s1 claude_a -> claude_b"]);
}

#[test]
fn a_member_whose_carry_is_refused_is_left_out_for_the_session_and_the_next_is_tried() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.refuses_carry.insert("claude_b".into());
    let ended = Ended::new("s1", "claude_a", HowEnded::Limit { until: None }, NOW);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("it moves");
    };
    assert_eq!(moved.chosen.member, "codex");
    assert_eq!(
        world.log.borrow()[..2],
        [
            "carry s1 claude_a -> claude_b",
            "carry s1 claude_a -> codex"
        ]
    );
    // When every carry is refused the session stays, with each reason.
    world.refuses_carry.insert("codex".into());
    let AtTurn::NoneLeft(none) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("none left");
    };
    let said = none.message("member");
    assert!(
        said.contains("claude_b: its history cannot be carried: it cannot import"),
        "{said}"
    );
}

#[test]
fn a_member_whose_harness_cannot_take_a_running_session_is_skipped_for_a_move_only() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.cannot_carry.insert("claude_b".into());
    let ended = Ended::new("s1", "claude_a", HowEnded::Limit { until: None }, NOW);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("it moves");
    };
    assert_eq!(moved.chosen.member, "codex");
    // A new session may use it.
    world.usage.insert("claude_a".into(), full(NOW + 60));
    let assigned = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW), TurnRecord::default())
        .expect("starts");
    assert_eq!(assigned.chosen.member, "claude_b");
}

#[test]
fn the_interrupted_turn_with_no_work_sends_the_persons_prompt_again() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let world = World::default();
    *world.carried.borrow_mut() = Some(
        Carried::new(LossReport::new(), false)
            .prompt(vec![json!({"type": "text", "text": "do it"})]),
    );
    let ended =
        Ended::new("s1", "claude_a", HowEnded::Limit { until: None }, NOW).resume_text("ignored");
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("it moves");
    };
    assert_eq!(
        moved.resume,
        Resume::Repeat(vec![json!({"type": "text", "text": "do it"})])
    );
    // With work done and no text of the role's, the default line.
    *world.carried.borrow_mut() = None;
    let again = Ended::new("s2", "claude_a", HowEnded::Limit { until: None }, NOW);
    let AtTurn::Moved(moved) = board(&pool, &memory, &world).at_turn(&again) else {
        panic!("it moves");
    };
    assert_eq!(
        moved.resume,
        Resume::Continue(DEFAULT_CONTINUATION.to_owned())
    );
}

#[test]
fn with_no_member_left_the_session_stays_and_a_session_the_pool_does_not_hold_is_said() {
    let pool = pool(Strategy::Priority);
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    for id in ["claude_a", "claude_b", "codex"] {
        world.usage.insert(id.into(), full(NOW + 600));
    }
    let ended = Ended::new(
        "s1",
        "claude_a",
        HowEnded::Limit {
            until: Some(NOW + 600),
        },
        NOW,
    );
    let AtTurn::NoneLeft(none) = board(&pool, &memory, &world).at_turn(&ended) else {
        panic!("none left");
    };
    assert_eq!(none.soonest, Some(NOW + 600));
    assert!(world.log.borrow().is_empty());
    let gone = Ended::new("s1", "removed", HowEnded::Limit { until: None }, NOW);
    assert_eq!(
        board(&pool, &memory, &world).at_turn(&gone),
        AtTurn::Unknown("removed".to_owned())
    );
}

#[test]
fn a_cap_counts_through_the_occupancy_seam() {
    let pool = Pool::new("p")
        .member(Member::new("busy", ()).max_concurrent(2))
        .member(Member::new("free", ()));
    let memory = RefCell::new(Memory::new());
    let mut world = World::default();
    world.active.insert("busy".into(), 2);
    let assigned = board(&pool, &memory, &world)
        .assign(&Ask::new(NOW), TurnRecord::default())
        .expect("free");
    assert_eq!(assigned.chosen.member, "free");
}
