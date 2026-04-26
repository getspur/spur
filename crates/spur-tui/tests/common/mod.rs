#![allow(dead_code)]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use tokio::sync::mpsc;

use spur_tui::{app::App, UserInput};

pub struct TestHarness {
    app: App,
    terminal: Terminal<TestBackend>,
    user_input_rx: mpsc::Receiver<UserInput>,
}

impl TestHarness {
    pub fn new(width: u16, height: u16) -> Self {
        let (app, user_input_rx) = spur_tui::test_support::app_with_user_input_tx();
        let terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        Self {
            app,
            terminal,
            user_input_rx,
        }
    }

    pub fn send_key(&mut self, code: KeyCode) {
        self.send_key_with_mods(code, KeyModifiers::NONE);
    }

    pub fn send_key_with_mods(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.app
            .handle_crossterm_event_for_test(KeyEvent::new(code, mods));
    }

    pub fn type_text(&mut self, s: &str) {
        for ch in s.chars() {
            self.send_key(KeyCode::Char(ch));
        }
    }

    pub fn send_paste(&mut self, text: &str) {
        self.app
            .handle_crossterm_event(Event::Paste(text.to_string()));
    }

    pub fn send_resize(&mut self, width: u16, height: u16) {
        self.app
            .handle_crossterm_event(Event::Resize(width, height));
    }

    pub fn render(&mut self) {
        self.terminal.draw(|f| self.app.render(f)).unwrap();
    }

    pub fn buffer(&self) -> &Buffer {
        self.terminal.backend().buffer()
    }

    pub fn buffer_lines(&self) -> Vec<String> {
        buffer_to_lines(self.buffer())
    }

    pub fn buffer_text(&self) -> String {
        self.buffer_lines().join("\n")
    }

    pub fn row_text(&self, y: u16) -> String {
        row_text(self.buffer(), y)
    }

    pub fn take_actions(&mut self) -> Vec<UserInput> {
        let mut out = Vec::new();
        while let Ok(action) = self.user_input_rx.try_recv() {
            out.push(action);
        }
        out
    }

    pub fn last_action(&mut self) -> Option<UserInput> {
        self.take_actions().pop()
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

pub fn buffer_to_lines(buf: &Buffer) -> Vec<String> {
    let mut out = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        out.push(row_text(buf, y).trim_end().to_string());
    }
    out
}

pub fn row_text(buf: &Buffer, y: u16) -> String {
    let mut row = String::new();
    for x in 0..buf.area.width {
        row.push_str(buf[(x, y)].symbol());
    }
    row
}

pub fn buffer_text(buf: &Buffer) -> String {
    buffer_to_lines(buf).join("\n")
}

pub fn assert_no_vertical_border_glyphs(buf: &Buffer, width: u16, height: u16) {
    assert_no_glyphs(buf, width, height, &["│"]);
}

pub fn assert_no_glyphs(buf: &Buffer, width: u16, height: u16, glyphs: &[&str]) {
    for y in 0..height {
        for x in 0..width {
            let cell = buf.cell((x, y)).expect("cell should be inside buffer");
            assert!(
                !glyphs.iter().any(|glyph| cell.symbol() == *glyph),
                "unexpected glyph {:?} at ({x}, {y})",
                cell.symbol()
            );
        }
    }
}
