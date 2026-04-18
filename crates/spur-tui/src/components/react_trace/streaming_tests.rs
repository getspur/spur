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
///
/// Note: uses `drain_fence_dispatches` (mid-stream, non-finalizing) rather than
/// `force_flush_all` (which now calls `flush_final`, a finalizing operation that
/// would prevent the subsequent `append_message`). The input `"First sentence.\n\nsep"`
/// contains `\n\n` with following content, so `has_authoritative_closure_pattern`
/// fires immediately inside `maybe_flush`, driving a synchronous `flush_now`.
#[test]
fn ghost_text_rc1_regression() {
    let mut trace = ReactTrace::new_for_tests();
    // First chunk ends with `\n\n<word>` so the paragraph's End event
    // lands at range.end < raw_text.len() and cursor advances after flush.
    trace.append_message("First sentence.\n\nsep", "claude", "10:00:00".to_string());

    // Mid-stream flush (non-finalizing) via the existing public API.
    // `has_authoritative_closure_pattern` fires on `\n\n` + content, so
    // `maybe_flush` takes the fast path and calls `flush_now` synchronously.
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

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

/// T5.2 — trailing fenced code block renders with markdown styling on
/// TurnComplete (force_flush_all), not as plain tail.
#[test]
fn turn_complete_final_code_fence_renders_styled() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Here's the code:\n\n```rust\nfn main() {}\n```",
        "claude",
        "10:00".to_string(),
    );
    trace.force_flush_all(&StateLookup::empty());

    // After flush_final, the stream's tail should be empty — everything committed.
    let stream = trace
        .entries_for_tests()
        .iter()
        .find_map(|e| e.markdown.as_ref())
        .expect("agent message entry has a markdown stream");
    let (_, tail) = stream.items_and_tail();
    assert_eq!(tail, "", "TurnComplete must commit all raw_text; tail={:?}", tail);
    assert!(stream.is_finalized());
}

/// TI.3 — open code fence containing `\n\n` must not busy-loop.
///
/// Under the stateless heuristic, this tail matches \n\n+content but
/// pulldown can't advance the cursor (fence still open). The
/// dirty_since guard must make the SECOND maybe_flush short-circuit.
#[test]
fn code_block_with_blank_line_does_not_cause_cpu_spike() {
    use crate::components::markdown_stream::{MarkdownStream, StateLookup};

    let mut s = MarkdownStream::new();
    s.append("```rust\nfn main() {\n\n    body\n\n}\n");

    let count_before = s.rebuild_count_for_tests();
    s.maybe_flush(&StateLookup::empty());
    let count_after_first = s.rebuild_count_for_tests();
    s.maybe_flush(&StateLookup::empty());
    s.maybe_flush(&StateLookup::empty());
    s.maybe_flush(&StateLookup::empty());
    let count_after_three_more = s.rebuild_count_for_tests();

    assert_eq!(count_after_first - count_before, 1,
        "first maybe_flush triggers one rebuild (fast-path fires)");
    assert_eq!(count_after_three_more - count_after_first, 0,
        "subsequent maybe_flushes must short-circuit on dirty_since=None; \
         got {} extra rebuilds", count_after_three_more - count_after_first);
}

/// T4.3 — both render paths produce the same textual content.
///
/// Note: all content is appended before `force_flush_all` because `force_flush_all`
/// now calls `flush_final`, which finalizes the stream and prevents further appends.
#[test]
fn both_render_paths_produce_identical_textual_content() {
    let mut trace = ReactTrace::new_for_tests();
    // Append all content before flushing — force_flush_all uses flush_final
    // (finalizing), so no appends may follow it.
    trace.append_message("# Title\n\nBody text.\n\nmore tail bytes", "claude", "10:00".to_string());
    trace.force_flush_all(&StateLookup::empty());

    let flat = trace.build_display_lines_for_tests("", None);
    let flat_text: String = flat.iter().map(|l| {
        l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
    }).collect::<Vec<_>>().join("\n");

    let (rows, _) = trace.build_virtual_rows_for_tests(0, 200, &std::collections::HashMap::new(), None);
    let virt_text: String = rows.iter().filter_map(|r| match r {
        crate::components::react_trace::VirtualRow::Text(line) => Some(
            line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        ),
        _ => None,
    }).collect::<Vec<_>>().join("\n");

    // Allow minor whitespace differences from wrapping; require every
    // substantive content fragment to appear in both.
    for needle in ["Title", "Body text", "tail bytes"] {
        assert!(flat_text.contains(needle), "flat missing {:?}: {}", needle, flat_text);
        assert!(virt_text.contains(needle), "virt missing {:?}: {}", needle, virt_text);
    }
}
