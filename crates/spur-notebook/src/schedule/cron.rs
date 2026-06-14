use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;

#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("invalid cron expression: {0}")]
    Parse(String),
    #[error("invalid timezone: {0}")]
    Timezone(String),
    #[error("could not compute next occurrence")]
    NoOccurrence,
}

/// Compute the next `n` fire instants in UTC, strictly after `after_utc`,
/// evaluating the 5-field cron expression in `tz_name`.
pub fn next_fires(
    expr: &str,
    tz_name: &str,
    after_utc: DateTime<Utc>,
    n: usize,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let tz: Tz = tz_name
        .parse()
        .map_err(|_| CronError::Timezone(tz_name.to_string()))?;
    let cron = Cron::new(expr)
        .parse()
        .map_err(|err| CronError::Parse(err.to_string()))?;

    let mut cursor = after_utc.with_timezone(&tz);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let next = cron
            .find_next_occurrence(&cursor, false)
            .map_err(|_| CronError::NoOccurrence)?;
        out.push(next.with_timezone(&Utc));
        cursor = next;
    }

    Ok(out)
}

/// Plain-English description for known presets. Unknown patterns are custom.
pub fn describe(expr: &str) -> String {
    match expr.trim() {
        "*/5 * * * *" => "Every 5 minutes",
        "*/10 * * * *" => "Every 10 minutes",
        "*/15 * * * *" => "Every 15 minutes",
        "*/30 * * * *" => "Every 30 minutes",
        "0 * * * *" => "Every hour, on the hour",
        "0 */2 * * *" => "Every 2 hours",
        "0 6 * * *" => "Every day at 06:00",
        "0 0 * * *" => "Every day at midnight",
        "0 6 * * 1" => "Every Monday at 06:00",
        "0 9 * * 1-5" => "Weekdays at 09:00",
        _ => "Custom schedule",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn cron_next_fires_every_15m() {
        let after = Utc.with_ymd_and_hms(2026, 6, 14, 14, 11, 0).unwrap();
        let fires = next_fires("*/15 * * * *", "UTC", after, 3).unwrap();
        assert_eq!(fires.len(), 3);
        assert_eq!(
            fires[0],
            Utc.with_ymd_and_hms(2026, 6, 14, 14, 15, 0).unwrap()
        );
        assert_eq!(
            fires[1],
            Utc.with_ymd_and_hms(2026, 6, 14, 14, 30, 0).unwrap()
        );
        assert_eq!(
            fires[2],
            Utc.with_ymd_and_hms(2026, 6, 14, 14, 45, 0).unwrap()
        );
    }

    #[test]
    fn cron_invalid_is_error() {
        assert!(next_fires("not a cron", "UTC", Utc::now(), 1).is_err());
    }

    #[test]
    fn cron_describe_presets() {
        assert_eq!(describe("*/15 * * * *"), "Every 15 minutes");
        assert_eq!(describe("0 6 * * *"), "Every day at 06:00");
        assert_eq!(describe("13 4 * * 2"), "Custom schedule");
    }
}
