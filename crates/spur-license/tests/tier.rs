use spur_license::{Plan, Tier};

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
