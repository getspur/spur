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

    let (rows, _, _) = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
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

    let (rows, _, _) = trace.build_virtual_rows_for_tests(0, 200, &std::collections::HashMap::new(), None);
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
        flushed_before, flushed_after,
        items_before_len, items_after_len,
        tail_before_len, tail_after_len,
        r_before, r_after,
        (r_after as i64) - (r_before as i64),
    );

    let dump = |rows: &[crate::components::react_trace::VirtualRow]| -> String {
        rows.iter().enumerate().map(|(i, r)| match r {
            crate::components::react_trace::VirtualRow::Text(line) => {
                let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                format!("  [{:3}] {}", i, s)
            }
            _ => format!("  [{:3}] <non-text row>", i),
        }).collect::<Vec<_>>().join("\n")
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

    let slice_to_string = |rows: &[crate::components::react_trace::VirtualRow],
                           start: usize,
                           end: usize| -> String {
        rows[start..end.min(rows.len())]
            .iter()
            .map(|r| match r {
                crate::components::react_trace::VirtualRow::Text(line) => line.spans.iter()
                    .map(|s| s.content.as_ref()).collect::<String>(),
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
        rows_initial.len(), rows_after.len(),
        (rows_after.len() as i64) - (rows_initial.len() as i64),
    );
    eprintln!("SIM-2 VISIBLE BEFORE (scroll_offset={}):\n{}", scroll_offset, visible_before);
    eprintln!("SIM-2 VISIBLE AFTER  (scroll_offset={}):\n{}", scroll_offset, visible_after);

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
    let occurrence = rows[..row_idx].iter().filter(|r| row_text(r) == target_text).count();
    ContentAnchor { text: target_text, occurrence }
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
/// Status: FAILS. F3 alone is insufficient — the anchor stabilizes the
/// TOP of the viewport, but rows BELOW the anchor still reflow. F1
/// (symmetric tail/items rendering) is required as well.
#[test]
#[ignore = "diagnostic - proves F3 alone is insufficient, F1 also required"]
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
    let scroll_offset_after = anchor_to_row(&rows_after, &anchor)
        .expect("anchor should be resolvable post-reflow");

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
        rows_before.len(), rows_after.len(),
        (rows_after.len() as i64) - (rows_before.len() as i64),
        scroll_offset_before, scroll_offset_after,
        (scroll_offset_after as i64) - (scroll_offset_before as i64),
        anchor.text, anchor.occurrence,
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
        let (rows_after, _, _) = trace.build_virtual_rows_for_tests(
            0, 80, &std::collections::HashMap::new(), None);
        let new_offset = anchor_to_row(&rows_after, &anchor);
        if let Some(off) = new_offset {
            let visible_after: Vec<String> = rows_after
                [off..(off + visible_height).min(rows_after.len())]
                .iter()
                .map(row_text)
                .collect();
            eprintln!(
                "SIM-4 round {}: rows={}, anchor resolved to offset={}, viewport stable={}",
                round, rows_after.len(), off, visible_after == last_visible
            );
            last_visible = visible_after;
        } else {
            eprintln!("SIM-4 round {}: anchor lost (text removed by reflow)", round);
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
    let visible_final: Vec<String> = rows_final
        [off..(off + visible_height).min(rows_final.len())]
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
        ("prose only",
         "Para A.\n\nPara B.\n\nPara C.\n\ntail"),
        ("heading + fence + list",
         "# Heading\n\nProse.\n\n```rust\nfn x() {}\n```\n\n- a\n- b\n\ntail"),
        ("nested list",
         "- top1\n  - sub1\n  - sub2\n- top2\n\ntail"),
        ("table",
         "| col1 | col2 |\n|---|---|\n| a | b |\n\ntail"),
    ];

    let dump_rows = |rows: &[crate::components::react_trace::VirtualRow]| -> Vec<String> {
        rows.iter().map(|r| match r {
            crate::components::react_trace::VirtualRow::Text(line) =>
                line.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            _ => "<non-text>".into(),
        }).collect()
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
        eprintln!("SIM-5 [{}]: pre={} rows, post={} rows, delta={}",
                  name, pre.len(), post.len(), delta);
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
    assert_eq!(rows_1.len(), rows_2.len(),
        "SIM-6: post-flush renders must be deterministic; got {} vs {}",
        rows_1.len(), rows_2.len());

    // Pick a viewport near the end (same as SIM-2).
    let visible_height = 8;
    let scroll_offset = rows_1.len().saturating_sub(visible_height);
    let dump = |rows: &[crate::components::react_trace::VirtualRow], from: usize, to: usize|
        -> Vec<String> {
        rows[from..to.min(rows.len())].iter().map(|r| match r {
            crate::components::react_trace::VirtualRow::Text(line) =>
                line.spans.iter().map(|s| s.content.as_ref()).collect(),
            _ => "<non-text>".into(),
        }).collect()
    };

    let v1 = dump(&rows_1, scroll_offset, scroll_offset + visible_height);
    let v2 = dump(&rows_2, scroll_offset, scroll_offset + visible_height);
    eprintln!("SIM-6 viewport stable across two post-flush renders:\n  v1={:?}\n  v2={:?}", v1, v2);
    assert_eq!(v1, v2, "SIM-6: F1 prototype must keep viewport stable across renders");
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
        "claude", "10:00:00".into());
    let _ = trace.drain_fence_dispatches(&StateLookup::empty());

    let (rows_initial, _, _) = trace.build_virtual_rows_for_tests(
        0, 80, &std::collections::HashMap::new(), None);
    let visible_height = 5;
    // Anchor near the TOP — this is the user reading earlier content
    // while new content streams in below them. The classic ghost-text scenario.
    let initial_offset = 1; // first content row after the header
    let anchor = row_to_anchor(&rows_initial, initial_offset);
    let visible_initial: Vec<String> = rows_initial
        [initial_offset..(initial_offset + visible_height).min(rows_initial.len())]
        .iter().map(row_text).collect();
    eprintln!("SIM-7 initial viewport at anchor={:?}:\n  {}",
        anchor.text, visible_initial.join("\n  "));

    // Stream more chunks, each followed by drain (debounce flush).
    let later_chunks = [
        " More text continuing the third paragraph.\n\nNew paragraph four.\n\ny",
        " Even more.\n\n- bullet alpha\n- bullet beta\n\nz",
        " Final batch.\n\n```rust\nfn end() {}\n```\n\nw",
    ];
    for (i, chunk) in later_chunks.iter().enumerate() {
        trace.append_message(chunk, "claude", "10:00:00".into());
        let _ = trace.drain_fence_dispatches(&StateLookup::empty());

        let (rows_now, _, _) = trace.build_virtual_rows_for_tests(
            0, 80, &std::collections::HashMap::new(), None);
        let new_offset = anchor_to_row(&rows_now, &anchor)
            .expect("anchor must remain resolvable");
        let visible_now: Vec<String> = rows_now
            [new_offset..(new_offset + visible_height).min(rows_now.len())]
            .iter().map(row_text).collect();
        eprintln!("SIM-7 round {}: rows={}, anchor at offset={}, viewport={:?}",
            i, rows_now.len(), new_offset, visible_now);
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
    let (rows_w80, _, _) = trace.build_virtual_rows_for_tests(
        0, 80, &std::collections::HashMap::new(), None);
    let marker_row_w80 = rows_w80.iter().position(|r| row_text(r).contains("MARKER_ALPHA"))
        .expect("marker must be present at width 80");
    let anchor = row_to_anchor(&rows_w80, marker_row_w80);

    // Resize to width 60 — wrapping changes substantially.
    let (rows_w60, _, _) = trace.build_virtual_rows_for_tests(
        0, 60, &std::collections::HashMap::new(), None);
    let resolved_w60 = anchor_to_row(&rows_w60, &anchor);
    eprintln!("SIM-8 width 80→60: marker at row {} (w80) resolves to {:?} (w60)",
        marker_row_w80, resolved_w60);
    eprintln!("SIM-8 row count w80={} w60={}", rows_w80.len(), rows_w60.len());

    // Anchor's text-match resolver may fail at narrower width because
    // the marker line might wrap into multiple rows where the marker
    // text is no longer a stand-alone line.
    if let Some(off) = resolved_w60 {
        let visible = rows_w60[off..(off + 1).min(rows_w60.len())]
            .iter().map(row_text).collect::<Vec<_>>();
        eprintln!("SIM-8 resolved row at w60: {:?}", visible);
        assert!(visible.iter().any(|l| l.contains("MARKER_ALPHA")),
            "SIM-8: text-match anchor failed to find MARKER_ALPHA at w60 — \
             text-match is too brittle for resize. Real F3 needs (entry_idx, byte_offset).");
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
    trace.append_message("para one\n\npara two\n\npara three", "claude", "10:00".into());
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(1);

    // Append more content WITHOUT rendering — this exercises the "stale metrics" path.
    trace.append_message(
        "\n\npara four\n\npara five\n\npara six\n\npara seven", "claude", "10:00".into());

    trace.scroll_to_bottom();
    assert!(trace.is_following(), "after scroll_to_bottom, anchor must be Following");

    trace.scroll_up();
    // With fresh row metrics, scroll_up from Following must transition to a Byte anchor
    // pointing one row above the bottom. With stale metrics, it would no-op (max_offset == 0).
    assert!(!trace.is_following(),
        "scroll_up must use fresh row count to transition out of Following; \
         got is_following={}, anchor={:?}",
        trace.is_following(), trace.anchor_for_tests());
}
