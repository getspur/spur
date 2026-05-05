//! Integration test: GitBlobOutcomeStore against a real (tempfile) git repo.

use chrono::Utc;
use sha2::{Digest, Sha256};
use spur_acp::{BrainSessionId, SessionId};
use spur_blob_store::{
    BackendTag, ContentType, OutcomeKey, OutcomeMetadata, OutcomeStore, Section, StoreError,
};
use spur_worktree::git_blob_store::GitBlobOutcomeStore;
use std::process::Command;
use tempfile::TempDir;

fn sha256_hex(content: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(content);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

fn init_repo(p: &std::path::Path) {
    let r = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(r.status.success());
    Command::new("git")
        .args(["config", "user.email", "t@e.com"])
        .current_dir(p)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(p)
        .output()
        .unwrap();
}

fn key(s: &str, d: &str, a: u32) -> OutcomeKey {
    OutcomeKey {
        brain_session_id: BrainSessionId::new(SessionId(s.into())),
        delegation_id: d.into(),
        attempt: a,
    }
}

fn metadata(content: &[u8]) -> OutcomeMetadata {
    OutcomeMetadata {
        created_at: Utc::now(),
        content_type: ContentType::Stdout,
        original_byte_size: content.len() as u64,
        stored_byte_size: content.len() as u64,
        sha256: sha256_hex(content),
    }
}

#[tokio::test]
async fn git_blob_store_put_get_roundtrip() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"hello git\n".to_vec();
    let r = store.put(&k, &body, &metadata(&body)).await.unwrap();
    assert_eq!(r.backend, BackendTag::GitBlob);
    assert_eq!(r.byte_size, body.len() as u64);

    let got = store.get(&k, Some(Section::Full)).await.unwrap();
    assert_eq!(got.bytes, body);
}

#[tokio::test]
async fn git_blob_store_idempotent_put() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"same".to_vec();
    let m = metadata(&body);
    let a = store.put(&k, &body, &m).await.unwrap();
    let b = store.put(&k, &body, &m).await.unwrap();
    assert_eq!(a.sha256, b.sha256);
}

#[tokio::test]
async fn git_blob_store_content_mismatch() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    store.put(&k, b"first", &metadata(b"first")).await.unwrap();
    let err = store
        .put(&k, b"second", &metadata(b"second"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::ContentMismatch { .. }));
}

#[tokio::test]
async fn git_blob_store_namespace_isolation() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let session_a = "550e8400-e29b-41d4-a716-446655440000";
    let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
    let k_a = key(session_a, "deadbeef-1111-2222-3333-444455556666", 1);
    let k_b = key(session_b, "deadbeef-1111-2222-3333-bbbbbbbbbbbb", 1);
    store.put(&k_a, b"A", &metadata(b"A")).await.unwrap();
    store.put(&k_b, b"B", &metadata(b"B")).await.unwrap();

    let report = store
        .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
        .await
        .unwrap();
    assert_eq!(report.count, 1);
    assert!(report.total_bytes >= 1);
    assert!(store.get(&k_a, None).await.is_err());
    assert!(store.get(&k_b, None).await.is_ok());
}

#[tokio::test]
async fn git_blob_store_rejects_non_uuid() {
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let bad = OutcomeKey {
        brain_session_id: BrainSessionId::new(SessionId("../etc/passwd".into())),
        delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
        attempt: 1,
    };
    let err = store.put(&bad, b"x", &metadata(b"x")).await.unwrap_err();
    assert!(matches!(err, StoreError::Backend(ref s) if s.contains("non-uuid")));
}

#[tokio::test]
async fn git_blob_store_accepts_short_hex_delegation_id() {
    // Regression: bd-ttyo's `mint_delegation_id` produces 16-char `[0-9a-f]+`
    // ids to fit the `br create --label` 50-char cap. Before this fix the
    // git blob store rejected them with "non-uuid delegation_id: wrong length
    // (16)", forcing fallback to the legacy artifact ref and breaking the
    // worker-output invariant downstream (no worker branch existed for
    // `git rev-list base..branch` to inspect).
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "d04edac4e67c4649",
        1,
    );
    let r = store.put(&k, b"hello", &metadata(b"hello")).await.unwrap();
    assert_eq!(r.sha256, sha256_hex(b"hello"));
    let got = store.get(&k, None).await.unwrap();
    assert_eq!(got.bytes, b"hello");
}

#[tokio::test]
async fn git_blob_store_per_attempt_granularity() {
    // Verifies Round 11 MF1 fix: distinct attempts under same delegation
    // get distinct refs (legacy bug overwrote the shared session ref).
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let session = "550e8400-e29b-41d4-a716-446655440000";
    let delegation = "deadbeef-1111-2222-3333-444455556666";
    let k1 = key(session, delegation, 1);
    let k2 = key(session, delegation, 2);

    store
        .put(&k1, b"first attempt", &metadata(b"first attempt"))
        .await
        .unwrap();
    store
        .put(&k2, b"second attempt", &metadata(b"second attempt"))
        .await
        .unwrap();

    let g1 = store.get(&k1, None).await.unwrap();
    let g2 = store.get(&k2, None).await.unwrap();
    assert_eq!(g1.bytes, b"first attempt");
    assert_eq!(g2.bytes, b"second attempt");
}

#[tokio::test]
async fn git_blob_store_sweep_prunes_legacy_artifact_refs() {
    // Spec §8.4 (Round 11 MF1+MF2): sweep_older_than walks both
    // refs/spur/outcomes/ AND legacy refs/spur/artifacts/. Legacy
    // refs have no sidecar metadata and are pruned unconditionally.
    use std::time::Duration;

    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());

    // Hand-create a legacy refs/spur/artifacts/<session> ref so we
    // can confirm sweep removes it. Use git hash-object + update-ref
    // to mimic Phase 1 worktree::artifact::persist behavior.
    let blob_sha = String::from_utf8(
        Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(td.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.as_mut().unwrap().write_all(b"legacy debt").unwrap();
                c.wait_with_output()
            })
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let legacy_ref = "refs/spur/artifacts/550e8400-e29b-41d4-a716-446655440000";
    let r = Command::new("git")
        .args(["update-ref", legacy_ref, &blob_sha])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(r.status.success(), "set legacy ref");

    // Confirm legacy ref exists pre-sweep.
    let pre = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", legacy_ref])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(pre.status.success(), "legacy ref exists pre-sweep");

    // Run sweep with a 1-day TTL — irrelevant for legacy refs (they
    // are treated as created_at = epoch, always eligible).
    let report = store
        .sweep_older_than(Duration::from_secs(86_400))
        .await
        .unwrap();

    // Legacy refs should be reported as pruned.
    assert!(
        report.namespaces_swept >= 1,
        "expected at least one namespace pruned (legacy debt)",
    );

    // Confirm legacy ref is gone post-sweep.
    let post = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", legacy_ref])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!post.status.success(), "legacy ref pruned post-sweep");
}

#[tokio::test]
async fn git_blob_store_returns_git_blob_sha_in_outcome_ref() {
    // Regression guard: WorkerArtifact.blob_sha (Phase 1 fetch_outcome_artifact
    // resolves via `git cat-file -p blob_sha`) MUST receive the 40-char git
    // SHA-1, not the 64-char content SHA-256. T9 review surfaced this hole.
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"sha-mapping check".to_vec();
    let r = store.put(&k, &body, &metadata(&body)).await.unwrap();

    let git_sha = r.git_blob_sha.expect("git_blob_sha must be populated");
    assert_eq!(git_sha.len(), 40, "git SHA-1 is 40 hex chars");
    assert!(
        git_sha.chars().all(|c| c.is_ascii_hexdigit()),
        "git SHA-1 must be hex"
    );
    // sha256 must be the 64-char content digest.
    assert_eq!(r.sha256.len(), 64, "content SHA-256 is 64 hex chars");
    assert_ne!(
        git_sha, r.sha256,
        "git SHA-1 and content SHA-256 must differ"
    );

    // Verify git can resolve the SHA via cat-file.
    let cat = Command::new("git")
        .args(["cat-file", "-p", &git_sha])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        cat.status.success(),
        "git cat-file -p git_blob_sha must succeed"
    );
    assert_eq!(cat.stdout, body, "git cat-file content must match input");
}

#[tokio::test]
async fn git_blob_store_idempotent_put_preserves_git_blob_sha() {
    // Second put for same key + same content recovers the existing
    // git SHA-1 via rev-parse.
    let td = TempDir::new().unwrap();
    init_repo(td.path());
    let store = GitBlobOutcomeStore::new(td.path().to_path_buf());
    let k = key(
        "550e8400-e29b-41d4-a716-446655440000",
        "deadbeef-1111-2222-3333-444455556666",
        1,
    );
    let body = b"idem".to_vec();
    let m = metadata(&body);
    let a = store.put(&k, &body, &m).await.unwrap();
    let b = store.put(&k, &body, &m).await.unwrap();
    assert_eq!(
        a.git_blob_sha, b.git_blob_sha,
        "idempotent put recovers git SHA"
    );
    assert!(a.git_blob_sha.is_some());
}
