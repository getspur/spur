//! sud-m2a deterministic full-sort ranker tests. `rank_full_sort` is the
//! reference implementation the later top-K selection
//! (`select_nth_unstable_by`) must reproduce exactly (2026-08-27 mentions
//! spec §5).

mod common;

use common::entry;
use nucleo_matcher::{Config, Matcher};
use spur_mentions::{rank_full_sort, MentionEntry, MentionKind, RankOptions, TierPolicy};

fn rank_all(entries: &[MentionEntry], query: &str) -> Vec<MentionEntry> {
    rank_with(entries, query, RankOptions::default())
}

fn rank_with(entries: &[MentionEntry], query: &str, options: RankOptions) -> Vec<MentionEntry> {
    let refs: Vec<&MentionEntry> = entries.iter().collect();
    let mut matcher = Matcher::new(Config::DEFAULT);
    rank_full_sort(&refs, query, &options, &mut matcher)
}

fn displays(entries: &[MentionEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.display.as_str()).collect()
}

#[test]
fn prefix_matches_rank_before_scattered_matches() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///scattered", "a-x-b.txt"),
        entry(1, MentionKind::File, "file:///prefix", "ab.rs"),
    ];
    assert_eq!(
        displays(&rank_all(&entries, "ab")),
        vec!["ab.rs", "a-x-b.txt"]
    );
}

#[test]
fn non_matching_entries_are_dropped() {
    let entries = vec![entry(0, MentionKind::File, "file:///a", "ab.rs")];
    assert!(rank_all(&entries, "zz").is_empty());
}

#[test]
fn empty_query_keeps_every_row_in_tier_then_key_order() {
    let entries = vec![
        entry(0, MentionKind::Custom("worker".into()), "worker://b", "w-b"),
        entry(1, MentionKind::File, "file:///b.rs", "b.rs"),
        entry(2, MentionKind::CodeSymbol, "graph://s", "s"),
        entry(3, MentionKind::File, "file:///a.rs", "a.rs"),
    ];
    let ranked = rank_all(&entries, "");
    assert_eq!(displays(&ranked), vec!["a.rs", "b.rs", "w-b", "s"]);
}

#[test]
fn stable_tie_key_uses_uri_for_code_rows() {
    let entries = vec![
        entry(0, MentionKind::CodeSymbol, "graph://b", "sym"),
        entry(1, MentionKind::CodeSymbol, "graph://a", "sym"),
    ];
    let ranked = rank_all(&entries, "");
    // Same tier, same rank (empty query), same display: the code tie key is
    // the URI, so ordering is deterministic.
    let uris: Vec<&str> = ranked.iter().map(|e| e.uri.as_str()).collect();
    assert_eq!(uris, vec!["graph://a", "graph://b"]);
}

#[test]
fn equal_display_non_code_rows_keep_input_order() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///second", "dup"),
        entry(1, MentionKind::File, "file:///first", "dup"),
    ];
    let ranked = rank_all(&entries, "");
    let uris: Vec<&str> = ranked.iter().map(|e| e.uri.as_str()).collect();
    // Stable sort: full ties (tier + rank + display) preserve source order.
    assert_eq!(uris, vec!["file:///second", "file:///first"]);
}

#[test]
fn limit_zero_one_and_beyond_match_count() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///a", "a"),
        entry(1, MentionKind::File, "file:///b", "b"),
    ];
    assert!(rank_with(&entries, "", limit(0)).is_empty());
    assert_eq!(displays(&rank_with(&entries, "", limit(1))), vec!["a"]);
    assert_eq!(rank_with(&entries, "", limit(5)).len(), 2);
    assert_eq!(
        rank_with(
            &entries,
            "",
            RankOptions {
                limit: None,
                tiers: TierPolicy::default()
            }
        )
        .len(),
        2
    );
}

fn limit(k: usize) -> RankOptions {
    RankOptions {
        limit: Some(k),
        tiers: TierPolicy::default(),
    }
}

#[test]
fn search_text_overrides_display_for_matching() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///a", "zz-hidden"),
        entry(1, MentionKind::File, "file:///b", "zz-shown"),
    ];
    let mut searchable = entries.clone();
    searchable[0].search_text = Some("alpha".into());
    // Both match "zz" by display; only the first matches "alpha" via
    // search_text while its display does not contain it.
    let alpha = rank_all(&searchable, "alpha");
    assert_eq!(displays(&alpha), vec!["zz-hidden"]);
}

#[test]
fn deterministic_across_repeated_calls() {
    let entries = vec![
        entry(0, MentionKind::File, "file:///b", "b.rs"),
        entry(1, MentionKind::File, "file:///a", "a.rs"),
        entry(2, MentionKind::Custom("worker".into()), "worker://a", "w"),
    ];
    let first = rank_all(&entries, "rs");
    let second = rank_all(&entries, "rs");
    assert_eq!(first, second);
}
