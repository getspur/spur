use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ShutdownGraceWindows {
    pub(crate) stdin_grace: Duration,
    pub(crate) sigterm_grace: Duration,
    pub(crate) sigkill_grace: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownStageOutcome {
    ExitedAfterStdinClose,
    ExitedAfterSigterm,
    ExitedAfterSigkill,
    TimedOutAfterSigkill,
}

pub(crate) async fn escalate_shutdown_stages(
    mut child_exited: impl FnMut() -> bool,
    grace: ShutdownGraceWindows,
    mut sigterm: impl FnMut(),
    mut sigkill: impl FnMut(),
) -> ShutdownStageOutcome {
    if wait_stage(&mut child_exited, grace.stdin_grace).await {
        return ShutdownStageOutcome::ExitedAfterStdinClose;
    }
    sigterm();
    if wait_stage(&mut child_exited, grace.sigterm_grace).await {
        return ShutdownStageOutcome::ExitedAfterSigterm;
    }
    sigkill();
    if wait_stage(&mut child_exited, grace.sigkill_grace).await {
        ShutdownStageOutcome::ExitedAfterSigkill
    } else {
        ShutdownStageOutcome::TimedOutAfterSigkill
    }
}

async fn wait_stage(child_exited: &mut impl FnMut() -> bool, grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        if child_exited() {
            return true;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::time::sleep(std::cmp::min(Duration::from_millis(1), remaining)).await;
    }
}
