use std::path::Path;

use beads_rust::storage::sqlite::SqliteStorage;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, IssueUpdate, PmService};
use tempfile::TempDir;

fn attach_beads_workspace(repo: &Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

fn init_beads_repo(repo: &Path) -> TestBeadsWorkspace {
    let w = TestBeadsWorkspace::init();
    attach_beads_workspace(repo, &w);
    w
}

fn repo_storage(repo: &Path) -> SqliteStorage {
    SqliteStorage::open(&repo.join(".beads/beads.db")).expect("open test beads db")
}

#[tokio::test]
async fn pm_service_poll_persists_cursor_across_restarts() {
    let dir = TempDir::new().unwrap();
    let mut w = TestBeadsWorkspace::init();
    w.create_issue("Issue1");
    attach_beads_workspace(dir.path(), &w);

    let first = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");
    let first_events = first.poll().await.unwrap();
    assert_eq!(
        first_events.len(),
        1,
        "first session must see the seeded issue"
    );

    let second = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");
    let second_events = second.poll().await.unwrap();
    assert!(
        second_events.is_empty(),
        "second session replayed already-polled events: {second_events:?}"
    );
}

#[tokio::test]
async fn pm_service_create_issue_uses_reconciler_actor_by_default() {
    let dir = TempDir::new().unwrap();
    let _w = init_beads_repo(dir.path());

    let service = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");

    let issue_id = service
        .create_issue(IssueCreate {
            title: "Actor threaded create".to_string(),
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let storage = repo_storage(dir.path());
    let issue = storage
        .get_issue(&issue_id)
        .expect("load issue")
        .expect("issue should exist");
    let created_by = issue
        .created_by
        .as_deref()
        .expect("br show must expose created_by");
    assert_eq!(created_by, "reconciler");
}

#[tokio::test]
async fn pm_service_update_issue_handles_multiple_label_changes() {
    let dir = TempDir::new().unwrap();
    let _w = init_beads_repo(dir.path());

    let service = PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("beads backend should be available");

    let issue_id = service
        .create_issue(IssueCreate {
            title: "Batch labels".to_string(),
            issue_type: Some("task".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    service
        .update_issue(
            &issue_id,
            IssueUpdate {
                add_labels: vec!["alpha".to_string(), "beta".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("multi-label add should succeed");

    let issue = service
        .get_issue(&issue_id)
        .await
        .expect("load issue after add");
    let labels_after_add = &issue.labels;
    assert!(
        labels_after_add.iter().any(|label| label == "alpha"),
        "alpha missing after add: {labels_after_add:?}"
    );
    assert!(
        labels_after_add.iter().any(|label| label == "beta"),
        "beta missing after add: {labels_after_add:?}"
    );

    service
        .update_issue(
            &issue_id,
            IssueUpdate {
                remove_labels: vec!["alpha".to_string(), "beta".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("multi-label remove should succeed");

    let issue = service
        .get_issue(&issue_id)
        .await
        .expect("load issue after remove");
    let labels_after_remove = &issue.labels;
    assert!(
        !labels_after_remove.iter().any(|label| label == "alpha"),
        "alpha still present after remove: {labels_after_remove:?}"
    );
    assert!(
        !labels_after_remove.iter().any(|label| label == "beta"),
        "beta still present after remove: {labels_after_remove:?}"
    );
}
