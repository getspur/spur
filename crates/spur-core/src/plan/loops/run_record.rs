use crate::plan::audit_sentinel::AuditSentinelKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRunOutcome {
    pub tasks_discovered: u32,
    pub approved: u32,
    pub rejected: u32,
    pub failed: u32,
    pub cancelled: u32,
}

impl LoopRunOutcome {
    fn outcome_str(self) -> &'static str {
        if self.rejected == 0 && self.failed == 0 && self.cancelled == 0 {
            "approved"
        } else if self.approved > 0 {
            "partial"
        } else {
            "failed"
        }
    }
}

pub fn sum_completion_cost_micros(audits: &[AuditSentinelKind]) -> u64 {
    audits.iter().fold(0u64, |sum, audit| match audit {
        AuditSentinelKind::Completion {
            estimated_cost_micros: Some(cost_micros),
            ..
        } => sum.saturating_add(*cost_micros),
        _ => sum,
    })
}

pub fn build_loop_run(
    loop_id: &str,
    generation: u32,
    plan_id: &str,
    outcome: LoopRunOutcome,
    audits: &[AuditSentinelKind],
    clock_now: i64,
) -> AuditSentinelKind {
    let escalations = audits
        .iter()
        .filter(|audit| matches!(audit, AuditSentinelKind::EscalationRequested { .. }))
        .count()
        .min(u32::MAX as usize) as u32;

    AuditSentinelKind::LoopRun {
        loop_id: loop_id.to_string(),
        generation,
        plan_id: plan_id.to_string(),
        outcome: outcome.outcome_str().to_string(),
        tasks_discovered: outcome.tasks_discovered,
        approved: outcome.approved,
        rejected: outcome.rejected,
        failed: outcome.failed,
        cancelled: outcome.cancelled,
        escalations,
        cost_micros: sum_completion_cost_micros(audits),
        started_at: clock_now,
        ended_at: clock_now,
    }
}

pub fn retired_loop_run(loop_id: &str, generation: u32, clock_now: i64) -> AuditSentinelKind {
    AuditSentinelKind::LoopRun {
        loop_id: loop_id.to_string(),
        generation,
        plan_id: String::new(),
        outcome: "retired".to_string(),
        tasks_discovered: 0,
        approved: 0,
        rejected: 0,
        failed: 0,
        cancelled: 0,
        escalations: 0,
        cost_micros: 0,
        started_at: clock_now,
        ended_at: clock_now,
    }
}
