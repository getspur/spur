use super::entry::CommandEntry;
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};

/// Rank `entries` by fuzzy-match against `query`.
///
/// * Empty query: input order preserved.
/// * Non-empty query: entries with a positive nucleo score are sorted by
///   descending score; unmatched entries are omitted.
pub fn rank(entries: &[CommandEntry], query: &str) -> Vec<CommandEntry> {
    if query.is_empty() {
        return entries.to_vec();
    }
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, CommandEntry)> = entries
        .iter()
        .filter_map(|e| {
            let haystack = e.name.clone();
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                &mut matcher,
            )?;
            Some((score, e.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, e)| e).collect()
}
