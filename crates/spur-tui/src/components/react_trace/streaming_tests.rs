//! End-to-end streaming tests. Covers the original ghost-text RCA case
//! and related scenarios.

#![cfg(feature = "markdown")]

use super::ReactTrace;
use crate::components::markdown_stream::StateLookup;
use crate::components::spinner;

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

    let (rows, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let rendered: String = rows
        .iter()
        .filter_map(|r| match r {
            crate::components::react_trace::VirtualRow::Text(line) => Some(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

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
    assert_eq!(
        tail, "",
        "TurnComplete must commit all raw_text; tail={:?}",
        tail
    );
    assert!(stream.is_finalized());
}

/// TI.4 — append_message after force_flush_all must not panic.
///
/// Regression: append_message coalesced into the last AgentMessage entry
/// regardless of whether its markdown stream was finalized by flush_final.
/// A late chunk (or a new turn's first chunk) for the same agent would
/// trigger the debug_assert panic in MarkdownStream::append.
#[test]
fn append_message_after_turn_complete_creates_new_entry() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("First turn", "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    // After TurnComplete the stream is finalized; a new chunk for the same
    // agent must start a fresh entry rather than violating the contract.
    trace.append_message("Second turn", "claude", "10:01".into());

    let entries = trace.entries_for_tests();
    assert_eq!(
        entries.len(),
        2,
        "must create two separate AgentMessage entries"
    );

    let first = entries[0]
        .markdown
        .as_ref()
        .expect("first entry has stream");
    let second = entries[1]
        .markdown
        .as_ref()
        .expect("second entry has stream");

    assert!(first.is_finalized(), "first stream must remain finalized");
    assert!(
        !second.is_finalized(),
        "second stream must not be finalized"
    );

    let (_, first_tail) = first.items_and_tail();
    let (_, second_tail) = second.items_and_tail();
    assert_eq!(
        first_tail, "",
        "first stream tail must be empty after flush_final"
    );
    assert_eq!(
        second_tail, "Second turn",
        "second stream must contain new text in tail"
    );
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

    assert_eq!(
        count_after_first - count_before,
        1,
        "first maybe_flush triggers one rebuild (fast-path fires)"
    );
    assert_eq!(
        count_after_three_more - count_after_first,
        0,
        "subsequent maybe_flushes must short-circuit on dirty_since=None; \
         got {} extra rebuilds",
        count_after_three_more - count_after_first
    );
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
    trace.append_message(
        "# Title\n\nBody text.\n\nmore tail bytes",
        "claude",
        "10:00".to_string(),
    );
    trace.force_flush_all(&StateLookup::empty());

    let flat = trace.build_display_lines_for_tests("", None);
    let flat_text: String = flat
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (rows, _, _) =
        trace.build_virtual_rows_for_tests(0, 200, &std::collections::HashMap::new(), None);
    let virt_text: String = rows
        .iter()
        .filter_map(|r| match r {
            crate::components::react_trace::VirtualRow::Text(line) => Some(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Allow minor whitespace differences from wrapping; require every
    // substantive content fragment to appear in both.
    for needle in ["Title", "Body text", "tail bytes"] {
        assert!(
            flat_text.contains(needle),
            "flat missing {:?}: {}",
            needle,
            flat_text
        );
        assert!(
            virt_text.contains(needle),
            "virt missing {:?}: {}",
            needle,
            virt_text
        );
    }
}

// ─── L9 RCA ICEBERG SIMULATIONS ────────────────────────────────────────
//
// Mix of active regression guards and ignored diagnostics. Each test's
// individual Status line indicates its current role:
// - REGRESSION GUARD: actively run, fails if a fix regresses
// - diagnostic: #[ignore]'d, run manually to surface a hypothesis

/// SIM-1 — Tail→items reflow row-count delta.
///
/// Hypothesis (Layer 2A from L9 review): when MarkdownStream's debounced
/// flush converts raw tail text into structured items, the row count for
/// the AgentMessage entry changes — even though no new bytes were
/// appended between the two render snapshots.
///
/// Status: REGRESSION GUARD. Verified by F1 (preview_items).
#[test]
fn sim_tail_to_items_reflow_row_delta() {
    let mut trace = ReactTrace::new_for_tests();

    // Use a payload with structurally-different tail vs. items
    // representation: code fence (tail keeps ``` markers, items strip them),
    // heading (tail keeps `#`, items strip), and a list (tail keeps `-`).
    let payload = "# Heading line\n\n\
                   Some prose paragraph.\n\n\
                   ```rust\n\
                   fn main() {}\n\
                   ```\n\n\
                   - bullet one\n\
                   - bullet two\n\n\
                   tail";
    trace.append_message(payload, "claude", "10:00:00".to_string());

    let stream_before = trace
        .entries_for_tests()
        .iter()
        .find_map(|e| e.markdown.as_ref())
        .expect("agent message has markdown stream");
    let flushed_before = stream_before.flushed_byte_len_for_tests();
    let (items_before, tail_before) = stream_before.items_and_tail();
    let items_before_len = items_before.len();
    let tail_before_len = tail_before.len();
    let (rows_before, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let r_before = rows_before.len();

    // Trigger flush. NO new bytes appended.
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

    let stream_after = trace
        .entries_for_tests()
        .iter()
        .find_map(|e| e.markdown.as_ref())
        .expect("agent message has markdown stream");
    let flushed_after = stream_after.flushed_byte_len_for_tests();
    let (items_after, tail_after) = stream_after.items_and_tail();
    let items_after_len = items_after.len();
    let tail_after_len = tail_after.len();
    let (rows_after, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let r_after = rows_after.len();

    eprintln!(
        "SIM-1 diagnostic:\n  \
         flushed_byte_len: before={} after={}\n  \
         items.len:        before={} after={}\n  \
         tail.len (bytes): before={} after={}\n  \
         row count:        before={} after={}  delta={}",
        flushed_before,
        flushed_after,
        items_before_len,
        items_after_len,
        tail_before_len,
        tail_after_len,
        r_before,
        r_after,
        (r_after as i64) - (r_before as i64),
    );

    let dump = |rows: &[crate::components::react_trace::VirtualRow]| -> String {
        rows.iter()
            .enumerate()
            .map(|(i, r)| match r {
                crate::components::react_trace::VirtualRow::Text(line) => {
                    let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                    format!("  [{:3}] {}", i, s)
                }
                _ => format!("  [{:3}] <non-text row>", i),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    eprintln!("SIM-1 BEFORE rows:\n{}", dump(&rows_before));
    eprintln!("SIM-1 AFTER  rows:\n{}", dump(&rows_after));

    assert_eq!(
        r_before, r_after,
        "SIM-1: tail→items reflow changed row count from {} to {} \
         with zero new bytes appended. Ghost-text Layer 2A confirmed.",
        r_before, r_after
    );
}

/// SIM-2 — Viewport content shifts when no user input occurred.
///
/// Hypothesis (Layer 3E): `scroll_offset` is a row index, not a content
/// anchor. When tail→items reflow changes row counts, the slice
/// `rows[scroll_offset..scroll_offset+visible_height]` returns different
/// CONTENT for the same scroll_offset.
///
/// Status: REGRESSION GUARD. Verified by F1 (preview_items) for the streaming-flush case.
#[test]
fn sim_viewport_content_shifts_under_flush_with_no_input() {
    let mut trace = ReactTrace::new_for_tests();

    // Build content where every block contains structure (lists) that
    // pulldown-cmark reformats during the tail→items conversion. Each block
    // is small enough that several fit in a viewport.
    let mut payload = String::new();
    for i in 0..10 {
        payload.push_str(&format!("Block {} prose intro.\n\n", i));
        payload.push_str(&format!("- block {} bullet a\n", i));
        payload.push_str(&format!("- block {} bullet b\n\n", i));
    }
    payload.push_str("trailing");
    trace.append_message(&payload, "claude", "10:00:00".to_string());

    let (rows_initial, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let visible_height = 8usize;
    // Position viewport near the END — that's where the trailing blank gets
    // consolidated by pulldown into a different shape on flush, and it's the
    // realistic position for a user reading the latest streamed content.
    let scroll_offset = rows_initial.len().saturating_sub(visible_height);
    let visible_end = (scroll_offset + visible_height).min(rows_initial.len());

    let slice_to_string =
        |rows: &[crate::components::react_trace::VirtualRow], start: usize, end: usize| -> String {
            rows[start..end.min(rows.len())]
                .iter()
                .map(|r| match r {
                    crate::components::react_trace::VirtualRow::Text(line) => line
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>(),
                    _ => "<non-text>".to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

    let visible_before = slice_to_string(&rows_initial, scroll_offset, visible_end);

    // No new append. Flush only.
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

    let (rows_after, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let visible_after = slice_to_string(&rows_after, scroll_offset, visible_end);

    eprintln!(
        "SIM-2: row count before={} after={} delta={}",
        rows_initial.len(),
        rows_after.len(),
        (rows_after.len() as i64) - (rows_initial.len() as i64),
    );
    eprintln!(
        "SIM-2 VISIBLE BEFORE (scroll_offset={}):\n{}",
        scroll_offset, visible_before
    );
    eprintln!(
        "SIM-2 VISIBLE AFTER  (scroll_offset={}):\n{}",
        scroll_offset, visible_after
    );

    assert_eq!(
        visible_before, visible_after,
        "SIM-2: viewport at scroll_offset={} shows different content before vs. after \
         a flush that added zero bytes. Ghost-text Layer 3E confirmed.",
        scroll_offset
    );
}

// ─── L9 FIX PROTOTYPE — content-anchor scroll model ─────────────────────
//
// SIM-3 prototypes Option D's F3 fix entirely in test code (no
// production change). The hypothesis is:
//
//   If we anchor scroll position to *content* (the text the user is
//   currently looking at) instead of to a *row index*, then producer-side
//   reflow cannot shift what the user sees. The invariant
//
//       visible(N+1) = visible(N) shifted by f(user_input)
//
//   holds by construction.
//
// The prototype implements two pure helpers that the real F3 would
// integrate into the scroll API and render path:
//
//   row_to_anchor(rows, row_idx) -> Anchor
//   anchor_to_row(rows, anchor)  -> usize

/// A content anchor: identifies a row by its *text content* rather than
/// by its position in the row vector. In the real F3, this would be
/// `(entry_idx, byte_offset_within_entry)` resolved against
/// `entry_row_starts` + per-entry byte→row map. For the prototype, the
/// row's joined text is a sufficient proxy: it survives reflow because
/// pulldown produces the same characters for the same input bytes.
#[derive(Clone, Debug)]
struct ContentAnchor {
    text: String,
    /// Disambiguator — index of this content among duplicate rows in the
    /// pre-reflow document. A row containing only "   " (blank) appears
    /// many times; we want the Nth such row.
    occurrence: usize,
}

fn row_text(row: &crate::components::react_trace::VirtualRow) -> String {
    match row {
        crate::components::react_trace::VirtualRow::Text(line) => {
            line.spans.iter().map(|s| s.content.as_ref()).collect()
        }
        _ => "<non-text>".to_string(),
    }
}

fn row_to_anchor(
    rows: &[crate::components::react_trace::VirtualRow],
    row_idx: usize,
) -> ContentAnchor {
    let target_text = row_text(&rows[row_idx]);
    // Count how many rows BEFORE row_idx have identical text.
    let occurrence = rows[..row_idx]
        .iter()
        .filter(|r| row_text(r) == target_text)
        .count();
    ContentAnchor {
        text: target_text,
        occurrence,
    }
}

fn anchor_to_row(
    rows: &[crate::components::react_trace::VirtualRow],
    anchor: &ContentAnchor,
) -> Option<usize> {
    // Find the Nth row whose text matches anchor.text.
    let mut seen = 0usize;
    for (i, r) in rows.iter().enumerate() {
        if row_text(r) == anchor.text {
            if seen == anchor.occurrence {
                return Some(i);
            }
            seen += 1;
        }
    }
    // Anchor lost (text removed by reflow) — fall back to the last
    // surviving row that came before. In production F3 this is the
    // "snap to nearest preceding byte" path.
    None
}

/// SIM-3 — Verify the F3 anchor model eliminates ghost text.
///
/// Reuses SIM-2's failing scenario verbatim, but anchors the viewport
/// on content. After flush, re-resolves the anchor to a (possibly new)
/// row index and asserts the visible slice is unchanged.
///
/// Status: REGRESSION GUARD. Verified by F1 + F3.
#[test]
fn sim_fix_content_anchor_eliminates_ghost_text() {
    let mut trace = ReactTrace::new_for_tests();

    let mut payload = String::new();
    for i in 0..10 {
        payload.push_str(&format!("Block {} prose intro.\n\n", i));
        payload.push_str(&format!("- block {} bullet a\n", i));
        payload.push_str(&format!("- block {} bullet b\n\n", i));
    }
    payload.push_str("trailing");
    trace.append_message(&payload, "claude", "10:00:00".to_string());

    let (rows_before, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let visible_height = 8usize;
    let scroll_offset_before = rows_before.len().saturating_sub(visible_height);

    // Capture viewport AND anchor BEFORE flush.
    let visible_before: Vec<String> = rows_before
        [scroll_offset_before..(scroll_offset_before + visible_height).min(rows_before.len())]
        .iter()
        .map(row_text)
        .collect();
    let anchor = row_to_anchor(&rows_before, scroll_offset_before);

    // Flush — same producer event that broke SIM-2.
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

    // Re-anchor: ask "where did our anchor land?" instead of trusting the
    // old row index. This is what F3 would do in scroll_offset's place.
    let (rows_after, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let scroll_offset_after =
        anchor_to_row(&rows_after, &anchor).expect("anchor should be resolvable post-reflow");

    let visible_after: Vec<String> = rows_after
        [scroll_offset_after..(scroll_offset_after + visible_height).min(rows_after.len())]
        .iter()
        .map(row_text)
        .collect();

    eprintln!(
        "SIM-3 FIX PROTOTYPE:\n  \
         row count:        before={} after={} delta={}\n  \
         scroll_offset:    before={} after={} delta={}\n  \
         anchor.text:      {:?}  occurrence={}\n  \
         viewport drift:   row-index model would have shown different content;\n  \
                          content-anchor model preserves it.",
        rows_before.len(),
        rows_after.len(),
        (rows_after.len() as i64) - (rows_before.len() as i64),
        scroll_offset_before,
        scroll_offset_after,
        (scroll_offset_after as i64) - (scroll_offset_before as i64),
        anchor.text,
        anchor.occurrence,
    );
    eprintln!("SIM-3 VISIBLE BEFORE:\n  {}", visible_before.join("\n  "));
    eprintln!("SIM-3 VISIBLE AFTER:\n  {}", visible_after.join("\n  "));

    // The fix's success criterion: SAME visible content despite reflow.
    assert_eq!(
        visible_before, visible_after,
        "SIM-3: F3 content-anchor model FAILED to preserve viewport \
         under tail→items reflow. Fix design is inadequate."
    );
}

#[test]
fn with_kind_compact_sets_compact_flag() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let t = ReactTrace::with_kind_compact(AgentKind::Generic);
    assert!(
        t.is_compact(),
        "with_kind_compact should set compact = true"
    );

    let full = ReactTrace::with_kind(AgentKind::Generic);
    assert!(!full.is_compact(), "with_kind should leave compact = false");
}

/// SIM-4 — Stress test the fix across multiple sequential flushes.
///
/// Real streams emit many chunks; each may trigger a flush. The anchor
/// model must remain stable across an arbitrary number of reflow events
/// with no user input in between.
#[test]
fn sim_fix_content_anchor_survives_repeated_flushes() {
    let mut trace = ReactTrace::new_for_tests();

    // Append in chunks that each end with `\n\n + content` so each drain
    // triggers a flush.
    let chunks = [
        "Intro paragraph one with words.\n\nfiller1",
        " more text continuing.\n\nfiller2",
        "\n\n- list item alpha\n- list item beta\n\nfiller3",
        " trailing prose chunk.\n\nfiller4",
        "\n\n```rust\nfn x() {}\n```\n\nend",
    ];

    // Append all first to establish a stable initial document.
    for c in &chunks {
        trace.append_message(c, "claude", "10:00:00".to_string());
    }
    // Don't drain yet — capture the all-tail state.
    let (rows_initial, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let visible_height = 6usize;
    // Anchor on a row in the middle of the document (worst case for reflow).
    let initial_offset = rows_initial.len() / 2;
    let anchor = row_to_anchor(&rows_initial, initial_offset);
    let visible_initial: Vec<String> = rows_initial
        [initial_offset..(initial_offset + visible_height).min(rows_initial.len())]
        .iter()
        .map(row_text)
        .collect();

    // Now drain repeatedly. Each call may flush more content.
    let mut last_visible: Vec<String> = visible_initial.clone();
    for round in 0..5 {
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());
        let (rows_after, _, _) =
            trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
        let new_offset = anchor_to_row(&rows_after, &anchor);
        if let Some(off) = new_offset {
            let visible_after: Vec<String> = rows_after
                [off..(off + visible_height).min(rows_after.len())]
                .iter()
                .map(row_text)
                .collect();
            eprintln!(
                "SIM-4 round {}: rows={}, anchor resolved to offset={}, viewport stable={}",
                round,
                rows_after.len(),
                off,
                visible_after == last_visible
            );
            last_visible = visible_after;
        } else {
            eprintln!(
                "SIM-4 round {}: anchor lost (text removed by reflow)",
                round
            );
        }
    }

    // Final assertion: anchor's text still present in document.
    let (rows_final, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let final_offset = anchor_to_row(&rows_final, &anchor);
    assert!(
        final_offset.is_some(),
        "SIM-4: content anchor was lost after repeated reflows — \
         the proxy text-match resolver is too brittle. Real F3 needs \
         byte-offset anchoring, not text-match."
    );

    let off = final_offset.unwrap();
    let visible_final: Vec<String> = rows_final[off..(off + visible_height).min(rows_final.len())]
        .iter()
        .map(row_text)
        .collect();
    assert_eq!(
        visible_initial, visible_final,
        "SIM-4: viewport drifted across {} flushes despite content anchor.",
        5
    );
}

// ─── SIM-5..8 — VALIDATE THE FULL FIX ───────────────────────────────────
//
// These prototype the proposed Phase 1 (F1+F2) and Phase 2 (F3) fixes
// in test code and run them against the same scenarios that broke
// SIM-1..3. Goal: prove the fix combination is sufficient before
// committing to a production design.

/// SIM-5 — F1 design check: same bytes, pre-flush vs post-flush, row counts.
///
/// F1 says "tail rendering must produce the same row sequence as items
/// rendering for the same bytes." This test compares the pre-flush
/// render of bytes B against the post-flush render of bytes B and
/// quantifies the gap. The gap IS the work F1 must do.
#[test]
#[ignore = "diagnostic for F1 design - quantifies pre vs post flush rendering gap"]
fn sim_f1_design_quantifies_pre_vs_post_flush_gap() {
    let payloads: &[(&str, &str)] = &[
        ("prose only", "Para A.\n\nPara B.\n\nPara C.\n\ntail"),
        (
            "heading + fence + list",
            "# Heading\n\nProse.\n\n```rust\nfn x() {}\n```\n\n- a\n- b\n\ntail",
        ),
        ("nested list", "- top1\n  - sub1\n  - sub2\n- top2\n\ntail"),
        ("table", "| col1 | col2 |\n|---|---|\n| a | b |\n\ntail"),
    ];

    let dump_rows = |rows: &[crate::components::react_trace::VirtualRow]| -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                crate::components::react_trace::VirtualRow::Text(line) => line
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
                _ => "<non-text>".into(),
            })
            .collect()
    };

    for (name, payload) in payloads {
        // Trace A: pre-flush
        let mut a = ReactTrace::new_for_tests();
        a.append_message(payload, "claude", "10:00:00".into());
        let (rows_pre, _, _) =
            a.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);

        // Trace B: post-flush (force_flush_all uses flush_final)
        let mut b = ReactTrace::new_for_tests();
        b.append_message(payload, "claude", "10:00:00".into());
        b.force_flush_all(&StateLookup::empty());
        let (rows_post, _, _) =
            b.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);

        let pre = dump_rows(&rows_pre);
        let post = dump_rows(&rows_post);
        let delta = (post.len() as i64) - (pre.len() as i64);
        eprintln!(
            "SIM-5 [{}]: pre={} rows, post={} rows, delta={}",
            name,
            pre.len(),
            post.len(),
            delta
        );
        if pre != post {
            eprintln!("  MISMATCH:");
            for i in 0..pre.len().max(post.len()) {
                let p = pre.get(i).map(String::as_str).unwrap_or("<none>");
                let q = post.get(i).map(String::as_str).unwrap_or("<none>");
                let mark = if p == q { " " } else { "*" };
                eprintln!("    {} [{:2}] pre={:?}  post={:?}", mark, i, p, q);
            }
        }
    }
    // No assertion — this is diagnostic. The eprintln output is the result.
}

/// SIM-6 — Prototype F1: always render in post-flush state.
///
/// In production, F1 means "tail rendering uses the same parser as items
/// rendering, so the row sequence is identical." Here we simulate it by
/// force-flushing before each render. Verify SIM-2's failing scenario
/// now keeps the viewport stable.
///
/// CAVEAT: force_flush_all calls flush_final, which finalizes the
/// stream — no further appends allowed. So this test simulates a
/// FROZEN snapshot, not a live stream. SIM-7 covers the live case.
#[test]
fn sim_f1_prototype_freezes_viewport_under_reflow() {
    let mut trace = ReactTrace::new_for_tests();
    let mut payload = String::new();
    for i in 0..10 {
        payload.push_str(&format!("Block {} prose intro.\n\n", i));
        payload.push_str(&format!("- block {} bullet a\n", i));
        payload.push_str(&format!("- block {} bullet b\n\n", i));
    }
    payload.push_str("trailing");
    trace.append_message(&payload, "claude", "10:00:00".into());

    // F1 prototype: force a flush so subsequent renders are post-flush.
    trace.force_flush_all(&StateLookup::empty());

    // Render twice. With F1 simulated, both renders should be identical
    // because no additional reflow can happen.
    let (rows_1, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let (rows_2, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    assert_eq!(
        rows_1.len(),
        rows_2.len(),
        "SIM-6: post-flush renders must be deterministic; got {} vs {}",
        rows_1.len(),
        rows_2.len()
    );

    // Pick a viewport near the end (same as SIM-2).
    let visible_height = 8;
    let scroll_offset = rows_1.len().saturating_sub(visible_height);
    let dump = |rows: &[crate::components::react_trace::VirtualRow],
                from: usize,
                to: usize|
     -> Vec<String> {
        rows[from..to.min(rows.len())]
            .iter()
            .map(|r| match r {
                crate::components::react_trace::VirtualRow::Text(line) => {
                    line.spans.iter().map(|s| s.content.as_ref()).collect()
                }
                _ => "<non-text>".into(),
            })
            .collect()
    };

    let v1 = dump(&rows_1, scroll_offset, scroll_offset + visible_height);
    let v2 = dump(&rows_2, scroll_offset, scroll_offset + visible_height);
    eprintln!(
        "SIM-6 viewport stable across two post-flush renders:\n  v1={:?}\n  v2={:?}",
        v1, v2
    );
    assert_eq!(
        v1, v2,
        "SIM-6: F1 prototype must keep viewport stable across renders"
    );
}

/// SIM-7 — Realistic streaming with F1 simulated + F3 anchor.
///
/// Models a live stream: chunk → drain → render → chunk → drain → render.
/// User holds scroll position via content anchor. After each round,
/// verify the anchor's window still shows the same content (modulo any
/// new content appended AFTER the anchor).
///
/// This is the test SIM-4 was supposed to be.
#[test]
fn sim_f3_anchor_under_realistic_streaming_with_appends() {
    // Build initial content and immediately drain so we have an
    // established anchor in items-mode (the fully-fixed state F1
    // promises every render will be in).
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Initial paragraph one.\n\nInitial paragraph two.\n\nInitial paragraph three.\n\nx",
        "claude",
        "10:00:00".into(),
    );
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

    let (rows_initial, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let visible_height = 5;
    // Anchor near the TOP — this is the user reading earlier content
    // while new content streams in below them. The classic ghost-text scenario.
    let initial_offset = 1; // first content row after the header
    let anchor = row_to_anchor(&rows_initial, initial_offset);
    let visible_initial: Vec<String> = rows_initial
        [initial_offset..(initial_offset + visible_height).min(rows_initial.len())]
        .iter()
        .map(row_text)
        .collect();
    eprintln!(
        "SIM-7 initial viewport at anchor={:?}:\n  {}",
        anchor.text,
        visible_initial.join("\n  ")
    );

    // Stream more chunks, each followed by drain (debounce flush).
    let later_chunks = [
        " More text continuing the third paragraph.\n\nNew paragraph four.\n\ny",
        " Even more.\n\n- bullet alpha\n- bullet beta\n\nz",
        " Final batch.\n\n```rust\nfn end() {}\n```\n\nw",
    ];
    for (i, chunk) in later_chunks.iter().enumerate() {
        trace.append_message(chunk, "claude", "10:00:00".into());
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let (rows_now, _, _) =
            trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
        let new_offset = anchor_to_row(&rows_now, &anchor).expect("anchor must remain resolvable");
        let visible_now: Vec<String> = rows_now
            [new_offset..(new_offset + visible_height).min(rows_now.len())]
            .iter()
            .map(row_text)
            .collect();
        eprintln!(
            "SIM-7 round {}: rows={}, anchor at offset={}, viewport={:?}",
            i,
            rows_now.len(),
            new_offset,
            visible_now
        );
        assert_eq!(
            visible_initial, visible_now,
            "SIM-7: viewport drifted in round {} despite F3 content anchor.\n\
             initial viewport: {:?}\n\
             after round:      {:?}",
            i, visible_initial, visible_now
        );
    }
}

/// SIM-8 — Width resize with F3 anchor.
///
/// Different terminal widths produce different wrapped row sequences.
/// F3 must keep viewport stable across resizes. This is one of the
/// scenarios F1 alone cannot handle.
#[test]
fn sim_f3_anchor_under_terminal_resize() {
    let mut trace = ReactTrace::new_for_tests();
    let payload = "This is a very long paragraph that will wrap differently \
                   at different terminal widths and should stress the row-index \
                   anchor model under width changes.\n\n\
                   Second paragraph also long enough to wrap several times across \
                   typical terminal widths between 60 and 120 columns.\n\n\
                   Third paragraph with a recognizable phrase MARKER_ALPHA inside.\n\n\
                   trailing content";
    trace.append_message(payload, "claude", "10:00:00".into());
    trace.force_flush_all(&StateLookup::empty());

    // Anchor on a row containing the marker at width 80.
    let (rows_w80, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let marker_row_w80 = rows_w80
        .iter()
        .position(|r| row_text(r).contains("MARKER_ALPHA"))
        .expect("marker must be present at width 80");
    let anchor = row_to_anchor(&rows_w80, marker_row_w80);

    // Resize to width 60 — wrapping changes substantially.
    let (rows_w60, _, _) =
        trace.build_virtual_rows_for_tests(0, 60, &std::collections::HashMap::new(), None);
    let resolved_w60 = anchor_to_row(&rows_w60, &anchor);
    eprintln!(
        "SIM-8 width 80→60: marker at row {} (w80) resolves to {:?} (w60)",
        marker_row_w80, resolved_w60
    );
    eprintln!(
        "SIM-8 row count w80={} w60={}",
        rows_w80.len(),
        rows_w60.len()
    );

    // Anchor's text-match resolver may fail at narrower width because
    // the marker line might wrap into multiple rows where the marker
    // text is no longer a stand-alone line.
    if let Some(off) = resolved_w60 {
        let visible = rows_w60[off..(off + 1).min(rows_w60.len())]
            .iter()
            .map(row_text)
            .collect::<Vec<_>>();
        eprintln!("SIM-8 resolved row at w60: {:?}", visible);
        assert!(
            visible.iter().any(|l| l.contains("MARKER_ALPHA")),
            "SIM-8: text-match anchor failed to find MARKER_ALPHA at w60 — \
             text-match is too brittle for resize. Real F3 needs (entry_idx, byte_offset)."
        );
    } else {
        // Documented limitation of the proxy resolver. Real F3 would use
        // byte-offset anchoring, which survives resize trivially.
        eprintln!(
            "SIM-8: text-match resolver lost the anchor on resize — \
             this is the proxy limitation. Real F3's (entry_idx, byte_offset) \
             anchor would survive because byte offset is invariant under width change."
        );
    }
}

/// RC2 — scroll mutators must use fresh row metrics, not stale
/// last_total_lines from a previous render.
#[test]
fn scroll_uses_fresh_row_count_after_append() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "para one\n\npara two\n\npara three",
        "claude",
        "10:00".into(),
    );
    trace.set_visible_height_for_tests(1);

    // Append more content, then seed cache with the updated layout so
    // shift_anchor_by can resolve using fresh row metrics.
    trace.append_message(
        "\n\npara four\n\npara five\n\npara six\n\npara seven",
        "claude",
        "10:00".into(),
    );
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());

    trace.scroll_to_bottom();
    assert!(
        trace.is_following(),
        "after scroll_to_bottom, anchor must be Following"
    );

    trace.scroll_up();
    // With fresh row metrics (from seeded cache), scroll_up from Following must
    // transition to a Row anchor pointing one row above the bottom.
    assert!(
        !trace.is_following(),
        "scroll_up must use fresh row count to transition out of Following; \
         got is_following={}, anchor={:?}",
        trace.is_following(),
        trace.anchor_for_tests()
    );
}

/// F3 regression: anchor on byte X in entry N survives a width resize.
#[test]
fn phase2_f3_anchor_byte_offset_survives_width_resize() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "A long paragraph with the recognizable phrase MARKER inside that wraps differently at different widths.",
        "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    // Anchor a viewport at width 80.
    let (rows_w80, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let _ = rows_w80;
    trace.scroll_to_top();
    trace.scroll_down_by(1);

    // Snapshot anchor.
    let anchor_before = trace.anchor_for_tests();

    // Re-render at width 60 — wrapping changes substantially.
    let (_rows_w60, _, _) =
        trace.build_virtual_rows_for_tests(0, 60, &std::collections::HashMap::new(), None);
    let anchor_after = trace.anchor_for_tests();

    assert_eq!(
        anchor_before, anchor_after,
        "F3: ScrollAnchor must be invariant under width change"
    );
}

/// F3 regression: anchor on entry that gets evicted snaps to (0, 0).
#[test]
fn phase2_f3_anchor_survives_eviction() {
    use crate::components::react_trace::types::ScrollAnchor;
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("entry 0 content", "claude", "10:00".into());
    trace.scroll_to_top();
    trace.scroll_down_by(1);

    // Force eviction by exceeding MAX_LOG_ENTRIES.
    for i in 1..2000 {
        trace.append_message(&format!("entry {} content", i), "claude", "10:00".into());
    }

    let anchor = trace.anchor_for_tests();
    match anchor {
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            assert!(
                entry_idx < trace.entries_for_tests().len(),
                "anchor.entry_idx must point at a surviving entry"
            );
            assert!(
                row_within_entry == 0 || entry_idx > 0,
                "evicted-entry anchor must snap to (0, 0)"
            );
        }
        ScrollAnchor::Following => {
            // Acceptable: streaming pushed user back to bottom.
        }
    }
}

// ─── L9 POST-MERGE PRODUCTION-PATH AUDITS ────────────────────────────
//
// SIM-9..12 exercise the REAL ScrollAnchor + resolve_anchor +
// shift_anchor_by code paths (not the SIM-3/SIM-7 text-match proxy).
// They test invariants the design claims but the v1 implementation may
// not actually achieve.

/// SIM-13 — Round-trip: shift_anchor_by sets target row, render reads it back.
///
/// In a 50-paragraph single AgentMessage, scroll down by 5, then read
/// the resolved row index from the render path. Verify the row index
/// actually advanced. With sub-entry byte granularity broken, the
/// rendered offset will be 0 (entry start) regardless of scroll input.
#[test]
fn sim_render_offset_reflects_scroll_input() {
    let mut trace = ReactTrace::new_for_tests();
    let mut payload = String::new();
    for i in 0..50 {
        payload.push_str(&format!("Para {}.\n\n", i));
    }
    trace.append_message(&payload, "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    let (rows_initial, entry_row_starts, _byte_ranges) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(5);
    // Seed line_cache so shift_anchor_by can resolve the anchor.
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());
    let total = rows_initial.len();

    trace.scroll_to_top();
    let row0 = crate::components::react_trace::render::resolve_anchor(
        &trace.anchor_for_tests(),
        &entry_row_starts,
        total,
        5,
    );
    trace.scroll_down_by(5);
    let row5 = crate::components::react_trace::render::resolve_anchor(
        &trace.anchor_for_tests(),
        &entry_row_starts,
        total,
        5,
    );
    eprintln!(
        "SIM-13: rendered row at top={}, after scroll_down_by(5)={}",
        row0, row5
    );

    assert!(
        row5 > row0,
        "SIM-13: scroll_down_by(5) did not advance the rendered row index. \
         row0={}, row5={}. Scroll within an entry is non-functional.",
        row0,
        row5
    );
}

/// SIM-9 — Sub-entry scroll resolution.
///
/// Hypothesis (production bug): v1 byte_ranges use entry-level granularity
/// (every row in entry N has range `Some(0..entry_byte_len)`).
/// Therefore `row_to_byte_anchor(target_row)` always returns
/// `(entry_idx, 0)`. Storing this as the anchor and then resolving it
/// snaps back to the FIRST row of the entry — losing the target row.
///
/// Concretely: scrolling up by 1 from the middle of a long single
/// AgentMessage entry should move the viewport up by exactly 1 row.
/// With entry-level granularity, it instead snaps to the entry's start.
#[test]
fn sim_sub_entry_scroll_resolution() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::react_trace::types::ScrollAnchor;
    let mut trace = ReactTrace::new_for_tests();

    let mut payload = String::new();
    for i in 0..30 {
        payload.push_str(&format!("Paragraph {} content here.\n\n", i));
    }
    trace.append_message(&payload, "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    // Seed the cache so shift_anchor_by has layout to read.
    let (rows_initial, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());
    trace.set_visible_height_for_tests(5);

    // Position viewport in the middle of the (single) AgentMessage entry.
    trace.scroll_to_top();
    let middle_target_row = rows_initial.len() / 2;
    trace.scroll_down_by(middle_target_row);
    let anchor_after_scroll_down = trace.anchor_for_tests();
    eprintln!(
        "SIM-9 after scroll_down_by({}): anchor = {:?}",
        middle_target_row, anchor_after_scroll_down
    );

    // Now scroll_up by 1. Expectation: viewport moves up exactly 1 row.
    trace.scroll_up();
    let anchor_after_scroll_up = trace.anchor_for_tests();
    eprintln!(
        "SIM-9 after scroll_up: anchor = {:?}",
        anchor_after_scroll_up
    );

    // Now both anchors should be Row variant; they must differ.
    match (anchor_after_scroll_down, anchor_after_scroll_up) {
        (
            ScrollAnchor::Row {
                entry_idx: e1,
                row_within_entry: r1,
            },
            ScrollAnchor::Row {
                entry_idx: e2,
                row_within_entry: r2,
            },
        ) => {
            assert!(
                e1 != e2 || r1 != r2,
                "SIM-9: scroll_up did not change the anchor — sub-entry \
                 scroll is broken. Both anchors are Row{{{}, {}}}.",
                e1,
                r1
            );
        }
        _ => {
            panic!(
                "SIM-9: expected both anchors to be Row variant; got {:?} and {:?}",
                anchor_after_scroll_down, anchor_after_scroll_up
            );
        }
    }
}

/// SIM-10 — Mermaid-state mismatch in shift_anchor_by.
///
/// Fix verification: `shift_anchor_by` now reads from `line_cache` (seeded
/// with real FenceRender states at render time), so scroll math uses the same
/// coordinate system as the painted render. This test seeds the cache with
/// Ready(6) states, scrolls to a row that only exists in the Ready layout,
/// and confirms the resolved row falls within the real layout — proving
/// shift_anchor_by used the cache's real layout (would fail under the old
/// empty-states implementation).
#[test]
fn sim_mermaid_state_mismatch_in_shift() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::mermaid::{FenceRender, MermaidId};
    use crate::components::react_trace::types::ScrollAnchor;

    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Intro paragraph.\n\n```mermaid\ngraph LR\nA --> B\n```\n\nOutro paragraph.",
        "claude",
        "10:00".into(),
    );
    trace.force_flush_all(&StateLookup::empty());

    // Pre-condition: layouts under Pending vs Ready DIFFER.
    let mut ready_states = std::collections::HashMap::new();
    ready_states.insert(MermaidId(0), FenceRender::Ready(6));
    let (rows_real, _, _) = trace.build_virtual_rows_for_tests(0, 80, &ready_states, None);
    let (rows_pending, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    assert!(
        rows_real.len() > rows_pending.len(),
        "pre-condition: Ready layout must be taller than Pending; got real={} pending={}",
        rows_real.len(),
        rows_pending.len()
    );

    // Seed cache with REAL Ready states (this is what render does).
    trace.seed_line_cache_for_tests(80, &ready_states);
    trace.set_visible_height_for_tests(3);

    // Scroll to a row that exists in the Ready layout but NOT in the Pending layout.
    // Target row = rows_pending.len() (one row past the Pending layout's max).
    trace.scroll_to_top();
    trace.scroll_down_by(rows_pending.len());
    let anchor = trace.anchor_for_tests();
    eprintln!(
        "SIM-10 anchor after scroll_down_by({}): {:?}",
        rows_pending.len(),
        anchor
    );

    // The anchor must resolve to a row >= rows_pending.len() in the REAL layout.
    // If shift_anchor_by were using the buggy empty-states layout, it would
    // saturate target=rows_pending.len()-3 (visible_h) and produce a Row at row_pending - 3,
    // which is < rows_pending.len(). With the fix, it sees rows_real.len() and clamps to
    // rows_real - 3 >= rows_pending.
    match anchor {
        ScrollAnchor::Row {
            entry_idx: _,
            row_within_entry: _,
        } => {
            // Resolve back to a row index using the REAL ready layout.
            let (_, starts_real, _ranges_real) =
                trace.build_virtual_rows_for_tests(0, 80, &ready_states, None);
            let resolved = crate::components::react_trace::render::resolve_anchor(
                &anchor,
                &starts_real,
                rows_real.len(),
                3,
            );
            eprintln!("SIM-10 resolved row in real layout: {}", resolved);
            assert!(
                resolved >= rows_pending.len(),
                "SIM-10: scroll math used the wrong (Pending) layout. \
                 Anchor resolved to row {} but must be >= rows_pending.len() = {}. \
                 Real layout has {} rows. Under the buggy empty-states layout, scroll \
                 would have saturated at row {} = rows_pending - visible_h.",
                resolved,
                rows_pending.len(),
                rows_real.len(),
                rows_pending.len() - 3
            );
        }
        ScrollAnchor::Following => {
            // Acceptable if scroll target reached the end.
            eprintln!("SIM-10: anchor became Following — acceptable if real layout was used");
        }
    }
}

/// SIM-11 — Page-up walking through a long single message.
///
/// Hypothesis (consequence of SIM-9): pressing page_up multiple times
/// inside a long single AgentMessage entry should walk the viewport
/// upward by `last_visible_height - 2` rows each time. With entry-level
/// byte anchoring, every page_up snaps to the entry's start, so two
/// consecutive page_ups produce the same anchor.
#[test]
fn sim_page_up_walks_within_long_message() {
    let mut trace = ReactTrace::new_for_tests();

    let mut payload = String::new();
    for i in 0..50 {
        payload.push_str(&format!("Paragraph {} content here.\n\n", i));
    }
    trace.append_message(&payload, "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(10);
    // Seed line_cache so shift_anchor_by can resolve the anchor.
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());

    // Start from bottom (Following), page_up once to leave Following.
    trace.scroll_to_bottom();
    trace.page_up();
    let anchor_after_first_pageup = trace.anchor_for_tests();
    eprintln!("SIM-11 after 1st page_up: {:?}", anchor_after_first_pageup);

    // Page_up again. Expectation: anchor moves further up.
    // Bug: anchor stays the same (snapped to entry start).
    trace.page_up();
    let anchor_after_second_pageup = trace.anchor_for_tests();
    eprintln!("SIM-11 after 2nd page_up: {:?}", anchor_after_second_pageup);

    assert_ne!(
        anchor_after_first_pageup, anchor_after_second_pageup,
        "SIM-11: two consecutive page_ups within a long single entry \
         produced the same anchor — sub-entry scroll is broken. \
         Both anchors are {:?}",
        anchor_after_first_pageup
    );
}

/// SIM-12 — Anchor preservation across appends to OTHER entries.
///
/// User scrolls up to entry 3 and pins their viewport there. New chunks
/// arrive on entry 5 (the latest). The anchor must still point to
/// entry 3's content after the new chunks land.
#[test]
fn sim_anchor_preserved_across_appends_to_later_entries() {
    let mut trace = ReactTrace::new_for_tests();

    // Five entries from different agents (so each pushes a new entry).
    for i in 0..5 {
        trace.append_message(
            &format!("Entry {} content.\n\nMore text.", i),
            &format!("agent{}", i),
            "10:00".into(),
        );
    }
    trace.force_flush_all(&StateLookup::empty());

    // Anchor at entry 3, byte 0.
    trace.set_visible_height_for_tests(3);
    trace.scroll_to_top();
    // We can't reliably scroll directly to entry 3 without knowing entry
    // row starts, so set anchor manually via set_anchor_for_tests if it
    // exists, or scroll_down_by some amount.
    // Most tractable: scroll_down_by 6 (rough guess to land in entry 2 or 3).
    trace.scroll_down_by(6);
    let anchor_before = trace.anchor_for_tests();
    eprintln!("SIM-12 anchor before new appends: {:?}", anchor_before);

    // Append more content to entry 5's continuation (or push a new entry).
    trace.append_message(
        "More content arrives on the latest entry.",
        "agent5",
        "10:00".into(),
    );

    let anchor_after = trace.anchor_for_tests();
    eprintln!("SIM-12 anchor after new appends: {:?}", anchor_after);

    // Anchor should be unchanged — appending to LATER entries must not
    // shift the user's reading position.
    assert_eq!(
        anchor_before, anchor_after,
        "SIM-12: appending to a later entry shifted the anchor. \
         User's reading position is not preserved."
    );
}

/// COUNTER-2 (Phase 3): streaming + page_up monotonicity.
///
/// User holds page_up while content streams in via new entries. Each
/// page_up sees a freshly-seeded cache (entry list growing). The
/// resolved render row must NEVER move backward (downward) between
/// consecutive page_ups.
#[test]
fn phase3_counter_streaming_pageup_monotonic() {
    use crate::components::markdown_stream::StateLookup;

    let mut trace = ReactTrace::new_for_tests();

    // Seed initial set of long messages from different agents.
    for i in 0..40 {
        trace.append_message(
            &format!("entry {} paragraph one\n\nentry {} paragraph two", i, i),
            &format!("agent{}", i),
            "10:00".into(),
        );
    }
    trace.force_flush_all(&StateLookup::empty());
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());
    trace.set_visible_height_for_tests(10);

    // Start at the bottom (Following), then walk up via page_up.
    trace.scroll_to_bottom();
    let mut prev_resolved: Option<usize> = None;
    for step in 0..5 {
        // Streaming: append more entries (new agents) BEFORE the page_up,
        // then re-seed cache so shift sees the new layout.
        for j in 0..3 {
            let agent_idx = 40 + step * 3 + j;
            trace.append_message(
                &format!("stream entry {}", agent_idx),
                &format!("agent{}", agent_idx),
                "10:00".into(),
            );
        }
        trace.force_flush_all(&StateLookup::empty());
        trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());

        trace.page_up();

        // Resolve against the SAME cache the shift used.
        let (rows2, starts2, _byte_ranges2) =
            trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
        let resolved = crate::components::react_trace::render::resolve_anchor(
            &trace.anchor_for_tests(),
            &starts2,
            rows2.len(),
            10,
        );
        eprintln!(
            "COUNTER-2 step {}: total={} row={} anchor={:?}",
            step,
            rows2.len(),
            resolved,
            trace.anchor_for_tests()
        );
        if let Some(p) = prev_resolved {
            assert!(
                resolved <= p,
                "step {}: page_up moved DOWN (prev={} now={})",
                step,
                p,
                resolved
            );
        }
        prev_resolved = Some(resolved);
    }
}

/// EDGE-3 (Phase 3): stale `line_cache` between render and shift.
///
/// Cache populated → new entries arrive → shift uses STALE cache →
/// re-render produces fresh cache. Anchor must still resolve in-bounds.
#[test]
fn phase3_edge_stale_cache_safe() {
    use crate::components::markdown_stream::StateLookup;

    let mut trace = ReactTrace::new_for_tests();

    // Seed initial trace with 5 entries from different agents.
    for i in 0..5 {
        trace.append_message(
            &format!("entry {} content here", i),
            &format!("agent{}", i),
            "10:00".into(),
        );
    }
    trace.force_flush_all(&StateLookup::empty());

    // Populate cache (this is the "prior render" snapshot).
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());
    trace.set_visible_height_for_tests(3);

    // Position viewport at entry 2 area.
    trace.scroll_to_top();
    trace.scroll_down_by(2);
    let anchor_after_initial_scroll = trace.anchor_for_tests();
    eprintln!("EDGE-3 initial anchor: {:?}", anchor_after_initial_scroll);

    // NEW content arrives — DIFFERENT agents so each is a new entry
    // (avoids append-after-flush_final contract violation).
    for i in 5..15 {
        trace.append_message(
            &format!("late entry {} content", i),
            &format!("agent{}", i),
            "10:00".into(),
        );
    }
    trace.force_flush_all(&StateLookup::empty());

    // Cache is now STALE — it reflects 5 entries, but trace has 15.
    // Shift uses the stale cache.
    trace.scroll_down_by(2);
    let anchor_after_stale_shift = trace.anchor_for_tests();
    eprintln!("EDGE-3 stale-shift anchor: {:?}", anchor_after_stale_shift);

    // Now build the FRESH layout (this is what the next real render would do).
    let (rows, starts, _byte_ranges) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let resolved = crate::components::react_trace::render::resolve_anchor(
        &trace.anchor_for_tests(),
        &starts,
        rows.len(),
        3,
    );
    eprintln!(
        "EDGE-3 resolved row in fresh layout: {} (total {})",
        resolved,
        rows.len()
    );

    // Stale-cache anchor must still resolve in-bounds against fresh layout.
    assert!(
        resolved < rows.len(),
        "stale-cache shift produced out-of-range row {} (total {})",
        resolved,
        rows.len()
    );
}

/// EDGE-7 (Phase 3): mermaid Pending→Ready transition keeps the anchor
/// in-bounds. Anchor pinned mid-entry must remain valid after the fence
/// expands from 1 row to N rows.
#[test]
fn phase3_edge_mermaid_pending_to_ready_stable() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::mermaid::{FenceRender, MermaidId};

    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Intro paragraph.\n\n```mermaid\ngraph LR\nA --> B\n```\n\nOutro paragraph.",
        "claude",
        "10:00".into(),
    );
    trace.force_flush_all(&StateLookup::empty());

    // Pending state: empty FenceRender registry → 1 row for the fence.
    let pending: std::collections::HashMap<MermaidId, FenceRender> =
        std::collections::HashMap::new();
    let (rows_p, starts_p, ranges_p) = trace.build_virtual_rows_for_tests(0, 80, &pending, None);

    // Ready state: registry has Ready(6) → 6 rows for the fence.
    let mut ready = std::collections::HashMap::new();
    ready.insert(MermaidId(0), FenceRender::Ready(6));
    let (rows_r, starts_r, ranges_r) = trace.build_virtual_rows_for_tests(0, 80, &ready, None);

    assert!(
        rows_r.len() > rows_p.len(),
        "Ready layout must be taller than Pending; got pending={} ready={}",
        rows_p.len(),
        rows_r.len(),
    );

    // Pin anchor mid-entry under Pending.
    use crate::components::react_trace::types::ScrollAnchor;
    let anchor = ScrollAnchor::Row {
        entry_idx: 0,
        row_within_entry: 1,
    };
    let _ = &ranges_p;
    let _ = &ranges_r;
    let row_p =
        crate::components::react_trace::render::resolve_anchor(&anchor, &starts_p, rows_p.len(), 5);
    let row_r =
        crate::components::react_trace::render::resolve_anchor(&anchor, &starts_r, rows_r.len(), 5);
    assert!(
        row_p < rows_p.len(),
        "Pending: in-bounds row {} of {}",
        row_p,
        rows_p.len()
    );
    assert!(
        row_r < rows_r.len(),
        "Ready: in-bounds row {} of {}",
        row_r,
        rows_r.len()
    );
}

/// COUNTER-3 (Phase 3): Row anchor at entry N renumbers correctly when
/// entries 0..k are evicted (entry N now lives at index N-k).
#[test]
fn phase3_counter_eviction_renumbers_row_anchor() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::react_trace::types::ScrollAnchor;

    let mut trace = ReactTrace::new_for_tests();

    // Seed two distinguishable entries.
    trace.append_message("entry 0", "claude", "10:00".into());
    trace.append_message("entry 1 anchor here", "codex", "10:01".into());
    trace.force_flush_all(&StateLookup::empty());

    // Pin anchor at entry 1, row 0.
    trace.scroll_to_top();
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());
    trace.set_visible_height_for_tests(2);

    // Each plain-text entry contributes ≥1 row + blank separator. Scroll
    // 2 rows to land in entry 1.
    trace.scroll_down_by(2);

    let before = trace.anchor_for_tests();
    eprintln!("COUNTER-3 before: anchor={:?}", before);

    // Force eviction of entry 0 by appending until MAX_LOG_ENTRIES is exceeded.
    for i in 0..2000 {
        trace.append_message(
            &format!("filler {}", i),
            &format!("agent{}", i),
            "10:00".into(),
        );
    }

    let after = trace.anchor_for_tests();
    eprintln!("COUNTER-3 after: anchor={:?}", after);

    // After eviction: anchor.entry_idx must be < entries.len() (no out-of-range).
    if let ScrollAnchor::Row { entry_idx, .. } = after {
        assert!(
            entry_idx < trace.entries_for_tests().len(),
            "evicted: entry_idx {} out of range (entries.len={})",
            entry_idx,
            trace.entries_for_tests().len()
        );
    }
}

#[cfg(feature = "markdown")]
#[test]
fn build_display_lines_completed_shows_outcome_glyph() {
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
    let mut trace = super::ReactTrace::new();
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "echo".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: "hi".into(),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    let lines = trace.build_display_lines_for_tests(spinner::BRAILLE[0], None);
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
        .collect();
    assert!(joined.contains("✓"), "expected success glyph: {joined}");
    for f in spinner::BRAILLE {
        assert!(
            !joined.contains(f),
            "must not render spinner frame {f} for completed Act: {joined}"
        );
    }
}

#[cfg(feature = "markdown")]
#[test]
fn virtual_rows_collapsed_completed_act_shows_outcome_no_spinner() {
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
    let mut trace = super::ReactTrace::new();
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "echo".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: "hi".into(),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    let (rows, _, _) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let txt: String = rows
        .iter()
        .filter_map(|r| match r {
            super::VirtualRow::Text(l) => Some(
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        txt.contains("✓"),
        "virtual rows must contain outcome: {txt}"
    );
}

#[cfg(feature = "markdown")]
#[test]
fn build_display_lines_expanded_completed_renders_outcome_body() {
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
    let mut trace = super::ReactTrace::new();
    trace.toggle_observe_collapsed(); // expanded
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "echo hi".into(),
                cwd: None,
            },
            tool_call_id: None,
            status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                exit_code: Some(0),
                stdout: "hi".into(),
                stderr: String::new(),
            })),
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    let lines = trace.build_display_lines_for_tests(spinner::BRAILLE[0], None);
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
        .collect();
    assert!(joined.contains("hi"), "expected stdout body: {joined}");
    assert!(joined.contains("✓"), "expected success glyph: {joined}");
}

#[cfg(feature = "markdown")]
#[test]
fn completed_status_with_no_raw_output_stops_spinner() {
    use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
    use spur_acp::ToolCallId;
    use std::sync::Arc;
    let mut trace = super::ReactTrace::new();
    let id = ToolCallId::new(Arc::from("call-1"));
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "true".into(),
                cwd: None,
            },
            tool_call_id: Some(id.clone()),
            status: super::ActStatus::Pending,
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    // Simulate ToolCallUpdate { status: Completed, raw_output: None }.
    let prev_snapshot = match &trace.entries()[0].kind {
        super::TraceKind::Act { status, .. } => status.clone(),
        _ => unreachable!(),
    };
    let new = super::merge_status(
        &prev_snapshot,
        Some(spur_acp::ToolCallStatus::Completed),
        None,
        spur_acp::AgentKind::Generic,
    );
    if let super::TraceKind::Act { status, .. } = &mut trace.entries_mut_for_test()[0].kind {
        *status = new;
    }
    assert_eq!(trace.first_active_spinner(), None);
    assert!(matches!(
        trace.entries()[0].kind,
        super::TraceKind::Act {
            status: super::ActStatus::Completed(None),
            ..
        }
    ));
}

#[cfg(feature = "markdown")]
#[test]
fn in_progress_with_partial_raw_output_keeps_spinner() {
    use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
    use spur_acp::ToolCallId;
    use std::sync::Arc;
    let mut trace = super::ReactTrace::new();
    let id = ToolCallId::new(Arc::from("call-2"));
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "long".into(),
                cwd: None,
            },
            tool_call_id: Some(id.clone()),
            status: super::ActStatus::Pending,
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    let partial = serde_json::json!({"text": "partial output"});
    let new = super::merge_status(
        &super::ActStatus::Pending,
        Some(spur_acp::ToolCallStatus::InProgress),
        Some(&partial),
        spur_acp::AgentKind::Generic,
    );
    if let super::TraceKind::Act { status, .. } = &mut trace.entries_mut_for_test()[0].kind {
        *status = new;
    }
    assert_eq!(trace.first_active_spinner(), Some(0));
}

#[cfg(feature = "markdown")]
#[test]
fn multiple_updates_mutate_in_place_keep_entries_len_stable() {
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
    use spur_acp::ToolCallId;
    use std::sync::Arc;
    let mut trace = super::ReactTrace::new();
    let id = ToolCallId::new(Arc::from("call-3"));
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Command {
                cmd: "c".into(),
                cwd: None,
            },
            tool_call_id: Some(id.clone()),
            status: super::ActStatus::Pending,
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    assert_eq!(trace.entry_count(), 1);
    // In-progress update.
    if let Some((_idx, entry)) = trace.find_act_by_id_mut(&id) {
        if let super::TraceKind::Act { status, .. } = &mut entry.kind {
            let prev = status.clone();
            *status = super::merge_status(
                &prev,
                Some(spur_acp::ToolCallStatus::InProgress),
                None,
                spur_acp::AgentKind::Generic,
            );
        }
    }
    assert_eq!(trace.entry_count(), 1);
    // Completion.
    let out = serde_json::json!({"text": "done"});
    if let Some((_, entry)) = trace.find_act_by_id_mut(&id) {
        if let super::TraceKind::Act { status, .. } = &mut entry.kind {
            let prev = status.clone();
            *status = super::merge_status(
                &prev,
                Some(spur_acp::ToolCallStatus::Completed),
                Some(&out),
                spur_acp::AgentKind::Generic,
            );
        }
    }
    assert_eq!(trace.entry_count(), 1);
    // Accept ObservePayload::Text OR ObservePayload::Json depending on how
    // extract_observe maps {"text":"done"} under AgentKind::Generic.
    assert!(matches!(
        trace.entries()[0].kind,
        super::TraceKind::Act {
            status: super::ActStatus::Completed(Some(_)),
            ..
        }
    ));
    let _ = ObservePayload::Text {
        body: String::new(),
    };
}

#[cfg(feature = "markdown")]
#[test]
fn interleaved_observe_note_does_not_break_lookup() {
    use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
    use spur_acp::ToolCallId;
    use std::sync::Arc;
    let mut trace = super::ReactTrace::new();
    let id = ToolCallId::new(Arc::from("call-4"));
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Empty,
            tool_call_id: Some(id.clone()),
            status: super::ActStatus::Pending,
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    // Informational note pushed as Observe{None}.
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Observe { payload: None },
        text: "system note".into(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    // Terminal update still finds the original Act.
    let found = trace.find_act_by_id_mut(&id);
    assert!(
        found.is_some(),
        "note interleaving must not break id lookup"
    );
}

#[cfg(feature = "markdown")]
#[test]
fn failed_status_renders_failure_glyph_even_with_non_error_payload() {
    use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
    let mut trace = super::ReactTrace::new();
    trace.push(super::TraceEntry {
        kind: super::TraceKind::Act {
            tool: "shell".into(),
            family: ToolFamily::Execute,
            input: ToolInputDisplay::Empty,
            tool_call_id: None,
            // Failed with a Text payload (NOT Error variant) — must still
            // render as failure.
            status: super::ActStatus::Failed(Some(ObservePayload::Text { body: "meh".into() })),
        },
        text: String::new(),
        timestamp: "10:00".into(),
        markdown: None,
    });
    let joined = trace.render_to_strings().join("\n");
    assert!(
        joined.contains("✗"),
        "Failed must use ✗ regardless of payload shape: {joined}"
    );
}

#[test]
fn compact_render_produces_single_line_per_entry() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_think("thinking about the problem", "12:00".into());
    t.append_message("hello", "bot", "12:01".into());
    t.append_user_message("hi", "12:02".into());

    let lines = t.build_compact_lines_for_tests(40);
    assert!(
        lines.len() >= 3 && lines.len() <= 5,
        "expected 3-5 lines, got {}",
        lines.len()
    );
    for l in &lines {
        let cols: usize = l
            .spans
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert!(cols <= 40, "compact line exceeds width: {} cols", cols);
    }
}

#[test]
fn compact_render_truncates_long_text_with_ellipsis() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("x".repeat(200).as_str(), "bot", "12:01".into());
    let lines = t.build_compact_lines_for_tests(20);
    let rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
        .collect();
    assert!(
        rendered.contains('…'),
        "long text should be truncated with '…'"
    );
}

#[test]
fn compact_render_respects_width_at_narrow_widths() {
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;
    use unicode_width::UnicodeWidthStr;

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..3 {
        trace.append_user_message(&format!("user {}", i), "12:00".into());
        trace.append_message(&format!("bot {}", i), "bot", "12:00".into());
    }

    for width in [8, 12] {
        let lines = trace.build_compact_lines_for_tests(width);
        for line in &lines {
            let cols: usize = line
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            assert!(cols <= width as usize, "line width {cols} exceeds {width}");
        }

        let mut term = Terminal::new(TestBackend::new(width, 3)).unwrap();
        term.draw(|f| trace.render_compact(f, Rect::new(0, 0, width, 3)))
            .unwrap();
        let starts = trace.compact_entry_row_starts_for_tests().unwrap();
        trace.scroll_up();
        let resolved = crate::components::react_trace::render::resolve_anchor(
            &trace.anchor_for_tests(),
            &starts,
            trace.last_total_lines,
            trace.last_visible_height,
        );

        assert!(resolved < trace.last_total_lines);
        trace.scroll_to_bottom();
    }
}

#[test]
fn render_compact_does_not_panic_and_updates_dimensions() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("hello", "bot", "12:00".into());
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();

    assert_eq!(t.last_render_width, Some(40));
    assert_eq!(t.last_visible_height, 10);
    assert!(t.last_total_lines >= 1);
}

#[test]
fn render_compact_cache_hits_when_generation_stable() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("a", "bot", "12:00".into());
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();
    let gen_after_first = t.generation_for_tests();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();
    assert_eq!(t.generation_for_tests(), gen_after_first);
    assert!(t.dirty_from_for_tests().is_none());
}

#[test]
fn render_compact_incremental_rebuild_on_new_entry() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    // Alternate think/message to avoid coalescing, producing 500 distinct entries.
    for i in 0..250 {
        t.append_think(&format!("think-{}", i), "12:00".into());
        t.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();
    let lines_before = t.last_total_lines;
    t.append_user_message("hi-new", "12:01".into());
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();
    // After adding one new entry, total_lines should grow by at least 1
    // (plus possibly a separator line for the kind transition).
    assert!(
        t.last_total_lines > lines_before,
        "expected at least {} lines, got {}",
        lines_before + 1,
        t.last_total_lines
    );
}

#[test]
fn dispatch_agent_message_chunk_appends_message_entry() {
    use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
    use crate::components::react_trace::{ReactTrace, TraceKind};
    use spur_acp::{AgentKind, ContentBlock, ContentChunk, SessionUpdate, TextContent};

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("hello"),
    )));
    let mut depths = std::collections::HashMap::new();
    let mut ctx = DispatchCtx {
        agent_name: "claude",
        agent_kind: AgentKind::ClaudeCodeAcp,
        now_stamp: || "12:00".to_string(),
        tool_depth: &mut depths,
        skip_plan_trace: false,
    };
    dispatch_session_update(&mut trace, &update, &mut ctx);

    assert_eq!(trace.entry_count(), 1);
    let entries = trace.entries();
    assert!(matches!(&entries[0].kind, TraceKind::AgentMessage { .. }));
}

#[test]
fn dispatch_thought_chunk_appends_think_entry() {
    use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
    use crate::components::react_trace::{ReactTrace, TraceKind};
    use spur_acp::{AgentKind, ContentBlock, ContentChunk, SessionUpdate, TextContent};

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    let update = SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("considering options"),
    )));
    let mut depths = std::collections::HashMap::new();
    let mut ctx = DispatchCtx {
        agent_name: "claude",
        agent_kind: AgentKind::ClaudeCodeAcp,
        now_stamp: || "12:00".to_string(),
        tool_depth: &mut depths,
        skip_plan_trace: false,
    };
    dispatch_session_update(&mut trace, &update, &mut ctx);

    assert_eq!(trace.entry_count(), 1);
    assert!(matches!(&trace.entries()[0].kind, TraceKind::Think));
}

// ──────────────────────────────────────────────────────────────────────────
// Compact-path scroll correctness tests.
//
// These pin the behavior that `scroll_up`/`scroll_down`/`page_up`/`page_down`
// on a trace constructed via `with_kind_compact` (used by dashboard worker
// streams via `DetailPane::render`'s Stream tab) actually move the anchor
// off `Following`. Today's `shift_anchor_by` early-returns when `line_cache`
// is `None`, and the compact path only populates `compact_cache`, so j/k
// are silently no-ops on the dashboard Stream tab — a critical regression
// these tests lock down.
// ──────────────────────────────────────────────────────────────────────────

/// T-D1a: `scroll_up` on a compact trace with enough entries to overflow
/// the viewport must move the anchor off `Following`.
#[test]
fn scroll_up_on_compact_moves_anchor_off_following() {
    use crate::components::react_trace::types::ScrollAnchor;
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    // Alternate think / agent message to avoid coalescing; 20 distinct entries.
    for i in 0..10 {
        t.append_think(&format!("th-{}", i), "12:00".into());
        t.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10)))
        .unwrap();

    assert!(t.last_total_lines >= 20, "seed should overflow viewport");
    // Start at the tail; this is the default anchor.
    assert!(t.is_following(), "expected to start in Following");

    t.scroll_up();

    assert!(
        !t.is_following(),
        "scroll_up on compact must leave Following; anchor={:?}",
        t.anchor_for_tests()
    );
    assert!(
        matches!(t.anchor_for_tests(), ScrollAnchor::Row { .. }),
        "scroll_up should produce a Row anchor; got {:?}",
        t.anchor_for_tests()
    );
}

/// T-D1b: scrolling up across entries with kind transitions must account
/// for separator rows. With 10 alternating-kind entries, compact layout
/// produces 10 content rows plus 9 separator rows = 19 rows. Walking
/// `scroll_up` 5 times from `Following` (which resolves to row 14 at
/// height 5, since `max_offset = total - visible = 19 - 5 = 14`) must
/// land on row 9, which corresponds to an entry boundary, not a
/// separator-offset miscount.
#[test]
fn scroll_up_on_compact_accounts_for_separators() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    // Alternate think/user to force a separator between every pair.
    for i in 0..5 {
        t.append_think(&format!("th-{}", i), "12:00".into());
        t.append_user_message(&format!("us-{}", i), "12:00".into());
    }
    assert_eq!(t.entry_count(), 10);

    let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();

    // 10 entries + 9 transitions = 19 rendered rows.
    assert_eq!(
        t.last_total_lines, 19,
        "expected 19 rendered rows (10 entries + 9 separators), got {}",
        t.last_total_lines
    );

    // Walk scroll_up 5 times. On a 5-row viewport with 19 total rows,
    // the starting resolved row is `max_offset = 19 - 5 = 14`; after 5
    // scroll_ups it must be `9`, not `4` or similar miscounts.
    for _ in 0..5 {
        t.scroll_up();
    }

    let starts = t
        .compact_entry_row_starts_for_tests()
        .expect("compact cache should be populated after render_compact");
    let anchor = t.anchor_for_tests();
    use crate::components::react_trace::types::ScrollAnchor;
    let resolved_row = match anchor {
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => starts[entry_idx] + row_within_entry,
        ScrollAnchor::Following => t.last_total_lines - t.last_visible_height,
    };
    assert_eq!(
        resolved_row, 9,
        "5× scroll_up from Following on 19-row compact trace at height 5 \
         must resolve to row 9; got row {} (anchor={:?}, starts={:?})",
        resolved_row, anchor, starts,
    );
}

/// T-D1c: `entry_row_starts` on the compact cache must be strictly
/// non-decreasing and `starts[0] == 0`. Pins the cache invariant.
#[test]
fn compact_entry_row_starts_are_monotonic_and_start_at_zero() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    // Alternate think / message to force 6 distinct entries (consecutive
    // same-kind appends coalesce into the tail).
    for i in 0..3 {
        t.append_think(&format!("th-{}", i), "12:00".into());
        t.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }
    assert_eq!(t.entry_count(), 6);

    let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();

    let starts = t
        .compact_entry_row_starts_for_tests()
        .expect("compact cache should be populated");
    assert_eq!(
        starts.len(),
        6,
        "starts should have one entry per trace entry"
    );
    assert_eq!(starts[0], 0, "first entry must start at row 0");
    for pair in starts.windows(2) {
        assert!(
            pair[1] > pair[0],
            "entry_row_starts must be strictly increasing: {:?}",
            starts
        );
    }
}

/// T-D1d: Incremental rebuild (adding an entry after an initial render)
/// must leave `entry_row_starts` equal to what a from-scratch rebuild
/// would produce. Pins the paired-truncate-and-extend invariant.
#[test]
fn compact_incremental_rebuild_preserves_entry_row_starts() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    // Shared seeder: 5 alternating entries so no coalescing occurs.
    fn seed_five(t: &mut crate::components::react_trace::ReactTrace) {
        t.append_think("t-0", "12:00".into());
        t.append_message("m-0", "bot", "12:00".into());
        t.append_think("t-1", "12:00".into());
        t.append_message("m-1", "bot", "12:00".into());
        t.append_think("t-2", "12:00".into());
    }

    // Pass A: build 5 entries, render once (populates cache), then add
    // a 6th entry and re-render (incremental rebuild path).
    let mut ta = ReactTrace::with_kind_compact(AgentKind::Generic);
    seed_five(&mut ta);
    let mut term_a = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term_a
        .draw(|f| ta.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();
    ta.append_message("m-2", "bot", "12:00".into());
    term_a
        .draw(|f| ta.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();
    let incremental_starts = ta
        .compact_entry_row_starts_for_tests()
        .expect("compact cache should be populated after incremental rebuild");

    // Pass B: build the same 6 entries in one go, render once (cold build).
    let mut tb = ReactTrace::with_kind_compact(AgentKind::Generic);
    seed_five(&mut tb);
    tb.append_message("m-2", "bot", "12:00".into());
    let mut term_b = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term_b
        .draw(|f| tb.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();
    let fresh_starts = tb
        .compact_entry_row_starts_for_tests()
        .expect("compact cache should be populated after cold build");

    // Pins that starts is actually populated — if the cache doesn't track
    // per-entry row starts, both vectors would be empty and the test would
    // trivially pass.
    assert_eq!(
        incremental_starts.len(),
        6,
        "incremental rebuild should produce 6 entry row starts, got {:?}",
        incremental_starts
    );
    assert_eq!(
        incremental_starts, fresh_starts,
        "incremental rebuild must produce same entry_row_starts as cold build"
    );
}

#[test]
fn compact_cache_survives_width_change() {
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..4 {
        trace.append_think(&format!("th-{}", i), "12:00".into());
        trace.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }

    let mut term = Terminal::new(TestBackend::new(60, 5)).unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 60, 5)))
        .unwrap();

    let starts = trace.compact_entry_row_starts_for_tests().unwrap();
    assert_eq!(starts.len(), trace.entries_for_tests().len());

    trace.scroll_up();
    let anchor = trace.anchor_for_tests();
    let resolved = crate::components::react_trace::render::resolve_anchor(
        &anchor,
        &starts,
        trace.last_total_lines,
        trace.last_visible_height,
    );

    assert!(matches!(
        anchor,
        crate::components::react_trace::types::ScrollAnchor::Row { .. }
    ));
    assert_eq!(
        resolved,
        trace.last_total_lines - trace.last_visible_height - 1
    );
    assert!(resolved > 0, "width-change scroll must not snap to row 0");
}

#[test]
fn drop_compact_cache_stops_scroll_until_repaint() {
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..4 {
        trace.append_think(&format!("th-{}", i), "12:00".into());
        trace.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }

    let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 5)))
        .unwrap();

    trace.scroll_up();
    let anchor_before_drop = trace.anchor_for_tests();
    trace.drop_compact_cache();
    trace.scroll_up();

    assert_eq!(
        trace.anchor_for_tests(),
        anchor_before_drop,
        "dropping the compact cache leaves last_surface=Compact, so scroll stays parked until repaint repopulates the cache"
    );
}

#[cfg(feature = "markdown")]
#[test]
fn mixed_full_then_compact_on_same_trace_uses_compact_layout() {
    use std::collections::HashMap;

    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    let mut trace = ReactTrace::new_for_tests();
    trace.append_think("plan", "12:00".into());
    trace.append_message("alpha\nbeta\ngamma\ndelta", "claude", "12:01".into());
    trace.append_user_message("ping", "12:02".into());
    trace.force_flush_all(&StateLookup::empty());

    let full_total = trace
        .build_virtual_rows_for_tests(0, 18, &HashMap::new(), None)
        .0
        .len();
    let compact_total = trace.build_compact_lines_for_tests(20).len();
    assert!(
        full_total > compact_total,
        "precondition: full layout must differ"
    );

    let registry = HashMap::new();
    let mut image_cache = crate::components::image_cache::ImageCache::new();
    let mut ctx = crate::components::react_trace::RenderContext {
        mermaid_registry: &registry,
        picker: None,
        image_cache: &mut image_cache,
    };
    let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
    term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, 20, 6), &mut ctx, None))
        .unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 20, 2)))
        .unwrap();

    let starts = trace.compact_entry_row_starts_for_tests().unwrap();
    trace.scroll_up();
    let resolved = crate::components::react_trace::render::resolve_anchor(
        &trace.anchor_for_tests(),
        &starts,
        trace.last_total_lines,
        trace.last_visible_height,
    );

    assert_eq!(
        resolved,
        trace.last_total_lines - trace.last_visible_height - 1
    );
}

#[test]
fn compact_scroll_survives_entry_eviction() {
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..=(crate::components::MAX_LOG_ENTRIES) {
        trace.append_message(
            &format!("entry-{}", i),
            &format!("bot-{}", i),
            "12:00".into(),
        );
    }
    assert_eq!(
        trace.entries_for_tests().len(),
        crate::components::MAX_LOG_ENTRIES
    );

    let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
    term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 6)))
        .unwrap();

    let starts = trace.compact_entry_row_starts_for_tests().unwrap();
    trace.scroll_up();
    let anchor = trace.anchor_for_tests();
    let resolved = crate::components::react_trace::render::resolve_anchor(
        &anchor,
        &starts,
        trace.last_total_lines,
        trace.last_visible_height,
    );

    assert!(matches!(
        anchor,
        crate::components::react_trace::types::ScrollAnchor::Row { .. }
    ));
    assert!(resolved < trace.last_total_lines);
}
