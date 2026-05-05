//! Issue read adapter for `BeadsCrateAdapter`.

use std::str::FromStr;

use chrono::Utc;

use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::types::{Issue, IssueCreate, IssueFilter, IssueSummary, PmSource};

pub(crate) fn br_to_pm_issue(br: beads_rust::model::Issue) -> Issue {
    let url = format!("beads://{}", br.id);
    Issue {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        body: br.description.unwrap_or_default(),
        status: br.status.to_string(),
        labels: br.labels,
        assignee: br.assignee,
        url,
        priority: Some(br.priority.0),
        issue_type: Some(br.issue_type.to_string()),
        blocked_by: vec![],
        due_at: br.due_at,
        created_at: br.created_at,
        updated_at: br.updated_at,
    }
}

pub(crate) fn br_to_pm_summary(br: beads_rust::model::Issue) -> IssueSummary {
    let url = format!("beads://{}", br.id);
    IssueSummary {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        status: br.status.to_string(),
        labels: br.labels,
        url,
        priority: Some(br.priority.0),
        issue_type: Some(br.issue_type.to_string()),
        assignee: br.assignee,
    }
}

impl BeadsCrateAdapter {
    pub async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        let id = id.to_string();
        self.read(move |s| {
            let mut br = s
                .get_issue(&id)?
                .ok_or_else(|| anyhow::anyhow!("issue {id} not found"))?;
            // `get_issue` only reads the `issues` table; labels are stored
            // out-of-line and must be loaded separately.
            let mut by_id = s.get_labels_for_issues(std::slice::from_ref(&id))?;
            br.labels = by_id.remove(&id).unwrap_or_default();
            Ok(br_to_pm_issue(br))
        })
        .await
    }

    pub async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        self.read(move |s| {
            let mut br_filters = beads_rust::storage::sqlite::ListFilters::default();

            if !filter.labels.is_empty() {
                br_filters.labels = Some(filter.labels.clone());
            }
            if let Some(status) = filter.status.as_deref() {
                let parsed = beads_rust::model::Status::from_str(status)
                    .unwrap_or(beads_rust::model::Status::Open);
                br_filters.statuses = Some(vec![parsed]);
            }
            if let Some(itype) = filter.issue_type.as_deref() {
                let parsed = beads_rust::model::IssueType::from_str(itype)
                    .unwrap_or(beads_rust::model::IssueType::Task);
                br_filters.types = Some(vec![parsed]);
            }
            br_filters.assignee = filter.assignee.clone();
            if let Some(min) = filter.priority_min {
                let max = filter.priority_max.unwrap_or(4);
                let priorities: Vec<beads_rust::model::Priority> =
                    (min..=max).map(beads_rust::model::Priority).collect();
                br_filters.priorities = Some(priorities);
            } else if let Some(max) = filter.priority_max {
                let priorities: Vec<beads_rust::model::Priority> =
                    (0..=max).map(beads_rust::model::Priority).collect();
                br_filters.priorities = Some(priorities);
            }
            br_filters.title_contains = filter.text_search.clone();
            br_filters.include_closed = filter.include_closed;
            br_filters.limit = filter.limit;
            br_filters.offset = filter.offset;
            if let Some(since) = filter.since {
                br_filters.updated_after = Some(since);
            }

            let issues = s.list_issues(&br_filters)?;
            Ok(issues.into_iter().map(br_to_pm_summary).collect())
        })
        .await
    }

    pub async fn create_issue(&self, params: IssueCreate) -> anyhow::Result<String> {
        self.write(move |s| {
            let now = Utc::now();
            let id = beads_rust::util::generate_id(
                &params.title,
                params.description.as_deref(),
                Some("spur"),
                now,
            );

            let issue_type = params
                .issue_type
                .as_deref()
                .map(|t| {
                    beads_rust::model::IssueType::from_str(t)
                        .unwrap_or(beads_rust::model::IssueType::Task)
                })
                .unwrap_or_default();

            let priority = params
                .priority
                .map(beads_rust::model::Priority)
                .unwrap_or_default();

            let issue = beads_rust::model::Issue {
                id: id.clone(),
                title: params.title,
                description: params.description,
                status: beads_rust::model::Status::Open,
                priority,
                issue_type,
                created_at: now,
                updated_at: now,
                assignee: params.assignee,
                owner: None,
                estimated_minutes: params.estimate_minutes.and_then(|m| i32::try_from(m).ok()),
                due_at: None,
                defer_until: None,
                external_ref: None,
                ephemeral: false,
                content_hash: None,
                design: None,
                acceptance_criteria: None,
                notes: None,
                created_by: Some("spur".to_string()),
                closed_at: None,
                close_reason: None,
                closed_by_session: None,
                source_system: None,
                source_repo: None,
                deleted_at: None,
                deleted_by: None,
                delete_reason: None,
                original_type: None,
                compaction_level: None,
                compacted_at: None,
                compacted_at_commit: None,
                original_size: None,
                sender: None,
                pinned: false,
                is_template: false,
                labels: Vec::new(),
                dependencies: Vec::new(),
                comments: Vec::new(),
            };
            let labels = params.labels;

            s.create_issue(&issue, "spur")?;

            if !labels.is_empty() {
                s.set_labels(&id, &labels, "spur")?;
            }

            if let Some(parent) = params.parent.as_deref() {
                s.add_dependency(&id, parent, "parent-child", "spur")?;
            }
            for dep in &params.depends_on {
                s.add_dependency(&id, dep, "blocks", "spur")?;
            }

            Ok(id)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use beads_rust::model::{Issue as BrIssue, IssueType, Priority, Status};
    use chrono::Utc;
    use tempfile::TempDir;

    use crate::beads_crate::adapter::{AdapterConfig, BeadsCrateAdapter};

    fn minimal_issue(id: &str, title: &str) -> BrIssue {
        let now = Utc::now();
        BrIssue {
            id: id.into(),
            title: title.into(),
            description: None,
            status: Status::Open,
            priority: Priority::MEDIUM,
            issue_type: IssueType::Task,
            created_at: now,
            updated_at: now,
            assignee: None,
            owner: None,
            estimated_minutes: None,
            due_at: None,
            defer_until: None,
            external_ref: None,
            ephemeral: false,
            content_hash: None,
            design: None,
            acceptance_criteria: None,
            notes: None,
            created_by: None,
            closed_at: None,
            close_reason: None,
            closed_by_session: None,
            source_system: None,
            source_repo: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            original_type: None,
            compaction_level: None,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: None,
            sender: None,
            pinned: false,
            is_template: false,
            labels: Vec::new(),
            dependencies: Vec::new(),
            comments: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_issue_returns_seeded_issue() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        let seed_issue = minimal_issue("bd-test-get-issue", "seeded issue");
        let expected_id = seed_issue.id.clone();
        let expected_title = seed_issue.title.clone();

        adapter
            .write(move |s| s.create_issue(&seed_issue, "test").map_err(Into::into))
            .await
            .unwrap();

        let issue = adapter.get_issue(&expected_id).await.unwrap();

        assert_eq!(issue.id, expected_id);
        assert_eq!(issue.title, expected_title);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_issue_round_trips_via_get_issue() {
        use crate::types::IssueCreate;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let id = adapter
            .create_issue(IssueCreate {
                title: "Hello".into(),
                description: Some("Body".into()),
                priority: Some(1),
                labels: vec!["lbl-a".into(), "lbl-b".into()],
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(!id.is_empty());

        let fetched = adapter.get_issue(&id).await.unwrap();
        assert_eq!(fetched.title, "Hello");
        assert_eq!(fetched.body, "Body");
        assert_eq!(fetched.priority, Some(1));
        assert_eq!(
            fetched.labels,
            vec!["lbl-a".to_string(), "lbl-b".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_issues_returns_seeded_data() {
        use crate::types::IssueFilter;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        adapter
            .batch(|s| {
                for i in 0..3 {
                    let issue = minimal_issue(&format!("bd-list-{i}"), &format!("issue {i}"));
                    s.create_issue(&issue, "test")?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let summaries = adapter.list_issues(IssueFilter::default()).await.unwrap();
        assert_eq!(summaries.len(), 3);
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"bd-list-0"));
        assert!(ids.contains(&"bd-list-1"));
        assert!(ids.contains(&"bd-list-2"));
    }
}
