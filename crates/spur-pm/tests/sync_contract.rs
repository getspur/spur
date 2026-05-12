use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use spur_pm::{
    ConflictReason, DepHint, DepHintKind, DepHintSource, ExternalPmSync, FetchOneOutcome,
    LocalMutation, LocalMutationKind, PushOutcome, RemoteComment, RemoteConflict, RemoteDelta,
    RemoteKind, RemoteNode, RemoteRef, RemoteState, SyncError, SyncResult, SyncWatermark,
};

fn ts(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

struct ContractSync {
    source_repo: String,
}

#[async_trait]
impl ExternalPmSync for ContractSync {
    fn source_system(&self) -> &'static str {
        "github"
    }

    fn source_repo(&self) -> &str {
        &self.source_repo
    }

    async fn fetch_changes_since(&self, since: Option<DateTime<Utc>>) -> SyncResult<RemoteDelta> {
        Ok(RemoteDelta {
            nodes: vec![remote_node()],
            deletions: since
                .map(|_| RemoteRef {
                    source_system: "github".to_string(),
                    remote_id: "deleted-node".to_string(),
                })
                .into_iter()
                .collect(),
            watermark: ts(30),
        })
    }

    async fn fetch_one(
        &self,
        remote_id: &str,
        if_none_match: Option<&str>,
    ) -> SyncResult<FetchOneOutcome> {
        if remote_id == "missing" {
            return Ok(FetchOneOutcome::Gone);
        }
        if if_none_match == Some("etag-1") {
            return Ok(FetchOneOutcome::Unchanged);
        }
        Ok(FetchOneOutcome::Updated(remote_node()))
    }

    async fn push_mutations(&self, diff: Vec<LocalMutation>) -> SyncResult<Vec<PushOutcome>> {
        Ok(diff
            .into_iter()
            .map(|mutation| PushOutcome::Skipped {
                reason: format!("{}:{}", mutation.beads_id, mutation.remote_id),
            })
            .collect())
    }

    async fn detect_conflicts(
        &self,
        watermarks: &[SyncWatermark],
    ) -> SyncResult<Vec<RemoteConflict>> {
        Ok(watermarks
            .iter()
            .map(|watermark| RemoteConflict {
                beads_id: watermark.beads_id.clone(),
                remote_id: watermark.remote_id.clone(),
                local_updated_at: ts(40),
                remote_updated_at: ts(50),
                watermark_remote_updated_at: watermark.last_synced_remote_updated_at,
                reason: ConflictReason::LocalAndRemoteBothMutated,
            })
            .collect())
    }
}

fn remote_node() -> RemoteNode {
    RemoteNode {
        remote_id: "I_kwDOExample123".to_string(),
        remote_number: Some(42),
        kind: RemoteKind::Issue,
        title: "Contract issue".to_string(),
        body: "Closes owner/repo#41".to_string(),
        state: RemoteState::Open,
        labels: vec!["bug".to_string()],
        assignees: vec!["alice".to_string()],
        created_at: ts(10),
        updated_at: ts(20),
        html_url: "https://github.com/owner/repo/issues/42".to_string(),
        etag: Some("etag-1".to_string()),
        dep_hints: vec![DepHint {
            kind: DepHintKind::Closes,
            remote_keyword: "Closes".to_string(),
            remote_ref: "owner/repo#41".to_string(),
            remote_node_id: Some("I_kwDOExample122".to_string()),
            raw_span: "Closes owner/repo#41".to_string(),
            source: DepHintSource::Body,
        }],
        comments: vec![RemoteComment {
            remote_id: "IC_kwDOComment123".to_string(),
            author: "alice".to_string(),
            body: "remote comment".to_string(),
            created_at: ts(11),
            updated_at: ts(12),
        }],
        raw: json!({ "future_field": true }),
    }
}

#[tokio::test]
async fn external_pm_sync_contract_supports_bulk_single_push_and_conflicts() {
    let sync: Box<dyn ExternalPmSync> = Box::new(ContractSync {
        source_repo: "owner/repo".to_string(),
    });

    assert_eq!(sync.source_system(), "github");
    assert_eq!(sync.source_repo(), "owner/repo");

    let delta = sync.fetch_changes_since(None).await.unwrap();
    assert_eq!(delta.nodes[0].raw["future_field"], true);
    assert_eq!(
        delta.nodes[0].dep_hints[0].remote_node_id.as_deref(),
        Some("I_kwDOExample122")
    );

    assert!(matches!(
        sync.fetch_one("I_kwDOExample123", Some("etag-1"))
            .await
            .unwrap(),
        FetchOneOutcome::Unchanged
    ));

    let outcomes = sync
        .push_mutations(vec![LocalMutation {
            beads_id: "bd-123".to_string(),
            remote_id: "I_kwDOExample123".to_string(),
            kind: LocalMutationKind::CommentAdded {
                body: "hello".to_string(),
            },
        }])
        .await
        .unwrap();
    assert!(matches!(outcomes[0], PushOutcome::Skipped { .. }));

    let conflicts = sync
        .detect_conflicts(&[SyncWatermark {
            beads_id: "bd-123".to_string(),
            remote_id: "I_kwDOExample123".to_string(),
            last_synced_at: ts(20),
            last_synced_etag: Some("etag-1".to_string()),
            last_synced_remote_updated_at: ts(20),
        }])
        .await
        .unwrap();
    assert!(matches!(
        conflicts[0].reason,
        ConflictReason::LocalAndRemoteBothMutated
    ));
}

#[test]
fn sync_types_serialize_without_resolved_beads_id_on_dep_hints() {
    let value = serde_json::to_value(remote_node()).unwrap();
    assert!(value["raw"]["future_field"].as_bool().unwrap());
    assert!(value["dep_hints"][0].get("remote_node_id").is_some());
    assert!(value["dep_hints"][0].get("resolved_beads_id").is_none());
}

#[test]
fn sync_error_messages_are_stable_enough_for_cli_surfacing() {
    assert_eq!(
        SyncError::RateLimited { retry_after_s: 30 }.to_string(),
        "rate limited; retry after 30s"
    );
}
