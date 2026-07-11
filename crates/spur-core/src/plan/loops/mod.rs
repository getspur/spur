pub mod doctor;
pub(crate) mod leadership;
pub mod run_record;
pub mod scheduler;
pub mod spec;
pub mod status;
pub mod validation;

pub(crate) const LOOP_ISSUE_TYPE: &str = "loop";
pub(crate) const LOOP_RUNTIME_OWNER_ID: &str = "spur-loop-runtime";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopSweepScope {
    All,
    BrainArmedOnly,
    L3Only,
}

impl LoopSweepScope {
    pub fn allows(self, autonomy: spec::AutonomyLevel) -> bool {
        match self {
            Self::All => true,
            Self::BrainArmedOnly => {
                matches!(autonomy, spec::AutonomyLevel::L1 | spec::AutonomyLevel::L2)
            }
            Self::L3Only => autonomy == spec::AutonomyLevel::L3,
        }
    }
}
