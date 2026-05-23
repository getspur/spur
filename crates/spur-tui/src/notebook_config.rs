use std::path::Path;

use serde::Deserialize;

pub const DEFAULT_CHAT_RESPONSE_CHAR_CAP: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotebookUiConfig {
    pub chat_response_char_cap: usize,
}

impl Default for NotebookUiConfig {
    fn default() -> Self {
        Self {
            chat_response_char_cap: DEFAULT_CHAT_RESPONSE_CHAR_CAP,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct NotebookConfigFile {
    ui: Option<NotebookUiSection>,
}

#[derive(Debug, Default, Deserialize)]
struct NotebookUiSection {
    chat_response_char_cap: Option<usize>,
}

impl NotebookUiConfig {
    pub fn load_from_config_path(config_path: Option<&Path>) -> Self {
        let repo_root = config_path
            .and_then(|path| path.parent())
            .and_then(|spur_dir| spur_dir.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());

        repo_root
            .as_deref()
            .map(Self::load_from_repo_root)
            .unwrap_or_default()
    }

    pub fn load_from_repo_root(repo_root: &Path) -> Self {
        let path = repo_root.join(".spur").join("notebook.toml");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let parsed: NotebookConfigFile = match toml::from_str(&contents) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    path = %path.display(),
                    "failed to parse notebook UI config; using defaults"
                );
                return Self::default();
            }
        };
        let mut config = Self::default();
        if let Some(cap) = parsed
            .ui
            .and_then(|ui| ui.chat_response_char_cap)
            .filter(|cap| *cap > 0)
        {
            config.chat_response_char_cap = cap;
        }
        config
    }
}
