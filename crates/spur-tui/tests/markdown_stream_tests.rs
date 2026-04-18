#![cfg(feature = "markdown")]

use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};

#[test]
fn append_chunks_then_flush_equals_full_parse() {
    let full = "# Heading\n\nSome **bold** and *italic* text.\n\n- a\n- b\n";
    let mut incremental = MarkdownStream::new();
    for ch in full.chars() {
        incremental.append(&ch.to_string());
    }
    incremental.flush_now(&StateLookup::empty());

    let mut one_shot = MarkdownStream::new();
    one_shot.append(full);
    one_shot.flush_now(&StateLookup::empty());

    assert_eq!(
        incremental.cached_lines_debug(),
        one_shot.cached_lines_debug(),
        "incremental parse must equal full parse after flush"
    );
}

#[test]
fn debounce_does_not_rebuild_until_flush_or_timeout() {
    let mut s = MarkdownStream::new();
    // Trailing paragraph ensures cursor advances past the heading block.
    s.append("# A\n\nbody\n");
    s.flush_now(&StateLookup::empty());
    assert!(!s.cached_lines_debug().is_empty());
}

#[test]
fn empty_stream_renders_to_empty_lines() {
    let mut s = MarkdownStream::new();
    s.flush_now(&StateLookup::empty());
    assert!(s.cached_lines_debug().is_empty());
}

#[test]
fn headings_preserve_bold_style() {
    let mut s = MarkdownStream::new();
    // Trailing paragraph ensures cursor advances past the heading block.
    s.append("# Heading\n\nbody\n");
    s.flush_now(&StateLookup::empty());
    // The first Line carries BOLD via Line::style (tui-markdown uses
    // Line::styled for heading prefixes, placing the style on the line
    // rather than on individual spans).
    let lines = s.lines();
    let first_line = lines.first().expect("expected at least one line");
    let has_bold = first_line
        .style
        .add_modifier
        .contains(ratatui::style::Modifier::BOLD);
    assert!(
        has_bold,
        "heading line must carry BOLD modifier — styles were dropped"
    );
}

#[test]
fn closed_mermaid_fence_emits_new_fence_ref() {
    let mut s = MarkdownStream::new();
    s.append("# Plan\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nMore text\n");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 1, "expected exactly one fence");
    let f = &fences[0];
    assert!(f.code.contains("flowchart LR"));
    assert!(f.code.contains("A-->B"));
    let rendered_text: String = s.cached_lines_debug().join("\n");
    assert!(
        rendered_text.contains("mermaid"),
        "expected placeholder mention of mermaid, got: {rendered_text}"
    );
    assert!(
        !rendered_text.contains("A-->B"),
        "mermaid source must not appear in rendered trace"
    );
}

#[test]
fn fence_emission_is_idempotent_across_flushes() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```\n");
    let first = s.flush_now(&StateLookup::empty());
    assert_eq!(first.len(), 1);
    let second = s.flush_now(&StateLookup::empty());
    assert_eq!(second.len(), 0, "re-flush must not re-emit existing fences");
}

#[test]
fn open_fence_does_not_emit() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 0, "open fence must not yield a fence ref");
}

#[test]
fn non_mermaid_fences_are_ignored() {
    let mut s = MarkdownStream::new();
    s.append("```rust\nfn main() {}\n```\n");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 0);
}

#[test]
fn error_state_emits_warning_placeholder() {
    use spur_tui::components::mermaid::MermaidId;
    use std::collections::HashSet;

    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```\n");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 1);
    let id = fences[0].id;

    // Now simulate that this fence errored
    let errors: HashSet<MermaidId> = std::iter::once(id).collect();
    let pending: HashSet<MermaidId> = HashSet::new();
    let states = StateLookup {
        errors: &errors,
        pending: &pending,
    };
    s.mark_dirty_now();
    s.flush_now(&states);

    let rendered: String = s.cached_lines_debug().join("\n");
    assert!(
        rendered.contains('⚠'),
        "expected warning glyph, got: {rendered}"
    );
    assert!(
        rendered.contains("error"),
        "expected 'error' in placeholder"
    );
}

#[test]
fn pending_state_emits_rendering_placeholder() {
    use spur_tui::components::mermaid::MermaidId;
    use std::collections::HashSet;

    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```\n");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 1);
    let id = fences[0].id;

    // Now mark this fence as pending and rebuild.
    let errors = HashSet::new();
    let pending: HashSet<MermaidId> = std::iter::once(id).collect();
    let states = StateLookup {
        errors: &errors,
        pending: &pending,
    };
    s.mark_dirty_now();
    s.flush_now(&states);

    let rendered: String = s.cached_lines_debug().join("\n");
    assert!(
        rendered.contains('⏳'),
        "expected hourglass glyph, got: {rendered}"
    );
    assert!(rendered.contains("rendering"), "expected 'rendering' text");
}

#[test]
fn pending_to_ready_transition_flips_placeholder() {
    use spur_tui::components::mermaid::MermaidId;
    use std::collections::HashSet;

    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```\n");
    let fences = s.flush_now(&StateLookup::empty());
    let id = fences[0].id;

    // 1st pass: pending.
    let errors = HashSet::new();
    let pending: HashSet<MermaidId> = std::iter::once(id).collect();
    s.mark_dirty_now();
    s.flush_now(&StateLookup {
        errors: &errors,
        pending: &pending,
    });
    assert!(s.cached_lines_debug().join("\n").contains('⏳'));

    // 2nd pass: ready (empty pending set simulates completion).
    let pending_empty = HashSet::new();
    s.mark_dirty_now();
    s.flush_now(&StateLookup {
        errors: &errors,
        pending: &pending_empty,
    });
    let rendered = s.cached_lines_debug().join("\n");
    assert!(
        !rendered.contains('⏳'),
        "should no longer say rendering: {rendered}"
    );
    assert!(
        rendered.contains('📊'),
        "should show ready placeholder: {rendered}"
    );
}

#[test]
fn flushed_byte_len_starts_at_zero() {
    let s = MarkdownStream::new();
    assert_eq!(s.flushed_byte_len_for_tests(), 0);
}

#[test]
fn finalized_starts_false() {
    let s = MarkdownStream::new();
    assert!(!s.is_finalized());
}

#[test]
fn items_and_tail_empty_stream() {
    let s = MarkdownStream::new();
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0);
    assert_eq!(tail, "");
}

#[test]
fn items_and_tail_before_flush_shows_entire_raw_text_as_tail() {
    let mut s = MarkdownStream::new();
    s.append("Hello world");
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0, "no flush yet, no committed items");
    assert_eq!(tail, "Hello world", "all raw_text should be in the tail");
}

#[test]
fn fence_placeholder_for_unknown_id_returns_none() {
    use spur_tui::components::mermaid::MermaidId;
    let s = MarkdownStream::new();
    assert!(s.fence_placeholder_for(MermaidId(999)).is_none());
}

#[test]
fn scan_authoritative_empty_input() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let (end, fences) = scan_authoritative_for_tests("", /*mermaid*/ true, /*permit_eof*/ false);
    assert_eq!(end, 0);
    assert!(fences.is_empty());
}

#[test]
fn scan_authoritative_paragraph_at_eof_not_authoritative() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let (end, _) = scan_authoritative_for_tests("Hello", true, false);
    assert_eq!(end, 0, "paragraph at EOF must not advance");
}

#[test]
fn scan_authoritative_paragraph_with_content_after_advances() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "Hello\n\nworld";
    let (end, _) = scan_authoritative_for_tests(input, true, false);
    assert!(end > 0 && end < input.len(),
        "end={} len={}", end, input.len());
}

#[test]
fn scan_authoritative_open_list_not_authoritative() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    // Depth-gate test: open list at EOF must not leak End(Item) authority.
    let (end, _) = scan_authoritative_for_tests("- item1\n- item2\n", true, false);
    assert_eq!(end, 0);
}

#[test]
fn scan_authoritative_eof_permissive_commits_final_paragraph() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "Hello";
    let (end, _) = scan_authoritative_for_tests(input, true, /*permit_eof*/ true);
    assert_eq!(end, input.len());
}

#[test]
fn scan_authoritative_registers_closed_mermaid_fence() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    let input = "```mermaid\nflowchart LR\nA-->B\n```\nmore\n";
    let (_, fences) = scan_authoritative_for_tests(input, true, false);
    assert_eq!(fences.len(), 1);
    assert!(fences[0].1.contains("flowchart LR"));
}

#[test]
fn scan_authoritative_does_not_register_fence_at_eof() {
    use spur_tui::components::markdown_stream::scan_authoritative_for_tests;
    // Fence close is the last byte; coherence fix must not register it.
    let input = "```mermaid\nflowchart LR\nA-->B\n```";
    let (_, fences) = scan_authoritative_for_tests(input, true, false);
    assert_eq!(fences.len(), 0, "fence ending at EOF must not register");
}

#[test]
fn rebuild_advances_cursor_past_authoritative_events() {
    let mut s = MarkdownStream::new();
    s.append("# Title\n\nBody paragraph\n\nMore");
    s.flush_now(&StateLookup::empty());
    let flushed = s.flushed_byte_len_for_tests();
    assert!(flushed > 0 && flushed < s.raw_text().len(),
        "flushed={} raw_len={}", flushed, s.raw_text().len());
}

#[test]
fn rebuild_does_not_advance_past_open_list() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n");
    s.flush_now(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0,
        "open list at EOF must not advance cursor");
}

#[test]
fn flushed_byte_len_is_monotonic() {
    let mut s = MarkdownStream::new();
    s.append("# A\n\n");
    s.flush_now(&StateLookup::empty());
    let a = s.flushed_byte_len_for_tests();
    s.append("# B\n\n");
    s.flush_now(&StateLookup::empty());
    let b = s.flushed_byte_len_for_tests();
    assert!(b >= a, "monotonic: {} -> {}", a, b);
}

#[test]
fn flush_final_commits_trailing_paragraph() {
    let mut s = MarkdownStream::new();
    s.append("# Title\n\nFinal paragraph");
    s.flush_final(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), s.raw_text().len(),
        "flush_final must commit all bytes including EOF");
    assert!(s.is_finalized());
}

#[test]
fn flush_final_commits_trailing_fence() {
    let mut s = MarkdownStream::new();
    s.append("Intro\n\n```rust\nfn x() {}\n```");
    s.flush_final(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), s.raw_text().len());
}

#[test]
fn heuristic_fires_on_double_newline_with_content() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(has_authoritative_closure_pattern_for_tests("para\n\nmore"));
}

#[test]
fn heuristic_declines_double_newline_at_eof() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(!has_authoritative_closure_pattern_for_tests("para\n\n"));
}

#[test]
fn heuristic_fires_on_fence_close_with_content() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(has_authoritative_closure_pattern_for_tests("```\ncode\n```\nmore"));
}

#[test]
fn heuristic_declines_fence_close_at_eof() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(!has_authoritative_closure_pattern_for_tests("```\ncode\n```\n"));
}

#[test]
fn safety_cap_is_64kib() {
    use spur_tui::components::markdown_stream::SAFETY_CAP_BYTES;
    assert_eq!(SAFETY_CAP_BYTES, 64 * 1024);
}

#[test]
fn maybe_flush_short_circuits_when_not_dirty() {
    let mut s = MarkdownStream::new();
    s.append("# A\n\nbody");
    s.flush_now(&StateLookup::empty());
    assert!(!s.is_dirty(), "flush_now clears dirty_since");
    // maybe_flush on a clean stream must return empty without work.
    let out = s.maybe_flush(&StateLookup::empty());
    assert!(out.is_empty());
}

#[test]
fn maybe_flush_fast_path_fires_on_boundary_pattern() {
    let mut s = MarkdownStream::new();
    // Append content that contains \n\n with trailing content — heuristic
    // fires before DEBOUNCE elapses.
    s.append("paragraph\n\nmore content");
    let before = s.flushed_byte_len_for_tests();
    s.maybe_flush(&StateLookup::empty());
    let after = s.flushed_byte_len_for_tests();
    assert!(after > before,
        "fast path should have flushed immediately; before={} after={}",
        before, after);
}

#[test]
fn maybe_flush_declines_when_no_boundary_before_debounce() {
    let mut s = MarkdownStream::new();
    s.append("streaming without boundaries");
    // Immediately after append, dirty but no boundary, debounce not
    // elapsed → no flush.
    s.maybe_flush(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0);
    // Stream still dirty.
    assert!(s.is_dirty());
}

#[test]
fn maybe_flush_safety_cap_suppresses_rebuild() {
    use spur_tui::components::markdown_stream::SAFETY_CAP_BYTES;
    let mut s = MarkdownStream::new();
    // A long boundary-free tail.
    let huge = "x".repeat(SAFETY_CAP_BYTES + 100);
    s.append(&huge);
    let out = s.maybe_flush(&StateLookup::empty());
    assert!(out.is_empty());
    // Safety valve clears dirty_since so we don't re-enter on next tick.
    assert!(!s.is_dirty(),
        "safety valve must clear dirty_since to prevent tight looping");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "append after flush_final")]
fn append_after_flush_final_debug_asserts() {
    let mut s = MarkdownStream::new();
    s.append("hello");
    s.flush_final(&StateLookup::empty());
    // Contract violation:
    s.append("more");
}

#[test]
fn append_before_flush_final_never_panics() {
    let mut s = MarkdownStream::new();
    s.append("hello");
    s.append(" world");
    assert_eq!(s.raw_text(), "hello world");
}
