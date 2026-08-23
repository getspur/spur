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
    wrap_line_to_width_impl(line, width, None)
}

/// Wrap a line and also return the source character index where each visual
/// row begins. The indices are co-indexed with the returned lines.
///
/// This is intentionally crate-private: ordinary callers should avoid the
/// metadata allocation. The streaming trace cache uses it to retain visual
/// rows that can no longer change after an append.
pub(crate) fn wrap_line_to_width_with_char_starts(
    line: &Line<'_>,
    width: u16,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut starts = Vec::new();
    let lines = wrap_line_to_width_impl(line, width, Some(&mut starts));
    debug_assert_eq!(lines.len(), starts.len());
    (lines, starts)
}

fn wrap_line_to_width_impl(
    line: &Line<'_>,
    width: u16,
    starts: Option<&mut Vec<usize>>,
) -> Vec<Line<'static>> {
    let ascii_len = printable_ascii_span_stream_len(line, width);
    #[cfg(test)]
    record_wrap_path(line, width, ascii_len.is_some());

    if let Some(total_len) = ascii_len {
        return wrap_printable_ascii_spans(line, width, total_len, starts);
    }

    wrap_line_to_width_generic(line, width, starts)
}

fn wrap_line_to_width_generic(
    line: &Line<'_>,
    width: u16,
    mut starts: Option<&mut Vec<usize>>,
) -> Vec<Line<'static>> {
    if width == 0 {
        record_start(&mut starts, 0);
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
        record_start(&mut starts, 0);
        return vec![Line::from("")];
    }
    let total_width: u16 = flat
        .iter()
        .map(|(_, c)| char_width(*c))
        .sum::<u32>()
        .min(u32::from(u16::MAX)) as u16;
    if total_width <= width {
        record_start(&mut starts, 0);
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
            record_start(&mut starts, cur_start);
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
        record_start(&mut starts, cur_start);
        out.push(build_line(&flat[cur_start..]));
    }
    if out.is_empty() {
        record_start(&mut starts, 0);
        out.push(Line::from(""));
    }

    out
}

fn printable_ascii_span_stream_len(line: &Line<'_>, width: u16) -> Option<usize> {
    if width == 0 || line.spans.is_empty() {
        return None;
    }

    let mut total_len = 0usize;
    for span in &line.spans {
        let bytes = span.content.as_bytes();
        if !bytes.iter().all(|byte| matches!(byte, b' '..=b'~')) {
            return None;
        }
        total_len += bytes.len();
    }
    (total_len > 0).then_some(total_len)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AsciiCursor {
    span_idx: usize,
    byte_idx: usize,
    char_idx: usize,
}

impl AsciiCursor {
    fn start(line: &Line<'_>) -> Self {
        let mut cursor = Self {
            span_idx: 0,
            byte_idx: 0,
            char_idx: 0,
        };
        cursor.normalize(line);
        cursor
    }

    fn byte(self, line: &Line<'_>) -> Option<u8> {
        line.spans
            .get(self.span_idx)
            .and_then(|span| span.content.as_bytes().get(self.byte_idx))
            .copied()
    }

    fn advance(&mut self, line: &Line<'_>) {
        debug_assert!(self.byte(line).is_some());
        self.byte_idx += 1;
        self.char_idx += 1;
        self.normalize(line);
    }

    fn normalize(&mut self, line: &Line<'_>) {
        while self.span_idx < line.spans.len()
            && self.byte_idx == line.spans[self.span_idx].content.len()
        {
            self.span_idx += 1;
            self.byte_idx = 0;
        }
    }
}

fn wrap_printable_ascii_spans(
    line: &Line<'_>,
    width: u16,
    total_len: usize,
    mut starts: Option<&mut Vec<usize>>,
) -> Vec<Line<'static>> {
    debug_assert_eq!(
        printable_ascii_span_stream_len(line, width),
        Some(total_len)
    );

    if total_len <= usize::from(width) {
        record_start(&mut starts, 0);
        return vec![line_to_owned(line)];
    }

    let mut out = Vec::new();
    let mut cur_start = AsciiCursor::start(line);
    let mut cursor = cur_start;
    let mut cur_width = 0usize;
    let mut break_end_exclusive: Option<AsciiCursor> = None;
    let mut break_continuation_start: Option<AsciiCursor> = None;
    let mut in_ws = false;

    while let Some(byte) = cursor.byte(line) {
        let is_ws = byte == b' ';
        if is_ws && !in_ws {
            break_end_exclusive = Some(cursor);
            break_continuation_start = None;
            in_ws = true;
        } else if !is_ws && in_ws {
            break_continuation_start = Some(cursor);
            in_ws = false;
        }

        if cur_width.saturating_add(1) > usize::from(width) && cursor.char_idx > cur_start.char_idx
        {
            let (emit_end, next_start) = match (break_end_exclusive, break_continuation_start) {
                (Some(end), Some(cont))
                    if end.char_idx > cur_start.char_idx && cont.char_idx > cur_start.char_idx =>
                {
                    (end, cont)
                }
                (Some(end), None) if end.char_idx > cur_start.char_idx => (end, end),
                _ => (cursor, cursor),
            };
            record_start(&mut starts, cur_start.char_idx);
            out.push(build_ascii_line_range(line, cur_start, emit_end));

            cursor = next_start;
            while cursor.byte(line) == Some(b' ') {
                cursor.advance(line);
            }
            cur_start = cursor;
            cur_width = 0;
            break_end_exclusive = None;
            break_continuation_start = None;
            in_ws = false;
            continue;
        }

        cur_width = cur_width.saturating_add(1);
        cursor.advance(line);
    }

    if cur_start.char_idx < total_len {
        record_start(&mut starts, cur_start.char_idx);
        out.push(build_ascii_line_range(line, cur_start, cursor));
    }
    if out.is_empty() {
        record_start(&mut starts, 0);
        out.push(Line::from(""));
    }
    out
}

fn build_ascii_line_range(line: &Line<'_>, start: AsciiCursor, end: AsciiCursor) -> Line<'static> {
    if start.char_idx == end.char_idx {
        return Line::from("");
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = start;
    while cursor.char_idx < end.char_idx {
        let source = &line.spans[cursor.span_idx];
        let remaining = end.char_idx - cursor.char_idx;
        let available = source.content.len() - cursor.byte_idx;
        let take = remaining.min(available);
        let chunk = &source.content[cursor.byte_idx..cursor.byte_idx + take];

        if let Some(last) = spans.last_mut().filter(|span| span.style == source.style) {
            last.content.to_mut().push_str(chunk);
        } else {
            spans.push(Span::styled(chunk.to_owned(), source.style));
        }

        cursor.byte_idx += take;
        cursor.char_idx += take;
        cursor.normalize(line);
    }
    Line::from(spans)
}

fn record_start(starts: &mut Option<&mut Vec<usize>>, start: usize) {
    if let Some(starts) = starts.as_deref_mut() {
        starts.push(start);
    }
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
#[derive(Clone, Copy, Debug, Default)]
struct WrapPathStats {
    observed_calls: u64,
    observed_chars: u64,
    ascii_eligible_calls: u64,
    ascii_eligible_chars: u64,
    ascii_fast_path_hits: u64,
    ascii_fast_path_chars: u64,
}

#[cfg(test)]
thread_local! {
    static WRAP_PATH_STATS: std::cell::Cell<Option<WrapPathStats>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
fn begin_wrap_path_tracking() {
    WRAP_PATH_STATS.with(|stats| stats.set(Some(WrapPathStats::default())));
}

#[cfg(test)]
fn end_wrap_path_tracking() -> WrapPathStats {
    WRAP_PATH_STATS.with(|stats| stats.take().expect("wrap path tracking must be active"))
}

#[cfg(test)]
fn record_wrap_path(line: &Line<'_>, width: u16, ascii_fast_path_hit: bool) {
    WRAP_PATH_STATS.with(|slot| {
        let Some(mut stats) = slot.get() else {
            return;
        };
        if width == 0 {
            return;
        }

        let chars = line
            .spans
            .iter()
            .map(|span| span.content.chars().count() as u64)
            .sum::<u64>();
        stats.observed_calls += 1;
        stats.observed_chars += chars;
        if printable_ascii_span_stream_len(line, width).is_some() {
            stats.ascii_eligible_calls += 1;
            stats.ascii_eligible_chars += chars;
        }
        if ascii_fast_path_hit {
            stats.ascii_fast_path_hits += 1;
            stats.ascii_fast_path_chars += chars;
        }
        slot.set(Some(stats));
    });
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
        let expected_blue: String = "blue words here"
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let actual_blue: String = blue_chars_found
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert_eq!(actual_blue, expected_blue);
    }

    #[test]
    fn wide_emoji_accounted() {
        let line = s("字字字字字");
        let out = wrap_line_to_width(&line, 5);
        for part in &out {
            assert!(
                w(part) <= 5,
                "part {:?} has width {}",
                part.to_string(),
                w(part)
            );
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

    fn assert_ascii_matches_generic(line: &Line<'_>, width: u16) {
        let total_len = printable_ascii_span_stream_len(line, width)
            .expect("test input must be printable ASCII");
        let mut ascii_starts = Vec::new();
        let ascii = wrap_printable_ascii_spans(line, width, total_len, Some(&mut ascii_starts));
        let mut generic_starts = Vec::new();
        let generic = wrap_line_to_width_generic(line, width, Some(&mut generic_starts));
        assert_eq!(ascii, generic, "line mismatch at width {width}");
        assert_eq!(
            ascii_starts, generic_starts,
            "source starts mismatch at width {width}"
        );
    }

    #[test]
    fn printable_ascii_span_stream_matches_generic_reference() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let cases = vec![
            s("hello"),
            s("hello world"),
            s("abcdefghijklmnopqrstuvwxyz"),
            s("   indented text with   weird  spacing"),
            s("             "),
            Line::from(vec![
                Span::styled("prefix ", red),
                Span::styled("same-style ", red),
                Span::styled("blue words here", blue),
            ]),
            Line::from(vec![
                Span::styled("", red),
                Span::styled("abc def ghi", blue),
                Span::styled("", red),
            ]),
        ];

        for line in &cases {
            for width in [1, 2, 3, 5, 8, 13, 64] {
                assert_ascii_matches_generic(line, width);
            }
        }
    }

    #[test]
    fn unsupported_ascii_span_stream_inputs_use_generic_reference() {
        let cases = vec![
            (s("hello"), 0),
            (Line::from(""), 8),
            (s("hello\tworld"), 8),
            (Line::from(vec![Span::raw("hello\nworld")]), 8),
            (s("delete\u{7f}"), 8),
            (s("héllo"), 8),
            (s("字字字"), 8),
        ];

        for (line, width) in cases {
            assert_eq!(printable_ascii_span_stream_len(&line, width), None);
            let mut actual_starts = Vec::new();
            let actual = wrap_line_to_width_impl(&line, width, Some(&mut actual_starts));
            let mut generic_starts = Vec::new();
            let generic = wrap_line_to_width_generic(&line, width, Some(&mut generic_starts));
            assert_eq!(actual, generic, "fallback line mismatch at width {width}");
            assert_eq!(
                actual_starts, generic_starts,
                "fallback starts mismatch at width {width}"
            );
        }
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn sustained_eight_append_journey_uses_ascii_fast_path() {
        use std::collections::HashMap;

        use ratatui::{backend::TestBackend, Terminal};
        use spur_acp::AgentKind;

        use crate::components::image_cache::ImageCache;
        use crate::components::mermaid::{MermaidId, MermaidState};
        use crate::components::react_trace::{ReactTrace, RenderContext};

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test backend must initialize");
        let mermaid_registry = HashMap::<MermaidId, MermaidState>::new();
        let mut image_cache = ImageCache::new();
        let mut trace = ReactTrace::with_kind(AgentKind::ClaudeCodeAcp);
        let streaming_agent = "stream-live";
        trace.append_message("stream bootstrap", streaming_agent, "10:00:00".to_string());

        let mut draw = |trace: &mut ReactTrace| {
            terminal
                .draw(|frame| {
                    let mut ctx = RenderContext {
                        mermaid_registry: &mermaid_registry,
                        mermaid_registry_version: 0,
                        picker: None,
                        image_cache: &mut image_cache,
                    };
                    trace.render_with_ctx(frame, frame.area(), &mut ctx, None);
                })
                .expect("draw must succeed");
        };
        draw(&mut trace);

        const STREAM_FRAMES: usize = 32;
        const APPENDS_PER_DRAW: usize = 8;
        begin_wrap_path_tracking();
        for frame in 0..STREAM_FRAMES {
            for chunk in 0..APPENDS_PER_DRAW {
                let append_index = frame * APPENDS_PER_DRAW + chunk;
                trace.append_message(
                    " +token",
                    streaming_agent,
                    format!("10:{:02}:{:02}", append_index / 60, append_index % 60),
                );
            }
            draw(&mut trace);
        }
        let stats = end_wrap_path_tracking();
        let eligibility_permille = stats.ascii_eligible_chars * 1_000 / stats.observed_chars;
        eprintln!(
            "wrap_path_stats observed_calls={} observed_chars={} eligible_calls={} eligible_chars={} fast_hits={} fast_chars={} eligibility_permille={}",
            stats.observed_calls,
            stats.observed_chars,
            stats.ascii_eligible_calls,
            stats.ascii_eligible_chars,
            stats.ascii_fast_path_hits,
            stats.ascii_fast_path_chars,
            eligibility_permille,
        );

        assert!(
            stats.observed_chars > 0,
            "journey must exercise line wrapping"
        );
        assert!(
            stats.ascii_eligible_chars * 1_000 >= stats.observed_chars * 818,
            "ASCII eligibility gate missed: {eligibility_permille} permille"
        );
        assert_eq!(
            stats.ascii_fast_path_chars, stats.ascii_eligible_chars,
            "every eligible character should bypass generic flattening"
        );
        assert_eq!(
            stats.ascii_fast_path_hits, stats.ascii_eligible_calls,
            "every eligible call should select the ASCII span stream"
        );
    }
}
