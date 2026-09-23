//! Deterministic full-sort ranking (2026-08-27 mentions spec §5).
//!
//! [`rank_full_sort`] is the reference ranking path: score every candidate
//! with a Nucleo smart-case fuzzy pattern over `search_text` (falling back
//! to `display`), full-sort with the TUI's transitive typed-query
//! comparator, then truncate to the caller's limit. The later top-K
//! selection (`select_nth_unstable_by` + prefix sort) must reproduce this
//! function's output exactly; the property test comparing the two lands
//! with that extraction.
//!
//! Empty queries skip the pattern entirely: every row matches with rank 0
//! and orders by tier, then stable tie key. Empty-query *section* layout
//! (headers, per-kind caps) stays frontend policy and never moves here.

use std::cmp::Ordering;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use crate::entry::{MentionEntry, MentionKind};

use std::collections::BTreeMap;

/// Tier assignment for caller-defined kinds in the comparator. File-tree
/// kinds always rank tier 0 and code kinds tier 4; [`MentionKind::Custom`]
/// names map through `custom` with `custom_fallback` for unmapped names.
/// The TUI registers `worker -> 1`, `issue -> 2`, `datasource -> 3`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierPolicy {
    /// Tier per caller-defined kind name.
    pub custom: BTreeMap<Box<str>, u8>,
    /// Tier for custom kinds absent from the map.
    pub custom_fallback: u8,
}

impl TierPolicy {
    /// Neutral default: no mapped kinds, unmapped customs after the file
    /// tree and before the code kinds.
    pub const fn new() -> Self {
        Self {
            custom: BTreeMap::new(),
            custom_fallback: 3,
        }
    }
}

impl Default for TierPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for one ranking pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankOptions {
    /// Output cap applied after the full sort. `None` keeps every match;
    /// `Some(0)` yields no rows.
    pub limit: Option<usize>,
    /// Cross-kind tier policy.
    pub tiers: TierPolicy,
}

impl RankOptions {
    /// Unlimited ranking under the default tier policy.
    pub const fn new() -> Self {
        Self {
            limit: None,
            tiers: TierPolicy::new(),
        }
    }
}

impl Default for RankOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Tier of the file-tree kinds (best).
pub const FILE_TREE_TIER: u8 = 0;
/// Tier of the code-graph kinds (worst).
pub const CODE_TIER: u8 = 4;

/// One scored candidate during ranking.
#[derive(Debug, Clone, Copy)]
pub struct RankedRef<'a> {
    /// Nucleo match score (higher is better).
    pub rank: u32,
    /// Tier from [`tier_of`].
    pub tier: u8,
    /// The candidate row.
    pub entry: &'a MentionEntry,
}

/// Score every candidate against `query` (empty query: every row matches
/// with rank 0), dropping non-matches.
pub fn score_all<'a>(
    entries: &[&'a MentionEntry],
    query: &str,
    tiers: &TierPolicy,
    matcher: &mut Matcher,
) -> Vec<RankedRef<'a>> {
    let pattern = (!query.is_empty())
        .then(|| Pattern::parse(query, CaseMatching::Smart, Normalization::Smart));
    let mut buf: Vec<char> = Vec::new();
    entries
        .iter()
        .filter_map(|entry| {
            let rank = match pattern.as_ref() {
                None => 0,
                Some(pattern) => {
                    buf.clear();
                    let haystack = entry.search_text.as_deref().unwrap_or(&entry.display);
                    pattern.score(Utf32Str::new(haystack, &mut buf), matcher)?
                }
            };
            Some(RankedRef {
                rank,
                tier: tier_of(&entry.kind, tiers),
                entry,
            })
        })
        .collect()
}

/// Full-sort the scored candidates with the typed-query comparator
/// (bucket-rank desc, tier asc, raw rank desc, stable tie key asc).
pub fn sort_ranked(scored: &mut [RankedRef<'_>]) {
    let max_rank = scored.iter().map(|ranked| ranked.rank).max().unwrap_or(0);
    scored.sort_by(|a, b| typed_query_cmp(a, b, max_rank));
}

/// Deterministic full-sort reference ranking: score all, sort all, then
/// truncate to `options.limit`.
pub fn rank_full_sort(
    entries: &[&MentionEntry],
    query: &str,
    options: &RankOptions,
    matcher: &mut Matcher,
) -> Vec<MentionEntry> {
    let mut scored = score_all(entries, query, &options.tiers, matcher);
    sort_ranked(&mut scored);
    if let Some(limit) = options.limit {
        scored.truncate(limit);
    }
    scored
        .into_iter()
        .map(|ranked| ranked.entry.clone())
        .collect()
}

/// Neutral tier of a kind under `tiers`.
pub fn tier_of(kind: &MentionKind, tiers: &TierPolicy) -> u8 {
    match kind {
        MentionKind::File | MentionKind::Directory => FILE_TREE_TIER,
        MentionKind::CodeFile | MentionKind::CodeSymbol => CODE_TIER,
        MentionKind::Custom(name) => tiers
            .custom
            .get(name.as_ref())
            .copied()
            .unwrap_or(tiers.custom_fallback),
    }
}

/// Stable tie key: the URI for code rows, the display label otherwise
/// (today's TUI `stable_tie_key`).
pub fn stable_tie_key(entry: &MentionEntry) -> &str {
    if matches!(entry.kind, MentionKind::CodeFile | MentionKind::CodeSymbol) {
        entry.uri.as_str()
    } else {
        entry.display.as_str()
    }
}

/// Score bucket relative to the global maximum (today's TUI
/// `score_bucket`): rank deciles, so near-identical scores tie before the
/// raw rank tiebreak.
pub fn score_bucket(score: u32, global_max: u32) -> u32 {
    if global_max == 0 {
        return 0;
    }
    let bucket_width = global_max.saturating_div(10).max(1);
    score.saturating_div(bucket_width)
}

/// The typed-query comparator, ported verbatim from the TUI registry:
/// bucket rank (desc), then tier (asc), then raw rank (desc), then the
/// stable tie key (asc). Full ties fall through to the stable sort's input
/// order, which is deterministic (source registration + snapshot order).
pub fn typed_query_cmp(a: &RankedRef<'_>, b: &RankedRef<'_>, max_rank: u32) -> Ordering {
    let bucket_a = score_bucket(a.rank, max_rank);
    let bucket_b = score_bucket(b.rank, max_rank);
    bucket_b
        .cmp(&bucket_a)
        .then(a.tier.cmp(&b.tier))
        .then(b.rank.cmp(&a.rank))
        .then_with(|| stable_tie_key(a.entry).cmp(stable_tie_key(b.entry)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::MentionId;

    fn file_entry(id: u64, display: &str) -> MentionEntry {
        MentionEntry {
            id: MentionId::new(id),
            kind: MentionKind::File,
            uri: format!("file:///{display}"),
            display: display.to_owned(),
            secondary: None,
            search_text: None,
            insert_text: None,
        }
    }

    fn ranked(entry: &MentionEntry, rank: u32, tier: u8) -> RankedRef<'_> {
        RankedRef { rank, tier, entry }
    }

    #[test]
    fn score_bucket_edges_match_the_tui_port() {
        assert_eq!(score_bucket(0, 0), 0);
        assert_eq!(score_bucket(500, 0), 0);
        // max 100 -> bucket width 10.
        assert_eq!(score_bucket(100, 100), 10);
        assert_eq!(score_bucket(99, 100), 9);
        assert_eq!(score_bucket(9, 100), 0);
        // max 5 -> width 1: bucket == score.
        assert_eq!(score_bucket(5, 5), 5);
        assert_eq!(score_bucket(0, 5), 0);
    }

    #[test]
    fn comparator_orders_bucket_then_tier_then_rank_then_key() {
        let low_tier_late = file_entry(0, "a");
        let high_tier_early = file_entry(1, "b");
        let same_bucket_higher_raw = file_entry(2, "c");
        let same_bucket_lower_raw = file_entry(3, "d");

        // Bucket 10 vs bucket 4: bucket wins regardless of tier.
        let a = ranked(&low_tier_late, 100, 4);
        let b = ranked(&high_tier_early, 40, 0);
        assert_eq!(typed_query_cmp(&a, &b, 100), Ordering::Less);

        // Same bucket (9), tier decides: 0 before 4.
        let a = ranked(&high_tier_early, 95, 0);
        let b = ranked(&low_tier_late, 94, 4);
        assert_eq!(typed_query_cmp(&a, &b, 100), Ordering::Less);

        // Same bucket and tier: raw rank desc.
        let a = ranked(&same_bucket_higher_raw, 98, 0);
        let b = ranked(&same_bucket_lower_raw, 91, 0);
        assert_eq!(typed_query_cmp(&a, &b, 100), Ordering::Less);

        // Full rank/tier tie: stable tie key asc.
        let a = ranked(&same_bucket_higher_raw, 90, 0);
        let b = ranked(&same_bucket_lower_raw, 90, 0);
        assert_eq!(typed_query_cmp(&a, &b, 100), Ordering::Less); // "c" < "d"
        assert_eq!(typed_query_cmp(&a, &a, 100), Ordering::Equal);
    }

    #[test]
    fn comparator_is_strict_weak_ordering() {
        let entries = [
            file_entry(0, "w"),
            file_entry(1, "x"),
            file_entry(2, "y"),
            file_entry(3, "z"),
        ];
        let base = vec![
            ranked(&entries[0], 95, 1),
            ranked(&entries[1], 100, 4),
            ranked(&entries[2], 86, 0),
            ranked(&entries[3], 45, 0),
        ];
        let max_rank = base.iter().map(|r| r.rank).max().unwrap_or(0);

        for a in &base {
            assert_eq!(typed_query_cmp(a, a, max_rank), Ordering::Equal);
        }
        for a in &base {
            for b in &base {
                assert_eq!(
                    typed_query_cmp(a, b, max_rank),
                    typed_query_cmp(b, a, max_rank).reverse()
                );
            }
        }
        for a in &base {
            for b in &base {
                for c in &base {
                    if typed_query_cmp(a, b, max_rank) == Ordering::Less
                        && typed_query_cmp(b, c, max_rank) == Ordering::Less
                    {
                        assert_eq!(typed_query_cmp(a, c, max_rank), Ordering::Less);
                    }
                }
            }
        }
    }

    #[test]
    fn tier_policy_maps_custom_kinds_with_fallback() {
        let mut tiers = TierPolicy::new();
        tiers.custom.insert("worker".into(), 1);
        assert_eq!(tier_of(&MentionKind::File, &tiers), FILE_TREE_TIER);
        assert_eq!(tier_of(&MentionKind::Directory, &tiers), FILE_TREE_TIER);
        assert_eq!(tier_of(&MentionKind::CodeSymbol, &tiers), CODE_TIER);
        assert_eq!(tier_of(&MentionKind::Custom("worker".into()), &tiers), 1,);
        assert_eq!(
            tier_of(&MentionKind::Custom("unknown".into()), &tiers),
            tiers.custom_fallback,
        );
    }
}
