//! Vendor-neutral arg-picker descriptors derived from agent-advertised data
//! (config_options for v1 synthetic commands; AvailableCommand.input + _meta
//! for v2 advertised commands). Consumed by spur-tui without ACP-schema
//! imports — spur-tui sees only the types defined here.

#[derive(Debug, Clone, PartialEq)]
pub struct ArgPickerSpec {
    /// Hint string for picker placeholder. Empty when the source is a typed
    /// select with no free-text fallback (e.g. v1 ConfigOption commands).
    pub free_text_hint: String,
    /// If Some, the picker uses a typed query source. None means free-text.
    pub typed_hint: Option<ArgPickerHint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArgPickerHint {
    /// v1: picker reads choices from the agent's cached SessionConfigOption
    /// select for the given config_id.
    ConfigOption { config_id: String },
    // v2 will add: GitRef { kind: GitRefKind }, etc.
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(ArgPickerHint::ConfigOption { ref config_id }) if config_id == "model"
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
