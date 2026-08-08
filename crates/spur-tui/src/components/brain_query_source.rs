//! Brain picker source for the local `/brain` command.
//!
//! Snapshots brain-capable agents from a `BrainPickerOpen` event and returns
//! a submitted slash command on accept so selection follows the same route as
//! typing `/brain <name>`.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use spur_acp::{AgentKind, BrainInfo};

use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrainChoice {
    name: String,
    kind_tag: String,
    is_default: bool,
    active: bool,
}

pub struct BrainQuerySource {
    choices: Vec<BrainChoice>,
    last_picked: Vec<BrainChoice>,
    matcher: Matcher,
}

impl BrainQuerySource {
    pub fn new(brains: Vec<BrainInfo>, active: &str) -> Self {
        let choices: Vec<BrainChoice> = brains
            .into_iter()
            .map(|b| BrainChoice {
                name: b.name.clone(),
                kind_tag: agent_kind_tag(b.kind).to_string(),
                is_default: b.is_default,
                active: b.name == active,
            })
            .collect();
        let last_picked = choices.clone();
        Self {
            choices,
            last_picked,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }
}

fn agent_kind_tag(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeStreamJson => "claude-stream-json",
        AgentKind::ClaudeCodeAcp => "claude-code-acp",
        AgentKind::CodexAcp => "codex-acp",
        AgentKind::Kiro => "kiro",
        AgentKind::Kimi => "kimi",
        AgentKind::Gemini => "gemini",
        AgentKind::OpenCode => "opencode",
        AgentKind::Grok => "grok",
        AgentKind::Generic => "generic",
    }
}

impl BrainChoice {
    fn primary(&self) -> String {
        if self.active {
            format!("* {}", self.name)
        } else {
            self.name.clone()
        }
    }

    fn secondary(&self) -> String {
        let mut parts = Vec::new();
        if self.active {
            parts.push("active");
        }
        if self.is_default {
            parts.push("default");
        }
        parts.join(", ")
    }
}

impl QuerySource for BrainQuerySource {
    fn title(&self) -> &str {
        "Brain"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let picked: Vec<BrainChoice> = if query.is_empty() {
            self.choices.clone()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, BrainChoice)> = self
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
                secondary: choice.secondary(),
                tag: choice.kind_tag.clone(),
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
            text: format!("/brain {name}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::AgentKind;

    fn fixture() -> BrainQuerySource {
        BrainQuerySource::new(
            vec![
                BrainInfo {
                    name: "grok".into(),
                    kind: AgentKind::Grok,
                    is_default: true,
                },
                BrainInfo {
                    name: "codex".into(),
                    kind: AgentKind::CodexAcp,
                    is_default: false,
                },
                BrainInfo {
                    name: "opencode".into(),
                    kind: AgentKind::OpenCode,
                    is_default: false,
                },
            ],
            "grok",
        )
    }

    #[test]
    fn refresh_empty_query_returns_all_and_marks_active() {
        let mut src = fixture();
        let rows = src.refresh("");

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| row.primary == "* grok"));
        assert!(rows.iter().any(|row| row.secondary.contains("default")));
        assert!(rows.iter().any(|row| row.tag == "codex-acp"));
    }

    #[test]
    fn refresh_filters_against_brain_name_without_active_marker() {
        let mut src = fixture();
        let rows = src.refresh("code");

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.primary == "codex"));
        assert!(rows.iter().any(|row| row.primary == "opencode"));
    }

    #[test]
    fn accept_returns_brain_slash_command_for_filtered_row() {
        let mut src = fixture();
        let _ = src.refresh("open");

        let accept = src.accept(0).unwrap();

        match accept {
            RetrievalAccept::SubmitText { text } => assert_eq!(text, "/brain opencode"),
            other => panic!("expected SubmitText, got {other:?}"),
        }
    }
}
