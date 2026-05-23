pub fn rust_full_rewrite_after(records: &[&str]) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        if record.len() > 3 {
            seen.insert(record.trim().to_ascii_lowercase());
        }
    }
    seen.len()
}
