use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use super::MAX_LOG_ENTRIES;

/// What kind of ReAct trace step this entry represents.
#[derive(Debug, Clone)]
pub enum TraceKind {
    Think,
    Act { tool: String, args: String },
    Observe,
    Delegate { agent: String, task: String, status: String },
    UserMessage,
    Permission { description: String, pending: bool, countdown: u8 },
}

/// A single entry in the full ReAct trace.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub kind: TraceKind,
    pub text: String,
    pub timestamp: String,
}

/// Spinner frames for delegation animation.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct ReactTrace {
    entries: Vec<TraceEntry>,
    scroll_offset: usize,
    is_following: bool,
    tick_counter: u8,
}

impl ReactTrace {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll_offset: 0,
            is_following: true,
            tick_counter: 0,
        }
    }

    /// Push a new trace entry, evicting oldest if over capacity, and
    /// auto-scroll to bottom when following.
    pub fn push(&mut self, entry: TraceEntry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            self.scroll_offset = self.scroll_offset.saturating_sub(drain);
        }
        if self.is_following {
            self.scroll_to_bottom();
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        self.is_following = false;
    }

    pub fn scroll_down(&mut self, visible_height: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
        if self.scroll_offset >= self.entries.len().saturating_sub(visible_height) {
            self.is_following = true;
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.is_following = false;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.entries.len().saturating_sub(1);
        self.is_following = true;
    }

    /// Called on each tick: advance spinner counter and decrement pending
    /// permission countdowns.
    pub fn tick(&mut self) {
        self.tick_counter = self.tick_counter.wrapping_add(1);

        for entry in &mut self.entries {
            if let TraceKind::Permission {
                pending,
                countdown,
                ..
            } = &mut entry.kind
            {
                if *pending && *countdown > 0 {
                    *countdown = countdown.saturating_sub(1);
                }
            }
        }
    }

    /// Returns true if any entry has a pending permission request.
    pub fn has_pending_permission(&self) -> bool {
        self.entries.iter().any(|e| {
            matches!(
                &e.kind,
                TraceKind::Permission { pending: true, .. }
            )
        })
    }

    /// Render the full ReAct trace into the given frame area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let block = Block::default()
            .title(" Session ")
            .title_bottom(following_indicator)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let spinner_frame = SPINNER_FRAMES
            [(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];

        let mut lines: Vec<Line> = Vec::new();

        for entry in &self.entries {
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(Color::DarkGray),
            );

            match &entry.kind {
                TraceKind::Think => {
                    // Header line: timestamp + "🧠 THINK"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "🧠 THINK",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines indented 3 spaces
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                text_line.to_string(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }

                TraceKind::Act { tool, args } => {
                    // Header line: timestamp + "🔧 ACT  {tool}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("🔧 ACT  {}", tool),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Args below if present
                    if !args.is_empty() {
                        for arg_line in args.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    arg_line.to_string(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                    // Entry text (e.g. additional description)
                    if !entry.text.is_empty() {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                }

                TraceKind::Observe => {
                    // Header line: timestamp + "👁 OBSERVE"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "👁 OBSERVE",
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines indented
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                text_line.to_string(),
                                Style::default().fg(Color::Green),
                            ),
                        ]));
                    }
                }

                TraceKind::Delegate { agent, task, status } => {
                    // Header line: timestamp + "→ DELEGATE to {agent}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("→ DELEGATE to {}", agent),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Task line
                    if !task.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                task.as_str(),
                                Style::default().fg(Color::Cyan),
                            ),
                        ]));
                    }
                    // Status with spinner if active
                    if !status.is_empty() {
                        let is_active = status == "running" || status == "active" || status == "delegated";
                        let status_text = if is_active {
                            format!("   {} {}", spinner_frame, status)
                        } else {
                            format!("   {}", status)
                        };
                        lines.push(Line::from(vec![Span::styled(
                            status_text,
                            Style::default().fg(Color::Cyan),
                        )]));
                    }
                }

                TraceKind::UserMessage => {
                    // Header line: timestamp + "💬 YOU"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            "💬 YOU",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Body lines
                    for text_line in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("   "),
                            Span::styled(
                                text_line.to_string(),
                                Style::default().fg(Color::Yellow),
                            ),
                        ]));
                    }
                }

                TraceKind::Permission {
                    description,
                    pending,
                    countdown,
                } => {
                    // Header line: timestamp + "⚠ PERMISSION: {description}"
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("⚠ PERMISSION: {}", description),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    // Key hints + optional countdown
                    if *pending {
                        let hint_text = if *countdown > 0 {
                            format!("   [y]es [n]o [a]lways  (auto-deny in {}s)", countdown)
                        } else {
                            "   [y]es [n]o [a]lways".to_string()
                        };
                        lines.push(Line::from(vec![Span::styled(
                            hint_text,
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::RAPID_BLINK),
                        )]));
                    }
                    // Body text if any
                    if !entry.text.is_empty() {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                }
            }

            // Blank separator between entries
            lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset as u16, 0));

        frame.render_widget(paragraph, area);
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

impl Default for ReactTrace {
    fn default() -> Self {
        Self::new()
    }
}
