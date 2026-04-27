//! Synthesizes CommandEntry rows from an agent's cached config_options.
//! Vendor-neutral; calls into spur-acp's synthesize() function.

use spur_acp::adapter::config_options::{synthesize, AdvertisedCommand};
use spur_acp::SessionConfigOption;

use super::entry::{CommandEntry, CommandSource, Dispatch};

pub struct AdvertisedSource;

impl AdvertisedSource {
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
    use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

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
}
