use crate::events::{Event, IntoProp, Props, Tier};
use crate::tier1_events::ModelName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedShort(String);

impl HashedShort {
    pub fn from_sha256_prefix(input: &str) -> Self {
        Self(crate::redact::sha256_prefix(input))
    }
}

impl crate::events::sealed::Sealed for HashedShort {}
impl IntoProp for HashedShort {
    fn into_prop(self) -> serde_json::Value {
        self.0.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillName {
    SpurWay,
    WorkerSignals,
    PlanTaskDiscipline,
    BeadsLifecycle,
    BrainReviewGate,
    SystematicDebugging,
    TestDrivenDevelopment,
    VerificationBeforeCompletion,
    Other,
}

impl crate::events::sealed::Sealed for SkillName {}
impl IntoProp for SkillName {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            SkillName::SpurWay => "spur-way",
            SkillName::WorkerSignals => "worker-signals",
            SkillName::PlanTaskDiscipline => "plan-task-discipline",
            SkillName::BeadsLifecycle => "beads-lifecycle",
            SkillName::BrainReviewGate => "brain-review-gate",
            SkillName::SystematicDebugging => "systematic-debugging",
            SkillName::TestDrivenDevelopment => "test-driven-development",
            SkillName::VerificationBeforeCompletion => "verification-before-completion",
            SkillName::Other => "other",
        };
        value.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerName {
    Github,
    Posthog,
    SpurMcp,
    Stitch,
    Playwright,
    Context7,
    Firebase,
    SequentialThinking,
    Custom(HashedShort),
}

impl crate::events::sealed::Sealed for McpServerName {}
impl IntoProp for McpServerName {
    fn into_prop(self) -> serde_json::Value {
        match self {
            McpServerName::Github => "github".into(),
            McpServerName::Posthog => "posthog".into(),
            McpServerName::SpurMcp => "spur-mcp".into(),
            McpServerName::Stitch => "stitch".into(),
            McpServerName::Playwright => "playwright".into(),
            McpServerName::Context7 => "context7".into(),
            McpServerName::Firebase => "firebase".into(),
            McpServerName::SequentialThinking => "sequential-thinking".into(),
            McpServerName::Custom(hash) => hash.into_prop(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolName {
    SubmitPlan,
    DispatchTask,
    ReviewTask,
    GetTaskDiff,
    ListTools,
    Custom(HashedShort),
}

impl crate::events::sealed::Sealed for McpToolName {}
impl IntoProp for McpToolName {
    fn into_prop(self) -> serde_json::Value {
        match self {
            McpToolName::SubmitPlan => "submit_plan".into(),
            McpToolName::DispatchTask => "dispatch_task".into(),
            McpToolName::ReviewTask => "review_task".into(),
            McpToolName::GetTaskDiff => "get_task_diff".into(),
            McpToolName::ListTools => "list_tools".into(),
            McpToolName::Custom(hash) => hash.into_prop(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    Accept,
    Reject,
    RequestChanges,
}

impl crate::events::sealed::Sealed for ReviewOutcome {}
impl IntoProp for ReviewOutcome {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            ReviewOutcome::Accept => "accept",
            ReviewOutcome::Reject => "reject",
            ReviewOutcome::RequestChanges => "request_changes",
        };
        value.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewName {
    Dashboard,
    SessionDetail,
    IssueBrowser,
    PlanBrowser,
    PlanInspector,
    Other,
}

impl crate::events::sealed::Sealed for ViewName {}
impl IntoProp for ViewName {
    fn into_prop(self) -> serde_json::Value {
        let value = match self {
            ViewName::Dashboard => "dashboard",
            ViewName::SessionDetail => "session_detail",
            ViewName::IssueBrowser => "issue_browser",
            ViewName::PlanBrowser => "plan_browser",
            ViewName::PlanInspector => "plan_inspector",
            ViewName::Other => "other",
        };
        value.into()
    }
}

pub struct PlanCreated {
    pub task_count: u32,
    pub brain_model: ModelName,
    pub duration_ms: u64,
}

impl Event for PlanCreated {
    const NAME: &'static str = "plan_created";
    const TIER: Tier = Tier::Two;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("brain_model", self.brain_model.into_prop());
        props.insert("duration_ms", self.duration_ms.into_prop());
        props.insert("task_count", self.task_count.into_prop());
        props
    }
}

pub struct WorkerDispatched {
    pub worker_model: ModelName,
    pub skill_used: SkillName,
    pub attempt_num: u32,
}

impl Event for WorkerDispatched {
    const NAME: &'static str = "worker_dispatched";
    const TIER: Tier = Tier::Two;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("attempt_num", self.attempt_num.into_prop());
        props.insert("skill_used", self.skill_used.into_prop());
        props.insert("worker_model", self.worker_model.into_prop());
        props
    }
}

pub struct McpToolCalled {
    pub server_name: McpServerName,
    pub tool_name: McpToolName,
    pub outcome: crate::tier1_events::Outcome,
}

impl Event for McpToolCalled {
    const NAME: &'static str = "mcp_tool_called";
    const TIER: Tier = Tier::Two;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("outcome", self.outcome.into_prop());
        props.insert("server_name", self.server_name.into_prop());
        props.insert("tool_name", self.tool_name.into_prop());
        props
    }
}

pub struct ReviewCompleted {
    pub outcome: ReviewOutcome,
    pub iteration_count: u32,
}

impl Event for ReviewCompleted {
    const NAME: &'static str = "review_completed";
    const TIER: Tier = Tier::Two;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("iteration_count", self.iteration_count.into_prop());
        props.insert("outcome", self.outcome.into_prop());
        props
    }
}

pub struct TuiViewOpened {
    pub view_name: ViewName,
}

impl Event for TuiViewOpened {
    const NAME: &'static str = "tui_view_opened";
    const TIER: Tier = Tier::Two;

    fn into_props(self) -> Props {
        let mut props = Props::new();
        props.insert("view_name", self.view_name.into_prop());
        props
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(props: &Props) -> Vec<&'static str> {
        props.keys().copied().collect()
    }

    #[test]
    fn custom_server_and_tool_emit_only_hashed_short() {
        let server_raw = "internal-mcp-prod";
        let tool_raw = "search_customer_by_email";
        let server = McpServerName::Custom(HashedShort::from_sha256_prefix(server_raw));
        let tool = McpToolName::Custom(HashedShort::from_sha256_prefix(tool_raw));
        let props = McpToolCalled {
            server_name: server,
            tool_name: tool,
            outcome: crate::tier1_events::Outcome::Ok,
        }
        .into_props();

        assert_eq!(keys(&props), vec!["outcome", "server_name", "tool_name"]);
        assert!(props["server_name"].is_string());
        assert!(props["tool_name"].is_string());
        assert_ne!(props["server_name"], server_raw);
        assert_ne!(props["tool_name"], tool_raw);
        assert_eq!(props["server_name"].as_str().unwrap().len(), 8);
        assert_eq!(props["tool_name"].as_str().unwrap().len(), 8);
    }
}
