use std::{fmt::Write as _, ops::Range};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;

/// Telegram caps message text at 4096 UTF-16 code units.
pub const TELEGRAM_TEXT_MAX_UTF16_UNITS: usize = 4096;

/// Telegram caps inline-button labels at 64 UTF-8 bytes.
pub const TELEGRAM_BUTTON_LABEL_MAX_BYTES: usize = 64;

const FINAL_ANSWER_SPLIT_BOUNDARY_WINDOW_UTF16_UNITS: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Chunk {
    pub html: String,
    pub plain: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ChunkBudget {
    pub max_units: usize,
    pub min_safety_floor: usize,
    pub max_nesting_depth: u8,
}

impl Default for ChunkBudget {
    fn default() -> Self {
        Self {
            max_units: TELEGRAM_TEXT_MAX_UTF16_UNITS,
            min_safety_floor: 32,
            max_nesting_depth: 8,
        }
    }
}

pub struct ChunkedHtmlRenderer<'a, I: Iterator<Item = Event<'a>>> {
    events: I,
    state: RendererState,
    budget: ChunkBudget,
    chunks: Vec<Chunk>,
}

#[derive(Default)]
struct RendererState {
    current_html: String,
    current_plain: String,
    open_blocks: Vec<BlockContext>,
    open_inlines: Vec<InlineContext>,
    list_stack: Vec<ListContext>,
    table_state: Option<TableState>,
    suspended_blockquotes: u8,
    suppressed_depth: usize,
    plain_code_ranges: Vec<Range<usize>>,
}

#[derive(Clone)]
enum BlockContext {
    BlockQuote,
    PreCode,
}

#[derive(Clone)]
enum InlineContext {
    Bold {
        html_start: usize,
        plain_start: usize,
    },
    Italic {
        html_start: usize,
        plain_start: usize,
    },
    Strike {
        html_start: usize,
        plain_start: usize,
    },
    Code,
    Link {
        href: String,
        tagged: bool,
        html_start: usize,
        plain_start: usize,
    },
    Image {
        dest_url: String,
        html_start: usize,
        plain_start: usize,
    },
}

enum InlineKind {
    Bold,
    Italic,
    Strike,
}

#[derive(Clone)]
struct ListContext {
    kind: ListKind,
    next_number: u64,
    current_number: Option<u64>,
    item_continuation: bool,
}

#[derive(Clone, Copy)]
enum ListKind {
    Bullet,
    Numbered,
}

#[derive(Default)]
struct TableState {
    in_header: bool,
    column_count: u8,
    current_cell_index: u8,
    in_row: bool,
}

pub fn markdown_to_telegram_chunks(input: &str) -> Vec<Chunk> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(input, options);
    ChunkedHtmlRenderer::new(parser, ChunkBudget::default()).into_chunks()
}

/// Legacy single-string renderer. Prefer `markdown_to_telegram_chunks` for
/// Telegram sends so chunk boundaries are computed after HTML escaping.
pub fn markdown_to_telegram_html(input: &str) -> String {
    markdown_to_telegram_chunks(input)
        .into_iter()
        .map(|chunk| chunk.html)
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl<'a, I: Iterator<Item = Event<'a>>> ChunkedHtmlRenderer<'a, I> {
    pub fn new(events: I, budget: ChunkBudget) -> Self {
        Self {
            events,
            state: RendererState::default(),
            budget,
            chunks: Vec::new(),
        }
    }

    pub fn into_chunks(mut self) -> Vec<Chunk> {
        while let Some(event) = self.events.next() {
            if matches!(&event, Event::Start(Tag::TableHead | Tag::TableRow)) {
                let row_events = self.collect_table_row(event);
                self.apply_table_row(row_events);
                continue;
            }

            let cost = self.event_cost(&event);
            let reserve = self.dynamic_reserve();
            if self.state.current_html_units() + cost + reserve > self.budget.max_units
                && self.state.at_safe_flush_point()
            {
                self.flush_chunk();
            }
            self.apply_event(event);
        }
        self.finalize();
        self.chunks
    }

    fn collect_table_row(&mut self, first: Event<'a>) -> Vec<Event<'a>> {
        let is_head = matches!(&first, Event::Start(Tag::TableHead));
        let mut events = vec![first];
        for event in self.events.by_ref() {
            let is_end = matches!(
                (&event, is_head),
                (Event::End(TagEnd::TableHead), true) | (Event::End(TagEnd::TableRow), false)
            );
            events.push(event);
            if is_end {
                break;
            }
        }
        events
    }

    fn apply_table_row(&mut self, events: Vec<Event<'a>>) {
        let row = render_table_row(&events);
        self.ensure_line_start();

        let reserve = self.dynamic_reserve();
        if self.state.current_html_units() + row.html_units() + reserve > self.budget.max_units
            && self.state.at_safe_flush_point()
        {
            self.flush_chunk();
            self.ensure_line_start();
        }

        if row.html_units() + self.dynamic_reserve() > self.budget.max_units {
            self.push_escaped_text_budgeted(&row.plain);
        } else {
            self.state.current_html.push_str(&row.html);
            self.state.current_plain.push_str(&row.plain);
        }

        if let Some(table) = &mut self.state.table_state {
            if row.is_header {
                table.column_count = table.column_count.max(row.column_count);
            }
            table.in_header = false;
            table.in_row = false;
            table.current_cell_index = 0;
        }
    }

    fn event_cost(&self, event: &Event<'_>) -> usize {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => 11 + escaped_attr_units(dest_url),
            Event::Start(Tag::BlockQuote(_)) => 12,
            Event::Start(Tag::CodeBlock(_)) => self.state.open_blockquote_count() * 13 + 13,
            Event::Start(Tag::TableHead | Tag::TableRow | Tag::TableCell) => 3,
            Event::Start(Tag::Image { .. }) => 0,
            Event::Start(_) => 8,
            Event::End(TagEnd::BlockQuote(_)) => 15,
            Event::End(TagEnd::CodeBlock) => {
                17 + 12 * usize::from(self.state.suspended_blockquotes)
            }
            Event::End(TagEnd::List(_) | TagEnd::Image | TagEnd::Table | TagEnd::TableCell) => 0,
            Event::End(_) => 8,
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                escaped_text_units(text)
            }
            Event::Code(code) => 13 + escaped_text_units(code),
            Event::InlineMath(math) | Event::DisplayMath(math) => escaped_text_units(math),
            Event::SoftBreak | Event::HardBreak => 1,
            Event::Rule => 5,
            Event::FootnoteReference(label) => escaped_text_units(label) + 2,
            Event::TaskListMarker(_) => 4,
        }
    }

    fn dynamic_reserve(&self) -> usize {
        let inline_close: usize = self
            .state
            .open_inlines
            .iter()
            .rev()
            .map(|inline| close_inline_tag(inline).len())
            .sum();
        let block_close: usize = self
            .state
            .open_blocks
            .iter()
            .rev()
            .map(|block| close_block_tag(block).len())
            .sum();
        let list_prefix = self.state.list_stack.len().saturating_mul(4);
        let table_close = self
            .state
            .table_state
            .as_ref()
            .filter(|table| table.in_row)
            .map_or(0, |_| 16);
        self.budget
            .min_safety_floor
            .max(inline_close + block_close + list_prefix + table_close + 16)
    }

    fn apply_event(&mut self, event: Event<'_>) {
        if self.state.is_suppressing() {
            self.apply_suppressed_event(event);
            return;
        }

        match event {
            Event::Start(tag) => self.start_tag(&tag),
            Event::End(tag) => self.end_tag(&tag),
            Event::Text(text) => self.push_escaped_text_budgeted(&text),
            Event::Code(code) => {
                self.push_inline_code(&code);
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_escaped_text_budgeted(&html),
            Event::SoftBreak | Event::HardBreak => self.push_text_literal("\n"),
            Event::Rule => {
                self.ensure_blank_line();
                self.push_text_literal("───\n\n");
            }
            Event::FootnoteReference(label) => self.push_escaped_text_budgeted(&label),
            Event::TaskListMarker(checked) => {
                self.push_text_literal(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                self.push_escaped_text_budgeted(&math);
            }
        }
    }

    fn start_tag(&mut self, tag: &Tag<'_>) {
        if self.would_exceed_depth(tag) {
            self.state.suppressed_depth = 1;
            return;
        }

        match tag {
            Tag::Paragraph => {
                if self.state.list_stack.is_empty()
                    && self.state.table_state.is_none()
                    && self.state.open_blockquote_count() == 0
                {
                    self.ensure_blank_line();
                }
            }
            Tag::Heading { .. } | Tag::HtmlBlock => self.ensure_blank_line(),
            Tag::BlockQuote(_) => {
                if self.state.open_blockquote_count() == 0 {
                    self.ensure_blank_line();
                }
                self.push_html_literal("<blockquote>");
                self.state.open_blocks.push(BlockContext::BlockQuote);
            }
            Tag::CodeBlock(kind) => self.start_code_block(kind),
            Tag::List(start) => {
                if self.state.list_stack.is_empty() {
                    self.ensure_blank_line();
                } else {
                    self.ensure_line_start();
                }
                self.state.list_stack.push(ListContext {
                    kind: start.map_or(ListKind::Bullet, |_| ListKind::Numbered),
                    next_number: start.unwrap_or(1),
                    current_number: None,
                    item_continuation: false,
                });
            }
            Tag::Item => self.start_list_item(),
            Tag::Emphasis => self.open_inline(InlineKind::Italic),
            Tag::Strong => self.open_inline(InlineKind::Bold),
            Tag::Strikethrough => self.open_inline(InlineKind::Strike),
            Tag::Link { dest_url, .. } => {
                self.start_link(dest_url);
            }
            Tag::Image { dest_url, .. } => self.start_image(dest_url),
            Tag::Table(_) => {
                self.ensure_blank_line();
                self.state.table_state = Some(TableState::default());
            }
            Tag::TableCell => {
                let needs_separator = self
                    .state
                    .table_state
                    .as_ref()
                    .is_some_and(|table| table.current_cell_index > 0);
                if needs_separator {
                    self.push_text_literal(" | ");
                }
                if let Some(table) = &mut self.state.table_state {
                    table.current_cell_index = table.current_cell_index.saturating_add(1);
                    if table.in_header {
                        table.column_count = table.column_count.max(table.current_cell_index);
                    }
                }
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: &TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                if self.state.list_stack.is_empty()
                    && self.state.table_state.is_none()
                    && !self.state.in_code_block()
                {
                    self.push_text_literal("\n\n");
                }
            }
            TagEnd::HtmlBlock => self.push_text_literal("\n\n"),
            TagEnd::BlockQuote(_) => {
                if self.pop_blockquote() {
                    self.push_html_literal("</blockquote>");
                }
                if self.state.open_blockquote_count() == 0 {
                    self.push_text_literal("\n\n");
                }
            }
            TagEnd::CodeBlock => {
                self.end_code_block();
            }
            TagEnd::List(_) => {
                let _ = self.state.list_stack.pop();
            }
            TagEnd::Item => {
                if !self.state.current_html.ends_with('\n') {
                    self.push_text_literal("\n");
                }
                if let Some(list) = self.state.list_stack.last_mut() {
                    list.item_continuation = false;
                    list.current_number = None;
                }
            }
            TagEnd::Emphasis => self.close_inline(TagEnd::Emphasis),
            TagEnd::Strong => self.close_inline(TagEnd::Strong),
            TagEnd::Strikethrough => self.close_inline(TagEnd::Strikethrough),
            TagEnd::Link => self.end_link(),
            TagEnd::Image => self.end_image(),
            TagEnd::Table => {
                self.state.table_state = None;
            }
            TagEnd::TableCell => {}
            _ => {}
        }
    }

    fn start_list_item(&mut self) {
        let depth = self.state.list_stack.len().saturating_sub(1);
        self.ensure_line_start();
        for _ in 0..depth {
            self.push_text_literal("  ");
        }

        let Some(list) = self.state.list_stack.last_mut() else {
            return;
        };
        let prefix = match list.kind {
            ListKind::Bullet => "• ".to_string(),
            ListKind::Numbered => {
                let number = list.next_number;
                list.next_number += 1;
                list.current_number = Some(number);
                let mut prefix = String::new();
                let _ = write!(prefix, "{number}. ");
                prefix
            }
        };
        list.item_continuation = true;
        self.push_text_literal(&prefix);
    }

    fn ensure_blank_line(&mut self) {
        if self.state.current_html.is_empty() || self.state.in_code_block() {
            return;
        }
        let trailing_newlines = self
            .state
            .current_html
            .chars()
            .rev()
            .take_while(|ch| *ch == '\n')
            .count();
        match trailing_newlines {
            0 => self.push_text_literal("\n\n"),
            1 => self.push_text_literal("\n"),
            _ => {}
        }
    }

    fn ensure_line_start(&mut self) {
        if !self.state.current_html.is_empty() && !self.state.current_html.ends_with('\n') {
            self.push_text_literal("\n");
        }
    }

    fn start_code_block(&mut self, _kind: &CodeBlockKind<'_>) {
        let suspended = self.suspend_open_blockquotes();
        if suspended == 0 {
            self.ensure_blank_line();
        }
        self.push_html_literal("<pre><code>");
        self.state.open_blocks.push(BlockContext::PreCode);
    }

    fn end_code_block(&mut self) {
        if self.pop_pre_code() {
            self.push_html_literal("</code></pre>");
        }
        self.push_text_literal("\n\n");
        let suspended = self.state.suspended_blockquotes;
        self.state.suspended_blockquotes = 0;
        for _ in 0..suspended {
            self.push_html_literal("<blockquote>");
            self.state.open_blocks.push(BlockContext::BlockQuote);
        }
    }

    fn start_link(&mut self, dest_url: &str) {
        let href = dest_url.to_string();
        let html_start = self.state.current_html.len();
        let plain_start = self.state.current_plain.len();
        let tagged = !href.is_empty()
            && 15 + escaped_attr_units(&href) + self.dynamic_reserve() <= self.budget.max_units;
        if tagged {
            self.push_html_literal("<a href=\"");
            push_escaped_attr(&mut self.state.current_html, &href);
            self.push_html_literal("\">");
        }
        self.state.open_inlines.push(InlineContext::Link {
            href,
            tagged,
            html_start,
            plain_start,
        });
    }

    fn end_link(&mut self) {
        let Some(InlineContext::Link {
            href,
            tagged,
            plain_start,
            ..
        }) = self.state.open_inlines.pop()
        else {
            return;
        };
        if tagged {
            self.push_html_literal("</a>");
        } else if !href.is_empty() {
            let suffix = if self.state.current_plain.len() == plain_start {
                format!("({href})")
            } else {
                format!(" ({href})")
            };
            self.push_escaped_text_budgeted(&suffix);
        }
    }

    fn start_image(&mut self, dest_url: &str) {
        self.state.open_inlines.push(InlineContext::Image {
            dest_url: dest_url.to_string(),
            html_start: self.state.current_html.len(),
            plain_start: self.state.current_plain.len(),
        });
    }

    fn end_image(&mut self) {
        let Some(InlineContext::Image {
            dest_url,
            html_start,
            plain_start,
        }) = self.state.open_inlines.pop()
        else {
            return;
        };
        let alt = self.state.current_plain[plain_start..].trim().to_string();
        self.state.current_html.truncate(html_start);
        self.state.current_plain.truncate(plain_start);

        let projection = if alt.is_empty() {
            format!("({dest_url})")
        } else {
            format!("[image: {alt}] ({dest_url})")
        };
        self.push_escaped_text_budgeted(&projection);
    }

    fn open_inline(&mut self, kind: InlineKind) {
        let html_start = self.state.current_html.len();
        let plain_start = self.state.current_plain.len();
        let inline = match kind {
            InlineKind::Bold => {
                self.push_html_literal("<b>");
                InlineContext::Bold {
                    html_start,
                    plain_start,
                }
            }
            InlineKind::Italic => {
                self.push_html_literal("<i>");
                InlineContext::Italic {
                    html_start,
                    plain_start,
                }
            }
            InlineKind::Strike => {
                self.push_html_literal("<s>");
                InlineContext::Strike {
                    html_start,
                    plain_start,
                }
            }
        };
        self.state.open_inlines.push(inline);
    }

    fn close_inline(&mut self, tag: TagEnd) {
        let Some(inline) = self.state.open_inlines.pop() else {
            return;
        };
        let matches = matches!(
            (&inline, tag),
            (InlineContext::Italic { .. }, TagEnd::Emphasis)
                | (InlineContext::Bold { .. }, TagEnd::Strong)
                | (InlineContext::Strike { .. }, TagEnd::Strikethrough)
        );
        if matches {
            self.push_html_literal(close_inline_tag(&inline));
        }
    }

    fn push_inline_code(&mut self, code: &str) {
        let tagged_cost = 13 + escaped_text_units(code);
        if tagged_cost + self.dynamic_reserve() > self.budget.max_units {
            self.push_escaped_text_budgeted(code);
            return;
        }
        if self.state.current_html_units() + tagged_cost + self.dynamic_reserve()
            > self.budget.max_units
            && self.state.at_safe_flush_point()
        {
            self.flush_chunk();
        }
        if self.state.current_html_units() + tagged_cost + self.dynamic_reserve()
            > self.budget.max_units
        {
            self.push_escaped_text_budgeted(code);
            return;
        }

        self.state.open_inlines.push(InlineContext::Code);
        self.push_html_literal("<code>");
        push_escaped_text(&mut self.state.current_html, code);
        let plain_start = self.state.current_plain.len();
        self.state.current_plain.push_str(code);
        self.state
            .plain_code_ranges
            .push(plain_start..self.state.current_plain.len());
        self.push_html_literal("</code>");
        let _ = self.state.open_inlines.pop();
    }

    fn push_escaped_text_budgeted(&mut self, text: &str) {
        let mut remaining = text;
        while !remaining.is_empty() {
            let reserve = self.dynamic_reserve();
            if self.state.current_html_units() + escaped_text_units(remaining) + reserve
                <= self.budget.max_units
            {
                self.push_escaped_text_raw(remaining);
                break;
            }

            if self
                .state
                .table_state
                .as_ref()
                .is_some_and(|table| table.in_row)
                && !self.state.in_code_block()
            {
                self.push_escaped_text_raw(remaining);
                break;
            }

            if !self.state.open_inlines.is_empty() {
                self.degrade_open_inline_run_to_plain(remaining);
                break;
            }

            let available = self
                .budget
                .max_units
                .saturating_sub(self.state.current_html_units() + reserve);
            if available == 0 {
                self.flush_chunk();
                continue;
            }
            let split_at = best_escaped_text_split(remaining, available);
            let (head, tail) = remaining.split_at(split_at);
            self.push_escaped_text_raw(head);
            remaining = tail;
            if !remaining.is_empty() {
                self.flush_chunk();
            }
        }
    }

    fn push_escaped_text_raw(&mut self, text: &str) {
        push_escaped_text(&mut self.state.current_html, text);
        if self.state.in_code_block() {
            let plain_start = self.state.current_plain.len();
            self.state.current_plain.push_str(text);
            self.state
                .plain_code_ranges
                .push(plain_start..self.state.current_plain.len());
        } else {
            push_plain_projection_text(&mut self.state.current_plain, text);
        }
    }

    fn degrade_open_inline_run_to_plain(&mut self, pending: &str) {
        let Some((html_start, plain_start)) = self.state.open_inline_run_start() else {
            return;
        };
        let mut plain = self.state.current_plain[plain_start..].to_string();
        plain.push_str(pending);
        if let Some(projection) = self.state.open_inline_image_projection(pending) {
            plain = projection;
        } else if let Some(href) = self.state.open_inline_link_href() {
            if plain.is_empty() {
                let _ = write!(plain, "({href})");
            } else {
                let _ = write!(plain, " ({href})");
            }
        }

        self.state.current_html.truncate(html_start);
        self.state.current_plain.truncate(plain_start);
        self.state
            .plain_code_ranges
            .retain(|range| range.end <= plain_start);
        self.state.open_inlines.clear();
        self.push_escaped_text_budgeted(&plain);
    }

    fn push_html_literal(&mut self, text: &str) {
        self.state.current_html.push_str(text);
    }

    fn push_text_literal(&mut self, text: &str) {
        self.state.current_html.push_str(text);
        self.state.current_plain.push_str(text);
    }

    fn flush_chunk(&mut self) {
        if self.state.current_html.is_empty() && self.state.current_plain.is_empty() {
            return;
        }

        self.close_open_inlines_for_flush();
        let blocks = self.state.open_blocks.clone();
        for block in blocks.iter().rev() {
            self.state.current_html.push_str(close_block_tag(block));
        }

        let html = chunk_text(&self.state.current_html);
        let plain = plain_chunk_text(&self.state.current_plain, &self.state.plain_code_ranges);
        if !html.is_empty() || !plain.is_empty() {
            self.chunks.push(Chunk { html, plain });
        }

        self.state.current_html.clear();
        self.state.current_plain.clear();
        self.state.plain_code_ranges.clear();
        self.state.open_blocks.clear();
        for block in blocks {
            self.state.current_html.push_str(open_block_tag(&block));
            self.state.open_blocks.push(block);
        }
        if self
            .state
            .list_stack
            .last()
            .is_some_and(|list| list.item_continuation)
        {
            let depth = self.state.list_stack.len().saturating_sub(1);
            let prefix = self.list_continuation_prefix();
            for _ in 0..depth {
                self.push_text_literal("  ");
            }
            self.push_text_literal(&prefix);
        }
    }

    fn list_continuation_prefix(&self) -> String {
        let Some(list) = self.state.list_stack.last() else {
            return String::new();
        };
        match list.kind {
            ListKind::Bullet => "• ".to_string(),
            ListKind::Numbered => {
                let number = list
                    .current_number
                    .unwrap_or_else(|| list.next_number.saturating_sub(1));
                let mut prefix = String::new();
                let _ = write!(prefix, "{number}. ");
                prefix
            }
        }
    }

    fn close_open_inlines_for_flush(&mut self) {
        while let Some(inline) = self.state.open_inlines.pop() {
            match &inline {
                InlineContext::Link { tagged, .. } => {
                    if *tagged {
                        self.state.current_html.push_str("</a>");
                    }
                }
                _ => self.state.current_html.push_str(close_inline_tag(&inline)),
            }
        }
    }

    fn finalize(&mut self) {
        self.close_open_inlines_for_flush();
        let blocks = self.state.open_blocks.clone();
        for block in blocks.iter().rev() {
            self.state.current_html.push_str(close_block_tag(block));
        }
        self.state.open_blocks.clear();

        let html = chunk_text(&self.state.current_html);
        let plain = plain_chunk_text(&self.state.current_plain, &self.state.plain_code_ranges);
        if !html.is_empty() || !plain.is_empty() {
            self.chunks.push(Chunk { html, plain });
        }
    }

    fn apply_suppressed_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(_) => self.state.suppressed_depth += 1,
            Event::End(_) => {
                self.state.suppressed_depth = self.state.suppressed_depth.saturating_sub(1);
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => self.push_escaped_text_budgeted(&text),
            Event::SoftBreak | Event::HardBreak => self.push_text_literal("\n"),
            Event::Rule => self.push_text_literal("───\n\n"),
            Event::FootnoteReference(label) => self.push_escaped_text_budgeted(&label),
            Event::TaskListMarker(checked) => {
                self.push_text_literal(if checked { "[x] " } else { "[ ] " });
            }
        }
    }

    fn would_exceed_depth(&self, tag: &Tag<'_>) -> bool {
        let adds_depth = matches!(
            tag,
            Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::List(_)
                | Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Link { .. }
        );
        adds_depth && self.state.nesting_depth() + 1 > self.budget.max_nesting_depth as usize
    }

    fn suspend_open_blockquotes(&mut self) -> u8 {
        let count = self.state.open_blockquote_count().min(u8::MAX as usize) as u8;
        if count == 0 {
            return 0;
        }
        for _ in 0..count {
            self.push_html_literal("</blockquote>");
        }
        self.state
            .open_blocks
            .retain(|block| !matches!(block, BlockContext::BlockQuote));
        self.state.suspended_blockquotes = self.state.suspended_blockquotes.saturating_add(count);
        count
    }

    fn pop_blockquote(&mut self) -> bool {
        if self
            .state
            .open_blocks
            .last()
            .is_some_and(|block| matches!(block, BlockContext::BlockQuote))
        {
            let _ = self.state.open_blocks.pop();
            true
        } else {
            false
        }
    }

    fn pop_pre_code(&mut self) -> bool {
        if self
            .state
            .open_blocks
            .last()
            .is_some_and(|block| matches!(block, BlockContext::PreCode))
        {
            let _ = self.state.open_blocks.pop();
            true
        } else {
            false
        }
    }
}

struct RenderedTableRow {
    html: String,
    plain: String,
    column_count: u8,
    is_header: bool,
}

impl RenderedTableRow {
    fn html_units(&self) -> usize {
        self.html.encode_utf16().count()
    }
}

fn render_table_row(events: &[Event<'_>]) -> RenderedTableRow {
    let mut html = String::new();
    let mut plain = String::new();
    let mut column_count = 0u8;
    let mut is_header = false;
    let mut links: Vec<(String, usize)> = Vec::new();

    for event in events {
        match event {
            Event::Start(Tag::TableHead) => {
                is_header = true;
                html.push_str("| ");
                plain.push_str("| ");
            }
            Event::Start(Tag::TableRow) => {
                html.push_str("| ");
                plain.push_str("| ");
            }
            Event::Start(Tag::TableCell) => {
                if column_count > 0 {
                    html.push_str(" | ");
                    plain.push_str(" | ");
                }
                column_count = column_count.saturating_add(1);
            }
            Event::Start(Tag::Strong) => html.push_str("<b>"),
            Event::Start(Tag::Emphasis) => html.push_str("<i>"),
            Event::Start(Tag::Strikethrough) => html.push_str("<s>"),
            Event::Start(Tag::Link { dest_url, .. }) => {
                let href = dest_url.to_string();
                if !href.is_empty() {
                    html.push_str("<a href=\"");
                    push_escaped_attr(&mut html, &href);
                    html.push_str("\">");
                }
                links.push((href, plain.len()));
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                html.push_str(" |\n");
                plain.push_str(" |\n");
            }
            Event::End(TagEnd::Strong) => html.push_str("</b>"),
            Event::End(TagEnd::Emphasis) => html.push_str("</i>"),
            Event::End(TagEnd::Strikethrough) => html.push_str("</s>"),
            Event::End(TagEnd::Link) => {
                if let Some((href, plain_start)) = links.pop() {
                    if !href.is_empty() {
                        html.push_str("</a>");
                        if plain.len() == plain_start {
                            let _ = write!(plain, "({href})");
                        } else {
                            let _ = write!(plain, " ({href})");
                        }
                    }
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                push_escaped_text(&mut html, text);
                plain.push_str(text);
            }
            Event::Code(code) => {
                html.push_str("<code>");
                push_escaped_text(&mut html, code);
                html.push_str("</code>");
                plain.push_str(code);
            }
            Event::SoftBreak | Event::HardBreak => {
                html.push('\n');
                plain.push('\n');
            }
            Event::FootnoteReference(label)
            | Event::InlineMath(label)
            | Event::DisplayMath(label) => {
                push_escaped_text(&mut html, label);
                plain.push_str(label);
            }
            Event::TaskListMarker(checked) => {
                let marker = if *checked { "[x] " } else { "[ ] " };
                html.push_str(marker);
                plain.push_str(marker);
            }
            Event::Rule | Event::Start(_) | Event::End(_) => {}
        }
    }

    RenderedTableRow {
        html,
        plain,
        column_count,
        is_header,
    }
}

impl RendererState {
    fn current_html_units(&self) -> usize {
        self.current_html.encode_utf16().count()
    }

    fn at_safe_flush_point(&self) -> bool {
        self.open_inlines.is_empty() && !self.table_state.as_ref().is_some_and(|table| table.in_row)
    }

    fn in_code_block(&self) -> bool {
        self.open_blocks
            .iter()
            .any(|block| matches!(block, BlockContext::PreCode))
    }

    fn open_blockquote_count(&self) -> usize {
        self.open_blocks
            .iter()
            .filter(|block| matches!(block, BlockContext::BlockQuote))
            .count()
    }

    fn nesting_depth(&self) -> usize {
        self.open_blocks.len() + self.open_inlines.len() + self.list_stack.len()
    }

    fn is_suppressing(&self) -> bool {
        self.suppressed_depth > 0
    }

    fn open_inline_run_start(&self) -> Option<(usize, usize)> {
        self.open_inlines
            .iter()
            .filter_map(|inline| match inline {
                InlineContext::Bold {
                    html_start,
                    plain_start,
                }
                | InlineContext::Italic {
                    html_start,
                    plain_start,
                }
                | InlineContext::Strike {
                    html_start,
                    plain_start,
                }
                | InlineContext::Link {
                    html_start,
                    plain_start,
                    ..
                }
                | InlineContext::Image {
                    html_start,
                    plain_start,
                    ..
                } => Some((*html_start, *plain_start)),
                InlineContext::Code => None,
            })
            .min_by_key(|(html_start, plain_start)| (*html_start, *plain_start))
    }

    fn open_inline_image_projection(&self, pending: &str) -> Option<String> {
        self.open_inlines.iter().rev().find_map(|inline| {
            let InlineContext::Image {
                dest_url,
                plain_start,
                ..
            } = inline
            else {
                return None;
            };
            let mut alt = self.current_plain[*plain_start..].to_string();
            alt.push_str(pending);
            let alt = alt.trim();
            Some(if alt.is_empty() {
                format!("({dest_url})")
            } else {
                format!("[image: {alt}] ({dest_url})")
            })
        })
    }

    fn open_inline_link_href(&self) -> Option<&str> {
        self.open_inlines.iter().find_map(|inline| match inline {
            InlineContext::Link { href, .. } if !href.is_empty() => Some(href.as_str()),
            _ => None,
        })
    }
}

fn close_inline_tag(inline: &InlineContext) -> &'static str {
    match inline {
        InlineContext::Bold { .. } => "</b>",
        InlineContext::Italic { .. } => "</i>",
        InlineContext::Strike { .. } => "</s>",
        InlineContext::Code => "</code>",
        InlineContext::Link { .. } => "</a>",
        InlineContext::Image { .. } => "",
    }
}

fn open_block_tag(block: &BlockContext) -> &'static str {
    match block {
        BlockContext::BlockQuote => "<blockquote>",
        BlockContext::PreCode => "<pre><code>",
    }
}

fn close_block_tag(block: &BlockContext) -> &'static str {
    match block {
        BlockContext::BlockQuote => "</blockquote>",
        BlockContext::PreCode => "</code></pre>",
    }
}

fn best_escaped_text_split(text: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    let mut hard_split = 0usize;
    for (idx, ch) in text.char_indices() {
        let cost = escaped_text_char_units(ch);
        if units + cost > max_units {
            break;
        }
        units += cost;
        hard_split = idx + ch.len_utf8();
    }
    if hard_split == 0 {
        return text.chars().next().map(char::len_utf8).unwrap_or(0);
    }

    let prefix = &text[..hard_split];
    if let Some((idx, _)) = prefix.match_indices('\n').next_back() {
        return idx + 1;
    }
    if let Some((idx, ch)) = prefix
        .char_indices()
        .rev()
        .find(|(idx, ch)| *idx > 0 && ch.is_whitespace())
    {
        return idx + ch.len_utf8();
    }
    hard_split
}

fn escaped_text_units(text: &str) -> usize {
    text.chars().map(escaped_text_char_units).sum()
}

fn escaped_text_char_units(ch: char) -> usize {
    match ch {
        '&' => 5,
        '<' | '>' => 4,
        _ => ch.len_utf16(),
    }
}

fn escaped_attr_units(text: &str) -> usize {
    text.chars()
        .map(|ch| match ch {
            '&' => 5,
            '<' | '>' => 4,
            '"' => 6,
            _ => ch.len_utf16(),
        })
        .sum()
}

fn chunk_text(text: &str) -> String {
    text.trim_end_matches('\n').to_string()
}

fn plain_chunk_text(text: &str, code_ranges: &[Range<usize>]) -> String {
    let text = chunk_text(text);
    if code_ranges.is_empty() {
        return scrub_plain_markers(&text);
    }

    let mut output = String::new();
    let mut cursor = 0usize;
    for range in code_ranges {
        let start = range.start.min(text.len());
        let end = range.end.min(text.len());
        if cursor < start {
            output.push_str(&scrub_plain_markers(&text[cursor..start]));
        }
        if start < end {
            output.push_str(&text[start..end]);
        }
        cursor = cursor.max(end);
    }
    if cursor < text.len() {
        output.push_str(&scrub_plain_markers(&text[cursor..]));
    }
    output
}

fn scrub_plain_markers(text: &str) -> String {
    text.replace("```", "")
        .replace("**", "")
        .replace("~~", "")
        .replace("__", "")
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

fn push_plain_projection_text(output: &mut String, text: &str) {
    output.push_str(&scrub_plain_markers(text));
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
