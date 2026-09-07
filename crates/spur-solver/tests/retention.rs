use std::{
    error::Error,
    fs::{self, File, FileTimes},
    path::Path,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolModule};
use spur_solver::{
    mcp::SolverMcpModule,
    persist::{MAX_ARTIFACTS, MAX_ARTIFACT_BYTES},
    service::SolverService,
    types::SolveConstraintsResponse,
};
use tempfile::tempdir;

fn response() -> SolveConstraintsResponse {
    serde_json::from_value(json!({"status":"unsat", "duration_ms":1, "reason":null}))
        .expect("valid response")
}

fn artifact_path(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".spur/solver").join(format!("{id}.json"))
}

fn retention_path(root: &Path) -> std::path::PathBuf {
    root.join(".spur/solver/.retention/state.json")
}

fn set_time(path: &Path, seconds: u64) -> std::io::Result<()> {
    File::options()
        .write(true)
        .open(path)?
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
}

fn legacy_cache(root: &Path, count: usize) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(root.join(".spur/solver"))?;
    for i in 0..count {
        let path = artifact_path(root, &format!("sol_{i:016x}"));
        fs::write(
            &path,
            format!("{{\"created_at_wall\":\"2020-01-01T00:00:00.{i:09}Z\"}}"),
        )?;
        set_time(&path, 1_600_000_000 + i as u64)?;
    }
    Ok(())
}

async fn pin(module: &SolverMcpModule, id: &str, value: bool) -> Result<(), Box<dyn Error>> {
    module
        .call(
            ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
            "get_solve_result",
            json!({"solve_id":id, "pin":value}),
        )
        .await?;
    Ok(())
}

#[test]
fn mixed_size_legacy_receipts_evict_by_age_not_json_parse_cutoff() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    legacy_cache(repo.path(), MAX_ARTIFACTS)?;
    let recent = artifact_path(repo.path(), "sol_00000000000001ff");
    fs::write(
        &recent,
        serde_json::to_vec(&json!({
            "created_at_wall":"2026-01-01T00:00:00Z", "padding":"x".repeat(70_000)
        }))?,
    )?;
    set_time(&recent, 1_700_000_000)?;
    let service = SolverService::new().with_repo_root(repo.path());
    service.persist(&json!({}), &response())?;
    assert!(
        recent.exists(),
        "recent large receipt must survive an older small receipt"
    );
    assert!(!artifact_path(repo.path(), "sol_0000000000000000").exists());
    Ok(())
}

#[test]
fn insertion_sequence_survives_reopen_equal_timestamps_and_clock_rollback(
) -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());
    let first = service.persist(&json!({}), &response())?;
    let second = service.persist(&json!({}), &response())?;
    // Deliberately make timestamps disagree with insertion order; only the
    // persisted insertion sequence can retain the real ordering after restart.
    for (artifact, timestamp) in [
        (&first, "2099-01-01T00:00:00Z"),
        (&second, "2000-01-01T00:00:00Z"),
    ] {
        let path = artifact_path(repo.path(), &artifact.solve_id);
        let mut value = serde_json::to_value(artifact)?;
        value["created_at_wall"] = json!(timestamp);
        fs::write(&path, serde_json::to_vec(&value)?)?;
        set_time(&path, 1_700_000_000)?;
    }
    legacy_cache(repo.path(), MAX_ARTIFACTS - 2)?;
    let reopened = SolverService::new().with_repo_root(repo.path());
    reopened.persist(&json!({}), &response())?;
    assert!(
        !artifact_path(repo.path(), &first.solve_id).exists(),
        "first insertion is oldest despite its future wall clock"
    );
    assert!(reopened.get_solve_result(&second.solve_id).is_ok());
    Ok(())
}

#[tokio::test]
async fn explicit_pin_survives_pressure_and_unpin_restores_original_order(
) -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = Arc::new(SolverService::new().with_repo_root(repo.path()));
    let first = service.persist(&json!({}), &response())?;
    let module = SolverMcpModule::new(Arc::clone(&service));
    pin(&module, &first.solve_id, true)
        .await
        .expect("pin must be supported");
    pin(&module, &first.solve_id, true).await?; // idempotent
    legacy_cache(repo.path(), MAX_ARTIFACTS - 1)?;
    let reopened = SolverService::new().with_repo_root(repo.path());
    reopened.persist(&json!({}), &response())?;
    assert!(reopened.get_solve_result(&first.solve_id).is_ok());
    pin(&module, &first.solve_id, false).await?;
    reopened.persist(&json!({}), &response())?;
    assert!(!artifact_path(repo.path(), &first.solve_id).exists());
    Ok(())
}

#[tokio::test]
async fn all_pinned_quota_refusal_preserves_every_receipt() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = Arc::new(SolverService::new().with_repo_root(repo.path()));
    let template = service.persist(&json!({}), &response())?;
    let module = SolverMcpModule::new(Arc::clone(&service));
    pin(&module, &template.solve_id, true)
        .await
        .expect("pin must be supported");
    for i in 0..MAX_ARTIFACTS - 1 {
        let id = format!("sol_{i:016x}");
        let mut value = serde_json::to_value(&template)?;
        value["solve_id"] = json!(id);
        fs::write(artifact_path(repo.path(), &id), serde_json::to_vec(&value)?)?;
        pin(&module, &id, true).await?;
    }
    let error = service
        .persist(&json!({}), &response())
        .expect_err("pinned evidence cannot be evicted");
    assert!(error.to_string().contains("quota"));
    assert!(service.get_solve_result(&template.solve_id).is_ok());
    for i in 0..MAX_ARTIFACTS - 1 {
        assert!(artifact_path(repo.path(), &format!("sol_{i:016x}")).exists());
    }
    Ok(())
}

#[test]
fn corrupt_retention_metadata_fails_closed_without_deleting_receipts() -> Result<(), Box<dyn Error>>
{
    let repo = tempdir()?;
    legacy_cache(repo.path(), MAX_ARTIFACTS)?;
    fs::create_dir(repo.path().join(".spur/solver/.retention"))?;
    fs::write(retention_path(repo.path()), b"{broken")?;
    let service = SolverService::new().with_repo_root(repo.path());
    assert!(
        service.persist(&json!({}), &response()).is_err(),
        "cannot safely infer protection or order from corrupt metadata"
    );
    for i in 0..MAX_ARTIFACTS {
        assert!(artifact_path(repo.path(), &format!("sol_{i:016x}")).exists());
    }
    Ok(())
}

#[test]
fn eviction_intent_is_durable_and_bounded() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    legacy_cache(repo.path(), MAX_ARTIFACTS)?;
    let service = SolverService::new().with_repo_root(repo.path());
    service.persist(&json!({}), &response())?;
    let path = retention_path(repo.path());
    assert!(path.is_file(), "eviction must leave durable metadata");
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    assert_eq!(value["evictions"][0]["solve_id"], "sol_0000000000000000");
    assert_eq!(value["evictions"][0]["reason"], "count");
    assert!(value["evictions"].as_array().unwrap().len() <= MAX_ARTIFACTS);
    Ok(())
}

#[tokio::test]
async fn byte_quota_refusal_does_not_partially_evict_unpinned_receipts(
) -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = Arc::new(SolverService::new().with_repo_root(repo.path()));
    let protected = service.persist(&json!({}), &response())?;
    let module = SolverMcpModule::new(Arc::clone(&service));
    pin(&module, &protected.solve_id, true)
        .await
        .expect("pin must be supported");
    let unpinned = service.persist(&json!({}), &response())?;
    // Sparse fixture simulates byte pressure after protection was recorded.
    File::options()
        .write(true)
        .open(artifact_path(repo.path(), &protected.solve_id))?
        .set_len(MAX_ARTIFACT_BYTES - 10_000)?;
    let before = fs::read(retention_path(repo.path()))?;
    let error = service
        .persist(&json!("x".repeat(20_000)), &response())
        .expect_err("pins leave insufficient capacity");
    assert!(error.to_string().contains("quota"));
    assert!(artifact_path(repo.path(), &protected.solve_id).exists());
    assert!(
        service.get_solve_result(&unpinned.solve_id).is_ok(),
        "preflight must reject before deleting even eligible victims"
    );
    assert_eq!(fs::read(retention_path(repo.path()))?, before);
    Ok(())
}

#[tokio::test]
async fn ordinary_lookup_does_not_change_retention_and_pin_validates_ids(
) -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = Arc::new(SolverService::new().with_repo_root(repo.path()));
    let receipt = service.persist(&json!({}), &response())?;
    let module = SolverMcpModule::new(Arc::clone(&service));
    pin(&module, &receipt.solve_id, true)
        .await
        .expect("pin must be supported");
    let path = retention_path(repo.path());
    let before = fs::read(&path)?;
    module
        .call(
            ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
            "get_solve_result",
            json!({"solve_id":receipt.solve_id}),
        )
        .await?;
    assert_eq!(fs::read(&path)?, before);
    assert!(pin(&module, "../outside", true).await.is_err());
    assert!(pin(&module, "sol_ffffffffffffffff", true).await.is_err());
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_retention_metadata_is_rejected_without_following_it() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    legacy_cache(repo.path(), MAX_ARTIFACTS)?;
    let outside = repo.path().join("outside");
    fs::write(&outside, b"do not touch")?;
    std::os::unix::fs::symlink(&outside, repo.path().join(".spur/solver/.retention"))?;
    let service = SolverService::new().with_repo_root(repo.path());
    assert!(service.persist(&json!({}), &response()).is_err());
    assert_eq!(fs::read(&outside)?, b"do not touch");
    assert!(artifact_path(repo.path(), "sol_0000000000000000").exists());
    Ok(())
}

#[test]
fn retention_directory_makes_legacy_writers_fail_closed() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());
    service.persist(&json!({}), &response())?;
    // The old writer rejects any non-regular entry during its complete scan,
    // BEFORE eviction. A regular metadata file would itself be evictable.
    let metadata = fs::symlink_metadata(repo.path().join(".spur/solver/.retention"))?;
    assert!(
        metadata.is_dir(),
        "retention state must block older ring writers"
    );
    assert!(retention_path(repo.path()).is_file());
    Ok(())
}

#[test]
fn missing_receipt_reports_recorded_eviction_intent_after_reopen() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());
    let receipt = service.persist(&json!({}), &response())?;
    legacy_cache(repo.path(), MAX_ARTIFACTS - 1)?;
    service.persist(&json!({}), &response())?;
    let reopened = SolverService::new().with_repo_root(repo.path());
    let message = reopened
        .get_solve_result(&receipt.solve_id)
        .expect_err("receipt evicted")
        .to_string();
    assert!(
        message.contains("selected for eviction"),
        "lookup must explain recorded retention decisions: {message}"
    );
    assert!(message.contains("count"));
    Ok(())
}
