/// `recall_milli * gold_total = 1000 * |gold ∩ hits[:k]|` (`sol_5f73941594ed4d15`).
pub fn recall_at_k(gold: &[impl AsRef<str>], hits: &[impl AsRef<str>], k: usize) -> u32 {
    if gold.is_empty() {
        return 0;
    }
    let hit_set: std::collections::BTreeSet<&str> =
        hits.iter().take(k).map(AsRef::as_ref).collect();
    let matched = gold
        .iter()
        .filter(|id| hit_set.contains(id.as_ref()))
        .count();
    ((matched * 1000) / gold.len()) as u32
}

/// `coverage_milli * total = COVERED_WEIGHT * covered + PARTIAL_WEIGHT * partial`.
pub fn coverage_milli(covered: u32, partial: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    (covered * super::COVERED_WEIGHT + partial * super::PARTIAL_WEIGHT) / total
}

pub fn graphify_slice<T>(items: &[T], n: usize) -> &[T] {
    let end = n.min(items.len());
    &items[..end]
}
