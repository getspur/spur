//! Phase 2 Task 10: freestanding `fetch_outcome_artifact` handler.
//!
//! The freestanding handler MUST construct the `OutcomeKey`'s
//! `brain_session_id` from `WorkerCallContext`, NOT from any
//! caller-supplied parameter. This is the cross-session isolation
//! invariant (server.rs:2997-3001 in the original implementation).

use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use spur_acp::domain::outcome::OutcomeKey;
use spur_acp::domain::{DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, SessionId};
use spur_blob_store::{ContentType, MemoryOutcomeStore, OutcomeMetadata, OutcomeStore};
use spur_mcp::handlers::{fetch_outcome_artifact, McpHandlerError, WorkerCallContext};
use spur_mcp::outcome_materializer::OutcomeMaterializer;

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

fn outcome_metadata(content: &[u8]) -> OutcomeMetadata {
    OutcomeMetadata {
        created_at: chrono::Utc::now(),
        content_type: ContentType::Json,
        original_byte_size: content.len() as u64,
        stored_byte_size: content.len() as u64,
        sha256: sha256_hex(content),
    }
}

fn success_result(summary: &str, diff: &str) -> DelegationResult {
    DelegationResult {
        status: DelegationStatus::Success,
        summary: Some(summary.into()),
        diff: Some(diff.into()),
        diff_summary: None,
        estimated_cost_usd: 0.0,
        worker_branch: None,
        artifact: None,
    }
}

async fn put_outcome(
    store: &Arc<dyn OutcomeStore>,
    brain_session: &BrainSessionId,
    delegation_id: &str,
    attempt: u32,
    result: &DelegationResult,
) {
    let bytes = serde_json::to_vec(result).expect("serialize result");
    let metadata = outcome_metadata(&bytes);
    let key = OutcomeKey {
        brain_session_id: brain_session.clone(),
        delegation_id: delegation_id.into(),
        attempt,
    };
    store.put(&key, &bytes, &metadata).await.expect("put");
}

#[tokio::test]
async fn fetch_outcome_artifact_returns_full_text_for_same_session() {
    let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
    let materializer = OutcomeMaterializer::new(store.clone());

    let session_a = "550e8400-e29b-41d4-a716-446655440000";
    let brain_session_a = BrainSessionId::new(SessionId(session_a.into()));
    let result = success_result("session A summary", "diff text\n");
    put_outcome(&store, &brain_session_a, "deleg-1", 1, &result).await;

    let ctx = WorkerCallContext {
        delegation_id: "deleg-1".into(),
        brain_session_id: session_a.into(),
    };
    let value = fetch_outcome_artifact(
        &materializer,
        store.as_ref(),
        &ctx,
        json!({
            "delegation_id": "deleg-1",
            "attempt": 1,
            "section": "full",
        }),
    )
    .await
    .expect("handler should succeed for same-session lookup");

    let text = value["content"][0]["text"]
        .as_str()
        .expect("content[0].text must be a string");
    let parsed: DelegationResult =
        serde_json::from_str(text).expect("full text is DelegationResult JSON");
    assert_eq!(parsed.summary.as_deref(), Some("session A summary"));
}

#[tokio::test]
async fn fetch_outcome_artifact_cross_session_returns_unauthorized() {
    // KEY SECURITY ASSERTION: a worker bound to session-A cannot fetch an
    // artifact stored under session-B, even if they guess the delegation_id.
    let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
    let materializer = OutcomeMaterializer::new(store.clone());

    // Artifact lives under session-B.
    let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
    let brain_session_b = BrainSessionId::new(SessionId(session_b.into()));
    let result = success_result("secret B summary", "secret diff");
    put_outcome(&store, &brain_session_b, "deleg-shared", 1, &result).await;

    // Worker is bound to session-A and asks for the same delegation_id.
    let session_a = "550e8400-e29b-41d4-a716-446655440000";
    let ctx = WorkerCallContext {
        delegation_id: "deleg-shared".into(),
        brain_session_id: session_a.into(),
    };
    let err = fetch_outcome_artifact(
        &materializer,
        store.as_ref(),
        &ctx,
        json!({
            "delegation_id": "deleg-shared",
            "attempt": 1,
            "section": "full",
        }),
    )
    .await
    .expect_err("cross-session lookup must be denied");

    assert!(
        matches!(err, McpHandlerError::Unauthorized(_)),
        "cross-session lookup must surface as Unauthorized to prevent probing; got {err:?}"
    );
}

#[tokio::test]
async fn fetch_outcome_artifact_missing_delegation_id_invalid_params() {
    let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
    let materializer = OutcomeMaterializer::new(store.clone());

    let ctx = WorkerCallContext {
        delegation_id: String::new(),
        brain_session_id: "550e8400-e29b-41d4-a716-446655440000".into(),
    };
    let err = fetch_outcome_artifact(&materializer, store.as_ref(), &ctx, json!({}))
        .await
        .expect_err("missing delegation_id must be InvalidParams");
    assert!(
        matches!(err, McpHandlerError::InvalidParams(_)),
        "got {err:?}"
    );
}
