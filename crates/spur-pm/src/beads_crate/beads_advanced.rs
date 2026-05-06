use std::str::FromStr;

use async_trait::async_trait;

use crate::advanced::{BeadsAdvanced, Comment, CommentId, DependencyCycle, ReadyFilter};
use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::beads_crate::issue_tracker::br_to_pm_summary;
use crate::types::IssueSummary;

impl BeadsCrateAdapter {
    pub async fn plan_id_label_for_epic(&self, id: &str) -> anyhow::Result<Option<String>> {
        let id = id.to_string();
        self.read(move |s| {
            let issue = s
                .get_issue(&id)?
                .ok_or_else(|| anyhow::anyhow!("issue {id} not found"))?;
            if !issue.issue_type.to_string().eq_ignore_ascii_case("epic") {
                return Ok(None);
            }

            Ok(s.get_labels(&id)?
                .into_iter()
                .find(|label| label.starts_with("spur:plan-id:")))
        })
        .await
    }
}

#[async_trait]
impl BeadsAdvanced for BeadsCrateAdapter {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
        self.read(move |s| {
            let issue_types = filter
                .issue_type
                .as_deref()
                .map(|issue_type| {
                    beads_rust::model::IssueType::from_str(issue_type).map(|t| vec![t])
                })
                .transpose()?;
            let priorities = if filter.priorities.is_empty() {
                None
            } else {
                Some(
                    filter
                        .priorities
                        .iter()
                        .map(|priority| {
                            beads_rust::model::Priority::from_str(&priority.to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
            };
            let ready_filter = beads_rust::storage::ReadyFilters {
                assignee: filter.assignee,
                labels_and: filter.labels_all,
                labels_or: filter.labels_any,
                types: issue_types,
                priorities,
                limit: Some(filter.limit.unwrap_or(20)),
                ..Default::default()
            };

            let mut issues =
                s.get_ready_issues(&ready_filter, beads_rust::storage::ReadySortPolicy::Hybrid)?;
            let ids: Vec<String> = issues.iter().map(|issue| issue.id.clone()).collect();
            let mut labels_by_id = s.get_labels_for_issues(&ids)?;
            for issue in &mut issues {
                issue.labels = labels_by_id.remove(&issue.id).unwrap_or_default();
            }

            Ok(issues.into_iter().map(br_to_pm_summary).collect())
        })
        .await
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>> {
        let issue_id = issue_id.to_string();
        self.read(move |s| {
            Ok(s.get_comments(&issue_id)?
                .into_iter()
                .map(|comment| Comment {
                    id: comment.id.to_string(),
                    body: comment.body,
                    actor: comment.author,
                    created_at: comment.created_at,
                })
                .collect())
        })
        .await
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId> {
        let issue_id = issue_id.to_string();
        let body = body.to_string();
        self.write(move |s| Ok(s.add_comment(&issue_id, "spur", &body)?.id.to_string()))
            .await
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        let issue_id = issue_id.to_string();
        let depends_on_id = depends_on_id.to_string();
        self.write(move |s| {
            s.remove_dependency(&issue_id, &depends_on_id, "spur")?;
            Ok(())
        })
        .await
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
        self.read(move |s| {
            Ok(s.detect_all_cycles()?
                .into_iter()
                .map(|issues| DependencyCycle { issues })
                .collect())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tempfile::TempDir;

    use crate::adapter::IssueTracker;
    use crate::advanced::{BeadsAdvanced, ReadyFilter};
    use crate::beads_crate::adapter::{AdapterConfig, BeadsCrateAdapter};
    use crate::types::IssueCreate;

    async fn setup_adapter() -> (TempDir, BeadsCrateAdapter) {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        (dir, adapter)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_ready_returns_unblocked_issue() {
        let (_dir, adapter) = setup_adapter().await;
        let a = adapter
            .create_issue(IssueCreate {
                title: "Task A".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let b = adapter
            .create_issue(IssueCreate {
                title: "Task B".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        adapter.add_dependency(&b, &a).await.unwrap();

        let ready = adapter
            .list_ready(ReadyFilter {
                limit: Some(50),
                ..Default::default()
            })
            .await
            .unwrap();
        let ids: HashSet<String> = ready.into_iter().map(|issue| issue.id).collect();

        assert!(
            ids.contains(&a),
            "expected unblocked issue {a}, got {ids:?}"
        );
        assert!(!ids.contains(&b), "blocked issue {b} should not be ready");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_comments_returns_existing_comment() {
        let (_dir, adapter) = setup_adapter().await;
        let issue_id = adapter
            .create_issue(IssueCreate {
                title: "Commented".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let issue_for_seed = issue_id.clone();
        adapter
            .write(move |s| {
                s.add_comment(&issue_for_seed, "test", "seeded comment")?;
                Ok(())
            })
            .await
            .unwrap();

        let comments = adapter.list_comments(&issue_id).await.unwrap();

        assert!(
            comments
                .iter()
                .any(|comment| comment.body == "seeded comment" && comment.actor == "test"),
            "expected seeded comment, got {comments:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_comment_persists_comment() {
        let (_dir, adapter) = setup_adapter().await;
        let issue_id = adapter
            .create_issue(IssueCreate {
                title: "New comment".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let comment_id = adapter
            .add_comment(&issue_id, "advanced comment")
            .await
            .unwrap();

        assert!(!comment_id.is_empty());
        let issue_for_check = issue_id.clone();
        let comments = adapter
            .read(move |s| Ok(s.get_comments(&issue_for_check)?))
            .await
            .unwrap();
        assert!(
            comments
                .iter()
                .any(|comment| comment.id.to_string() == comment_id
                    && comment.body == "advanced comment"
                    && comment.author == "spur"),
            "expected advanced comment id {comment_id}, got {comments:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_dependency_round_trips() {
        let (_dir, adapter) = setup_adapter().await;
        let parent = adapter
            .create_issue(IssueCreate {
                title: "Parent".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let child = adapter
            .create_issue(IssueCreate {
                title: "Child".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        adapter.add_dependency(&child, &parent).await.unwrap();

        adapter.remove_dependency(&child, &parent).await.unwrap();

        let child_for_check = child.clone();
        let deps = adapter
            .read(move |s| Ok(s.get_dependencies(&child_for_check)?))
            .await
            .unwrap();
        assert!(
            !deps.contains(&parent),
            "dependency should have been removed, got {deps:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dep_cycles_returns_storage_detected_cycles() {
        let (_dir, adapter) = setup_adapter().await;
        let a = adapter
            .create_issue(IssueCreate {
                title: "Cycle A".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let b = adapter
            .create_issue(IssueCreate {
                title: "Cycle B".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let a_for_seed = a.clone();
        let b_for_seed = b.clone();
        adapter
            .write(move |s| {
                s.add_dependency_with_metadata(&a_for_seed, &b_for_seed, "related", "test", None)?;
                s.add_dependency_with_metadata(&b_for_seed, &a_for_seed, "related", "test", None)?;
                Ok(())
            })
            .await
            .unwrap();

        let cycles = adapter.dep_cycles().await.unwrap();

        assert!(
            cycles
                .iter()
                .any(|cycle| cycle.issues.contains(&a) && cycle.issues.contains(&b)),
            "expected cycle containing {a} and {b}, got {cycles:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plan_id_label_for_epic_returns_plan_label_only_for_epics() {
        let (_dir, adapter) = setup_adapter().await;
        let epic = adapter
            .create_issue(IssueCreate {
                title: "Epic".into(),
                issue_type: Some("epic".into()),
                labels: vec!["other".into(), "spur:plan-id:P1".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        let task = adapter
            .create_issue(IssueCreate {
                title: "Task".into(),
                labels: vec!["spur:plan-id:P2".into()],
                ..Default::default()
            })
            .await
            .unwrap();

        let epic_label = adapter.plan_id_label_for_epic(&epic).await.unwrap();
        let task_label = adapter.plan_id_label_for_epic(&task).await.unwrap();

        assert_eq!(epic_label.as_deref(), Some("spur:plan-id:P1"));
        assert_eq!(task_label, None);
    }
}
