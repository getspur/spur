//! A zero-row ranking request should do no allocation, regardless of input size.
//! The allocator wrapper is confined to this integration-test executable.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use nucleo_matcher::{Config, Matcher};
use spur_mentions::{rank_top_k, MentionEntry, MentionId, MentionKind, RankOptions};

struct CountingAllocator;

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record_allocation() {
    let _ = COUNT.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

// SAFETY: every operation forwards its original pointer/layout arguments to
// System. Counting uses a non-allocating thread-local Cell and never touches data.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn zero_limit_ranking_does_not_allocate() {
    let entry = MentionEntry {
        id: MentionId::new(0),
        kind: MentionKind::File,
        uri: "file:///café.rs".into(),
        display: "café.rs".into(),
        secondary: None,
        search_text: None,
        insert_text: None,
    };
    let entries = vec![&entry; 1_000];
    let options = RankOptions {
        limit: Some(0),
        ..RankOptions::default()
    };
    let mut matcher = Matcher::new(Config::DEFAULT);
    for query in ["", "café", "missing"] {
        COUNT.with(|count| count.set(Some(0)));
        let result = rank_top_k(&entries, query, &options, &mut matcher);
        let allocations = COUNT.with(|count| count.replace(None).unwrap());
        assert!(result.is_empty());
        assert_eq!(allocations, 0, "zero-limit query {query:?} allocated");
    }
}
