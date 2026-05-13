//! QuerySource for free-text agent-command args (v2 PR-3).
//!
//! Drives the picker for any agent-advertised slash command whose
//! `AvailableCommand.input == Some(Unstructured(...))` — e.g. codex's
//! `/review`, `/review-branch`, `/review-commit`. Wire-confirmed against
//! codex-acp 0.12.0 in `crates/spur-acp/tests/codex_0_12_wire_probe.rs`.
//!
//! The picker is informational: it surfaces the agent-advertised hint as a
//! placeholder/title and confirms submit through `ReplaceTriggerToken`.
//!
//! ## Anchor / replacement contract (gemini #1 critical-fix)
//!
//! `InputCompletionPort::apply_accept` re-anchors the `ReplaceTriggerToken`
//! to `trigger_detector.current_prefix_start()`, which for `SlashArg` is the
//! byte offset *after* `/<cmd> ` (the start of the arg region — see
//! `parse_slash_arg_prefix` in `completion_trigger.rs`). The picker-supplied
//! `prefix_start` is therefore a sentinel the port ignores; the `replacement`
//! must be the **arg text only**, not `/<cmd> <arg>`. Returning the canonical
//! form here would cause the port to splice it onto the *arg-region anchor*,
//! producing duplicated buffers like `/review-branch /review-branch main`.
//! This mirrors `ConfigOptionQuerySource::accept` which returns just `value`.

use super::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

pub struct CommandInputQuerySource {
    pub command: String,
    pub free_text_hint: String,
    /// Most recent query string passed to `refresh`. Used by `accept` to
    /// build the replacement.
    last_query: String,
}

impl CommandInputQuerySource {
    pub fn new(command: String, free_text_hint: String) -> Self {
        Self {
            command,
            free_text_hint,
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
            selectable: true,
            dimmed: false,
        }]
    }

    fn accept(&self, _row_idx: usize) -> Option<RetrievalAccept> {
        // The InputCompletionPort re-anchors to the arg-region byte offset,
        // so the replacement is the arg text only. The `/<cmd> ` prefix
        // stays in the buffer untouched (it was preserved by the anchor).
        // `prefix_start: 0` is a sentinel the port ignores.
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: 0,
            replacement: self.last_query.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CommandInputQuerySource {
        CommandInputQuerySource::new("review-branch".to_string(), "branch name".to_string())
    }

    #[test]
    fn title_is_advertised_hint() {
        let src = fixture();
        assert_eq!(src.title(), "branch name");
    }

    #[test]
    fn title_falls_back_when_hint_empty() {
        let src = CommandInputQuerySource::new("review".to_string(), String::new());
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

    /// Gemini #1 regression: accept must return ONLY the arg text. The
    /// InputCompletionPort re-anchors the replacement to the arg region, so
    /// returning `/<cmd> <query>` would duplicate the prefix.
    #[test]
    fn accept_returns_arg_text_only_not_full_canonical_form() {
        let mut src = fixture();
        let _ = src.refresh("main");
        let accept = src.accept(0).expect("synthetic row always exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken { replacement, .. } => {
                assert_eq!(
                    replacement, "main",
                    "replacement must be ONLY the arg \
                     (port re-anchors to arg-region byte offset)"
                );
            }
            other => panic!("expected ReplaceTriggerToken, got {other:?}"),
        }
    }

    #[test]
    fn accept_with_empty_query_yields_empty_replacement() {
        let src = fixture();
        let accept = src.accept(0).expect("synthetic row always exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken { replacement, .. } => {
                assert_eq!(replacement, "");
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
