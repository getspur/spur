use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOwnerMatch {
    OwnedByCurrent,
    OwnedByOther { owner: String },
    Ambiguous { owners: Vec<String> },
    Unowned,
}

pub fn classify_owner(labels: &[String], current: &spur_acp::SessionId) -> PlanOwnerMatch {
    let owners = labels
        .iter()
        .filter_map(|label| crate::plan::labels::parse_plan_owner(label))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    if owners.is_empty() {
        return PlanOwnerMatch::Unowned;
    }

    if owners.len() > 1 {
        return PlanOwnerMatch::Ambiguous {
            owners: owners.into_iter().collect(),
        };
    }

    let owner = owners.into_iter().next().expect("owner set is non-empty");

    if owner == crate::plan::labels::compact_label_component(&current.0) {
        PlanOwnerMatch::OwnedByCurrent
    } else {
        PlanOwnerMatch::OwnedByOther { owner }
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
        let labels = vec![labels::plan_owner(other)];

        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::OwnedByOther {
                owner: labels::compact_label_component(other)
            }
        );
    }

    #[test]
    fn duplicate_owner_labels_are_not_ambiguous() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let labels = vec![
            labels::plan_owner(&current.0),
            labels::plan_owner(&current.0),
        ];

        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::OwnedByCurrent
        );
    }

    #[test]
    fn mixed_owner_labels_are_ambiguous_and_sorted() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let first = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let second = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let labels = vec![
            labels::plan_owner(second),
            labels::plan_owner(&current.0),
            labels::plan_owner(first),
        ];

        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::Ambiguous {
                owners: vec![
                    labels::compact_label_component(&current.0),
                    labels::compact_label_component(first),
                    labels::compact_label_component(second),
                ],
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
