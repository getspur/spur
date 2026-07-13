use similar::TextDiff;

/// Builds a line-level unified diff for an edit.
///
/// Only hunks containing changes are emitted, with at most `context_lines` of
/// unchanged context on either side. Identical inputs return an empty string.
/// Paths are formatted verbatim after the conventional `a/` and `b/` prefixes.
///
/// # Examples
///
/// ```
/// use spur_acp::adapter::unified_edit_diff;
///
/// let diff = unified_edit_diff("src/lib.rs", "one\ntwo\n", "one\nTWO\n", 3);
/// assert!(diff.contains("-two\n+TWO\n"));
/// ```
#[must_use]
pub fn unified_edit_diff(path: &str, old: &str, new: &str, context_lines: usize) -> String {
    let old_path = format!("a/{path}");
    let new_path = format!("b/{path}");
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context_lines)
        .header(&old_path, &new_path)
        .to_string()
}
