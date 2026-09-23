//! sud-m5 ranking benchmarks (2026-08-27 mentions spec §5 / §7 phase 6):
//! score, full sort, and bounded top-K selection at 1k / 10k / 100k
//! candidates. Report-only — this binary prints observed timings and
//! asserts nothing (the spec sets no latency thresholds).
//!
//! Run: `scripts/spur-cargo bench -p spur-mentions --bench rank_top_k`
//! (harness-free: std `Instant` medians, no bench-framework dependency).

use std::hint::black_box;
use std::time::Instant;

use nucleo_matcher::{Config, Matcher};
use spur_mentions::rank::{score_all, select_top_k, sort_ranked};
use spur_mentions::{MentionEntry, MentionId, MentionKind, TierPolicy};

/// Typical picker windows (the TUI's typed-query result limit is ~20).
const K_VALUES: [usize; 2] = [20, 100];
const SIZES: [usize; 3] = [1_000, 10_000, 100_000];
const WARMUP_REPS: usize = 3;
const TIMED_REPS: usize = 25;

fn main() {
    println!("spur-mentions rank: report-only timings (no thresholds asserted)");
    println!(
        "{:>9} {:>9} {:>12} {:>13} {:>10} {:>14} {:>12}",
        "entries",
        "matched",
        "score_us",
        "full_sort_us",
        "copy_us",
        "select_k20_us",
        "select_k100_us"
    );
    for size in SIZES {
        run_one(size);
    }
}

/// Deterministic candidate set: mostly path-like file rows with a slice of
/// directory and code rows, shuffled with a fixed LCG so the input is not
/// presorted. The query matches nearly every row, so M (matches) ~= N.
fn synthetic_entries(count: usize) -> Vec<MentionEntry> {
    let mut state: u64 = 0x243F6A8885A308D3;
    let mut lcg = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };
    let mut entries: Vec<MentionEntry> = (0..count)
        .map(|i| {
            let (kind, scheme) = match i % 10 {
                0 => (MentionKind::Directory, "file"),
                8 | 9 => (MentionKind::CodeSymbol, "graph"),
                _ => (MentionKind::File, "file"),
            };
            let display = format!("crates/mod{}/src/dir{}/file{}.rs", i % 7, i % 53, i % 997);
            MentionEntry {
                id: MentionId::new(i as u64),
                kind,
                uri: format!("{scheme}:///e{i}"),
                display,
                secondary: None,
                search_text: None,
                insert_text: None,
            }
        })
        .collect();
    for i in (1..count).rev() {
        let j = (lcg() >> 33) as usize % (i + 1);
        entries.swap(i, j);
    }
    entries
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn timed<F: FnMut()>(reps: usize, mut body: F) -> Vec<u128> {
    for _ in 0..WARMUP_REPS {
        body();
    }
    (0..reps)
        .map(|_| {
            let started = Instant::now();
            body();
            started.elapsed().as_nanos()
        })
        .collect()
}
fn run_one(count: usize) {
    let entries = synthetic_entries(count);
    let refs: Vec<&MentionEntry> = entries.iter().collect();
    let mut matcher = Matcher::new(Config::DEFAULT);
    let query = "src/file";

    // Scoring pass (O(N) fuzzy match) — the fixed cost both paths share.
    let scored = score_all(&refs, query, &TierPolicy::new(), &mut matcher);
    let matched = scored.len();
    // `scored` stays in natural (shuffled) input order: both selection
    // paths start each repetition from the same realistic, unsorted state.

    // The selection phase copies the ranked-ref slice before each rep so
    // repetitions never start from an already-sorted slice. The copy cost
    // is reported separately (black-boxed so LLVM cannot elide it) and the
    // "net" columns subtract it.
    let copy_us = median(&mut timed(TIMED_REPS, || {
        black_box(scored.clone());
    })) / 1_000;

    let mut full_samples = Vec::new();
    for _ in 0..TIMED_REPS {
        let mut rows = black_box(scored.clone());
        let started = Instant::now();
        sort_ranked(&mut rows);
        let elapsed = started.elapsed().as_nanos();
        black_box(&rows);
        full_samples.push(elapsed);
    }
    let full_sort_us = median(&mut full_samples) / 1_000;

    let mut select_cells = Vec::new();
    for &k in &K_VALUES {
        let mut samples = Vec::new();
        for _ in 0..TIMED_REPS {
            let mut rows = black_box(scored.clone());
            let started = Instant::now();
            let kept = select_top_k(&mut rows, Some(k));
            rows.truncate(kept);
            let elapsed = started.elapsed().as_nanos();
            black_box(&rows);
            samples.push(elapsed);
        }
        select_cells.push(median(&mut samples) / 1_000);
    }

    println!(
        "{:>9} {:>9} {:>12} {:>13} {:>10} {:>14} {:>12}",
        count,
        matched,
        score_pass_us(&refs, query),
        format!(
            "{} (net {})",
            full_sort_us,
            full_sort_us.saturating_sub(copy_us)
        ),
        copy_us,
        format!(
            "{} (net {})",
            select_cells[0],
            select_cells[0].saturating_sub(copy_us)
        ),
        format!(
            "{} (net {})",
            select_cells[1],
            select_cells[1].saturating_sub(copy_us)
        ),
    );
}

/// Score cost is measured with fewer reps (it dominates and is stable).
fn score_pass_us(refs: &[&MentionEntry], query: &str) -> u128 {
    let mut matcher = Matcher::new(Config::DEFAULT);
    median(&mut timed(5, || {
        drop(score_all(refs, query, &TierPolicy::new(), &mut matcher));
    })) / 1_000
}
