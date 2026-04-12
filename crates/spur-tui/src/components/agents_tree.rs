use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::AgentState;

const SPINNER_CHARS: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub struct AgentsTree {
    focused: bool,
    tick_counter: u8,
}

impl AgentsTree {
    pub fn new() -> Self {
        Self {
            focused: false,
            tick_counter: 0,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn tick(&mut self) {
        self.tick_counter = self.tick_counter.wrapping_add(1);
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, agents: &[AgentState]) {
        let border_style = if self.focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let block = Block::default()
            .title(" Agents ")
            .borders(Borders::ALL)
            .border_style(border_style);

        let now = Instant::now();
        let mut lines: Vec<Line> = Vec::new();

        // Separate roots (brains) from children (workers)
        let roots: Vec<&AgentState> = agents.iter().filter(|a| a.parent.is_none()).collect();
        let children: Vec<&AgentState> = agents.iter().filter(|a| a.parent.is_some()).collect();

        for root in &roots {
            lines.push(self.build_agent_line(root, "", &now));

            // Collect children belonging to this root
            let my_children: Vec<&&AgentState> = children
                .iter()
                .filter(|c| {
                    c.parent
                        .as_deref()
                        .map(|p| p == root.name)
                        .unwrap_or(false)
                })
                .collect();

            let child_count = my_children.len();
            for (i, child) in my_children.iter().enumerate() {
                let connector = if i + 1 == child_count {
                    "  └─ "
                } else {
                    "  ├─ "
                };
                lines.push(self.build_agent_line(child, connector, &now));
            }
        }

        // Render any orphan children (parent set but no matching root)
        let root_names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        for orphan in children
            .iter()
            .filter(|c| !root_names.contains(&c.parent.as_deref().unwrap_or("")))
        {
            lines.push(self.build_agent_line(orphan, "  └─ ", &now));
        }

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn build_agent_line<'a>(
        &self,
        agent: &'a AgentState,
        prefix: &'a str,
        now: &Instant,
    ) -> Line<'a> {
        // Spinner character
        let spinner = match agent.status.as_str() {
            "working" | "spawned" => SPINNER_CHARS[(self.tick_counter % 10) as usize],
            "idle" => '○',
            _ => '●',
        };

        // Status color
        let status_color = match agent.status.as_str() {
            "spawned" | "working" => Color::Green,
            "done" => Color::Blue,
            "error" => Color::Red,
            "rate-limited" => Color::Yellow,
            _ => Color::DarkGray,
        };

        // Elapsed time
        let elapsed_str = if let Some(started) = agent.started_at {
            let secs = now.duration_since(started).as_secs();
            let m = secs / 60;
            let s = secs % 60;
            format!("{}m {:02}s", m, s)
        } else {
            String::new()
        };

        // Cost
        let cost_str = if agent.cost > 0.0 {
            format!("${:.2}", agent.cost)
        } else {
            String::new()
        };

        // Role badge (uppercase)
        let role_badge = agent.role.to_uppercase();

        let mut spans: Vec<Span> = Vec::new();

        // Prefix connector (for children)
        if !prefix.is_empty() {
            spans.push(Span::styled(
                prefix.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Spinner
        spans.push(Span::styled(
            format!("{} ", spinner),
            Style::default().fg(status_color),
        ));

        // Name (padded to 12)
        spans.push(Span::styled(
            format!("{:<12} ", agent.name),
            Style::default().fg(Color::White),
        ));

        // Role badge
        spans.push(Span::styled(
            format!("{} ", role_badge),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::DIM),
        ));

        // Status (padded to 12)
        spans.push(Span::styled(
            format!("{:<12} ", agent.status),
            Style::default().fg(status_color),
        ));

        // Elapsed
        if !elapsed_str.is_empty() {
            spans.push(Span::styled(
                format!("{} ", elapsed_str),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Cost
        if !cost_str.is_empty() {
            spans.push(Span::styled(
                cost_str,
                Style::default().fg(Color::Yellow),
            ));
        }

        Line::from(spans)
    }
}
