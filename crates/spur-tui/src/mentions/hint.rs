//! Send-time helper: if the outgoing user message contains any
//! `worker://<name>` atoms whose names are known to the registry,
//! prepend a one-line preference hint as the first
//! `ContentBlock::Text` of the outgoing blocks.
//!
//! See design spec §4.6.

use std::collections::HashSet;

use spur_acp::{ContentBlock, TextContent};

use crate::components::input_bar::ProtectedRange;

/// Builds the hint by collecting `worker://<name>` URIs from `ranges`,
/// keeping only those present in `known_workers`, then sorting and
/// deduplicating (sort-then-dedup is required because `Vec::dedup`
/// only removes *consecutive* duplicates).
///
/// Returns `true` if a hint was prepended; otherwise leaves
/// `blocks` unchanged and returns `false`.
pub fn prepend_worker_hint(
    blocks: &mut Vec<ContentBlock>,
    ranges: &[ProtectedRange],
    known_workers: &HashSet<String>,
) -> bool {
    let mut names: Vec<String> = ranges
        .iter()
        .filter_map(|r| r.uri.strip_prefix("worker://"))
        .filter(|n| known_workers.contains(*n))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return false;
    }
    let hint = format!(
        "[UI hint] User-suggested workers for delegation this turn: {} \
         (preference, not override; honor unless `delegation.avoid_for` clearly matches).",
        names.join(", ")
    );
    blocks.insert(0, ContentBlock::Text(TextContent::new(hint)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn range(uri: &str) -> ProtectedRange {
        ProtectedRange {
            start: 0,
            end: 0,
            uri: uri.into(),
            name: String::new(),
        }
    }

    fn hint_text(blocks: &[ContentBlock]) -> Option<&str> {
        match blocks.first()? {
            ContentBlock::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    #[test]
    fn dedupes_and_sorts_known_workers() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("worker://a"),
            range("worker://a"),
            range("worker://missing"),
            range("worker://b"),
        ];
        let known = known(&["a", "b", "c"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        let h = hint_text(&blocks).expect("first block is Text");
        assert!(h.starts_with("[UI hint]"));
        assert!(h.contains("a, b"), "expected 'a, b' in hint, got: {}", h);
        assert!(!h.contains("missing"));
    }

    #[test]
    fn noop_when_no_worker_ranges() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("file:///abs/foo.rs")];
        let known = known(&["a"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
        assert_eq!(hint_text(&blocks), Some("user text"));
    }

    #[test]
    fn noop_when_all_worker_names_unknown() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("worker://ghost"), range("worker://phantom")];
        let known = known(&["a", "b"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
        assert_eq!(hint_text(&blocks), Some("user text"));
    }
}
