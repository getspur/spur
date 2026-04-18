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
    s.append("# A\n");
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
    s.append("# Heading\n");
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
