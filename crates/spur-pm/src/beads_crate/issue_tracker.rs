//! Issue read adapter for `BeadsCrateAdapter`.

use std::str::FromStr;

use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::types::{Issue, IssueFilter, IssueSummary, PmSource};

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
            let br = s
                .get_issue(&id)?
                .ok_or_else(|| anyhow::anyhow!("issue {id} not found"))?;
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
