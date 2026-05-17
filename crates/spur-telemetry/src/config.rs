use std::path::{Path, PathBuf};

use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub version: u32,
    pub anonymous_id: Uuid,
    pub tier1_crash: bool,
    pub tier1_perf: bool,
    pub tier2_usage: bool,
    pub last_consent_prompt_at: Option<DateTime<Utc>>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            anonymous_id: Uuid::new_v4(),
            tier1_crash: true,
            tier1_perf: true,
            tier2_usage: false,
            last_consent_prompt_at: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return config_path_for_root(&home);
    }
    config_path_for_root(Path::new("."))
}

fn config_path_for_root(root: &Path) -> PathBuf {
    root.join(".spur").join("telemetry.toml")
}

pub fn load_or_default() -> TelemetryConfig {
    load_or_default_at(&config_path())
}

fn load_or_default_at(path: &Path) -> TelemetryConfig {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TelemetryConfig::default()
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed reading telemetry config; using defaults"
            );
            return TelemetryConfig::default();
        }
    };

    let parsed: TelemetryConfig = match toml::from_str(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed parsing telemetry config; using defaults"
            );
            return TelemetryConfig::default();
        }
    };

    if parsed.version != SCHEMA_VERSION {
        tracing::warn!(
            path = %path.display(),
            file_version = parsed.version,
            expected_version = SCHEMA_VERSION,
            "unknown telemetry config schema version; using defaults"
        );
        return TelemetryConfig::default();
    }

    parsed
}

pub fn save_atomic(cfg: &TelemetryConfig) -> Result<()> {
    save_atomic_at(&config_path(), cfg)
}

fn save_atomic_at(path: &Path, cfg: &TelemetryConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension("toml.tmp");
    let encoded = toml::to_string_pretty(cfg)?;
    std::fs::write(&tmp_path, encoded)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        config_path_for_root, load_or_default_at, save_atomic_at, TelemetryConfig, SCHEMA_VERSION,
    };
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn default_values_enable_tier1_and_disable_tier2() {
        let cfg = TelemetryConfig::default();
        assert!(cfg.tier1_crash);
        assert!(cfg.tier1_perf);
        assert!(!cfg.tier2_usage);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = tempdir().expect("create tempdir");
        let path = config_path_for_root(root.path());

        let cfg = TelemetryConfig {
            version: SCHEMA_VERSION,
            anonymous_id: Uuid::new_v4(),
            tier1_crash: true,
            tier1_perf: false,
            tier2_usage: true,
            last_consent_prompt_at: Some(Utc.with_ymd_and_hms(2026, 5, 15, 8, 30, 0).unwrap()),
        };

        save_atomic_at(&path, &cfg).expect("save config");
        let loaded = load_or_default_at(&path);
        assert_eq!(loaded.version, cfg.version);
        assert_eq!(loaded.anonymous_id, cfg.anonymous_id);
        assert_eq!(loaded.tier1_crash, cfg.tier1_crash);
        assert_eq!(loaded.tier1_perf, cfg.tier1_perf);
        assert_eq!(loaded.tier2_usage, cfg.tier2_usage);
        assert_eq!(loaded.last_consent_prompt_at, cfg.last_consent_prompt_at);
    }

    #[test]
    fn corrupt_file_returns_defaults() {
        let root = tempdir().expect("create tempdir");
        let path = config_path_for_root(root.path());
        std::fs::create_dir_all(path.parent().expect("parent dir")).expect("create .spur");
        std::fs::write(&path, "not valid toml").expect("write corrupt file");

        let loaded = load_or_default_at(&path);
        let defaults = TelemetryConfig::default();
        assert_eq!(loaded.version, defaults.version);
        assert_eq!(loaded.tier1_crash, defaults.tier1_crash);
        assert_eq!(loaded.tier1_perf, defaults.tier1_perf);
        assert_eq!(loaded.tier2_usage, defaults.tier2_usage);
    }

    #[test]
    fn unknown_schema_version_returns_defaults() {
        let root = tempdir().expect("create tempdir");
        let path = config_path_for_root(root.path());
        std::fs::create_dir_all(path.parent().expect("parent dir")).expect("create .spur");
        let content = r#"
version = 99
anonymous_id = "e34d3859-2b90-4f43-88f8-a8df94911356"
tier1_crash = false
tier1_perf = false
tier2_usage = true
"#;
        std::fs::write(&path, content).expect("write config");

        let loaded = load_or_default_at(&path);
        let defaults = TelemetryConfig::default();
        assert_eq!(loaded.version, defaults.version);
        assert_eq!(loaded.tier1_crash, defaults.tier1_crash);
        assert_eq!(loaded.tier1_perf, defaults.tier1_perf);
        assert_eq!(loaded.tier2_usage, defaults.tier2_usage);
    }
}
