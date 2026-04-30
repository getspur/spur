use proptest::prelude::*;
use spur_bot::telegram::format::{
    markdown_to_telegram_chunks, render_truncated_text, short_button_label, split_for_final_answer,
    split_for_telegram, truncate_button_label_bytes, truncate_to_utf16_units,
    TELEGRAM_BUTTON_LABEL_MAX_BYTES, TELEGRAM_TEXT_MAX_UTF16_UNITS,
};

fn single_html(input: &str) -> String {
    let chunks = markdown_to_telegram_chunks(input);
    assert_eq!(chunks.len(), 1, "short input rendered as {chunks:?}");
    chunks[0].html.clone()
}

fn rendered_chunks(input: &str) -> Vec<spur_bot::telegram::format::Chunk> {
    markdown_to_telegram_chunks(input)
}

fn assert_no_partial_entities(chunks: &[spur_bot::telegram::format::Chunk]) {
    for chunk in chunks {
        for partial in ["&", "&a", "&am", "&amp", "&l", "&lt", "&g", "&gt"] {
            assert!(
                !chunk.html.ends_with(partial),
                "chunk ended with partial entity {partial:?}: {}",
                chunk.html
            );
        }
        for partial in ["amp;", "mp;", "p;", "lt;", "t;", "gt;"] {
            assert!(
                !chunk.html.starts_with(partial),
                "chunk started with partial entity {partial:?}: {}",
                chunk.html
            );
        }
    }
}

fn markdown_text() -> impl Strategy<Value = String> {
    let short = "[a-zA-Z0-9 <>&.,_:/=-]{0,160}".prop_map(|s| s);
    let long = prop_oneof![
        Just("a".repeat(5_000)),
        Just("<&>".repeat(1_700)),
        Just("word ".repeat(1_000)),
    ];
    prop_oneof![9 => short, 1 => long]
}

fn any_markdown() -> impl Strategy<Value = String> {
    let paragraph = markdown_text().prop_map(|s| format!("{s}\n\n"));
    let inline = (markdown_text(), markdown_text())
        .prop_map(|(a, b)| format!("**{a}** *{b}* ~~strike~~ and `code <&>`\n\n"));
    let link = (markdown_text(), "[a-z]{1,16}")
        .prop_map(|(label, path)| format!("[{label}](https://example.test/{path}?q=<&>)\n\n"));
    let code = markdown_text()
        .prop_map(|s| format!("```rust\nfn main() {{ println!(\"{s}\"); }}\n```\n\n"));
    let list = (markdown_text(), markdown_text()).prop_map(|(a, b)| format!("- {a}\n- {b}\n\n"));
    let quote = (1usize..=6, markdown_text()).prop_map(|(depth, s)| {
        let prefix = ">".repeat(depth);
        format!("{prefix} {s}\n\n")
    });
    let table = (markdown_text(), markdown_text())
        .prop_map(|(a, b)| format!("| name | value |\n| --- | --- |\n| {a} | {b} |\n\n"));

    prop::collection::vec(
        prop_oneof![paragraph, inline, link, code, list, quote, table],
        0..64,
    )
    .prop_map(|parts| parts.concat())
}

fn is_telegram_html_balanced(html: &str) -> bool {
    let mut stack = Vec::new();
    let mut rest = html;
    while let Some(offset) = rest.find('<') {
        rest = &rest[offset..];
        if let Some(tag) = ["b", "i", "s", "code", "pre", "blockquote"]
            .iter()
            .find(|tag| rest.starts_with(&format!("<{tag}>")))
        {
            stack.push(*tag);
            rest = &rest[tag.len() + 2..];
        } else if rest.starts_with("<a href=\"") {
            let Some(end) = rest.find("\">") else {
                return false;
            };
            stack.push("a");
            rest = &rest[end + 2..];
        } else if let Some(tag) = ["b", "i", "s", "code", "pre", "a", "blockquote"]
            .iter()
            .find(|tag| rest.starts_with(&format!("</{tag}>")))
        {
            if stack.pop() != Some(*tag) {
                return false;
            }
            rest = &rest[tag.len() + 3..];
        } else {
            return false;
        }
    }
    stack.is_empty()
}

fn has_markdown_link_marker(plain: &str) -> bool {
    plain
        .match_indices("](")
        .any(|(idx, _)| plain[..idx].rfind('[').is_some())
}

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
        single_html("**bold** *italic* ~~strike~~"),
        "<b>bold</b> <i>italic</i> <s>strike</s>"
    );
}

#[test]
fn markdown_to_telegram_html_inline_code() {
    assert_eq!(
        single_html("Use `x < y && z > q`."),
        "Use <code>x &lt; y &amp;&amp; z &gt; q</code>."
    );
}

#[test]
fn markdown_to_telegram_html_links() {
    assert_eq!(
        single_html("[label](https://example.test/?q=\"<&>)"),
        "<a href=\"https://example.test/?q=&quot;&lt;&amp;&gt;\">label</a>"
    );
    assert_eq!(single_html("[label]()"), "label");
}

#[test]
fn markdown_to_telegram_html_fenced_code_block() {
    let input = "Before\n\n```rust\nfn main() { println!(\"<&>\"); }\n```\n\nAfter";

    assert_eq!(
        single_html(input),
        "Before\n\n<pre><code>fn main() { println!(\"&lt;&amp;&gt;\"); }\n</code></pre>\n\nAfter"
    );
}

#[test]
fn blockquote_with_code_re_opens_after_code_block() {
    let input = "> quoted\n>\n> ```\n> code\n> ```\n> after";

    assert_eq!(
        single_html(input),
        "<blockquote>quoted\n\n</blockquote><pre><code>code\n</code></pre>\n\n<blockquote>after\n\n</blockquote>"
    );
}

#[test]
fn markdown_to_telegram_html_lists_bullets_and_numbered() {
    let input = "- one\n  - nested\n- two\n\n3. third\n4. fourth";

    assert_eq!(
        single_html(input),
        "• one\n  • nested\n• two\n\n3. third\n4. fourth"
    );
}

#[test]
fn markdown_to_telegram_html_headings_render_as_plain_paragraphs() {
    assert_eq!(single_html("# Heading\n\nbody"), "Heading\n\nbody");
}

#[test]
fn markdown_to_telegram_html_escapes_raw_html() {
    assert_eq!(
        single_html("<script>alert('&')</script>"),
        "&lt;script&gt;alert('&amp;')&lt;/script&gt;"
    );
}

#[test]
fn markdown_to_telegram_html_horizontal_rule() {
    assert_eq!(single_html("---"), "───");
}

#[test]
fn markdown_to_telegram_html_tables_render_plain_rows() {
    let input = "| name | value |\n| --- | --- |\n| a | **b** |\n| c | `d` |";

    assert_eq!(
        single_html(input),
        "| name | value |\n| a | <b>b</b> |\n| c | <code>d</code> |"
    );
}

#[test]
fn markdown_to_telegram_html_does_not_emit_unsupported_tags() {
    let output = single_html(
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
fn tasklists_enabled_renders_checkboxes() {
    assert_eq!(
        single_html("- [x] done\n- [ ] todo"),
        "• [x] done\n• [ ] todo"
    );
}

#[test]
fn entity_aware_split_never_slices_mid_amp() {
    let input = "&".repeat(1_200);
    let chunks = rendered_chunks(&input);

    assert!(chunks.len() > 1);
    assert_no_partial_entities(&chunks);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn inline_link_overflow_falls_back_to_plain_url() {
    let href = format!("https://example.test/{}", "a".repeat(4_200));
    let input = format!("[label]({href})");
    let chunks = rendered_chunks(&input);
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();

    assert!(!html.contains("<a href="));
    assert!(html.contains("label (https://example.test/"));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn inline_code_overflow_falls_back_to_escaped_text() {
    let input = format!("`{}`", "<&>".repeat(1_400));
    let chunks = rendered_chunks(&input);
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();

    assert!(!html.contains("<code>"));
    assert!(html.contains("&lt;&amp;&gt;"));
    assert_no_partial_entities(&chunks);
}

#[test]
fn long_bold_span_split_across_chunks_falls_back_to_plain() {
    let text = "a".repeat(5_000);
    let chunks = rendered_chunks(&format!("**{text}**"));
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();
    let plain = chunks
        .iter()
        .map(|chunk| chunk.plain.as_str())
        .collect::<String>();

    assert!(chunks.len() > 1);
    assert!(chunks
        .iter()
        .all(|chunk| is_telegram_html_balanced(&chunk.html)));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
    assert!(!html.contains("<b>"));
    assert!(!html.contains("</b>"));
    assert_eq!(plain, text);
}

#[test]
fn oversized_fenced_block_splits_at_line_boundary() {
    let input = format!("```rust\n{}```", "let value = 1;\n".repeat(400));
    let chunks = rendered_chunks(&input);

    assert!(chunks.len() > 1);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.starts_with("<pre><code>")));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.contains("</code></pre>")));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn oversized_fenced_block_single_line_falls_to_char_boundary() {
    let input = format!("```\n{}\n```", "<".repeat(2_000));
    let chunks = rendered_chunks(&input);

    assert!(chunks.len() > 1);
    assert_no_partial_entities(&chunks);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn nested_blockquote_with_code_re_opens_full_depth() {
    let input = "> outer\n> > inner\n> > ```\n> > code\n> > ```\n> > after";
    let html = single_html(input);

    assert!(html.contains(
        "</blockquote></blockquote><pre><code>code\n</code></pre>\n\n<blockquote><blockquote>after"
    ));
}

#[test]
fn tag_depth_cap_degrades_to_plain_at_excess() {
    let input = ">>>>>>>>>> deeply quoted";
    let html = single_html(input);

    assert!(html.matches("<blockquote>").count() <= 8);
    assert!(html.contains("deeply quoted"));
}

#[test]
fn table_cell_overflow_emits_pipe_row() {
    let href = format!("https://example.test/{}", "a".repeat(4_100));
    let input = format!("| name | value |\n| --- | --- |\n| a | [label]({href}) |");
    let chunks = rendered_chunks(&input);
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();

    assert!(html.contains("| a | label (https://example.test/"));
    assert!(!html.contains("<table"));
    assert!(!html.contains("<a href="));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn table_cell_with_5k_chars_falls_back_to_plain_pipe_row() {
    let cell = "a".repeat(5_000);
    let input = format!("| name | value |\n| --- | --- |\n| a | {cell} |");
    let chunks = rendered_chunks(&input);
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();
    let plain = chunks
        .iter()
        .map(|chunk| chunk.plain.as_str())
        .collect::<String>();

    assert!(chunks.len() > 1);
    assert!(plain.contains(&format!("| a | {cell} |")));
    assert!(!html.contains("<table"));
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn dynamic_reserve_accounts_for_open_block_depth() {
    let input = format!("> > {}", "&".repeat(1_100));
    let chunks = rendered_chunks(&input);

    assert!(chunks.len() > 1);
    assert_no_partial_entities(&chunks);
    assert!(chunks.iter().all(|chunk| {
        chunk.html.matches("<blockquote>").count() == chunk.html.matches("</blockquote>").count()
            && chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS
    }));
}

#[test]
fn numbered_list_continuation_resumes_at_correct_index() {
    let input = format!("3. first\n4. {}\n5. third", "second ".repeat(900));
    let chunks = rendered_chunks(&input);
    let html = chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();

    assert!(chunks.len() > 1);
    assert!(html.contains("3. first"));
    assert!(html.contains("4. second"));
    assert!(html.contains("5. third"));
    assert!(!html.contains("1. third"));
}

#[test]
fn non_table_pipe_text_renders_as_text() {
    assert_eq!(
        single_html("alpha | beta\nnot a table"),
        "alpha | beta\nnot a table"
    );
}

#[test]
fn plain_projection_strips_markdown_for_links_and_code() {
    let chunks = rendered_chunks("[label](https://example.test) and `code`");

    assert_eq!(chunks[0].plain, "label and code");
}

#[test]
fn html_escape_expansion_is_budgeted_after_rendering() {
    let chunks = rendered_chunks(&"<>&".repeat(1_200));

    assert!(chunks.len() > 1);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn malformed_markdown_markers_do_not_leak_to_plain_projection() {
    let chunks = rendered_chunks("** ** ** ~~strike~~ and `code <&>`\n\n");
    let plain = chunks
        .iter()
        .map(|chunk| chunk.plain.as_str())
        .collect::<String>();

    assert!(!plain.contains("**"));
}

#[test]
fn table_row_flushes_before_overflowing_current_chunk() {
    let input = format!(
        "{}\n\n| name | value |\n| --- | --- |\n| a | {} |",
        "&".repeat(700),
        "<&>".repeat(120)
    );
    let chunks = rendered_chunks(&input);

    assert!(chunks.len() > 1);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS));
}

#[test]
fn golden_llm_outputs_match_expected_chunks() {
    for (name, input, expected) in [
        (
            "llm-output-1",
            include_str!("telegram_format/golden/llm-output-1.md"),
            include_str!("telegram_format/golden/llm-output-1.expected.json"),
        ),
        (
            "llm-output-2",
            include_str!("telegram_format/golden/llm-output-2.md"),
            include_str!("telegram_format/golden/llm-output-2.expected.json"),
        ),
        (
            "llm-output-3",
            include_str!("telegram_format/golden/llm-output-3.md"),
            include_str!("telegram_format/golden/llm-output-3.expected.json"),
        ),
        (
            "llm-output-4",
            include_str!("telegram_format/golden/llm-output-4.md"),
            include_str!("telegram_format/golden/llm-output-4.expected.json"),
        ),
        (
            "llm-output-5",
            include_str!("telegram_format/golden/llm-output-5.md"),
            include_str!("telegram_format/golden/llm-output-5.expected.json"),
        ),
        (
            "llm-output-6",
            include_str!("telegram_format/golden/llm-output-6.md"),
            include_str!("telegram_format/golden/llm-output-6.expected.json"),
        ),
    ] {
        let actual = serde_json::to_string_pretty(&rendered_chunks(input)).unwrap();
        assert_eq!(actual.trim(), expected.trim(), "{name}");
    }
}

proptest! {
    #[test]
    fn no_chunk_exceeds_telegram_limit(input in any_markdown()) {
        for chunk in rendered_chunks(&input) {
            prop_assert!(
                chunk.html.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS,
                "html chunk exceeded limit: {}",
                chunk.html.encode_utf16().count()
            );
            prop_assert!(
                chunk.plain.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS,
                "plain chunk exceeded limit: {}",
                chunk.plain.encode_utf16().count()
            );
        }
    }

    #[test]
    fn html_chunks_have_balanced_tags(input in any_markdown()) {
        for chunk in rendered_chunks(&input) {
            prop_assert!(is_telegram_html_balanced(&chunk.html), "{}", chunk.html);
        }
    }

    #[test]
    fn plain_projection_strips_markdown_markers(input in any_markdown()) {
        for chunk in rendered_chunks(&input) {
            prop_assert!(!chunk.plain.contains("**"), "{}", chunk.plain);
            prop_assert!(!chunk.plain.contains("```"), "{}", chunk.plain);
            prop_assert!(!has_markdown_link_marker(&chunk.plain), "{}", chunk.plain);
        }
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
