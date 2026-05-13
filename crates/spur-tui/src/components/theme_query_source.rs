//! Theme picker source for the local `/theme` command.
//!
//! The source snapshots available themes at open time and returns a submitted
//! slash command on accept so selection follows the same route as typing
//! `/theme <name>`.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::theme::AvailableThemes;

use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeChoice {
    name: String,
    source: &'static str,
    active: bool,
}

pub struct ThemeQuerySource {
    choices: Vec<ThemeChoice>,
    last_picked: Vec<ThemeChoice>,
    matcher: Matcher,
}

impl ThemeQuerySource {
    pub fn new(available: AvailableThemes, active: &str) -> Self {
        let mut choices = Vec::new();
        choices.extend(
            available
                .built_in
                .into_iter()
                .map(|name| ThemeChoice::new(name, "built-in", active)),
        );
        choices.extend(
            available
                .project
                .into_iter()
                .map(|name| ThemeChoice::new(name, "project", active)),
        );
        choices.extend(
            available
                .user
                .into_iter()
                .map(|name| ThemeChoice::new(name, "user", active)),
        );
        let last_picked = choices.clone();
        Self {
            choices,
            last_picked,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }
}

impl ThemeChoice {
    fn new(name: String, source: &'static str, active: &str) -> Self {
        let active = name == active;
        Self {
            name,
            source,
            active,
        }
    }

    fn primary(&self) -> String {
        if self.active {
            format!("* {}", self.name)
        } else {
            self.name.clone()
        }
    }
}

impl QuerySource for ThemeQuerySource {
    fn title(&self) -> &str {
        "Theme"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let picked: Vec<ThemeChoice> = if query.is_empty() {
            self.choices.clone()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, ThemeChoice)> = self
                .choices
                .iter()
                .filter_map(|choice| {
                    buf.clear();
                    let score =
                        pattern.score(Utf32Str::new(&choice.name, &mut buf), &mut self.matcher)?;
                    Some((score, choice.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().map(|(_, choice)| choice).collect()
        };
        let rows: Vec<RetrievalRow> = picked
            .iter()
            .map(|choice| RetrievalRow {
                primary: choice.primary(),
                secondary: if choice.active {
                    "active".to_string()
                } else {
                    String::new()
                },
                tag: choice.source.to_string(),
                atoms: Vec::new(),
                selectable: true,
                dimmed: false,
            })
            .collect();
        self.last_picked = picked;
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let name = self.last_picked.get(row_idx)?.name.clone();
        Some(RetrievalAccept::SubmitText {
            text: format!("/theme {name}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ThemeQuerySource {
        ThemeQuerySource::new(
            AvailableThemes {
                built_in: vec![
                    "dark".to_string(),
                    "light".to_string(),
                    "high-contrast".to_string(),
                ],
                project: vec!["solarized".to_string()],
                user: vec!["midnight".to_string()],
            },
            "light",
        )
    }

    #[test]
    fn refresh_empty_query_returns_all_sources_and_marks_active() {
        let mut src = fixture();
        let rows = src.refresh("");

        assert_eq!(rows.len(), 5);
        assert!(rows.iter().any(|row| row.primary == "* light"));
        assert!(rows.iter().any(|row| row.tag == "project"));
        assert!(rows.iter().any(|row| row.tag == "user"));
    }

    #[test]
    fn refresh_filters_against_theme_name_without_active_marker() {
        let mut src = fixture();
        let rows = src.refresh("light");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "* light");
    }

    #[test]
    fn accept_returns_theme_slash_command_for_filtered_row() {
        let mut src = fixture();
        let _ = src.refresh("mid");

        let accept = src.accept(0).unwrap();

        match accept {
            RetrievalAccept::SubmitText { text } => assert_eq!(text, "/theme midnight"),
            other => panic!("expected SubmitText, got {other:?}"),
        }
    }
}
