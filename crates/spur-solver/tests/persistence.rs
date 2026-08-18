use std::{
    error::Error,
    fs::{self, File},
    sync::{Arc, Barrier},
    thread,
};

use spur_solver::{
    persist::{
        ArtifactQuotaKind, PersistError, ARTIFACT_SCHEMA_VERSION, MAX_ARTIFACTS, MAX_ARTIFACT_BYTES,
    },
    process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
    service::{SolverService, SolverServiceError},
    types::{
        SolveConstraintsRequest, SolveConstraintsResponse, SolveStatus, DEFAULT_MAX_SOLUTIONS,
        DEFAULT_TIMEOUT_MS,
    },
};
use tempfile::tempdir;

#[derive(Debug)]
struct UnsatRunner;

impl ProcessRunner for UnsatRunner {
    fn run(&self, _request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async {
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: b"unsat\n".to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[test]
fn persisted_artifact_round_trips_across_service_instances() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let writer = SolverService::new().with_repo_root(repo.path());
    let request = request(false);
    let response = unsat_response();

    let persisted = writer.persist(&request, &response)?;
    assert_valid_solve_id(&persisted.solve_id);
    assert_eq!(persisted.schema_version, ARTIFACT_SCHEMA_VERSION);
    assert_eq!(persisted.request, serde_json::to_value(&request)?);
    assert_eq!(persisted.result.status, SolveStatus::Unsat);

    let artifact_path = repo
        .path()
        .join(".spur/solver")
        .join(format!("{}.json", persisted.solve_id));
    assert!(artifact_path.is_file());

    let reader = SolverService::new().with_repo_root(repo.path());
    let loaded = reader.get_solve_result(&persisted.solve_id)?;
    assert_eq!(loaded.solve_id, persisted.solve_id);
    assert_eq!(loaded.z3_version, persisted.z3_version);
    assert_eq!(loaded.result, persisted.result);

    let retrieval_json = serde_json::to_value(&loaded)?;
    assert!(
        retrieval_json.get("request").is_none(),
        "get_solve_result must not expose the stored request"
    );
    assert!(
        retrieval_json.get("created_at_wall").is_none(),
        "get_solve_result must return the result envelope, not artifact metadata"
    );

    Ok(())
}

#[test]
fn get_solve_result_rejects_traversal_before_filesystem_access() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());

    for solve_id in [
        "../outside",
        "sol_0000000000000000/../../outside",
        "/tmp/sol_0000000000000000",
        "sol_ABCDEF0123456789",
        "sol_0123456789abcde",
    ] {
        let error = service
            .get_solve_result(solve_id)
            .expect_err("malformed solve_id must be rejected");
        assert!(matches!(
            error,
            SolverServiceError::Persistence(PersistError::InvalidSolveId { .. })
        ));
    }

    assert!(!repo.path().join(".spur/solver").exists());
    Ok(())
}

#[test]
fn persistence_requires_an_explicit_repo_root() {
    let service = SolverService::new();
    let error = service
        .get_solve_result("sol_0000000000000000")
        .expect_err("an unconfigured service must not use the process cwd");
    assert!(matches!(error, SolverServiceError::RepoRootNotConfigured));
}

#[test]
fn well_formed_missing_solve_id_has_a_distinct_error() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());
    let error = service
        .get_solve_result("sol_0000000000000000")
        .expect_err("a missing solve_id must not be reported as malformed");
    assert!(matches!(
        error,
        SolverServiceError::Persistence(PersistError::SolveIdNotFound { .. })
    ));
    Ok(())
}

#[test]
fn get_solve_result_rejects_corrupt_artifact_json() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let artifact_dir = repo.path().join(".spur/solver");
    fs::create_dir_all(&artifact_dir)?;
    fs::write(artifact_dir.join("sol_0000000000000000.json"), b"{not-json")?;

    let service = SolverService::new().with_repo_root(repo.path());
    let error = service
        .get_solve_result("sol_0000000000000000")
        .expect_err("corrupt artifact JSON must be rejected");
    assert!(matches!(
        error,
        SolverServiceError::Persistence(PersistError::Json {
            operation: "parse",
            ..
        })
    ));
    Ok(())
}

#[test]
fn get_solve_result_rejects_oversized_artifact_before_parsing() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let artifact_dir = repo.path().join(".spur/solver");
    fs::create_dir_all(&artifact_dir)?;
    File::create(artifact_dir.join("sol_0000000000000000.json"))?
        .set_len(MAX_ARTIFACT_BYTES + 1)?;

    let service = SolverService::new().with_repo_root(repo.path());
    let error = service
        .get_solve_result("sol_0000000000000000")
        .expect_err("oversized artifact must be rejected before JSON allocation");
    assert!(matches!(
        error,
        SolverServiceError::Persistence(PersistError::ArtifactTooLarge { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn solve_constraints_persists_when_requested() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::with_runner(Arc::new(UnsatRunner)).with_repo_root(repo.path());

    let response = service.solve_constraints(request(true)).await?;
    let solve_id = response
        .solve_id
        .as_deref()
        .ok_or_else(|| std::io::Error::other("persisted response omitted solve_id"))?;
    assert_valid_solve_id(solve_id);

    let loaded = service.get_solve_result(solve_id)?;
    assert_eq!(loaded.result.status, SolveStatus::Unsat);
    assert_eq!(loaded.result.duration_ms, response.duration_ms);
    assert_eq!(loaded.result.reason, response.reason);

    Ok(())
}

#[tokio::test]
async fn solve_constraints_does_not_touch_cache_when_persist_is_false() -> Result<(), Box<dyn Error>>
{
    let repo = tempdir()?;
    let service = SolverService::with_runner(Arc::new(UnsatRunner)).with_repo_root(repo.path());

    let response = service.solve_constraints(request(false)).await?;
    assert!(
        response.solve_id.is_none(),
        "ephemeral solve must omit solve_id"
    );
    assert!(
        !repo.path().join(".spur/solver").exists(),
        "ephemeral solve must not create the artifact directory"
    );
    Ok(())
}

#[test]
fn persist_evicts_oldest_when_artifact_count_is_full() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let artifact_dir = repo.path().join(".spur/solver");
    fs::create_dir_all(&artifact_dir)?;
    for index in 0..MAX_ARTIFACTS {
        // Fixed-width fractional seconds so lexical order matches creation order.
        let body = format!("{{\n  \"created_at_wall\": \"2026-01-01T00:00:00.{index:09}Z\"\n}}\n");
        fs::write(artifact_dir.join(format!("sol_{index:016x}.json")), body)?;
    }
    let oldest = artifact_dir.join(format!("sol_{:016x}.json", 0));
    assert!(oldest.is_file());

    let service = SolverService::new().with_repo_root(repo.path());
    let artifact = service.persist(&request(false), &unsat_response())?;

    assert!(
        !oldest.exists(),
        "oldest artifact must be ring-evicted to free a count slot"
    );
    assert!(artifact_dir
        .join(format!("{}.json", artifact.solve_id))
        .is_file());
    let remaining = count_json_artifacts(&artifact_dir)?;
    assert_eq!(
        remaining, MAX_ARTIFACTS,
        "ring must keep the cache at the count cap after eviction + write"
    );

    Ok(())
}

#[test]
fn independent_services_share_the_repository_quota_lock() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let artifact_dir = repo.path().join(".spur/solver");
    fs::create_dir_all(&artifact_dir)?;
    // Start full so both writers must serialize through the repo lock and
    // ring-evict under that lock without racing past MAX_ARTIFACTS.
    for index in 0..MAX_ARTIFACTS {
        let body = format!("{{\n  \"created_at_wall\": \"2026-01-01T00:00:00.{index:09}Z\"\n}}\n");
        fs::write(artifact_dir.join(format!("sol_{index:016x}.json")), body)?;
    }

    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _worker in 0..2 {
        let service = SolverService::new().with_repo_root(repo.path());
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            barrier.wait();
            service.persist(&request(false), &unsat_response())
        }));
    }
    barrier.wait();

    let mut persisted = 0;
    for worker in workers {
        let result = match worker.join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        match result {
            Ok(_artifact) => persisted += 1,
            Err(error) => return Err(error.into()),
        }
    }
    assert_eq!(
        persisted, 2,
        "both services must complete under the shared lock"
    );
    let remaining = count_json_artifacts(&artifact_dir)?;
    assert!(
        remaining <= MAX_ARTIFACTS,
        "concurrent ring writes must not exceed the count cap (got {remaining})"
    );
    assert!(
        artifact_dir.join(".lock").is_file(),
        "repository-scoped quota enforcement requires a filesystem lock"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn persisted_artifact_and_lock_are_private_on_unix() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());
    let artifact = service.persist(&request(false), &unsat_response())?;
    let artifact_dir = repo.path().join(".spur/solver");
    let artifact_path = artifact_dir.join(format!("{}.json", artifact.solve_id));

    assert_eq!(
        fs::metadata(&artifact_path)?.permissions().mode() & 0o777,
        0o600,
        "persisted formulas and models must be private"
    );
    assert_eq!(
        fs::metadata(artifact_dir.join(".lock"))?
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the repository lock must be private"
    );
    Ok(())
}

#[test]
fn persist_evicts_until_byte_budget_fits() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let artifact_dir = repo.path().join(".spur/solver");
    fs::create_dir_all(&artifact_dir)?;
    // One oversized occupant that leaves no room for a real JSON artifact
    // until it is ring-evicted.
    File::create(artifact_dir.join("sol_0000000000000001.json"))?.set_len(MAX_ARTIFACT_BYTES)?;

    let service = SolverService::new().with_repo_root(repo.path());
    let artifact = service.persist(&request(false), &unsat_response())?;

    assert!(
        !artifact_dir.join("sol_0000000000000001.json").exists(),
        "byte-budget pressure must evict the oversized occupant"
    );
    assert!(artifact_dir
        .join(format!("{}.json", artifact.solve_id))
        .is_file());

    Ok(())
}

#[test]
fn persist_rejects_single_artifact_larger_than_byte_budget() -> Result<(), Box<dyn Error>> {
    let repo = tempdir()?;
    let service = SolverService::new().with_repo_root(repo.path());

    // Build a response whose serialized form exceeds the whole cache budget
    // by stuffing an enormous reason string.
    let huge_reason = "x".repeat((MAX_ARTIFACT_BYTES as usize) + 1);
    let response = SolveConstraintsResponse {
        status: SolveStatus::Unsat,
        model: None,
        duration_ms: 1,
        solve_id: None,
        reason: Some(huge_reason),
        smt: None,
        unsat_core: None,
        cached: false,
        session_id: None,
        optimization: None,
        solver_version: None,
    };

    let error = service
        .persist(&request(false), &response)
        .expect_err("a single oversized artifact must still be rejected");
    assert!(matches!(
        error,
        SolverServiceError::Persistence(PersistError::QuotaExceeded {
            kind: ArtifactQuotaKind::TotalBytes,
            ..
        })
    ));
    Ok(())
}

fn request(persist: bool) -> SolveConstraintsRequest {
    SolveConstraintsRequest {
        vars: Vec::new(),
        constraints: Vec::new(),
        objectives: vec![],
        objective_priority: Default::default(),
        max_solutions: DEFAULT_MAX_SOLUTIONS,
        use_cache: true,
        session_id: None,
        session_op: Default::default(),
        timeout_ms: DEFAULT_TIMEOUT_MS,
        persist,
        include_smt: false,
    }
}

fn unsat_response() -> SolveConstraintsResponse {
    SolveConstraintsResponse {
        status: SolveStatus::Unsat,
        model: None,
        duration_ms: 12,
        solve_id: None,
        reason: None,
        smt: None,
        unsat_core: None,
        cached: false,
        session_id: None,
        optimization: None,
        solver_version: None,
    }
}

fn assert_valid_solve_id(solve_id: &str) {
    let Some(hex) = solve_id.strip_prefix("sol_") else {
        panic!("solve_id must start with sol_: {solve_id}");
    };
    assert_eq!(hex.len(), 16, "solve_id must contain exactly 16 hex digits");
    assert!(
        hex.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "solve_id suffix must contain only lowercase hexadecimal digits"
    );
}

fn count_json_artifacts(artifact_dir: &std::path::Path) -> Result<usize, Box<dyn Error>> {
    let mut count = 0;
    for entry in fs::read_dir(artifact_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json") {
            count += 1;
        }
    }
    Ok(count)
}
