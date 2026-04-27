//! QuerySource for free-text agent-command args (v2 PR-3).
//!
//! Drives the picker for any agent-advertised slash command whose
//! `AvailableCommand.input == Some(Unstructured(...))` — e.g. codex's
//! `/review`, `/review-branch`, `/review-commit`. Wire-confirmed against
//! codex-acp 0.12.0 in `crates/spur-acp/tests/codex_0_12_wire_probe.rs`.
//!
//! The picker is informational: it surfaces the agent-advertised hint as a
//! placeholder/title and confirms submit through `ReplaceTriggerToken`. The
//! actual arg text already lives in the InputBar (the user typed it after
//! `/<cmd> `), so `accept` re-emits the canonical `/<cmd> <query>` form.

use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

pub struct CommandInputQuerySource {
    pub command: String,
    pub free_text_hint: String,
    /// Byte offset in the `InputBar` text where the trigger's `/` lives.
    /// Captured at shell-open time so `accept` can replace `[prefix_start..cursor]`
    /// with the canonical `/<cmd> <query>` form.
    prefix_start: usize,
    /// Most recent query string passed to `refresh`. Used by `accept` to
    /// build the replacement.
    last_query: String,
}

impl CommandInputQuerySource {
    pub fn new(command: String, free_text_hint: String, prefix_start: usize) -> Self {
        Self {
            command,
            free_text_hint,
            prefix_start,
            last_query: String::new(),
        }
    }
}

impl QuerySource for CommandInputQuerySource {
    fn title(&self) -> &str {
        if self.free_text_hint.is_empty() {
            // Empty advertised hint — fall back to a generic placeholder so
            // the picker header isn't blank.
            "<arg>"
        } else {
            &self.free_text_hint
        }
    }

    fn query_mode(&self) -> QueryMode {
        // The query lives in the InputBar (everything after `/<cmd> `), so
        // ReadFromInputBar mirrors how /slash and @mention work.
        QueryMode::ReadFromInputBar
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        self.last_query = query.to_string();
        // One synthetic confirmation row. Visual feedback that the picker is
        // active and what will be submitted. Mirrors v1's effort/model picker
        // shape (one row per choice; here there's just one "Submit" choice).
        let primary = if query.is_empty() {
            format!("Submit /{}", self.command)
        } else {
            format!("Submit /{} {}", self.command, query)
        };
        let secondary = if self.free_text_hint.is_empty() {
            String::new()
        } else {
            self.free_text_hint.clone()
        };
        vec![RetrievalRow {
            primary,
            secondary,
            tag: String::new(),
            atoms: Vec::new(),
        }]
    }

    fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
        // ReplaceTriggerToken canonicalises the buffer to "/<cmd> <query>".
        // Idempotent when the user typed the buffer that way; corrects spacing
        // glitches if not. The InputBar dispatches submit on the next Enter
        // through the existing PromptText path.
        let replacement = if self.last_query.is_empty() {
            format!("/{}", self.command)
        } else {
            format!("/{} {}", self.command, self.last_query)
        };
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: self.prefix_start,
            replacement,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CommandInputQuerySource {
        CommandInputQuerySource::new("review-branch".to_string(), "branch name".to_string(), 7)
    }

    #[test]
    fn title_is_advertised_hint() {
        let src = fixture();
        assert_eq!(src.title(), "branch name");
    }

    #[test]
    fn title_falls_back_when_hint_empty() {
        let src = CommandInputQuerySource::new("review".to_string(), String::new(), 0);
        assert_eq!(src.title(), "<arg>");
    }

    #[test]
    fn query_mode_reads_from_input_bar() {
        let src = fixture();
        assert_eq!(src.query_mode(), QueryMode::ReadFromInputBar);
    }

    #[test]
    fn refresh_returns_one_synthetic_row_capturing_query() {
        let mut src = fixture();
        let rows = src.refresh("main");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].primary.contains("/review-branch"));
        assert!(rows[0].primary.contains("main"));
    }

    #[test]
    fn refresh_empty_query_yields_command_only_submit() {
        let mut src = fixture();
        let rows = src.refresh("");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Submit /review-branch");
    }

    #[test]
    fn accept_after_query_returns_canonical_replacement() {
        let mut src = fixture();
        let _ = src.refresh("main");
        let accept = src.accept(0).expect("synthetic row always exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                assert_eq!(prefix_start, 7);
                assert_eq!(replacement, "/review-branch main");
            }
            other => panic!("expected ReplaceTriggerToken, got {other:?}"),
        }
    }

    #[test]
    fn accept_with_no_refresh_yields_command_only() {
        let src = fixture();
        let accept = src.accept(0).expect("synthetic row always exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                assert_eq!(prefix_start, 7);
                assert_eq!(replacement, "/review-branch");
            }
            other => panic!("expected ReplaceTriggerToken, got {other:?}"),
        }
    }

    #[test]
    fn refresh_secondary_carries_hint_when_present() {
        let mut src = fixture();
        let rows = src.refresh("main");
        assert_eq!(rows[0].secondary, "branch name");
    }
}
