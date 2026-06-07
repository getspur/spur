//! Vendor-neutral arg-picker descriptors derived from agent-advertised data
//! (`config_options` for v1 synthetic commands; `AvailableCommand.input` + `_meta`
//! for v2 advertised commands). Consumed by spur-tui without ACP-schema
//! imports — spur-tui sees only the types defined here.

use agent_client_protocol::schema::{AvailableCommand, AvailableCommandInput};

/// Parse an `AvailableCommand` into an `ArgPickerSpec`.
///
/// Returns `None` when the command has no input region (i.e. no-arg slash
/// command — `cmd.input.is_none()`). Returns `Some(spec)` for `Unstructured`
/// input with the advertised hint as `free_text_hint`.
///
/// PR-3 implements only the free-text case: `typed_hint` is always `None`.
/// PR-4 will extend this to read `cmd.meta._<vendor>.dev.arg_picker_hint`
/// for typed pickers (`GitRef`, `FilePath`, etc.).
pub fn parse(cmd: &AvailableCommand) -> Option<ArgPickerSpec> {
    match cmd.input.as_ref()? {
        AvailableCommandInput::Unstructured(u) => Some(ArgPickerSpec {
            free_text_hint: u.hint.clone(),
            typed_hint: None,
        }),
        // `AvailableCommandInput` is `#[non_exhaustive]`; future ACP additions
        // (typed GitRef, FilePath, etc.) fall through to "no picker" until
        // spur grows a matching `QuerySource`. The user can still submit the
        // raw text — it's just routed via the existing PromptText path.
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgPickerSpec {
    /// Hint string for picker placeholder. Empty when the source is a typed
    /// select with no free-text fallback (e.g. v1 `ConfigOption` commands).
    pub free_text_hint: String,
    /// If Some, the picker uses a typed query source. None means free-text.
    pub typed_hint: Option<ArgPickerHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgPickerHint {
    /// v1: picker reads choices from the agent's cached `SessionConfigOption`
    /// select for the given `config_id`.
    ConfigOption { config_id: String },
    // v2 will add: GitRef { kind: GitRefKind }, etc.
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        AvailableCommand, AvailableCommandInput, UnstructuredCommandInput,
    };

    #[test]
    fn parse_returns_none_for_no_input() {
        let cmd = AvailableCommand::new("init", "create AGENTS.md");
        assert_eq!(parse(&cmd), None);
    }

    #[test]
    fn parse_unstructured_input_yields_free_text_spec() {
        let cmd = AvailableCommand::new("review-branch", "Review branch").input(
            AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("branch name")),
        );
        let spec = parse(&cmd).expect("Unstructured input must yield a spec");
        assert_eq!(spec.free_text_hint, "branch name");
        assert!(
            spec.typed_hint.is_none(),
            "PR-3 reads only Unstructured.hint; PR-4 will add _meta typed_hint"
        );
    }

    #[test]
    fn parse_unstructured_with_empty_hint() {
        let cmd = AvailableCommand::new("review", "Review changes").input(
            AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("")),
        );
        let spec = parse(&cmd).expect("empty hint still yields a spec — picker shows placeholder");
        assert_eq!(spec.free_text_hint, "");
        assert!(spec.typed_hint.is_none());
    }

    #[test]
    fn config_option_spec_has_no_free_text_fallback() {
        let spec = ArgPickerSpec {
            free_text_hint: String::new(),
            typed_hint: Some(ArgPickerHint::ConfigOption {
                config_id: "model".into(),
            }),
        };
        assert!(spec.free_text_hint.is_empty());
        assert!(matches!(
            spec.typed_hint,
            Some(ArgPickerHint::ConfigOption { config_id }) if config_id == "model"
        ));
    }

    #[test]
    fn arg_picker_hint_equality() {
        let a = ArgPickerHint::ConfigOption {
            config_id: "model".into(),
        };
        let b = ArgPickerHint::ConfigOption {
            config_id: "model".into(),
        };
        let c = ArgPickerHint::ConfigOption {
            config_id: "reasoning_effort".into(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
