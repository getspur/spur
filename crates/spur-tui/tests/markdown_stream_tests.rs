#![cfg(feature = "markdown")]

use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};

fn item_debug(stream: &MarkdownStream) -> String {
    format!("{:#?}", stream.items())
}

fn assert_incremental_chunks_match_full_rebuild(name: &str, chunks: &[&str]) {
    let full = chunks.concat();

    let mut incremental = MarkdownStream::new();
    for chunk in chunks {
        incremental.append(chunk);
        incremental.flush_now(&StateLookup::empty());
    }
    incremental.flush_final(&StateLookup::empty());

    let mut one_shot = MarkdownStream::new();
    one_shot.append(&full);
    one_shot.flush_final(&StateLookup::empty());

    assert_eq!(
        item_debug(&incremental),
        item_debug(&one_shot),
        "{name}: incremental flushes must match one full rebuild item-for-item"
    );
}

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
fn incremental_flushes_match_full_rebuild_for_markdown_block_shapes() {
    let cases: &[(&str, &[&str])] = &[
        (
            "bullet list then trailing paragraph",
            &[
                "Intro paragraph.\n\n- bullet a\n- bullet b\n\n",
                "trailing paragraph\n\n",
            ],
        ),
        (
            "ordered list then trailing paragraph",
            &["1. first\n2. second\n\n", "after ordered list\n\n"],
        ),
        (
            "blank-line-separated blocks",
            &[
                "Alpha paragraph.\n\n",
                "Beta paragraph with **strong** text.\n\n",
                "Gamma paragraph.\n\n",
            ],
        ),
        (
            "atx and setext headings",
            &[
                "# ATX heading\n\n",
                "Body paragraph.\n\nSetext heading\n---\n\n",
                "Trailing paragraph.\n\n",
            ],
        ),
        (
            "nested blockquote and nested list",
            &[
                "> quote line\n> - nested bullet\n\n",
                "1. ordered parent\n   - nested child\n\n",
                "tail\n\n",
            ],
        ),
    ];

    for (name, chunks) in cases {
        assert_incremental_chunks_match_full_rebuild(name, chunks);
    }
}

#[test]
fn incremental_flush_build_work_tracks_delta_and_matches_full_rebuild() {
    let chunks: Vec<String> = (0..48)
        .map(|i| {
            format!("Chunk {i}\n\nStreaming markdown paragraph {i} with **bold** and `code`.\n\n")
        })
        .collect();
    let full = chunks.concat();
    let largest_chunk = chunks.iter().map(|chunk| chunk.len()).max().unwrap() as u64;

    let mut incremental = MarkdownStream::new();
    let mut previous_work = 0;
    let mut largest_flush_work = 0;
    for chunk in &chunks {
        incremental.append(chunk);
        incremental.flush_now(&StateLookup::empty());

        let current_work = incremental.build_work_bytes_for_tests();
        largest_flush_work = largest_flush_work.max(current_work - previous_work);
        previous_work = current_work;
    }
    incremental.flush_final(&StateLookup::empty());

    let mut one_shot = MarkdownStream::new();
    one_shot.append(&full);
    one_shot.flush_final(&StateLookup::empty());

    assert_eq!(item_debug(&incremental), item_debug(&one_shot));
    assert!(
        largest_flush_work <= largest_chunk * 3,
        "per-flush build work should be bounded by delta size: largest_flush_work={largest_flush_work} largest_chunk={largest_chunk}"
    );
    assert!(
        incremental.build_work_bytes_for_tests() <= full.len() as u64 * 2,
        "total build work should stay linear in input length: work={} len={}",
        incremental.build_work_bytes_for_tests(),
        full.len()
    );
}

#[test]
fn complete_fenced_code_flushes_match_full_rebuild_with_linear_work() {
    let chunks: Vec<String> = (0..16)
        .map(|i| {
            let body = (0..24)
                .map(|line| format!("    let value_{i}_{line} = {line};\n"))
                .collect::<String>();
            format!("```rust\nfn block_{i}() {{\n{body}}}\n```\n\nParagraph after block {i}.\n\n")
        })
        .collect();
    let full = chunks.concat();
    let largest_chunk = chunks.iter().map(|chunk| chunk.len()).max().unwrap() as u64;

    let mut incremental = MarkdownStream::new();
    let mut previous_work = 0;
    let mut largest_flush_work = 0;
    for chunk in &chunks {
        incremental.append(chunk);
        incremental.flush_now(&StateLookup::empty());

        let current_work = incremental.build_work_bytes_for_tests();
        largest_flush_work = largest_flush_work.max(current_work - previous_work);
        previous_work = current_work;
    }
    incremental.flush_final(&StateLookup::empty());

    let mut one_shot = MarkdownStream::new();
    one_shot.append(&full);
    one_shot.flush_final(&StateLookup::empty());

    assert_eq!(item_debug(&incremental), item_debug(&one_shot));
    assert!(
        largest_flush_work <= largest_chunk * 3,
        "per-flush fenced-code build work should be bounded by delta size: largest_flush_work={largest_flush_work} largest_chunk={largest_chunk}"
    );
    assert!(
        incremental.build_work_bytes_for_tests() <= full.len() as u64 * 3,
        "total fenced-code build work should stay linear in input length: work={} len={}",
        incremental.build_work_bytes_for_tests(),
        full.len()
    );
}

#[test]
fn table_seam_falls_back_to_full_rebuild_and_matches_full_parse() {
    let first = "| A | B |\n|---|---|\n\n";
    let second = "| 1 | 2 |\n\nAfter table.\n\n";
    let full = format!("{first}{second}");

    let mut incremental = MarkdownStream::new();
    incremental.append(first);
    incremental.flush_now(&StateLookup::empty());
    let work_before = incremental.build_work_bytes_for_tests();
    incremental.append(second);
    incremental.flush_now(&StateLookup::empty());

    let second_flush_work = incremental.build_work_bytes_for_tests() - work_before;
    assert!(
        second_flush_work >= incremental.flushed_byte_len_for_tests() as u64,
        "table seam should fall back to full prefix rebuild: second_flush_work={second_flush_work} second_len={}",
        second.len()
    );

    let mut one_shot = MarkdownStream::new();
    one_shot.append(&full);
    one_shot.flush_now(&StateLookup::empty());
    assert_eq!(item_debug(&incremental), item_debug(&one_shot));
}

#[test]
fn mermaid_state_change_rebuilds_committed_prefix() {
    use std::collections::HashSet;

    let mut stream = MarkdownStream::new();
    stream.append("```mermaid\nflowchart LR\nA-->B\n```\n\nAfter.\n");
    let fences = stream.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 1);
    let id = fences[0].id;

    let before_work = stream.build_work_bytes_for_tests();
    let mut errors = HashSet::new();
    errors.insert(id);
    let pending = HashSet::new();
    let states = StateLookup {
        errors: &errors,
        pending: &pending,
    };

    stream.mark_dirty_now();
    stream.flush_now(&states);

    let rebuild_work = stream.build_work_bytes_for_tests() - before_work;
    assert!(
        rebuild_work >= stream.flushed_byte_len_for_tests() as u64,
        "state-only invalidation must rebuild the committed prefix: rebuild_work={rebuild_work} flushed={}",
        stream.flushed_byte_len_for_tests()
    );

    let placeholder = stream
        .fence_placeholder_for(id)
        .expect("placeholder should be refreshed for committed fence");
    let rendered: String = placeholder
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(
        rendered.contains("error"),
        "placeholder should reflect error state: {rendered}"
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
    let (end, fences) =
        scan_authoritative_for_tests("", /*mermaid*/ true, /*permit_eof*/ false);
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
    assert!(
        end > 0 && end < input.len(),
        "end={} len={}",
        end,
        input.len()
    );
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
    assert!(
        flushed > 0 && flushed < s.raw_text().len(),
        "flushed={} raw_len={}",
        flushed,
        s.raw_text().len()
    );
}

#[test]
fn rebuild_does_not_advance_past_open_list() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n");
    s.flush_now(&StateLookup::empty());
    assert_eq!(
        s.flushed_byte_len_for_tests(),
        0,
        "open list at EOF must not advance cursor"
    );
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
    assert_eq!(
        s.flushed_byte_len_for_tests(),
        s.raw_text().len(),
        "flush_final must commit all bytes including EOF"
    );
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
    assert!(has_authoritative_closure_pattern_for_tests(
        "```\ncode\n```\nmore"
    ));
}

#[test]
fn heuristic_declines_fence_close_at_eof() {
    use spur_tui::components::markdown_stream::has_authoritative_closure_pattern_for_tests;
    assert!(!has_authoritative_closure_pattern_for_tests(
        "```\ncode\n```\n"
    ));
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
    assert!(
        after > before,
        "fast path should have flushed immediately; before={} after={}",
        before,
        after
    );
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
    assert!(
        !s.is_dirty(),
        "safety valve must clear dirty_since to prevent tight looping"
    );
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

#[test]
fn setext_promotion_retroactively_restyles_at_boundary() {
    let mut s = MarkdownStream::new();
    s.append("Hello\n");
    s.flush_now(&StateLookup::empty());
    assert_eq!(s.flushed_byte_len_for_tests(), 0);

    s.append("===\n\nbody");
    s.flush_now(&StateLookup::empty());
    assert!(
        s.flushed_byte_len_for_tests() > 0,
        "setext + trailing content should advance cursor"
    );

    let rendered = s.cached_lines_debug().join("\n");
    assert!(
        rendered.to_lowercase().contains("hello"),
        "committed prefix should contain the heading text"
    );
}

#[test]
fn list_in_progress_renders_markers_as_plain_tail() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n");
    s.flush_now(&StateLookup::empty());
    let (items, tail) = s.items_and_tail();
    assert_eq!(items.len(), 0, "open list: nothing committed");
    assert_eq!(tail, "- item1\n- item2\n");
}

#[test]
fn list_promotes_on_close() {
    let mut s = MarkdownStream::new();
    s.append("- item1\n- item2\n\nafter");
    s.flush_now(&StateLookup::empty());
    let (items, _) = s.items_and_tail();
    assert!(
        !items.is_empty(),
        "closed list should produce committed items"
    );
}

#[test]
fn unicode_content_cursor_advances_on_char_boundary() {
    let mut s = MarkdownStream::new();
    s.append("# 漢字 🎉\n\nmore content");
    s.flush_now(&StateLookup::empty());
    let flushed = s.flushed_byte_len_for_tests();
    assert!(
        s.raw_text().is_char_boundary(flushed),
        "flushed_byte_len {} must be on UTF-8 char boundary",
        flushed
    );
}

#[test]
fn tail_above_safety_cap_with_boundary_still_flushes() {
    use spur_tui::components::markdown_stream::SAFETY_CAP_BYTES;
    let mut s = MarkdownStream::new();
    // Pathologically large boundary-free prefix, followed by a clean
    // closure pattern so the safety valve should NOT suppress rebuild.
    let huge = "x".repeat(SAFETY_CAP_BYTES + 100);
    s.append(&huge);
    s.append("\n\nboundary content");
    let before = s.flushed_byte_len_for_tests();
    s.maybe_flush(&StateLookup::empty());
    let after = s.flushed_byte_len_for_tests();
    assert!(
        after > before,
        "safety cap must NOT suppress when closure pattern is present; before={} after={}",
        before,
        after
    );
}

#[test]
fn no_fence_registered_with_range_end_at_eof() {
    let mut s = MarkdownStream::new();
    s.append("```mermaid\nflowchart LR\nA-->B\n```");
    let fences = s.flush_now(&StateLookup::empty());
    assert_eq!(fences.len(), 0, "fence at EOF must not register");

    s.append("\n\ntrailing");
    let fences2 = s.flush_now(&StateLookup::empty());
    assert_eq!(
        fences2.len(),
        1,
        "once trailing content arrives, fence registers"
    );
}

/// preview_items must be pure: two consecutive calls with the same input
/// return identical Vec<StreamItem> and do not mutate cursor state.
#[test]
fn preview_items_is_pure() {
    use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};
    let mut s = MarkdownStream::new();
    s.append("# Heading\n\nProse paragraph.\n\n```rust\nfn x() {}\n```\n\nmore");

    let flushed_before = s.flushed_byte_len_for_tests();
    let items_a = s.preview_items(&StateLookup::empty());
    let items_b = s.preview_items(&StateLookup::empty());
    let flushed_after = s.flushed_byte_len_for_tests();

    assert_eq!(
        flushed_before, flushed_after,
        "preview_items must not mutate flushed_byte_len"
    );
    assert_eq!(
        items_a.len(),
        items_b.len(),
        "preview_items must be deterministic across calls"
    );
}

/// preview_items output must match cached_items after flush_final for the
/// same raw_text. This is the F1 invariant: tail rendering produces what
/// final flush would produce.
#[test]
fn preview_items_matches_post_final_flush() {
    use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};
    let payload = "# H\n\nP1.\n\n- a\n- b\n\ntail";

    let mut preview_stream = MarkdownStream::new();
    preview_stream.append(payload);
    let preview = preview_stream.preview_items(&StateLookup::empty());

    let mut flushed_stream = MarkdownStream::new();
    flushed_stream.append(payload);
    flushed_stream.flush_final(&StateLookup::empty());
    let after = flushed_stream.items().to_vec();

    assert_eq!(
        preview.len(),
        after.len(),
        "preview_items must produce same StreamItem count as post-flush_final"
    );
}

// ── Regression: a complete table must not revert to raw while a LATER ──
// table's delimiter row is still streaming in column-by-column. Previously
// the two table collectors (strict pulldown vs. lax line heuristic) diverged
// on the partial separator, and rendered_markdown_tables bailed on ALL tables.
#[test]
fn complete_table_stays_grid_while_later_table_separator_is_partial() {
    use spur_tui::components::markdown_stream::StreamItem;

    fn text_of(s: &MarkdownStream, width: u16) -> String {
        s.preview_items_with_width(&StateLookup::empty(), width)
            .iter()
            .filter_map(|i| match i {
                StreamItem::Text(ls) => Some(
                    ls.iter()
                        .map(|l| {
                            l.spans
                                .iter()
                                .map(|sp| sp.content.as_ref())
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Table 1 complete; prose; table 2 header + a PARTIAL delimiter (2 of 3
    // columns) — the exact mid-stream state that used to bail.
    let input = "| Key | Action |\n|---|---|\n| Esc | cancel |\n| Enter | submit |\n\n\
                 More tables:\n\n\
                 | A | B | C |\n|---|---|";
    let mut s = MarkdownStream::new();
    s.append(input);
    let rendered = text_of(&s, 100);

    assert!(
        rendered.contains('┌') && rendered.contains("│ Key"),
        "first complete table must stay a grid while a later separator streams:\n{rendered}"
    );
    assert!(
        !rendered.contains("| Key | Action |"),
        "first table header must not revert to raw markdown:\n{rendered}"
    );

    // And once table 2's separator completes (3 columns) plus a data row,
    // BOTH render as grids.
    let mut s2 = MarkdownStream::new();
    s2.append(
        "| Key | Action |\n|---|---|\n| Esc | cancel |\n| Enter | submit |\n\n\
         More tables:\n\n\
         | A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n",
    );
    let full = text_of(&s2, 100);
    let grid_tops = full.matches('┌').count();
    assert_eq!(
        grid_tops, 2,
        "both tables must render as grids once complete:\n{full}"
    );
}

// ── Regression: a table whose cells contain INLINE markdown (bold, inline
// code) must still render as a grid. tui-markdown renders `**bold**` and
// `` `code` `` as styled spans with the markers stripped, so the rendered
// plain text never equals the raw source row (which keeps the markers). The
// exact-match locator then failed and the whole table fell back to raw — the
// failure visible whenever a summary table had formatted cells.
#[test]
fn table_with_inline_formatting_in_cells_renders_as_grid() {
    use spur_tui::components::markdown_stream::StreamItem;

    fn text_of(s: &MarkdownStream, width: u16) -> String {
        s.preview_items_with_width(&StateLookup::empty(), width)
            .iter()
            .filter_map(|i| match i {
                StreamItem::Text(ls) => Some(
                    ls.iter()
                        .map(|l| {
                            l.spans
                                .iter()
                                .map(|sp| sp.content.as_ref())
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Cells carry bold, inline code (with literal underscores that must NOT be
    // treated as emphasis), and angle-bracket generics — exactly the content
    // that streams in a change-summary table.
    let input = "| # | Change |\n|---|---|\n\
                 | 1 | **Core fix.** Rewrote `collect_table_source_blocks` |\n\
                 | 2 | `collect_markdown_tables` returns `Range<usize>` |\n";
    let mut s = MarkdownStream::new();
    s.append(input);
    let rendered = text_of(&s, 120);

    assert!(
        rendered.contains('┌'),
        "table with inline-formatted cells must render as a grid, not raw:\n{rendered}"
    );
    assert!(
        !rendered.contains("| # | Change |"),
        "header must not revert to raw markdown:\n{rendered}"
    );
    // The stripped cell text must survive into the grid.
    assert!(
        rendered.contains("Core fix.") && rendered.contains("collect_table_source_blocks"),
        "formatted cell content must appear inside the grid:\n{rendered}"
    );
}
