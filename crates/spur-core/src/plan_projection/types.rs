use std::time::SystemTime;

use spur_acp::{DiffSummary, PlanSnapshotCounts, SessionId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedTask {
    pub task_id: String,
    pub task_name: String,
    pub agent: String,
    pub issue_id: Option<String>,
    pub issue_title: Option<String>,
    pub status: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub depends_on: Vec<String>,
    pub blocked_by: Vec<String>,
    pub unblocks: Vec<String>,
    pub summary: Option<String>,
    pub feedback: Option<String>,
    pub error: Option<String>,
    pub worker_branch: Option<String>,
    pub delegation_id: Option<String>,
    pub diff_summary: Option<DiffSummary>,
    pub mutation_id: Option<String>,
    pub superseded_by: Vec<String>,
    pub next_action: String,
    pub stage_idx: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedPlan {
    pub session_id: SessionId,
    pub plan_id: String,
    pub epic_id: Option<String>,
    pub status: String,
    pub progress: String,
    pub next_action: String,
    pub ready_to_merge: bool,
    pub owner_brain_session_id: Option<String>,
    pub counts: PlanSnapshotCounts,
    pub tasks: Vec<TrackedTask>,
    pub updated_at: SystemTime,
}

impl TrackedPlan {
    pub fn task(&self, task_id: &str) -> Option<&TrackedTask> {
        self.tasks.iter().find(|task| task.task_id == task_id)
    }

    pub fn is_active(&self) -> bool {
        !matches!(
            self.status.as_str(),
            "approved" | "rejected" | "failed" | "cancelled"
        )
    }
}
