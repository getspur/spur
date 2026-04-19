use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use spur_acp::domain::events::{
    LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent, LicenseSubjectKind,
    SpurEventBody,
};
use spur_license::{BindingMode, LicenseState, LicenseStatus, Plan, SpurLicense, SubjectKind};

use crate::event_funnel::FunnelHandle;

pub fn spawn_license_runtime(license: SpurLicense, funnel: FunnelHandle) -> JoinHandle<()> {
    tokio::spawn(async move {
        emit_snapshot(&funnel, license.current_state());

        let policy = license.refresh_policy();

        // Track the current delays, allowing them to exponentially backoff
        let mut current_validate_delay = policy.validate_interval;
        let mut current_heartbeat_delay = policy.heartbeat_interval;

        let mut validate_sleep = Box::pin(tokio::time::sleep(current_validate_delay));
        let mut heartbeat_sleep = Box::pin(tokio::time::sleep(current_heartbeat_delay));
        let mut updates = license.subscribe();

        // Max backoff capped at 1 hour
        let max_backoff = std::time::Duration::from_secs(3600);

        loop {
            tokio::select! {
                _ = &mut validate_sleep => {
                    match license.validate().await {
                        Ok(_) => {
                            current_validate_delay = policy.validate_interval;
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "license validation failed");
                            emit_snapshot(&funnel, degraded_from(license.current_state(), format!("Validation failed: {err}")));
                            // Exponential backoff
                            current_validate_delay = std::cmp::min(current_validate_delay * 2, max_backoff);
                        }
                    }
                    // Add random jitter +/- 10%
                    let jitter_ms = rand::random::<f64>() * 0.2 - 0.1;
                    let jittered_delay = current_validate_delay.mul_f64(1.0 + jitter_ms);
                    validate_sleep.as_mut().reset(tokio::time::Instant::now() + jittered_delay);
                }
                _ = &mut heartbeat_sleep, if should_heartbeat(&license.current_state()) => {
                    match license.heartbeat().await {
                        Ok(_) => {
                            current_heartbeat_delay = policy.heartbeat_interval;
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "license heartbeat failed");
                            emit_snapshot(&funnel, degraded_from(license.current_state(), format!("Heartbeat failed: {err}")));
                            // Exponential backoff
                            current_heartbeat_delay = std::cmp::min(current_heartbeat_delay * 2, max_backoff);
                        }
                    }
                    // Add random jitter +/- 10%
                    let jitter_ms = rand::random::<f64>() * 0.2 - 0.1;
                    let jittered_delay = current_heartbeat_delay.mul_f64(1.0 + jitter_ms);
                    heartbeat_sleep.as_mut().reset(tokio::time::Instant::now() + jittered_delay);
                }
                result = updates.recv() => {
                    match result {
                        Ok(event) => emit_snapshot(&funnel, event.state),
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "license runtime event stream lagged");
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

fn should_heartbeat(state: &LicenseState) -> bool {
    state.is_active() && !matches!(state.binding_mode, BindingMode::Unknown)
}

fn degraded_from(mut state: LicenseState, message: String) -> LicenseState {
    match state.status {
        LicenseStatus::Active => {
            state.status = LicenseStatus::Degraded;
            state.status_text = message;
        }
        LicenseStatus::Degraded => {
            // Already degraded — refresh the transient reason to reflect
            // the most recent failure.
            state.status_text = message;
        }
        LicenseStatus::Inactive | LicenseStatus::Invalid | LicenseStatus::ConfigError => {
            // Keep authoritative status and text. A transient network error
            // must not overwrite a prior hard-fail reason (revoked, expired,
            // unconfigured, etc.).
        }
    }
    state
}

fn emit_snapshot(funnel: &FunnelHandle, state: LicenseState) {
    funnel.emit(SpurEventBody::LicenseUpdated {
        state: to_event_state(state),
    });
}

pub fn to_event_state(state: LicenseState) -> LicenseStateEvent {
    LicenseStateEvent {
        status: match state.status {
            LicenseStatus::Inactive => LicenseStatusEvent::Inactive,
            LicenseStatus::Active => LicenseStatusEvent::Active,
            LicenseStatus::Degraded => LicenseStatusEvent::Degraded,
            LicenseStatus::Invalid => LicenseStatusEvent::Invalid,
            LicenseStatus::ConfigError => LicenseStatusEvent::ConfigError,
        },
        subject_kind: match state.subject_kind {
            SubjectKind::User => LicenseSubjectKind::User,
            SubjectKind::Organization => LicenseSubjectKind::Organization,
            SubjectKind::Ci => LicenseSubjectKind::Ci,
            SubjectKind::Unknown => LicenseSubjectKind::Unknown,
        },
        plan: match state.plan {
            Plan::Community => LicensePlan::Community,
            Plan::StarterLtd => LicensePlan::StarterLtd,
            Plan::BuilderLtd => LicensePlan::BuilderLtd,
            Plan::FounderLtd => LicensePlan::FounderLtd,
            Plan::Pro => LicensePlan::Pro,
            Plan::Team => LicensePlan::Team,
            Plan::Enterprise => LicensePlan::Enterprise,
            Plan::Unknown => LicensePlan::Unknown,
        },
        features: state.features,
        expires_at: state.expires_at,
        binding_mode: match state.binding_mode {
            BindingMode::NodeLocked => LicenseBindingMode::NodeLocked,
            BindingMode::FloatingCi => LicenseBindingMode::FloatingCi,
            BindingMode::Organization => LicenseBindingMode::Organization,
            BindingMode::Unknown => LicenseBindingMode::Unknown,
        },
        offline_ok: state.offline_ok,
        status_text: state.status_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    use tokio::sync::broadcast;

    #[test]
    fn convert_state_maps_core_fields() {
        let mut features = BTreeSet::new();
        features.insert("alpha".to_string());
        let event = to_event_state(LicenseState {
            status: LicenseStatus::Active,
            subject_kind: SubjectKind::User,
            plan: Plan::Pro,
            features,
            expires_at: None,
            binding_mode: BindingMode::NodeLocked,
            offline_ok: true,
            status_text: "ok".into(),
        });

        assert!(matches!(event.status, LicenseStatusEvent::Active));
        assert!(matches!(event.subject_kind, LicenseSubjectKind::User));
        assert!(matches!(event.plan, LicensePlan::Pro));
        assert!(event.features.contains("alpha"));
    }

    #[tokio::test]
    async fn spawn_emits_initial_snapshot() {
        let (tx, mut rx) = broadcast::channel(16);
        let funnel = crate::event_funnel::spawn_funnel(
            tx,
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        let runtime =
            spawn_license_runtime(spur_license::SpurLicense::from_env_or_disabled(), funnel);

        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for license update")
            .expect("receive initial license event");

        assert!(matches!(ev.body, SpurEventBody::LicenseUpdated { .. }));

        runtime.abort();
    }
}
