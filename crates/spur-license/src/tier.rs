use serde::{Deserialize, Serialize};

use crate::Plan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    Community,
    Pro,
    Team,
    Enterprise,
}

impl Tier {
    pub fn from_plan(plan: Plan) -> Self {
        match plan {
            Plan::Pro | Plan::StarterLtd | Plan::BuilderLtd | Plan::FounderLtd => Self::Pro,
            Plan::Team => Self::Team,
            Plan::Enterprise => Self::Enterprise,
            Plan::Community | Plan::Unknown => Self::Community,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Community => "Community",
            Self::Pro => "Pro",
            Self::Team => "Team",
            Self::Enterprise => "Enterprise",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_from_plan_community() {
        assert_eq!(Tier::from_plan(Plan::Community), Tier::Community);
    }

    #[test]
    fn tier_from_plan_pro_variants() {
        assert_eq!(Tier::from_plan(Plan::Pro), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::StarterLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::BuilderLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::FounderLtd), Tier::Pro);
    }

    #[test]
    fn tier_label_matches() {
        assert_eq!(Tier::Community.label(), "Community");
        assert_eq!(Tier::Pro.label(), "Pro");
        assert_eq!(Tier::Team.label(), "Team");
        assert_eq!(Tier::Enterprise.label(), "Enterprise");
    }

    #[test]
    fn unknown_plan_defaults_to_community() {
        assert_eq!(Tier::from_plan(Plan::Unknown), Tier::Community);
    }

    #[test]
    fn ltd_plans_map_to_pro() {
        assert_eq!(Tier::from_plan(Plan::StarterLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::BuilderLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::FounderLtd), Tier::Pro);
    }
}
