//! What a hostile history costs in memory. One test function, since the counting allocator is
//! global and a second test running beside it would count too.

mod support;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::System;

use coqui_acp_history::{Document, Reader};
use support::ID;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let now = CURRENT.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the layout it was given, and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is `System`'s.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is `System`'s.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract is `System`'s.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract is `System`'s.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
            grow(new_size);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most memory `work` held at once beyond what was held before it started, in bytes.
fn peak_of<T>(work: impl FnOnce() -> T) -> (usize, T) {
    let before = CURRENT.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let result = work();
    (PEAK.load(Ordering::Relaxed) - before, result)
}

fn header() -> String {
    format!(
        r#"{{"format":"coqui-acp-history","version":1,"id":"{ID}","origin":{{"harness":"claude"}}}}"#
    )
}

/// A file of one `session/update` line whose `rawOutput` is `payload`, about `bytes` long.
fn one_line(payload: impl Fn(usize) -> String, bytes: usize) -> String {
    let mut text = header();
    text.push('\n');
    text.push_str(r#"{"method":"session/update","params":{"update":{"rawOutput":["#);
    let mut n = 0;
    let start = text.len();
    while text.len() - start < bytes {
        if n > 0 {
            text.push(',');
        }
        text.push_str(&payload(n));
        n += 1;
    }
    text.push_str(r#"],"sessionUpdate":"tool_call_update","toolCallId":"t"}}}"#);
    text.push('\n');
    text
}

const MIB: usize = 1 << 20;

type Payload = fn(usize) -> String;

/// The multiple of its text a document may hold, as the format document states it.
const DOCUMENT_FACTOR: usize = 64;

#[test]
fn a_hostile_history_costs_a_bounded_multiple_of_its_size() {
    // One 15 MiB line of the smallest values there are: 7.5 million of them.
    let zeros = one_line(|_| "0".into(), 15 * MIB);
    let (peak, outcome) = peak_of(|| {
        let mut reader = Reader::builder(zeros.as_bytes()).open().unwrap();
        reader.next_entry().map(|entry| entry.is_some())
    });
    assert!(
        peak < 192 * MIB,
        "one 15 MiB line of zeros held {} MiB at once (outcome {outcome:?})",
        peak / MIB
    );

    // The same size as objects with a key each.
    let keyed = one_line(|n| format!(r#"{{"k{n}":0}}"#), 15 * MIB);
    let (peak, outcome) = peak_of(|| {
        let mut reader = Reader::builder(keyed.as_bytes()).open().unwrap();
        reader.next_entry().map(|entry| entry.is_some())
    });
    assert!(
        peak < 192 * MIB,
        "one 15 MiB line of keyed objects held {} MiB at once (outcome {outcome:?})",
        peak / MIB
    );
    drop((zeros, keyed));

    // Many lines each within every bound, held as a document, in the shapes that cost the most
    // per byte of text.
    let shapes: [(&str, Payload); 4] = [
        ("numbers", |_| "0".into()),
        ("empty arrays", |_| "[]".into()),
        ("one-member objects", |_| r#"{"a":0}"#.into()),
        ("text", |n| format!(r#""line {n} of some plain text""#)),
    ];
    for (shape, payload) in shapes {
        let mut text = header();
        text.push('\n');
        for _ in 0..128 {
            let line = one_line(payload, 60 * 1024);
            text.push_str(line.split_once('\n').map(|(_, rest)| rest).unwrap());
        }
        let bytes = text.len();
        let (peak, document) = peak_of(|| text.parse::<Document>());
        let document = document.unwrap();
        assert_eq!(document.lines.len(), 128);
        eprintln!(
            "{shape}: {} KiB held {} KiB, {}x",
            bytes / 1024,
            peak / 1024,
            peak / bytes
        );
        assert!(
            peak < bytes * DOCUMENT_FACTOR,
            "a {} MiB document of {shape} held {} MiB at once, {}x",
            bytes / MIB,
            peak / MIB,
            peak / bytes
        );
    }
}
