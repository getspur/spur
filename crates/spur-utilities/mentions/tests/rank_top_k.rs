//! sud-m5 top-K exactness gate: `rank_top_k` must be byte-identical to
//! the first K rows of the `rank_full_sort` reference (2026-08-27 mentions
//! spec §5, Z3 decision `select_nth_then_sort_k`).
//!
//! The property sweeps the spec's exactness matrix: random scores and
//! stable tie keys, all mention kinds (file tree, code rows, mapped and
//! unmapped customs), optional `search_text`, ties, duplicate scores,
//! fully equal rows (duplicate displays and duplicate URIs — comparator
//! `Equal` pairs whose order the reference takes from stable-sort input
//! order), `K = 0`, `K = 1`, `K >= M`, and the unlimited `None` limit.
//! Deterministic cases pin the hazard the property can shrink away from:
//! equal-key runs straddling the K boundary.

mod common;

use common::entry;
use nucleo_matcher::{Config, Matcher};
use proptest::collection;
use proptest::prelude::*;
use proptest::sample::select;
use spur_mentions::{
    rank_full_sort, rank_top_k, MentionEntry, MentionId, MentionKind, RankOptions, TierPolicy,
};

fn options(limit: Option<usize>) -> RankOptions {
    RankOptions {
        limit,
        tiers: TierPolicy::default(),
    }
}

fn rank_both(
    entries: &[MentionEntry],
    query: &str,
    limit: Option<usize>,
) -> (Vec<MentionEntry>, Vec<MentionEntry>) {
    let refs: Vec<&MentionEntry> = entries.iter().collect();
    let mut reference_matcher = Matcher::new(Config::DEFAULT);
    let mut bounded_matcher = Matcher::new(Config::DEFAULT);
    let reference = rank_full_sort(&refs, query, &options(limit), &mut reference_matcher);
    let bounded = rank_top_k(&refs, query, &options(limit), &mut bounded_matcher);
    (reference, bounded)
}

fn ids(entries: &[MentionEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.id.as_u64()).collect()
}

// ---------------------------------------------------------------------------
// Property suite
// ---------------------------------------------------------------------------

fn any_kind() -> impl Strategy<Value = MentionKind> {
    select(vec![
        MentionKind::File,
        MentionKind::Directory,
        MentionKind::CodeFile,
        MentionKind::CodeSymbol,
        MentionKind::Custom("worker".into()),
        MentionKind::Custom("issue".into()),
        MentionKind::Custom("unmapped".into()),
    ])
}

/// Small alphabets force the interesting structure: duplicate displays,
/// duplicate URIs (comparator-`Equal` pairs), and heavy score ties.
fn small_text() -> impl Strategy<Value = String> {
    select(vec!["ab.rs", "ba.rs", "ab", "ba", "dup", "worker-1", "x"]).prop_map(str::to_owned)
}

fn any_query() -> impl Strategy<Value = String> {
    select(vec!["", "a", "ab", "b", "rs", "w", "zz"]).prop_map(str::to_owned)
}

fn scheme_of(kind: &MentionKind) -> &str {
    match kind {
        MentionKind::File | MentionKind::Directory => "file",
        MentionKind::CodeFile | MentionKind::CodeSymbol => "graph",
        MentionKind::Custom(name) => name.as_ref(),
    }
}

type EntryParts = (MentionKind, String, String, Option<String>);

fn one_entry() -> impl Strategy<Value = EntryParts> {
    (
        any_kind(),
        small_text(),
        small_text(),
        proptest::option::of(small_text()),
    )
}

#[derive(Debug)]
struct Case {
    entries: Vec<MentionEntry>,
    query: String,
    limit: Option<usize>,
}

fn any_case() -> impl Strategy<Value = Case> {
    (
        collection::vec(one_entry(), 0..=64),
        any_query(),
        any::<bool>(),
    )
        .prop_flat_map(|(parts, query, limited): (Vec<EntryParts>, String, bool)| {
            let matches = parts.len();
            (Just(parts), Just(query), Just(limited), 0..=(matches + 2))
        })
        .prop_map(
            |(parts, query, limited, limit): (Vec<EntryParts>, String, bool, usize)| Case {
                entries: parts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (kind, display, uri, search_text))| MentionEntry {
                        id: MentionId::new(index as u64),
                        uri: format!("{}://{uri}", scheme_of(&kind)),
                        display,
                        secondary: None,
                        search_text,
                        insert_text: None,
                        kind,
                    })
                    .collect(),
                query,
                limit: limited.then_some(limit),
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The exactness gate: bounded output == first K rows of the reference,
    /// row for row (every field), over ties, duplicate scores, fully equal
    /// rows, all kinds, K = 0, K >= M, and unlimited.
    #[test]
    fn rank_top_k_is_byte_identical_to_the_full_sort_reference(case in any_case()) {
        let (reference, bounded) = rank_both(&case.entries, &case.query, case.limit);
        let expected_len = case
            .limit
            .map_or(reference.len(), |limit| limit.min(reference.len()));
        prop_assert_eq!(bounded.len(), expected_len);
        prop_assert_eq!(bounded, reference);
    }
}

// ---------------------------------------------------------------------------
// Deterministic hazard cases
// ---------------------------------------------------------------------------

/// The classic unstable-select hazard: a run of fully equal rows straddles
/// the K boundary. The reference keeps input order via stable-sort
/// stability; `rank_top_k` must reproduce it, not an arbitrary permutation.
#[test]
fn equal_display_run_at_the_k_boundary_keeps_input_order() {
    let entries: Vec<MentionEntry> = (0..40)
        .map(|i| entry(i, MentionKind::File, &format!("file:///dup-{i}"), "dup"))
        .collect();
    for k in [0usize, 1, 7, 39, 40, 41] {
        let (reference, bounded) = rank_both(&entries, "", Some(k));
        assert_eq!(bounded, reference, "K={k}");
        assert_eq!(
            ids(&bounded),
            (0..k.min(40) as u64).collect::<Vec<_>>(),
            "K={k}"
        );
    }
}

/// Code rows tie-break on their URI; duplicate URIs make distinct rows
/// compare `Equal` — same requirement at the boundary.
#[test]
fn equal_uri_code_rows_at_the_k_boundary_keep_input_order() {
    let entries: Vec<MentionEntry> = (0..12)
        .map(|i| entry(i, MentionKind::CodeSymbol, "graph://dup", "sym"))
        .collect();
    let (reference, bounded) = rank_both(&entries, "", Some(5));
    assert_eq!(bounded, reference);
    assert_eq!(ids(&bounded), vec![0, 1, 2, 3, 4]);
}

#[test]
fn k_zero_is_empty_and_unlimited_matches_the_reference() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///a", "ab.rs"),
        entry(
            1,
            MentionKind::Custom("worker".into()),
            "worker://w",
            "worker-1",
        ),
        entry(2, MentionKind::CodeSymbol, "graph://s", "ab"),
    ];
    let (reference, bounded) = rank_both(&entries, "a", Some(0));
    assert!(reference.is_empty());
    assert!(bounded.is_empty());

    let (reference, bounded) = rank_both(&entries, "a", None);
    assert_eq!(bounded, reference);
    assert_eq!(bounded.len(), reference.len());

    // K >= M keeps every match in reference order.
    let (reference, bounded) = rank_both(&entries, "a", Some(99));
    assert_eq!(bounded, reference);
}

/// Tier and bucket ordering survive the selection: code rows rank below
/// the file tree within the same bucket, customs fall back to tier 3.
#[test]
fn tier_ordering_survives_selection_under_ties() {
    let mut entries = Vec::new();
    for i in 0..30 {
        entries.push(entry(
            i,
            MentionKind::CodeSymbol,
            &format!("graph://s{i}"),
            "x",
        ));
        entries.push(entry(
            100 + i,
            MentionKind::File,
            &format!("file:///f{i}"),
            "x",
        ));
        entries.push(entry(
            200 + i,
            MentionKind::Custom("unmapped".into()),
            &format!("worker://w{i}"),
            "x",
        ));
    }
    let (reference, bounded) = rank_both(&entries, "x", Some(10));
    assert_eq!(bounded, reference);
    let kinds: Vec<&str> = bounded
        .iter()
        .map(|entry| match entry.kind {
            MentionKind::File => "file",
            MentionKind::Custom(_) => "custom",
            _ => "code",
        })
        .collect();
    assert_eq!(kinds, vec!["file"; 10]);
}

#[test]
fn deterministic_across_repeated_calls_with_duplicates() {
    let entries: Vec<MentionEntry> = (0..24)
        .map(|i| entry(i, MentionKind::File, &format!("file:///d{}", i % 4), "dup"))
        .collect();
    let first = rank_both(&entries, "", Some(9)).1;
    let second = rank_both(&entries, "", Some(9)).1;
    assert_eq!(first, second);
}
