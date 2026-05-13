//! Synthesizes CommandEntry rows from an agent's cached config_options.
//! Vendor-neutral; calls into spur-acp's config-option synthesizers.

use spur_acp::adapter::config_options::{synthesize, synthesize_advertised, AdvertisedCommand};
use spur_acp::{SessionConfigOption, SpurAgentCaps};

use super::entry::{CommandEntry, CommandSource, Dispatch};

pub struct AdvertisedSource;

impl AdvertisedSource {
    /// Build CommandEntry rows from frozen per-session capabilities.
    pub fn entries_from_caps(handle: &str, caps: &SpurAgentCaps) -> Vec<CommandEntry> {
        synthesize_advertised(caps)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_string(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect()
    }

    /// Build CommandEntry rows from cached config_options. Each entry's
    /// `arg_picker_spec` is set from the synthesizer output.
    pub fn entries(handle: &str, opts: &[SessionConfigOption]) -> Vec<CommandEntry> {
        synthesize(opts)
            .into_iter()
            .map(|adv: AdvertisedCommand| CommandEntry {
                name: adv.name,
                description: adv.description,
                hint: adv.hint,
                source: CommandSource::Advertised {
                    handle: handle.to_string(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: adv.config_id,
                },
                arg_picker_spec: Some(adv.arg_picker_spec),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion, SessionId,
        SessionModelState,
    };
    use spur_acp::{
        AgentKind, SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SpurAgentCaps,
    };

    #[test]
    fn empty_options_yield_empty_entries() {
        assert!(AdvertisedSource::entries("codex", &[]).is_empty());
    }

    #[test]
    fn allowlisted_option_yields_advertised_entry() {
        let opt = SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "label".to_string(),
            "gpt-5-codex".to_string(),
            vec![SessionConfigSelectOption::new(
                "gpt-5-codex".to_string(),
                "GPT-5 Codex".to_string(),
            )],
        );
        let entries = AdvertisedSource::entries("codex", &[opt]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
        assert!(matches!(
            entries[0].source,
            CommandSource::Advertised { ref handle } if handle == "codex"
        ));
        assert!(matches!(
            entries[0].dispatch,
            Dispatch::SetSessionConfigOption { ref config_id } if config_id == "model"
        ));
        assert!(entries[0].arg_picker_spec.is_some());
    }

    #[test]
    fn model_caps_yield_model_advertised_entry() {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let new = NewSessionResponse::new(SessionId::new("sid")).models(SessionModelState::new(
            ModelId::new("gemini-3.1-pro-preview"),
            vec![ModelInfo::new(
                ModelId::new("gemini-3.1-pro-preview"),
                "Gemini 3.1 Pro Preview",
            )],
        ));
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Gemini);
        let entries = AdvertisedSource::entries_from_caps("gemini", &caps);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "model");
    }
}
