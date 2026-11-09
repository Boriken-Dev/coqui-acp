//! Seeding a new session from the latest compaction boundary, and fitting a window.

mod support;

use coqui_acp_history::{Body, Document, Line, Stop, StopReason};
use support::document;

fn turn(doc: &mut Document, text: &str) {
    doc.lines.push(Line::text_prompt(text));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
}

#[test]
fn with_no_compaction_the_seed_is_the_whole_history() {
    let doc = document(3);
    let seed = doc.seed();
    assert_eq!(seed.summary, None);
    assert_eq!(seed.lines.len(), doc.lines.len());
    assert_eq!(seed.first_line, 2);
    assert_eq!(seed.point.as_ref().map(|p| p.line), Some(2));
    assert_eq!(seed.turns(), 3);
    assert!(!seed.mid_turn);
}

#[test]
fn a_history_with_no_body_has_no_seed_point() {
    let doc = document(0);
    let seed = doc.seed();
    assert!(seed.point.is_none());
    assert!(seed.lines.is_empty());
    assert!(seed.fit(10).complete);
}

#[test]
fn the_seed_starts_at_the_latest_boundary_with_a_summary() {
    let mut doc = document(2);
    doc.lines.push(Line::compaction("first"));
    turn(&mut doc, "after first");
    doc.lines.push(Line::compaction("second"));
    let second_at = doc.lines.len() + 1;
    turn(&mut doc, "after second");
    doc.lines.push(Line::new(Body::Compaction {
        summary: None,
        trigger: None,
        pre_tokens: None,
    }));
    turn(&mut doc, "after the empty one");
    let seed = doc.seed();
    assert_eq!(seed.summary, Some("second"));
    assert_eq!(
        seed.turns(),
        2,
        "the turns after the boundary, those after a summary-less one included"
    );
    assert_eq!(seed.point.as_ref().unwrap().line, second_at as u64);
    assert_eq!(seed.first_line, second_at as u64 + 1);
}

#[test]
fn the_same_history_has_the_same_seed_point_digest_and_another_has_not() {
    let mut a = document(1);
    a.lines.push(Line::compaction("s"));
    turn(&mut a, "x");
    let mut b = a.clone();
    assert_eq!(a.seed().point, b.seed().point);
    b.lines[3] = Line::compaction("different");
    assert_ne!(
        a.seed().point.unwrap().digest,
        b.seed().point.unwrap().digest
    );
}

#[test]
fn a_boundary_inside_a_turn_is_reported_as_mid_turn() {
    let mut doc = document(0);
    doc.lines.push(Line::text_prompt("long"));
    doc.lines.push(Line::compaction("mid"));
    doc.lines
        .push(Line::turn_end(Stop::Acp(StopReason::EndTurn)));
    let seed = doc.seed();
    assert!(seed.mid_turn);
    assert_eq!(seed.turns(), 0);
    assert_eq!(seed.lines.len(), 1);
}

#[test]
fn the_fit_keeps_the_summary_and_the_latest_whole_turns() {
    let mut doc = document(0);
    doc.lines.push(Line::compaction("summary text"));
    for n in 0..5 {
        turn(&mut doc, &format!("turn number {n}"));
    }
    let seed = doc.seed();
    let one_turn: u64 = seed.lines[..2]
        .iter()
        .map(|l| l.to_json().unwrap().len() as u64 + 1)
        .sum();
    let summary = "summary text".len() as u64;

    let all = seed.fit(u64::MAX);
    assert!(all.complete);
    assert_eq!(all.lines.len(), 10);
    assert_eq!(all.dropped_turns, 0);

    let two = seed.fit(summary + 2 * one_turn + 1);
    assert!(!two.complete);
    assert_eq!(two.lines.len(), 4);
    assert_eq!(two.dropped_turns, 3);
    assert_eq!(two.first_line, seed.first_line + 6);
    assert!(matches!(&two.lines[0].body, Body::Prompt { .. }));
    assert!(two.bytes <= summary + 2 * one_turn + 1);

    let none = seed.fit(summary + one_turn - 1);
    assert!(none.lines.is_empty());
    assert!(!none.complete);
    assert_eq!(none.dropped_turns, 5);
    assert_eq!(
        none.summary,
        Some("summary text"),
        "the summary is always kept"
    );
}
