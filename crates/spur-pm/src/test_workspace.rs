use std::path::{Path, PathBuf};

use beads_rust::model::{Issue, IssueType, Priority, Status};
use beads_rust::storage::sqlite::SqliteStorage;
use chrono::Utc;
use tempfile::TempDir;

pub struct TestBeadsWorkspace {
    _dir: TempDir,
    pub path: PathBuf,
    pub storage: SqliteStorage,
}

impl TestBeadsWorkspace {
    pub fn init() -> Self {
        let dir = TempDir::new().expect("create temp beads workspace");
        let path = dir.path().to_path_buf();
        let db_path = path.join("beads.db");
        let storage = SqliteStorage::open(&db_path).expect("open beads test database");

        Self {
            _dir: dir,
            path,
            storage,
        }
    }

    pub fn create_issue(&mut self, title: &str) -> String {
        self.create_with_type(title, IssueType::Task)
    }

    pub fn create_epic(&mut self, title: &str) -> String {
        self.create_with_type(title, IssueType::Epic)
    }

    pub fn add_label(&mut self, id: &str, label: &str) {
        self.storage
            .add_label(id, label, "test")
            .expect("add test issue label");
    }

    pub fn close_issue(&mut self, id: &str) {
        let update = beads_rust::storage::sqlite::IssueUpdate {
            status: Some(Status::Closed),
            ..Default::default()
        };
        self.storage
            .update_issue(id, &update, "test")
            .expect("close test issue");
    }

    pub fn add_dep(&mut self, child: &str, parent: &str) {
        self.storage
            .add_dependency(child, parent, "blocks", "test")
            .expect("add test issue dependency");
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn create_with_type(&mut self, title: &str, issue_type: IssueType) -> String {
        let id = beads_rust::util::generate_id(title, None, Some("test"), Utc::now());
        let issue = build_issue(id.clone(), title.to_string(), issue_type);
        self.storage
            .create_issue(&issue, "test")
            .expect("create test issue");
        id
    }
}

fn build_issue(id: String, title: String, issue_type: IssueType) -> Issue {
    let now = Utc::now();
    Issue {
        id,
        title,
        description: None,
        status: Status::Open,
        priority: Priority::MEDIUM,
        issue_type,
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
        created_by: Some("test".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_init_and_create_round_trip() {
        let mut w = TestBeadsWorkspace::init();
        let id = w.create_issue("Hello");
        assert!(!id.is_empty());

        let issue = w.storage.get_issue(&id).unwrap().expect("present");
        assert_eq!(issue.title, "Hello");

        w.add_label(&id, "test-label");
        w.close_issue(&id);

        let epic_id = w.create_epic("Epic");
        let child = w.create_issue("Child");
        w.add_dep(&child, &epic_id);
    }
}
