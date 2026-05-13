//! QuerySource that pulls choices from cached SessionConfigOption select.
//! Used by v1 /model and /effort pickers. Static (snapshot at open).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use spur_acp::adapter::config_options::AdvertisedChoice;

use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

pub struct ConfigOptionQuerySource {
    pub command: String,
    pub config_id: String,
    pub choices: Vec<AdvertisedChoice>,
    /// The most recent set of choices returned by `refresh`, in display order.
    /// `accept(row_idx)` indexes THIS, not `choices` — otherwise picking after
    /// a fuzzy filter would return the wrong value (codex review feedback).
    last_picked: Vec<AdvertisedChoice>,
    matcher: Matcher,
}

impl ConfigOptionQuerySource {
    pub fn new(command: String, config_id: String, choices: Vec<AdvertisedChoice>) -> Self {
        let last_picked = choices.clone();
        Self {
            command,
            config_id,
            choices,
            last_picked,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }
}

impl QuerySource for ConfigOptionQuerySource {
    fn title(&self) -> &str {
        if self.command == "model" {
            "Model"
        } else if self.command == "effort" {
            "Effort"
        } else {
            &self.command
        }
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let picked: Vec<AdvertisedChoice> = if query.is_empty() {
            self.choices.clone()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, AdvertisedChoice)> = self
                .choices
                .iter()
                .filter_map(|c| {
                    buf.clear();
                    let score =
                        pattern.score(Utf32Str::new(&c.label, &mut buf), &mut self.matcher)?;
                    Some((score, c.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().map(|(_, c)| c).collect()
        };
        let rows: Vec<RetrievalRow> = picked
            .iter()
            .map(|c| RetrievalRow {
                primary: c.label.clone(),
                secondary: c.description.clone().unwrap_or_default(),
                tag: c.value.clone(),
                atoms: Vec::new(),
                selectable: true,
                dimmed: false,
            })
            .collect();
        self.last_picked = picked;
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let value = self.last_picked.get(row_idx)?.value.clone();
        // prefix_start is a placeholder; InputCompletionPort re-anchors using the
        // SlashArg trigger state. `replacement` carries only the value — the
        // /<cmd> prefix stays in the buffer (Option A from plan §10.2).
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: 0,
            replacement: value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ConfigOptionQuerySource {
        ConfigOptionQuerySource::new(
            "model".to_string(),
            "model".to_string(),
            vec![
                AdvertisedChoice {
                    value: "gpt-5-codex".into(),
                    label: "GPT-5 Codex".into(),
                    description: None,
                },
                AdvertisedChoice {
                    value: "gpt-5".into(),
                    label: "GPT-5".into(),
                    description: None,
                },
                AdvertisedChoice {
                    value: "o4-mini".into(),
                    label: "o4-mini".into(),
                    description: None,
                },
            ],
        )
    }

    #[test]
    fn refresh_empty_query_returns_all() {
        let mut src = fixture();
        let rows = src.refresh("");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn refresh_filters_by_query() {
        let mut src = fixture();
        let rows = src.refresh("gpt");
        assert!(rows.iter().any(|r| r.primary.contains("GPT-5")));
        assert!(!rows.iter().any(|r| r.primary == "o4-mini"));
    }

    #[test]
    fn accept_returns_value_only() {
        let src = fixture();
        let accept = src.accept(0).unwrap();
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                ref replacement, ..
            } => {
                assert_eq!(replacement, "gpt-5-codex");
            }
            _ => panic!("expected ReplaceTriggerToken"),
        }
    }

    #[test]
    fn accept_out_of_range_returns_none() {
        let src = fixture();
        assert!(src.accept(99).is_none());
    }

    /// Regression for codex review #1: accept(row_idx) must index the
    /// filtered/sorted display rows, not the original choices list.
    #[test]
    fn accept_after_filter_uses_filtered_row_order() {
        let mut src = fixture();
        let _ = src.refresh("o4");
        // After filtering by "o4", the only matching row is "o4-mini".
        // accept(0) MUST return "o4-mini", not "gpt-5-codex" (choices[0]).
        let accept = src.accept(0).unwrap();
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                ref replacement, ..
            } => {
                assert_eq!(replacement, "o4-mini");
            }
            _ => panic!("expected ReplaceTriggerToken"),
        }
    }

    #[test]
    fn title_renames_known_commands() {
        let src = fixture();
        assert_eq!(src.title(), "Model");
        let src2 =
            ConfigOptionQuerySource::new("effort".into(), "reasoning_effort".into(), Vec::new());
        assert_eq!(src2.title(), "Effort");
        let src3 = ConfigOptionQuerySource::new("custom".into(), "custom".into(), Vec::new());
        assert_eq!(src3.title(), "custom");
    }
}
