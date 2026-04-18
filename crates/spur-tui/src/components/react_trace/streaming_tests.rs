//! End-to-end streaming tests. Covers the original ghost-text RCA case
//! and related scenarios.

#![cfg(feature = "markdown")]

use super::ReactTrace;
use crate::components::markdown_stream::StateLookup;

/// TI.2 — the original ghost-text failing case.
///
/// Setup: a first chunk with an authoritative block boundary (`\n\n` + content)
/// so `flush_now` advances the cursor past the first sentence. cached_items
/// becomes non-empty. Then a second chunk appends BEFORE any further flush.
///
/// Old behavior (RC1 bug): builder.rs renders only `stream.items()` when
/// non-empty, silently dropping the tail.
/// New behavior: render_agent_message_body renders items + tail.
#[test]
fn ghost_text_rc1_regression() {
    let mut trace = ReactTrace::new_for_tests();
    // First chunk ends with `\n\n<word>` so the paragraph's End event
    // lands at range.end < raw_text.len() and cursor advances after flush.
    trace.append_message("First sentence.\n\nsep", "claude", "10:00:00".to_string());
    trace.force_flush_all(&StateLookup::empty());

    // Verify the setup produces a committed prefix.
    let committed_before = trace
        .entries_for_tests()
        .iter()
        .find_map(|e| e.markdown.as_ref())
        .map(|s| s.flushed_byte_len_for_tests())
        .unwrap_or(0);
    assert!(
        committed_before > 0,
        "test setup: cached_items must be non-empty after first flush"
    );

    // Second chunk — this is the one the RC1 bug would hide.
    trace.append_message(" Second chunk.", "claude", "10:00:00".to_string());

    let (rows, _) = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let rendered: String = rows.iter().filter_map(|r| match r {
        crate::components::react_trace::VirtualRow::Text(line) => Some(
            line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        ),
        _ => None,
    }).collect::<Vec<_>>().join("\n");

    assert!(
        rendered.contains("First sentence"),
        "committed prefix must still appear.\nRendered:\n{}",
        rendered
    );
    assert!(
        rendered.contains("Second chunk"),
        "ghost-text regression: second chunk must appear in rendered rows before the next flush.\nRendered:\n{}",
        rendered
    );
}
