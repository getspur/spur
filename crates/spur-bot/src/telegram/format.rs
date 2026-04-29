use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Telegram caps message text at 4096 UTF-16 code units.
pub const TELEGRAM_TEXT_MAX_UTF16_UNITS: usize = 4096;

/// Telegram caps inline-button labels at 64 UTF-8 bytes.
pub const TELEGRAM_BUTTON_LABEL_MAX_BYTES: usize = 64;

const FINAL_ANSWER_SPLIT_BOUNDARY_WINDOW_UTF16_UNITS: usize = 256;

pub fn markdown_to_telegram_html(input: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let events: Vec<_> = Parser::new_ext(input, options).collect();
    let mut renderer = TelegramHtmlRenderer::default();
    for (index, event) in events.iter().enumerate() {
        renderer.render_event(event, events.get(index + 1));
    }
    renderer.finish()
}

#[derive(Default)]
struct TelegramHtmlRenderer {
    output: String,
    lists: Vec<ListState>,
    links: Vec<bool>,
    blockquote_depth: usize,
    open_blockquotes: usize,
    in_code_block: bool,
    table: Option<TableState>,
}

#[derive(Clone, Copy)]
struct ListState {
    kind: ListKind,
}

#[derive(Clone, Copy)]
enum ListKind {
    Bullet,
    Ordered { next: u64 },
}

#[derive(Default)]
struct TableState {
    cells_in_row: usize,
    in_row: bool,
}

impl TelegramHtmlRenderer {
    fn render_event(&mut self, event: &Event<'_>, next: Option<&Event<'_>>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag, next),
            Event::Text(text) => self.push_escaped_text(text),
            Event::Code(code) => {
                self.output.push_str("<code>");
                self.push_escaped_text(code);
                self.output.push_str("</code>");
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_escaped_text(html),
            Event::SoftBreak | Event::HardBreak => self.output.push('\n'),
            Event::Rule => {
                self.ensure_blank_line();
                self.output.push_str("───\n\n");
            }
            Event::FootnoteReference(label) => {
                self.output.push('[');
                self.push_escaped_text(label);
                self.output.push(']');
            }
            Event::TaskListMarker(checked) => {
                self.output.push_str(if *checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => self.push_escaped_text(math),
        }
    }

    fn start_tag(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.lists.is_empty() && self.table.is_none() && self.open_blockquotes == 0 {
                    self.ensure_blank_line();
                }
            }
            Tag::Heading { .. } | Tag::HtmlBlock => self.ensure_blank_line(),
            Tag::BlockQuote(_) => {
                if self.open_blockquotes == 0 {
                    self.ensure_blank_line();
                }
                self.output.push_str("<blockquote>");
                self.blockquote_depth += 1;
                self.open_blockquotes += 1;
            }
            Tag::CodeBlock(_) => {
                if self.open_blockquotes > 0 {
                    for _ in 0..self.open_blockquotes {
                        self.output.push_str("</blockquote>");
                    }
                    self.open_blockquotes = 0;
                } else {
                    self.ensure_blank_line();
                }
                self.output.push_str("<pre><code>");
                self.in_code_block = true;
            }
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.ensure_blank_line();
                } else {
                    self.ensure_line_start();
                }
                self.lists.push(ListState {
                    kind: start.map_or(ListKind::Bullet, |next| ListKind::Ordered { next }),
                });
            }
            Tag::Item => self.start_list_item(),
            Tag::Emphasis => self.output.push_str("<i>"),
            Tag::Strong => self.output.push_str("<b>"),
            Tag::Strikethrough => self.output.push_str("<s>"),
            Tag::Link { dest_url, .. } => {
                let has_dest = !dest_url.is_empty();
                if has_dest {
                    self.output.push_str("<a href=\"");
                    push_escaped_attr(&mut self.output, dest_url);
                    self.output.push_str("\">");
                }
                self.links.push(has_dest);
            }
            Tag::Image { .. } => {}
            Tag::Table(_) => {
                self.ensure_blank_line();
                self.table = Some(TableState::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    if !self.output.ends_with('\n') && !self.output.is_empty() {
                        self.output.push('\n');
                    }
                    self.output.push_str("| ");
                    table.cells_in_row = 0;
                    table.in_row = true;
                }
            }
            Tag::TableCell => {
                if let Some(table) = &self.table {
                    if table.cells_in_row > 0 {
                        self.output.push_str(" | ");
                    }
                }
            }
            Tag::Superscript
            | Tag::Subscript
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: &TagEnd, next: Option<&Event<'_>>) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                if self.lists.is_empty()
                    && self.table.is_none()
                    && !matches!(next, Some(Event::End(TagEnd::BlockQuote(_))))
                {
                    self.output.push_str("\n\n");
                }
            }
            TagEnd::HtmlBlock => self.output.push_str("\n\n"),
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                if self.open_blockquotes > 0 {
                    self.output.push_str("</blockquote>");
                    self.open_blockquotes -= 1;
                }
                if self.open_blockquotes == 0 {
                    self.output.push_str("\n\n");
                }
            }
            TagEnd::CodeBlock => {
                self.output.push_str("</code></pre>\n\n");
                self.in_code_block = false;
            }
            TagEnd::List(_) => {
                let _ = self.lists.pop();
            }
            TagEnd::Item => {
                if !self.output.ends_with('\n') {
                    self.output.push('\n');
                }
            }
            TagEnd::Emphasis => self.output.push_str("</i>"),
            TagEnd::Strong => self.output.push_str("</b>"),
            TagEnd::Strikethrough => self.output.push_str("</s>"),
            TagEnd::Link => {
                if self.links.pop().unwrap_or(false) {
                    self.output.push_str("</a>");
                }
            }
            TagEnd::Image => {}
            TagEnd::Table => {
                self.table = None;
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    if table.in_row {
                        self.output.push_str(" |\n");
                        table.in_row = false;
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cells_in_row += 1;
                }
            }
            TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_list_item(&mut self) {
        let depth = self.lists.len().saturating_sub(1);
        self.ensure_line_start();
        for _ in 0..depth {
            self.output.push_str("  ");
        }

        let Some(list) = self.lists.last_mut() else {
            return;
        };
        match &mut list.kind {
            ListKind::Bullet => self.output.push_str("• "),
            ListKind::Ordered { next } => {
                let number = *next;
                *next += 1;
                self.output.push_str(&format!("{number}. "));
            }
        }
    }

    fn push_escaped_text(&mut self, text: &str) {
        push_escaped_text(&mut self.output, text);
    }

    fn ensure_blank_line(&mut self) {
        if self.output.is_empty() || self.in_code_block {
            return;
        }
        let trailing_newlines = self
            .output
            .chars()
            .rev()
            .take_while(|ch| *ch == '\n')
            .count();
        match trailing_newlines {
            0 => self.output.push_str("\n\n"),
            1 => self.output.push('\n'),
            _ => {}
        }
    }

    fn ensure_line_start(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn finish(self) -> String {
        self.output.trim_end().to_string()
    }
}

fn push_escaped_text(output: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(ch),
        }
    }
}

fn push_escaped_attr(output: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(ch),
        }
    }
}

/// Truncate `text` so its UTF-16 code-unit length is at most `max_units`.
/// Cuts on a char boundary. Returns the kept prefix and the count of dropped
/// chars.
pub fn truncate_to_utf16_units(text: &str, max_units: usize) -> (String, usize) {
    let total_chars = text.chars().count();
    let mut units = 0usize;
    let mut keep = String::with_capacity(text.len().min(max_units.saturating_mul(2)));
    for ch in text.chars() {
        let next = units + ch.len_utf16();
        if next > max_units {
            break;
        }
        units = next;
        keep.push(ch);
    }
    let dropped = total_chars - keep.chars().count();
    (keep, dropped)
}

/// Truncate `label` so its UTF-8 byte length is at most `max_bytes`. Cuts on a
/// char boundary and appends an ellipsis when truncation actually happens.
pub fn truncate_button_label_bytes(label: &str, max_bytes: usize) -> String {
    if label.len() <= max_bytes {
        return label.to_string();
    }
    const ELLIPSIS: &str = "\u{2026}"; // "…", 3 UTF-8 bytes
    let cap = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut out = String::with_capacity(cap);
    for ch in label.chars() {
        if out.len() + ch.len_utf8() > cap {
            break;
        }
        out.push(ch);
    }
    out.push_str(ELLIPSIS);
    out
}

/// Render `text` as a single message that fits Telegram's 4096-UTF-16-unit
/// limit. If truncation is required, append `\n\n…[truncated; N chars
/// dropped]` where N is the count of dropped chars.
///
/// Budget is computed against the worst-case tail length (digit width sized to
/// total chars), guaranteeing the final body never exceeds the limit
/// regardless of the actual dropped count.
pub fn render_truncated_text(text: &str) -> String {
    if text.encode_utf16().count() <= TELEGRAM_TEXT_MAX_UTF16_UNITS {
        return text.to_string();
    }
    let total_chars = text.chars().count();
    let worst_tail = format!("\n\n\u{2026}[truncated; {total_chars} chars dropped]");
    let worst_tail_units = worst_tail.encode_utf16().count();
    let budget = TELEGRAM_TEXT_MAX_UTF16_UNITS.saturating_sub(worst_tail_units);
    let (kept, dropped) = truncate_to_utf16_units(text, budget);
    let actual_tail = format!("\n\n\u{2026}[truncated; {dropped} chars dropped]");
    debug_assert!(
        kept.encode_utf16().count() + actual_tail.encode_utf16().count()
            <= TELEGRAM_TEXT_MAX_UTF16_UNITS,
    );
    format!("{kept}{actual_tail}")
}

pub fn split_for_final_answer(text: &str, max_units: usize) -> Vec<String> {
    assert!(max_units > 0, "max_units must be positive");

    if text.encode_utf16().count() <= max_units {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.encode_utf16().count() <= max_units {
            chunks.push(remaining.to_string());
            break;
        }

        let hard_split = truncate_to_utf16_units(remaining, max_units).0.len();
        let split_at = preferred_final_answer_split(remaining, hard_split, max_units)
            .unwrap_or(hard_split)
            .max(remaining.chars().next().map(char::len_utf8).unwrap_or(1));

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    chunks
}

fn preferred_final_answer_split(text: &str, hard_split: usize, max_units: usize) -> Option<usize> {
    let prefix = &text[..hard_split];
    let min_units = max_units.saturating_sub(FINAL_ANSWER_SPLIT_BOUNDARY_WINDOW_UTF16_UNITS);

    ["\n\n", "\n", " "]
        .into_iter()
        .find_map(|delimiter| best_split_after_delimiter(prefix, delimiter, min_units, max_units))
}

fn best_split_after_delimiter(
    text: &str,
    delimiter: &str,
    min_units: usize,
    max_units: usize,
) -> Option<usize> {
    text.match_indices(delimiter)
        .map(|(idx, matched)| idx + matched.len())
        .filter(|&split_at| {
            let units = text[..split_at].encode_utf16().count();
            units > min_units && units <= max_units
        })
        .last()
}

pub fn split_for_telegram(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if current.chars().count() == max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

pub fn short_button_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }

    let first_word = label.split_whitespace().next().unwrap_or(label);
    label
        .split_whitespace()
        .scan(String::new(), |acc, part| {
            let candidate = if acc.is_empty() {
                part.to_string()
            } else {
                format!("{acc} {part}")
            };
            if candidate.chars().count() < max_chars || acc.as_str() == first_word {
                *acc = candidate.clone();
                Some(acc.clone())
            } else {
                None
            }
        })
        .last()
        .unwrap_or_else(|| first_word.to_string())
}
