#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOwnerMatch {
    OwnedByCurrent,
    OwnedByOther { owner: String },
    Unowned,
}

pub fn classify_owner(labels: &[String], current: &spur_acp::SessionId) -> PlanOwnerMatch {
    let Some(owner) = labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_owner(label))
    else {
        return PlanOwnerMatch::Unowned;
    };

    if owner == crate::plan::labels::compact_label_component(&current.0) {
        PlanOwnerMatch::OwnedByCurrent
    } else {
        PlanOwnerMatch::OwnedByOther {
            owner: owner.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_owner, PlanOwnerMatch};
    use crate::plan::labels;
    use spur_acp::SessionId;

    #[test]
    fn matches_current_session() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let labels = vec![labels::plan_id("plan-1"), labels::plan_owner(&current.0)];

        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::OwnedByCurrent
        );
    }

    #[test]
    fn detects_other_session() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let other = "7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a";
        let labels = vec![labels::plan_owner(other), labels::plan_owner(&current.0)];

        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::OwnedByOther {
                owner: labels::compact_label_component(other)
            }
        );
    }

    #[test]
    fn missing_owner_is_unowned() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let labels = vec![labels::plan_id("plan-1"), labels::agent("codex")];

        assert_eq!(classify_owner(&labels, &current), PlanOwnerMatch::Unowned);
    }
}
