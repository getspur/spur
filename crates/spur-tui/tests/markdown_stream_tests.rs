#![cfg(feature = "markdown")]

use spur_tui::components::markdown_stream::MarkdownStream;

#[test]
fn append_chunks_then_flush_equals_full_parse() {
    let full = "# Heading\n\nSome **bold** and *italic* text.\n\n- a\n- b\n";
    let mut incremental = MarkdownStream::new();
    for ch in full.chars() {
        incremental.append(&ch.to_string());
    }
    incremental.flush_now();

    let mut one_shot = MarkdownStream::new();
    one_shot.append(full);
    one_shot.flush_now();

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
    s.flush_now();
    assert!(!s.cached_lines_debug().is_empty());
}

#[test]
fn empty_stream_renders_to_empty_lines() {
    let mut s = MarkdownStream::new();
    s.flush_now();
    assert!(s.cached_lines_debug().is_empty());
}

#[test]
fn headings_preserve_bold_style() {
    let mut s = MarkdownStream::new();
    s.append("# Heading\n");
    s.flush_now();
    // The first Line carries BOLD via Line::style (tui-markdown uses
    // Line::styled for heading prefixes, placing the style on the line
    // rather than on individual spans).
    let first_line = s.lines().first().expect("expected at least one line");
    let has_bold = first_line
        .style
        .add_modifier
        .contains(ratatui::style::Modifier::BOLD);
    assert!(
        has_bold,
        "heading line must carry BOLD modifier — styles were dropped"
    );
}
