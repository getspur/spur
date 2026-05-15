use crate::events::{Event, IntoProp, Props, Tier};
use crate::redact::bucket_model;

pub use crate::redact::PanicType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelName {
    ClaudeOpus47,
    ClaudeOpus46,
    ClaudeOpus45,
    ClaudeSonnet47,
    ClaudeSonnet46,
    ClaudeSonnet45,
    ClaudeHaiku45,
    Gpt5,
    Gpt5Codex,
    Gpt4o,
    Gpt4oMini,
    Gemini25Pro,
    Gemini25Flash,
    Other(&'static str),
}

impl crate::events::sealed::Sealed for ModelName {}
impl IntoProp for ModelName {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            ModelName::ClaudeOpus47 => "claude-opus-4-7",
            ModelName::ClaudeOpus46 => "claude-opus-4-6",
            ModelName::ClaudeOpus45 => "claude-opus-4-5",
            ModelName::ClaudeSonnet47 => "claude-sonnet-4-7",
            ModelName::ClaudeSonnet46 => "claude-sonnet-4-6",
            ModelName::ClaudeSonnet45 => "claude-sonnet-4-5",
            ModelName::ClaudeHaiku45 => "claude-haiku-4-5",
            ModelName::Gpt5 => "gpt-5",
            ModelName::Gpt5Codex => "gpt-5-codex",
            ModelName::Gpt4o => "gpt-4o",
            ModelName::Gpt4oMini => "gpt-4o-mini",
            ModelName::Gemini25Pro => "gemini-2.5-pro",
            ModelName::Gemini25Flash => "gemini-2.5-flash",
            ModelName::Other(name) => bucket_model(name),
        };
        value.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Timeout,
    Error,
}

impl crate::events::sealed::Sealed for Outcome {}
impl IntoProp for Outcome {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            Outcome::Ok => "ok",
            Outcome::Timeout => "timeout",
            Outcome::Error => "error",
        };
        value.into()
    }
}

impl crate::events::sealed::Sealed for PanicType {}
impl IntoProp for PanicType {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            PanicType::Bounds => "bounds",
            PanicType::Unwrap => "unwrap",
            PanicType::OptionUnwrap => "option_unwrap",
            PanicType::ResultUnwrap => "result_unwrap",
            PanicType::Assertion => "assertion",
            PanicType::Other => "other",
        };
        value.into()
    }
}

pub struct SessionStarted {
    pub os: &'static str,
    pub arch: &'static str,
    pub spur_version: &'static str,
    pub is_tui: bool,
}

impl Event for SessionStarted {
    const NAME: &'static str = "session_started";
    const TIER: Tier = Tier::One;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("os", self.os.into_prop());
        props.insert("arch", self.arch.into_prop());
        props.insert("spur_version", self.spur_version.into_prop());
        props.insert("is_tui", self.is_tui.into_prop());
        props
    }
}

pub struct LlmRequestDuration {
    pub model_name: ModelName,
    pub duration_ms: u64,
    pub token_count_bucket: u32,
    pub outcome: Outcome,
}

impl Event for LlmRequestDuration {
    const NAME: &'static str = "llm_request_duration";
    const TIER: Tier = Tier::One;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("model_name", self.model_name.into_prop());
        props.insert("duration_ms", self.duration_ms.into_prop());
        props.insert("token_count_bucket", self.token_count_bucket.into_prop());
        props.insert("outcome", self.outcome.into_prop());
        props
    }
}

pub struct McpRequestDuration {
    pub duration_ms: u64,
    pub outcome: Outcome,
}

impl Event for McpRequestDuration {
    const NAME: &'static str = "mcp_request_duration";
    const TIER: Tier = Tier::One;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("duration_ms", self.duration_ms.into_prop());
        props.insert("outcome", self.outcome.into_prop());
        props
    }
}

pub struct AcpRequestDuration {
    pub duration_ms: u64,
    pub outcome: Outcome,
}

impl Event for AcpRequestDuration {
    const NAME: &'static str = "acp_request_duration";
    const TIER: Tier = Tier::One;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("duration_ms", self.duration_ms.into_prop());
        props.insert("outcome", self.outcome.into_prop());
        props
    }
}

pub struct TuiFrameSlow {
    pub duration_ms: u64,
}

impl Event for TuiFrameSlow {
    const NAME: &'static str = "tui_frame_slow";
    const TIER: Tier = Tier::One;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("duration_ms", self.duration_ms.into_prop());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(props: &Props) -> Vec<&'static str> {
        props.keys().copied().collect()
    }

    #[test]
    fn session_started_into_props_shape() {
        let props = SessionStarted {
            os: "macos",
            arch: "aarch64",
            spur_version: "1.2.3",
            is_tui: true,
        }
        .into_props();

        assert_eq!(keys(&props), vec!["arch", "is_tui", "os", "spur_version"]);
        assert!(props["os"].is_string());
        assert!(props["arch"].is_string());
        assert!(props["spur_version"].is_string());
        assert!(props["is_tui"].is_boolean());
    }

    #[test]
    fn llm_request_duration_into_props_shape() {
        let props = LlmRequestDuration {
            model_name: ModelName::Other("claude-foo-bar"),
            duration_ms: 1200,
            token_count_bucket: 300,
            outcome: Outcome::Ok,
        }
        .into_props();

        assert_eq!(
            keys(&props),
            vec!["duration_ms", "model_name", "outcome", "token_count_bucket"]
        );
        assert!(props["model_name"].is_string());
        assert_eq!(props["model_name"], "anthropic_other");
        assert!(props["duration_ms"].is_number());
        assert!(props["token_count_bucket"].is_number());
        assert!(props["outcome"].is_string());
    }

    #[test]
    fn mcp_request_duration_into_props_shape() {
        let props = McpRequestDuration {
            duration_ms: 85,
            outcome: Outcome::Timeout,
        }
        .into_props();

        assert_eq!(keys(&props), vec!["duration_ms", "outcome"]);
        assert!(props["duration_ms"].is_number());
        assert!(props["outcome"].is_string());
        assert!(!props.contains_key("server_name"));
        assert!(!props.contains_key("tool_name"));
    }

    #[test]
    fn acp_request_duration_into_props_shape() {
        let props = AcpRequestDuration {
            duration_ms: 12,
            outcome: Outcome::Error,
        }
        .into_props();

        assert_eq!(keys(&props), vec!["duration_ms", "outcome"]);
        assert!(props["duration_ms"].is_number());
        assert!(props["outcome"].is_string());
    }

    #[test]
    fn tui_frame_slow_into_props_shape() {
        let props = TuiFrameSlow { duration_ms: 40 }.into_props();

        assert_eq!(keys(&props), vec!["duration_ms"]);
        assert!(props["duration_ms"].is_number());
    }

    #[test]
    fn outcome_into_prop_values() {
        assert_eq!(Outcome::Ok.into_prop(), "ok");
        assert_eq!(Outcome::Timeout.into_prop(), "timeout");
        assert_eq!(Outcome::Error.into_prop(), "error");
    }

    #[test]
    fn panic_type_into_prop_values() {
        assert_eq!(PanicType::Bounds.into_prop(), "bounds");
        assert_eq!(PanicType::Unwrap.into_prop(), "unwrap");
        assert_eq!(PanicType::OptionUnwrap.into_prop(), "option_unwrap");
        assert_eq!(PanicType::ResultUnwrap.into_prop(), "result_unwrap");
        assert_eq!(PanicType::Assertion.into_prop(), "assertion");
        assert_eq!(PanicType::Other.into_prop(), "other");
    }

    #[test]
    fn model_name_into_prop_uses_bucket_model() {
        assert_eq!(ModelName::Gpt5.into_prop(), "gpt-5");
        assert_eq!(
            ModelName::Other("gpt-experimental-x").into_prop(),
            "openai_other"
        );
    }
}
