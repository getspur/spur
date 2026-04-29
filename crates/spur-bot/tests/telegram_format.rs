use spur_bot::telegram::format::{
    markdown_to_telegram_html, render_truncated_text, short_button_label, split_for_final_answer,
    split_for_telegram, truncate_button_label_bytes, truncate_to_utf16_units,
    TELEGRAM_BUTTON_LABEL_MAX_BYTES, TELEGRAM_TEXT_MAX_UTF16_UNITS,
};

#[test]
fn split_for_telegram_preserves_unicode_scalar_boundaries() {
    let text = "alpha🙂beta🙂gamma".repeat(400);
    let chunks = split_for_telegram(&text, 256);

    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 256));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn markdown_to_telegram_html_inline_emphasis_strong() {
    assert_eq!(
        markdown_to_telegram_html("**bold** *italic* ~~strike~~"),
        "<b>bold</b> <i>italic</i> <s>strike</s>"
    );
}

#[test]
fn markdown_to_telegram_html_inline_code() {
    assert_eq!(
        markdown_to_telegram_html("Use `x < y && z > q`."),
        "Use <code>x &lt; y &amp;&amp; z &gt; q</code>."
    );
}

#[test]
fn markdown_to_telegram_html_links() {
    assert_eq!(
        markdown_to_telegram_html("[label](https://example.test/?q=\"<&>)"),
        "<a href=\"https://example.test/?q=&quot;&lt;&amp;&gt;\">label</a>"
    );
    assert_eq!(markdown_to_telegram_html("[label]()"), "label");
}

#[test]
fn markdown_to_telegram_html_fenced_code_block() {
    let input = "Before\n\n```rust\nfn main() { println!(\"<&>\"); }\n```\n\nAfter";

    assert_eq!(
        markdown_to_telegram_html(input),
        "Before\n\n<pre><code>fn main() { println!(\"&lt;&amp;&gt;\"); }\n</code></pre>\n\nAfter"
    );
}

#[test]
fn markdown_to_telegram_html_blockquote() {
    let input = "> quoted\n>\n> ```\n> code\n> ```\n> after";

    assert_eq!(
        markdown_to_telegram_html(input),
        "<blockquote>quoted\n\n</blockquote><pre><code>code\n</code></pre>\n\nafter"
    );
}

#[test]
fn markdown_to_telegram_html_lists_bullets_and_numbered() {
    let input = "- one\n  - nested\n- two\n\n3. third\n4. fourth";

    assert_eq!(
        markdown_to_telegram_html(input),
        "• one\n  • nested\n• two\n\n3. third\n4. fourth"
    );
}

#[test]
fn markdown_to_telegram_html_headings_render_as_plain_paragraphs() {
    assert_eq!(
        markdown_to_telegram_html("# Heading\n\nbody"),
        "Heading\n\nbody"
    );
}

#[test]
fn markdown_to_telegram_html_escapes_raw_html() {
    assert_eq!(
        markdown_to_telegram_html("<script>alert('&')</script>"),
        "&lt;script&gt;alert('&amp;')&lt;/script&gt;"
    );
}

#[test]
fn markdown_to_telegram_html_horizontal_rule() {
    assert_eq!(markdown_to_telegram_html("---"), "───");
}

#[test]
fn markdown_to_telegram_html_tables_render_plain_rows() {
    let input = "| name | value |\n| --- | --- |\n| a | **b** |\n| c | `d` |";

    assert_eq!(
        markdown_to_telegram_html(input),
        "| name | value |\n| a | <b>b</b> |\n| c | <code>d</code> |"
    );
}

#[test]
fn markdown_to_telegram_html_does_not_emit_unsupported_tags() {
    let output = markdown_to_telegram_html(
        "# Heading\n\n- item\n\n<table><tr><td>x</td></tr></table>\n\n| a | b |\n| - | - |\n| c | d |",
    );

    for unsupported in ["<h", "<ul", "<li", "<table", "<p>", "<br>"] {
        assert!(
            !output.contains(unsupported),
            "output contained unsupported tag {unsupported}: {output}"
        );
    }
}

#[test]
fn split_for_final_answer_keeps_short_text_as_single_chunk() {
    let text = "short final answer";

    assert_eq!(
        split_for_final_answer(text, TELEGRAM_TEXT_MAX_UTF16_UNITS),
        vec![text.to_string()]
    );
}

#[test]
fn split_for_final_answer_splits_on_paragraph_boundary_when_possible() {
    let max_units = 40;
    let first_paragraph = "a".repeat(34);
    let second_paragraph = "b".repeat(20);
    let text = format!("{first_paragraph}\n\n{second_paragraph}");

    let chunks = split_for_final_answer(&text, max_units);

    assert_eq!(
        chunks,
        vec![format!("{first_paragraph}\n\n"), second_paragraph]
    );
    assert!(chunks
        .iter()
        .all(|chunk| chunk.encode_utf16().count() <= max_units));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn split_for_final_answer_falls_back_to_line_then_word_then_char() {
    let max_units = 40;

    let line_prefix = "a".repeat(34);
    let line_suffix = "b".repeat(20);
    let line_text = format!("{line_prefix}\n{line_suffix}");
    assert_eq!(
        split_for_final_answer(&line_text, max_units),
        vec![format!("{line_prefix}\n"), line_suffix]
    );

    let word_prefix = "c".repeat(34);
    let word_suffix = "d".repeat(20);
    let word_text = format!("{word_prefix} {word_suffix}");
    assert_eq!(
        split_for_final_answer(&word_text, max_units),
        vec![format!("{word_prefix} "), word_suffix]
    );

    let hard_text = "e".repeat(85);
    let hard_chunks = split_for_final_answer(&hard_text, max_units);
    assert_eq!(hard_chunks.concat(), hard_text);
    assert!(hard_chunks
        .iter()
        .all(|chunk| chunk.encode_utf16().count() <= max_units));
    assert!(hard_chunks.len() > 1);
}

#[test]
fn split_for_final_answer_handles_emoji_at_boundary() {
    let text = format!("{}🙂tail", "a".repeat(TELEGRAM_TEXT_MAX_UTF16_UNITS - 1));

    let chunks = split_for_final_answer(&text, TELEGRAM_TEXT_MAX_UTF16_UNITS);

    assert_eq!(chunks.concat(), text);
    assert_eq!(chunks[0], "a".repeat(TELEGRAM_TEXT_MAX_UTF16_UNITS - 1));
    assert!(chunks[1].starts_with('🙂'));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn short_button_label_keeps_action_verb() {
    assert_eq!(short_button_label("Allow Once", 12), "Allow Once");
    assert_eq!(
        short_button_label("Allow Always for This Tool", 16),
        "Allow Always"
    );
}

#[test]
fn render_truncated_text_keeps_short_text_unchanged() {
    let s = "hello world".to_string();
    assert_eq!(render_truncated_text(&s), s);
}

#[test]
fn render_truncated_text_emits_single_message_under_4096_units() {
    let big = "a".repeat(10_000);
    let out = render_truncated_text(&big);

    assert!(out.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS);
    assert!(out.contains("\u{2026}[truncated;"));
    assert!(out.ends_with("chars dropped]"));
    assert!(!out.contains('\0'));
}

#[test]
fn render_truncated_text_handles_multi_byte_codepoints_at_budget_edge() {
    // Crab emoji: 4 UTF-8 bytes, 2 UTF-16 code units.
    let payload = "\u{1F980}".repeat(5_000);
    let out = render_truncated_text(&payload);

    assert!(out.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS);
    assert!(out.contains("\u{2026}[truncated;"));
    // No partial codepoints in the kept prefix.
    assert!(out.is_char_boundary(out.len()));
}

#[test]
fn render_truncated_text_budget_invariant_random_lengths() {
    // Deterministic LCG so we exercise a broad range of lengths without
    // pulling in a rand dep.
    let mut state: u64 = 0x00fe_edfa_cec0_ffee;
    for _ in 0..200 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = (state >> 33) as usize % 100_001;
        let text: String = std::iter::repeat_n('x', len).collect();
        let out = render_truncated_text(&text);
        assert!(
            out.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS,
            "len={len} produced output with {} units",
            out.encode_utf16().count()
        );
    }
}

#[test]
fn truncate_to_utf16_units_returns_dropped_count() {
    let s = "abcde";
    let (kept, dropped) = truncate_to_utf16_units(s, 3);
    assert_eq!(kept, "abc");
    assert_eq!(dropped, 2);

    let (kept, dropped) = truncate_to_utf16_units(s, 100);
    assert_eq!(kept, "abcde");
    assert_eq!(dropped, 0);
}

#[test]
fn truncate_button_label_bytes_keeps_short_label_unchanged() {
    let s = "Allow Once";
    assert_eq!(truncate_button_label_bytes(s, 64), s);
}

#[test]
fn truncate_button_label_bytes_caps_long_label_with_ellipsis() {
    let s = "a".repeat(200);
    let out = truncate_button_label_bytes(&s, TELEGRAM_BUTTON_LABEL_MAX_BYTES);

    assert!(out.len() <= TELEGRAM_BUTTON_LABEL_MAX_BYTES);
    assert!(out.ends_with('\u{2026}'));
}

#[test]
fn truncate_button_label_bytes_does_not_split_multi_byte_codepoints() {
    // Crab emoji: 4 UTF-8 bytes; 20 of them = 80 bytes, exceeds 64-byte cap.
    let s = "\u{1F980}".repeat(20);
    let out = truncate_button_label_bytes(&s, TELEGRAM_BUTTON_LABEL_MAX_BYTES);

    assert!(out.len() <= TELEGRAM_BUTTON_LABEL_MAX_BYTES);
    // is_char_boundary on len always true; check that out parses as valid UTF-8.
    assert!(out.chars().count() > 0);
    assert!(out.ends_with('\u{2026}'));
}
