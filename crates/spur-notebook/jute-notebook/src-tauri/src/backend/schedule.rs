//! Per-cell schedule metadata persisted in notebook cell metadata.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a scheduled fire runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunTarget {
    /// Run only the armed cell.
    CellOnly,
    /// Run the armed cell and cascade downstream (default).
    #[default]
    Cascade,
}

/// Persisted per-cell cron trigger config (`cell.metadata.spur.cron`).
///
/// Run history/last-run/next-fire are not stored here; they live in the
/// in-memory scheduler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct CellCronTrigger {
    /// Whether the schedule is currently armed.
    pub enabled: bool,
    /// 5-field `Unix` cron expression, such as every 15 minutes.
    pub cron: String,
    /// `IANA` timezone name, for example `America/Los_Angeles`.
    pub timezone: String,
    /// Cell-only vs cascade. Defaults to cascade.
    #[serde(default)]
    pub run_target: RunTarget,
    /// Skip a fire if the previous run is still going. Defaults true.
    #[serde(default = "default_true")]
    pub skip_if_running: bool,
    /// Back-fill a window that elapsed while `SPUR` was closed. Defaults false.
    #[serde(default)]
    pub catch_up: bool,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_roundtrip() {
        let t = CellCronTrigger {
            enabled: true,
            cron: "*/15 * * * *".to_string(),
            timezone: "America/Los_Angeles".to_string(),
            run_target: RunTarget::Cascade,
            skip_if_running: true,
            catch_up: false,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"run_target\":\"cascade\""));
        let back: CellCronTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn schedule_roundtrip_run_target_defaults_to_cascade() {
        let de: CellCronTrigger =
            serde_json::from_str(r#"{"enabled":true,"cron":"0 6 * * *","timezone":"UTC"}"#)
                .unwrap();
        assert_eq!(de.run_target, RunTarget::Cascade);
        assert!(de.skip_if_running);
        assert!(!de.catch_up);
    }
}
