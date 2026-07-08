pub(crate) fn model_name_from_config_value(
    value: Option<&str>,
    fallback: &'static str,
) -> spur_telemetry::tier1_events::ModelName {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return spur_telemetry::tier1_events::ModelName::Other(fallback);
    };

    match value {
        "claude-opus-4-7" => spur_telemetry::tier1_events::ModelName::ClaudeOpus47,
        "claude-opus-4-6" => spur_telemetry::tier1_events::ModelName::ClaudeOpus46,
        "claude-opus-4-5" => spur_telemetry::tier1_events::ModelName::ClaudeOpus45,
        "claude-sonnet-4-7" => spur_telemetry::tier1_events::ModelName::ClaudeSonnet47,
        "claude-sonnet-4-6" => spur_telemetry::tier1_events::ModelName::ClaudeSonnet46,
        "claude-sonnet-4-5" => spur_telemetry::tier1_events::ModelName::ClaudeSonnet45,
        "claude-haiku-4-5" => spur_telemetry::tier1_events::ModelName::ClaudeHaiku45,
        "gpt-5" => spur_telemetry::tier1_events::ModelName::Gpt5,
        "gpt-5-codex" => spur_telemetry::tier1_events::ModelName::Gpt5Codex,
        "gpt-4o" => spur_telemetry::tier1_events::ModelName::Gpt4o,
        "gpt-4o-mini" => spur_telemetry::tier1_events::ModelName::Gpt4oMini,
        "gemini-2.5-pro" => spur_telemetry::tier1_events::ModelName::Gemini25Pro,
        "gemini-2.5-flash" => spur_telemetry::tier1_events::ModelName::Gemini25Flash,
        _ => spur_telemetry::tier1_events::ModelName::Other(fallback),
    }
}

pub(crate) fn current_model_config_value(
    options: &[spur_acp::SessionConfigOption],
) -> Option<&str> {
    let option = options
        .iter()
        .find(|option| {
            matches!(
                option.category.as_ref(),
                Some(spur_acp::SessionConfigOptionCategory::Model)
            )
        })
        .or_else(|| {
            options
                .iter()
                .find(|option| option.category.is_none() && option.id.0.as_ref() == "model")
        })?;

    let spur_acp::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let current = select.current_value.0.as_ref();
    if current.is_empty() {
        None
    } else {
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use spur_telemetry::tier1_events::ModelName;

    #[test]
    fn model_name_from_config_value_maps_known_model_ids() {
        assert_eq!(
            super::model_name_from_config_value(Some("gpt-5-codex"), "unknown"),
            ModelName::Gpt5Codex
        );
        assert_eq!(
            super::model_name_from_config_value(Some("gemini-2.5-pro"), "unknown"),
            ModelName::Gemini25Pro
        );
        assert_eq!(
            super::model_name_from_config_value(Some("claude-sonnet-4-7"), "unknown"),
            ModelName::ClaudeSonnet47
        );
    }

    #[test]
    fn model_name_from_config_value_uses_static_fallback_for_unknown_or_missing_values() {
        assert_eq!(
            super::model_name_from_config_value(Some("vendor-new-model"), "worker"),
            ModelName::Other("worker")
        );
        assert_eq!(
            super::model_name_from_config_value(None, "unknown"),
            ModelName::Other("unknown")
        );
    }
}
