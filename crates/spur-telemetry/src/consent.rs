use std::env;

use crate::config::TelemetryConfig;
use crate::events::Tier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Crash,
    Perf,
    Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consent {
    pub crash: bool,
    pub perf: bool,
    pub usage: bool,
}

impl Consent {
    fn none() -> Self {
        Self {
            crash: false,
            perf: false,
            usage: false,
        }
    }
}

pub fn resolve(cfg: &TelemetryConfig) -> Consent {
    if telemetry_env_disables() || ci_env_disables() {
        return Consent::none();
    }

    Consent {
        crash: cfg.tier1_crash,
        perf: cfg.tier1_perf,
        usage: cfg.tier2_usage,
    }
}

pub fn is_event_allowed(consent: &Consent, tier: Tier, kind: EventKind) -> bool {
    match (tier, kind) {
        (Tier::One, EventKind::Crash) => consent.crash,
        (Tier::One, EventKind::Perf) => consent.perf,
        (Tier::Two, EventKind::Usage) => consent.usage,
        _ => false,
    }
}

fn telemetry_env_disables() -> bool {
    match env::var("SPUR_TELEMETRY") {
        Ok(value) => {
            let value = value.trim();
            value.is_empty() || value == "0"
        }
        Err(_) => false,
    }
}

fn ci_env_disables() -> bool {
    match env::var("CI") {
        Ok(value) => {
            let value = value.trim();
            !(value.is_empty() || value.eq_ignore_ascii_case("false"))
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_event_allowed, resolve, Consent, EventKind, Tier};
    use crate::config::TelemetryConfig;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn spur_telemetry_zero_disables_all() {
        with_env(&[("SPUR_TELEMETRY", Some("0")), ("CI", None)], || {
            let cfg = TelemetryConfig {
                tier1_crash: true,
                tier1_perf: true,
                tier2_usage: true,
                ..TelemetryConfig::default()
            };

            let consent = resolve(&cfg);
            assert_eq!(consent, none());
        });
    }

    #[test]
    fn ci_true_disables_all() {
        with_env(
            &[("SPUR_TELEMETRY", Some("1")), ("CI", Some("true"))],
            || {
                let cfg = TelemetryConfig {
                    tier1_crash: true,
                    tier1_perf: true,
                    tier2_usage: true,
                    ..TelemetryConfig::default()
                };

                let consent = resolve(&cfg);
                assert_eq!(consent, none());
            },
        );
    }

    #[test]
    fn ci_false_honors_config() {
        with_env(
            &[("SPUR_TELEMETRY", Some("1")), ("CI", Some("false"))],
            || {
                let cfg = TelemetryConfig {
                    tier1_crash: true,
                    tier1_perf: true,
                    tier2_usage: false,
                    ..TelemetryConfig::default()
                };

                let consent = resolve(&cfg);
                assert_eq!(
                    consent,
                    Consent {
                        crash: true,
                        perf: true,
                        usage: false,
                    }
                );
            },
        );
    }

    #[test]
    fn unset_env_uses_default_on_tier1_config() {
        with_env(&[("SPUR_TELEMETRY", None), ("CI", None)], || {
            let cfg = TelemetryConfig {
                tier1_crash: true,
                tier1_perf: true,
                tier2_usage: false,
                ..TelemetryConfig::default()
            };

            let consent = resolve(&cfg);
            assert!(consent.crash);
            assert!(consent.perf);
            assert!(!consent.usage);
        });
    }

    #[test]
    fn event_allowance_honors_per_subtier_disables() {
        let consent = Consent {
            crash: false,
            perf: true,
            usage: true,
        };

        assert!(!is_event_allowed(&consent, Tier::One, EventKind::Crash));
        assert!(is_event_allowed(&consent, Tier::One, EventKind::Perf));
        assert!(is_event_allowed(&consent, Tier::Two, EventKind::Usage));
        assert!(!is_event_allowed(&consent, Tier::One, EventKind::Usage));
        assert!(!is_event_allowed(&consent, Tier::Two, EventKind::Crash));
    }

    fn none() -> Consent {
        Consent {
            crash: false,
            perf: false,
            usage: false,
        }
    }

    fn with_env<F>(vars: &[(&str, Option<&str>)], f: F)
    where
        F: FnOnce(),
    {
        let _guard = env_lock().lock().expect("env test lock");
        let mut old_values = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            old_values.push((
                *key,
                env::var(key).ok(),
                env::var_os(key).is_some() && env::var(key).is_err(),
            ));
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }

        f();

        for (key, old_utf8, old_non_utf8) in old_values {
            if old_non_utf8 {
                // Best effort: if an original value was non-UTF8, we cannot restore bytes via var.
                env::remove_var(key);
            } else if let Some(old) = old_utf8 {
                env::set_var(key, old);
            } else {
                env::remove_var(key);
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
