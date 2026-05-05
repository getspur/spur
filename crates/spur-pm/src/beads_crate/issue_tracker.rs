//! Issue read adapter for `BeadsCrateAdapter`.

use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::types::{Issue, PmSource};

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
}
