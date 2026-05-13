use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::components::picker_shell::{token, truncate_preview_lines_to_fit};
use crate::components::query_source::{CodeFilePreview, CodeSymbolPreview, RetrievalPreview};
use crate::components::snippet::SnippetState;
use crate::theme::Theme;

pub struct PickerPopover<'a> {
    pub preview: &'a RetrievalPreview,
    pub theme: &'a Theme,
}

impl PickerPopover<'_> {
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        match self.preview {
            RetrievalPreview::Text { title, lines } => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(token(self.theme, "picker.match.fg")))
                    .title(Span::styled(
                        format!(" {title} "),
                        Style::default().fg(token(self.theme, "picker.match.fg")),
                    ));
                let inner = block.inner(area);
                let body_rows = inner.height as usize;
                let body_width = inner.width as usize;
                let lines =
                    truncate_preview_lines_to_fit(lines.clone(), body_rows, body_width, self.theme);
                frame.render_widget(
                    Paragraph::new(lines)
                        .block(block)
                        .wrap(Wrap { trim: false }),
                    area,
                );
            }
            RetrievalPreview::CodeSymbol(symbol) => self.render_code_symbol(frame, area, symbol),
            RetrievalPreview::CodeFile(file) => self.render_code_file(frame, area, file),
        }
    }

    fn render_code_symbol(&self, frame: &mut Frame, area: Rect, symbol: &CodeSymbolPreview) {
        let icon = symbol_kind_icon(&symbol.symbol_kind);
        let file_base = std::path::Path::new(&symbol.file_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(symbol.file_path.as_str());
        let title = format!(
            " {icon} {} ─ {}:{} ",
            symbol.entity_name, file_base, symbol.line_range[0]
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(self.theme, "picker.match.fg")))
            .title(Span::styled(
                title,
                Style::default().fg(token(self.theme, "picker.match.fg")),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let scope = symbol.enclosing_scope.as_deref().unwrap_or("-");
        let meta = format!(
            "{scope}    kind {}  lines {}-{}    graph_index {}",
            symbol.symbol_kind,
            symbol.line_range[0],
            symbol.line_range[1],
            symbol.graph_index_version
        );
        let meta_line = truncate_with_ellipsis(&meta, inner.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                meta_line,
                Style::default().fg(token(self.theme, "picker.row.fg")),
            ))),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );

        if inner.height >= 2 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "─".repeat(inner.width as usize),
                    Style::default().fg(token(self.theme, "picker.hint.fg")),
                ))),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }

        if inner.height <= 2 {
            return;
        }

        let body_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
        match &symbol.snippet {
            SnippetState::Ready(lines) => {
                let body = truncate_preview_lines_to_fit(
                    lines.clone(),
                    body_area.height as usize,
                    body_area.width as usize,
                    self.theme,
                );
                frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), body_area);
            }
            SnippetState::Failed(reason) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("(snippet unavailable: {reason})"),
                        Style::default()
                            .fg(token(self.theme, "picker.hint.fg"))
                            .add_modifier(Modifier::DIM),
                    ))),
                    body_area,
                );
            }
        }
    }

    fn render_code_file(&self, frame: &mut Frame, area: Rect, file: &CodeFilePreview) {
        let title = format!(" 📄 {} ", file.file_path);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(self.theme, "picker.match.fg")))
            .title(Span::styled(
                title,
                Style::default().fg(token(self.theme, "picker.match.fg")),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_with_ellipsis(
                    &format!("file mention · graph_index {}", file.graph_index_version),
                    inner.width as usize,
                ),
                Style::default()
                    .fg(token(self.theme, "picker.hint.fg"))
                    .add_modifier(Modifier::DIM),
            ))),
            inner,
        );
    }
}

fn symbol_kind_icon(kind: &str) -> &'static str {
    match kind {
        "fn" => "ƒ",
        "impl" => "■",
        "struct" => "▢",
        "enum" => "◇",
        _ => "·",
    }
}

fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let width = text.chars().count();
    if width <= max_width {
        return text.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let keep = max_width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::picker_shell::PickerShell;
    use crate::components::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).expect("cell").symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn find_text_start(
        buffer: &Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        let needle_chars: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
        if needle_chars.is_empty() {
            return None;
        }
        for y in 0..height {
            for x in 0..width.saturating_sub(needle_chars.len() as u16 - 1) {
                let matches = needle_chars.iter().enumerate().all(|(offset, expected)| {
                    buffer
                        .cell((x + offset as u16, y))
                        .is_some_and(|cell| cell.symbol() == expected)
                });
                if matches {
                    return Some((x, y));
                }
            }
        }
        None
    }

    fn code_symbol_preview(snippet: SnippetState) -> RetrievalPreview {
        RetrievalPreview::CodeSymbol(CodeSymbolPreview {
            entity_name: "handle_key".to_string(),
            symbol_kind: "fn".to_string(),
            file_path: "crates/spur-tui/src/components/picker_shell.rs".to_string(),
            line_range: [117, 167],
            enclosing_scope: Some("impl PickerShell".to_string()),
            graph_index_version: "spur-graph-phase2".to_string(),
            snippet,
        })
    }

    #[test]
    fn renders_code_symbol_ready_snippet() {
        let preview = code_symbol_preview(SnippetState::Ready(vec![Line::raw(
            "pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {",
        )]));
        let backend = TestBackend::new(120, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                PickerPopover {
                    preview: &preview,
                    theme: crate::theme::fallback_theme(),
                }
                .render(f, Rect::new(0, 0, 120, 14));
            })
            .expect("draw");

        let text = buffer_text(terminal.backend().buffer(), 120, 14);
        assert!(text.contains("ƒ handle_key"), "{text}");
        assert!(text.contains("kind fn  lines 117-167"), "{text}");
        assert!(text.contains("pub fn handle_key"), "{text}");
    }

    #[test]
    fn renders_text_preview() {
        let preview = RetrievalPreview::Text {
            title: "bd-1234".to_string(),
            lines: vec![Line::raw("Issue title here")],
        };
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                PickerPopover {
                    preview: &preview,
                    theme: crate::theme::fallback_theme(),
                }
                .render(f, Rect::new(0, 0, 80, 10));
            })
            .expect("draw");

        let text = buffer_text(terminal.backend().buffer(), 80, 10);
        assert!(text.contains(" bd-1234 "), "{text}");
        assert!(text.contains("Issue title here"), "{text}");
    }

    #[test]
    fn renders_code_symbol_failed_snippet() {
        let preview = code_symbol_preview(SnippetState::Failed("unreadable".to_string()));
        let backend = TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                PickerPopover {
                    preview: &preview,
                    theme: crate::theme::fallback_theme(),
                }
                .render(f, Rect::new(0, 0, 100, 12));
            })
            .expect("draw");

        let text = buffer_text(terminal.backend().buffer(), 100, 12);
        assert!(text.contains("(snippet unavailable: unreadable)"), "{text}");
    }

    #[test]
    fn renders_code_file_preview() {
        let preview = RetrievalPreview::CodeFile(CodeFilePreview {
            file_path: "crates/spur-tui/src/views/session_detail.rs".to_string(),
            graph_index_version: "spur-graph-phase2".to_string(),
        });
        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                PickerPopover {
                    preview: &preview,
                    theme: crate::theme::fallback_theme(),
                }
                .render(f, Rect::new(0, 0, 100, 8));
            })
            .expect("draw");

        let text = buffer_text(terminal.backend().buffer(), 100, 8);
        assert!(text.contains("session_detail.rs"), "{text}");
        assert!(text.contains("graph_index spur-graph-phase2"), "{text}");
    }

    struct CodeSymbolSource;

    impl QuerySource for CodeSymbolSource {
        fn title(&self) -> &str {
            "Mentions · @"
        }

        fn query_mode(&self) -> QueryMode {
            QueryMode::ReadFromInputBar
        }

        fn refresh(&mut self, _query: &str) -> Vec<RetrievalRow> {
            vec![RetrievalRow {
                primary: "ƒ @handle_key".to_string(),
                secondary: "picker_shell.rs:117".to_string(),
                tag: "code-symbol".to_string(),
                atoms: Vec::new(),
                selectable: true,
                dimmed: false,
            }]
        }

        fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
            None
        }

        fn preview_for(&self, row_idx: usize) -> Option<RetrievalPreview> {
            (row_idx == 0)
                .then(|| code_symbol_preview(SnippetState::Ready(vec![Line::raw("fn x() {}")])))
        }
    }

    fn render_shell(width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut shell = PickerShell::open(Box::new(CodeSymbolSource));
        terminal
            .draw(|f| {
                let anchor = Rect::new(width / 4, height - 1, width.saturating_sub(width / 4), 1);
                let container = Rect::new(0, 0, width, height);
                shell.render(f, anchor, container, crate::theme::fallback_theme());
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn picker_shell_renders_popover_to_right_when_wide() {
        let width = 160;
        let height = 40;
        let buffer = render_shell(width, height);
        let popover_title =
            find_text_start(&buffer, width, height, "graph_index spur-graph-phase2")
                .expect("popover metadata");
        let row = find_text_start(&buffer, width, height, "@handle_key").expect("row");
        assert!(
            popover_title.0 > row.0,
            "title={popover_title:?} row={row:?}"
        );
    }

    #[test]
    fn picker_shell_narrow_layout_stacks_above_or_suppresses() {
        let width = 60;
        let height = 40;
        let buffer = render_shell(width, height);
        let row = find_text_start(&buffer, width, height, "@handle_key").expect("row");
        if let Some(snippet) = find_text_start(&buffer, width, height, "fn x() {}") {
            assert!(snippet.1 < row.1, "snippet={snippet:?} row={row:?}");
        }
    }

    // Manual verification (for PR description):
    // 1. scripts/spur-cargo run -p spur-tui
    // 2. Ensure workspace has .spur/graph-index.json with code-symbol entries.
    // 3. In input, type @handle_key and move selection over code rows.
    // 4. Verify popover behavior at terminal widths 160, 120, 90, and 60.
}
