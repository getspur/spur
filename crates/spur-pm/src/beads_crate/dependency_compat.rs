use std::collections::HashMap;

use beads_rust::model::Dependency;
use beads_rust::storage::sqlite::SqliteStorage;

pub(crate) fn get_dependencies_full_for_issues(
    storage: &SqliteStorage,
    issue_ids: &[String],
) -> beads_rust::Result<HashMap<String, Vec<Dependency>>> {
    let mut deps_by_id = HashMap::new();

    for issue_id in issue_ids {
        let deps = storage.get_dependencies_full(issue_id)?;
        if !deps.is_empty() {
            deps_by_id.insert(issue_id.clone(), deps);
        }
    }

    Ok(deps_by_id)
}

#[cfg(test)]
pub(crate) fn add_dependency_with_metadata(
    storage: &mut SqliteStorage,
    issue_id: &str,
    depends_on_id: &str,
    dep_type: &str,
    actor: &str,
    metadata: Option<&str>,
) -> beads_rust::Result<bool> {
    let added = storage.add_dependency(issue_id, depends_on_id, dep_type, actor)?;

    if let Some(metadata) = metadata {
        let mut deps = storage.get_dependencies_full(issue_id)?;
        for dep in &mut deps {
            if dep.depends_on_id == depends_on_id {
                dep.metadata = Some(metadata.to_string());
            }
        }
        storage.sync_dependencies_for_import(issue_id, &deps)?;
    }

    Ok(added)
}

#[cfg(test)]
mod tests {
    use beads_rust::model::{DependencyType, Issue};
    use beads_rust::storage::sqlite::SqliteStorage;

    use super::{add_dependency_with_metadata, get_dependencies_full_for_issues};

    fn seed_issue(storage: &mut SqliteStorage, id: &str) {
        storage
            .create_issue(
                &Issue {
                    id: id.to_string(),
                    title: id.to_string(),
                    ..Default::default()
                },
                "tester",
            )
            .unwrap();
    }

    #[test]
    fn get_dependencies_full_for_issues_groups_full_dependencies_by_source_issue() {
        let mut storage = SqliteStorage::open_memory().unwrap();
        seed_issue(&mut storage, "bd-a");
        seed_issue(&mut storage, "bd-b");
        seed_issue(&mut storage, "bd-c");

        storage
            .add_dependency("bd-a", "bd-b", "blocks", "tester")
            .unwrap();
        storage
            .add_dependency("bd-c", "bd-b", "related", "tester")
            .unwrap();

        let ids = vec!["bd-a".to_string(), "bd-b".to_string(), "bd-c".to_string()];
        let deps_by_id = get_dependencies_full_for_issues(&storage, &ids).unwrap();

        assert_eq!(deps_by_id["bd-a"].len(), 1);
        assert_eq!(deps_by_id["bd-a"][0].depends_on_id, "bd-b");
        assert_eq!(deps_by_id["bd-a"][0].dep_type, DependencyType::Blocks);
        assert!(!deps_by_id.contains_key("bd-b"));
        assert_eq!(deps_by_id["bd-c"][0].dep_type, DependencyType::Related);
    }

    #[test]
    fn add_dependency_with_metadata_accepts_empty_metadata_for_cycle_seeding() {
        let mut storage = SqliteStorage::open_memory().unwrap();
        seed_issue(&mut storage, "bd-a");
        seed_issue(&mut storage, "bd-b");

        let added =
            add_dependency_with_metadata(&mut storage, "bd-a", "bd-b", "related", "tester", None)
                .unwrap();
        let duplicate =
            add_dependency_with_metadata(&mut storage, "bd-a", "bd-b", "related", "tester", None)
                .unwrap();

        let deps = storage.get_dependencies_full("bd-a").unwrap();
        assert!(added);
        assert!(!duplicate);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on_id, "bd-b");
        assert_eq!(deps[0].metadata.as_deref(), Some("{}"));
        assert_eq!(deps[0].created_by.as_deref(), Some("tester"));
    }
}
