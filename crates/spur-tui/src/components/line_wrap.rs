//! Row-exact line wrapping for the session trace.
//!
//! `wrap_line_to_width` splits a single `ratatui::text::Line` at word
//! boundaries such that each returned `Line` has display width `<= width`.
//! Span styles are preserved across splits (a mid-span break produces two
//! spans with the same `Style`). Whitespace at a break point is dropped
//! from the end of the first output line; leading whitespace on the source
//! line is preserved.
//!
//! This exists because `ratatui::widgets::Paragraph` with `.wrap(...)` uses
//! word-wrap internally, but `Paragraph::scroll((y, _))` counts visual rows
//! post-wrap — and consumers cannot ask how many rows the Paragraph will
//! produce. To make scroll and scrollbar state row-exact, we pre-wrap here
//! and render the Paragraph without its own wrap.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Wrap a single `Line` to the given display width.
///
/// Returns one or more `Line`s each with `width() <= width`. Concatenating
/// the returned lines reproduces the source content (text order and span
/// styles preserved).
///
/// - If the source already fits in `width`, returns `vec![line.clone()]`.
/// - If `width == 0`, returns `vec![line.clone()]` (degenerate — caller
///   must ensure width > 0 for correct behavior).
pub fn wrap_line_to_width(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line_to_owned(line)];
    }

    // Flatten to (Style, char) pairs so we can walk character-by-character
    // with stable per-char styles.
    let flat: Vec<(Style, char)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (style, c))
        })
        .collect();

    // Early-out for trivial cases.
    if flat.is_empty() {
        return vec![Line::from("")];
    }
    let total_width: u16 = flat
        .iter()
        .map(|(_, c)| char_width(*c))
        .sum::<u32>()
        .min(u32::from(u16::MAX)) as u16;
    if total_width <= width {
        return vec![line_to_owned(line)];
    }

    // Greedy word-wrap walker. State:
    //   cur_start          — index into `flat` where current output line begins
    //   i                  — current cursor
    //   cur_width          — display width of flat[cur_start..i]
    //   break_end_exclusive   — set to Some(i) on is_ws && !in_ws (entering
    //     whitespace); the exclusive upper bound for the line being emitted.
    //   break_continuation_start — set to Some(i) on !is_ws && in_ws (exiting
    //     whitespace); where the next line begins after the break.
    //   Both are cleared to None after each break or reset.
    //   in_ws              — are we currently in a whitespace run?
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur_start: usize = 0;
    let mut i: usize = 0;
    let mut cur_width: u16 = 0;
    let mut break_end_exclusive: Option<usize> = None;
    let mut break_continuation_start: Option<usize> = None;
    let mut in_ws: bool = false;

    while i < flat.len() {
        let (_, c) = flat[i];
        let cw = char_width(c) as u16;
        let is_ws = is_wrap_whitespace(c);

        // Detect whitespace-run transitions BEFORE committing `c`.
        if is_ws && !in_ws {
            // Entering whitespace — the word boundary is right here at i.
            // Record break_end_exclusive eagerly so that any overflow that
            // fires during this whitespace run uses the correct boundary,
            // not a stale value from a prior run.
            break_end_exclusive = Some(i);
            // Continuation will be determined when the run ends; clear it so
            // a mid-run overflow falls through to char-break (correct).
            break_continuation_start = None;
            in_ws = true;
        } else if !is_ws && in_ws {
            // Ending whitespace — the continuation starts here at i.
            break_continuation_start = Some(i);
            in_ws = false;
        }

        // Can we fit this character?
        if cur_width.saturating_add(cw) > width && i > cur_start {
            // Must break.
            let (emit_end, next_start) = match (break_end_exclusive, break_continuation_start) {
                (Some(end), Some(cont)) if end > cur_start && cont > cur_start => (end, cont),
                // Overflow fired mid-whitespace-run: break_end_exclusive is set
                // (the word boundary just entered) but break_continuation_start
                // is not yet set (the run hasn't ended). Emit up to break_end
                // and let the skip loop consume the remaining whitespace.
                (Some(end), None) if end > cur_start => (end, end),
                _ => {
                    // No usable word break since `cur_start`. Char-break fallback.
                    (i, i)
                }
            };
            out.push(build_line(&flat[cur_start..emit_end]));
            // Skip any whitespace that immediately follows the break point so
            // it doesn't appear at the start of the next visual line.
            let mut skip = next_start;
            while skip < flat.len() && is_wrap_whitespace(flat[skip].1) {
                skip += 1;
            }
            cur_start = skip;
            i = skip;
            cur_width = 0;
            break_end_exclusive = None;
            break_continuation_start = None;
            in_ws = false;
            // Do not advance `i`; re-evaluate at the new cur_start.
            continue;
        }

        cur_width = cur_width.saturating_add(cw);
        i += 1;
    }

    // Flush remainder.
    if cur_start < flat.len() {
        out.push(build_line(&flat[cur_start..]));
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }

    out
}

/// Display width of a single char. Control chars count as 0; width-unknown
/// chars default to 1 to avoid undercount.
fn char_width(c: char) -> u32 {
    UnicodeWidthChar::width(c).unwrap_or(1) as u32
}

/// A character is a "wrap whitespace" if it's an ASCII space or tab. We
/// intentionally exclude other whitespace categories (e.g. non-breaking
/// space) to keep behavior predictable; newlines shouldn't appear in Line
/// input because callers split on '\n' upstream.
fn is_wrap_whitespace(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Build a `Line<'static>` from a slice of (Style, char) pairs, merging
/// consecutive chars with the same Style into one Span.
fn build_line(chars: &[(Style, char)]) -> Line<'static> {
    if chars.is_empty() {
        return Line::from("");
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style: Style = chars[0].0;
    let mut cur_buf = String::new();
    for (style, c) in chars {
        if *style == cur_style {
            cur_buf.push(*c);
        } else {
            if !cur_buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut cur_buf), cur_style));
            }
            cur_style = *style;
            cur_buf.push(*c);
        }
    }
    if !cur_buf.is_empty() {
        spans.push(Span::styled(cur_buf, cur_style));
    }
    Line::from(spans)
}

/// Clone a borrowed `Line` into a `'static`-lifetimed owned `Line`. Needed
/// because the wrap helper returns owned Lines but input may borrow.
fn line_to_owned(line: &Line<'_>) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.to_string(), s.style))
        .collect();
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn w(line: &Line<'_>) -> u16 {
        line.width() as u16
    }

    fn s(text: &str) -> Line<'static> {
        Line::from(text.to_string())
    }

    #[test]
    fn width_zero_returns_clone() {
        let line = s("hello world");
        let out = wrap_line_to_width(&line, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].width(), 11);
    }

    #[test]
    fn empty_line_returns_single_empty() {
        let line = Line::from("");
        let out = wrap_line_to_width(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(w(&out[0]), 0);
    }

    #[test]
    fn fits_within_width_returns_clone() {
        let line = s("hello");
        let out = wrap_line_to_width(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(w(&out[0]), 5);
    }

    #[test]
    fn plain_word_break_at_whitespace() {
        let line = s("hello world");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_string(), "hello");
        assert_eq!(out[1].to_string(), "world");
        assert!(w(&out[0]) <= 5);
        assert!(w(&out[1]) <= 5);
    }

    #[test]
    fn multiple_breaks() {
        let line = s("aaaaa bbbbb ccccc");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].to_string(), "aaaaa");
        assert_eq!(out[1].to_string(), "bbbbb");
        assert_eq!(out[2].to_string(), "ccccc");
    }

    #[test]
    fn long_word_char_break_fallback() {
        let line = s("abcdefghijklmno");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 3);
        for part in &out {
            assert!(w(part) <= 5, "part {:?} exceeds width", part.to_string());
        }
        let joined: String = out.iter().map(|l| l.to_string()).collect();
        assert_eq!(joined, "abcdefghijklmno");
    }

    #[test]
    fn multi_span_preserved_across_break() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let line = Line::from(vec![
            Span::styled("red_", red),
            Span::styled("blue words here", blue),
        ]);
        let out = wrap_line_to_width(&line, 7);
        assert!(out.len() >= 2);
        for part in &out {
            assert!(w(part) <= 7);
        }
        // Collect every output character with its style, then verify that
        // characters originally in the red span carry Red style and
        // characters originally in the blue span carry Blue style.
        let mut red_chars_found = String::new();
        let mut blue_chars_found = String::new();
        for part in &out {
            for span in &part.spans {
                for c in span.content.chars() {
                    if span.style == red {
                        red_chars_found.push(c);
                    } else if span.style == blue {
                        blue_chars_found.push(c);
                    } else {
                        panic!("unexpected style {:?} on char {:?}", span.style, c);
                    }
                }
            }
        }
        // All four chars of "red_" must appear with Red style.
        assert_eq!(red_chars_found, "red_");
        // All non-whitespace chars of "blue words here" must appear with
        // Blue style. Whitespace at break points is intentionally dropped,
        // so we check non-whitespace parity.
        let expected_blue: String = "blue words here".chars().filter(|c| !c.is_whitespace()).collect();
        let actual_blue: String = blue_chars_found.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(actual_blue, expected_blue);
    }

    #[test]
    fn wide_emoji_accounted() {
        let line = s("字字字字字");
        let out = wrap_line_to_width(&line, 5);
        for part in &out {
            assert!(w(part) <= 5, "part {:?} has width {}", part.to_string(), w(part));
        }
        let joined: String = out.iter().map(|l| l.to_string()).collect();
        assert_eq!(joined, "字字字字字");
    }

    #[test]
    fn leading_whitespace_preserved_on_first_line() {
        let line = s("   hello world");
        let out = wrap_line_to_width(&line, 10);
        for part in &out {
            assert!(w(part) <= 10);
        }
        assert!(out[0].to_string().starts_with("   "));
        assert!(out.iter().any(|l| l.to_string() == "world"));
    }

    #[test]
    fn trailing_whitespace_dropped_at_break() {
        let line = s("aaa  bbb");
        let out = wrap_line_to_width(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_string(), "aaa");
        assert_eq!(out[1].to_string(), "bbb");
    }

    #[test]
    fn invariant_all_outputs_within_width() {
        let cases: Vec<(&str, u16)> = vec![
            ("", 10),
            ("short", 10),
            ("one two three four five", 7),
            ("one two three four five", 10),
            ("one two three four five", 100),
            ("abcdefghijklmnopqrstuvwxyz", 5),
            ("   indented text with   weird  spacing", 10),
            ("a b c d e f g h i j k l m n o p", 3),
            ("字字字 字字字字 字", 4),
        ];
        for (input, width) in cases {
            let line = s(input);
            let out = wrap_line_to_width(&line, width);
            for part in &out {
                assert!(
                    w(part) <= width,
                    "input {:?} width {}: output {:?} has width {}",
                    input,
                    width,
                    part.to_string(),
                    w(part)
                );
            }
        }
    }

    #[test]
    fn overflow_inside_whitespace_run_uses_current_run_not_prior() {
        // "aaa bbb ccc" (11 cols) at width=9 should break at the space between
        // "bbb" and "ccc", yielding ["aaa bbb", "ccc"] (2 rows).
        //
        // Bug: the second space char (position 7) triggers overflow while the
        // state machine is still inside the whitespace run. break_end_exclusive
        // is stale from the "aaa"/"bbb" boundary, so it emits "aaa" and
        // continues with "bbb ccc" → 2 rows still but wrong break. Also, for
        // "aaa bbb  ccc" (12 cols, two spaces) at width=8, the same bug forces
        // 3 rows rather than 2.
        let line = Line::from("aaa bbb ccc".to_string());
        let out = wrap_line_to_width(&line, 9);
        assert_eq!(out.len(), 2, "expected 2 rows at width=9");
        assert_eq!(out[0].to_string(), "aaa bbb");
        assert_eq!(out[1].to_string(), "ccc");

        // Double-space variant:
        let line2 = Line::from("aaa bbb  ccc".to_string());
        let out2 = wrap_line_to_width(&line2, 8);
        assert_eq!(out2.len(), 2, "expected 2 rows at width=8");
        assert_eq!(out2[0].to_string(), "aaa bbb");
        assert_eq!(out2[1].to_string(), "ccc");
    }
}
