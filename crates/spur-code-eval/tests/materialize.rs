use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::json;
use spur_code_eval::{
    compute_materialization_hash, CaseStatus, CodeEvalCase, ContentPin, GoldEvidence, Language,
    MaterializeError, Materializer, QueryPolicy, RepositoryPin, SourceIdentity, Suite,
};

static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "spur-code-eval-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn git(current_dir: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(current_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(parent: &Path, name: &str, contents: &str) -> (PathBuf, String) {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    git(&path, &["init", "."]);
    git(&path, &["config", "user.name", "SPUR Test"]);
    git(
        &path,
        &["config", "user.email", "spur-test@example.invalid"],
    );
    fs::write(path.join("source.rs"), contents).unwrap();
    git(&path, &["add", "--", "source.rs"]);
    git(&path, &["commit", "-m", "fixture"]);
    let commit = git(&path, &["rev-parse", "HEAD"]);
    (path, commit)
}

fn code_eval_case(case_id: &str, repository: &Path, commit: &str) -> CodeEvalCase {
    let materialization_hash = compute_materialization_hash(repository, None).unwrap();
    code_eval_case_with_hash(case_id, repository, commit, materialization_hash)
}

fn code_eval_case_with_hash(
    case_id: &str,
    repository: &Path,
    commit: &str,
    materialization_hash: String,
) -> CodeEvalCase {
    code_eval_case_with_repository_pin(case_id, repository, commit, None, materialization_hash)
}

fn code_eval_case_with_repository_pin(
    case_id: &str,
    repository: &Path,
    commit: &str,
    subdirectory: Option<String>,
    materialization_hash: String,
) -> CodeEvalCase {
    CodeEvalCase::new(
        Suite::RepoQa,
        case_id,
        Language::new("rust").unwrap(),
        ContentPin::new(
            "https://example.invalid/dataset.jsonl",
            format!("dataset-{case_id}"),
            format!("sha256:dataset-{case_id}"),
            "MIT",
        )
        .unwrap(),
        RepositoryPin::new(
            repository.canonicalize().unwrap().to_string_lossy(),
            commit,
            subdirectory,
            materialization_hash,
        )
        .unwrap(),
        QueryPolicy::new("explain source", "sha256:query-policy").unwrap(),
        GoldEvidence::new(
            vec![SourceIdentity::new("source.rs", 0, 1, None).unwrap()],
            Vec::new(),
        )
        .unwrap(),
        CaseStatus::eligible(),
        json!({"fixture": case_id}),
    )
    .unwrap()
}

fn with_dataset_hash(case: &CodeEvalCase, dataset_hash: &str) -> CodeEvalCase {
    CodeEvalCase::new(
        case.suite(),
        case.case_id(),
        case.language().clone(),
        ContentPin::new(
            case.dataset_pin().uri(),
            case.dataset_pin().revision(),
            dataset_hash,
            case.dataset_pin().license(),
        )
        .unwrap(),
        case.repository_pin().clone(),
        case.query_policy().clone(),
        case.gold_evidence().clone(),
        case.status().clone(),
        case.raw_upstream().clone(),
    )
    .unwrap()
}

#[test]
fn rejects_cross_case_repository_root_reuse() {
    let fixture = TempDirectory::new("mixed-root");
    let (repository_a, commit_a) = repository(fixture.path(), "repository-a", "fn a() {}\n");
    let (repository_b, commit_b) = repository(fixture.path(), "repository-b", "fn b() {}\n");
    let case_a = code_eval_case("case-a", &repository_a, &commit_a);
    let case_b = code_eval_case("case-b", &repository_b, &commit_b);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root_a = materializer.materialize(&case_a).unwrap();
    let root_b = materializer.materialize(&case_b).unwrap();

    assert_ne!(root_a.root(), root_b.root());

    let error = materializer
        .verify_existing(&case_b, root_a.root())
        .unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::MixedRepositoryRoot { .. }
    ));
}

#[test]
fn rejects_origin_mismatch() {
    let fixture = TempDirectory::new("origin-mismatch");
    let (repository, commit) = repository(fixture.path(), "repository", "fn origin() {}\n");
    let case = code_eval_case("origin", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    git(
        root.repository_root(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/wrong-origin.git",
        ],
    );

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(error, MaterializeError::OriginMismatch { .. }));
}

#[test]
fn rejects_head_revision_mismatch() {
    let fixture = TempDirectory::new("head-mismatch");
    let (repository, first_commit) =
        repository(fixture.path(), "repository", "fn revision_one() {}\n");
    fs::write(repository.join("source.rs"), "fn revision_two() {}\n").unwrap();
    git(&repository, &["commit", "-am", "second fixture"]);
    let second_commit = git(&repository, &["rev-parse", "HEAD"]);
    let case = code_eval_case("head", &repository, &second_commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    git(
        root.repository_root(),
        &["checkout", "--detach", &first_commit],
    );

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(error, MaterializeError::HeadMismatch { .. }));
}

#[test]
fn rejects_attached_head_at_the_pinned_revision() {
    let fixture = TempDirectory::new("attached-head");
    let (repository, commit) = repository(fixture.path(), "repository", "fn attached() {}\n");
    let case = code_eval_case("attached", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    git(
        root.repository_root(),
        &["checkout", "-B", "materialized-branch"],
    );

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(error, MaterializeError::AttachedHead { .. }));
}

#[test]
fn rejects_dirty_checkout() {
    let fixture = TempDirectory::new("dirty-checkout");
    let (repository, commit) = repository(fixture.path(), "repository", "fn clean() {}\n");
    let case = code_eval_case("dirty", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    fs::write(
        root.repository_root().join("source.rs"),
        "fn modified() {}\n",
    )
    .unwrap();

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(error, MaterializeError::DirtyCheckout { .. }));
}

#[test]
fn rejects_declared_content_hash_mismatch_before_promotion() {
    let fixture = TempDirectory::new("content-hash-mismatch");
    let (repository, commit) = repository(fixture.path(), "repository", "fn hashed() {}\n");
    let case = code_eval_case_with_hash(
        "content-hash",
        &repository,
        &commit,
        format!("sha256:{}", "0".repeat(64)),
    );
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let final_root = materializer.root_for(&case);

    let error = materializer.materialize(&case).unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::ContentHashMismatch { .. }
    ));
    assert!(!final_root.exists());
}

#[test]
fn rejects_reuse_under_a_different_declared_dataset_hash() {
    let fixture = TempDirectory::new("dataset-hash-mismatch");
    let (repository, commit) = repository(fixture.path(), "repository", "fn dataset() {}\n");
    let case = code_eval_case("dataset-hash", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    let altered_case = with_dataset_hash(&case, "sha256:altered-dataset");

    let error = materializer
        .verify_existing(&altered_case, root.root())
        .unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::DatasetHashMismatch { .. }
    ));
}

#[test]
fn interrupted_temporary_checkout_does_not_block_atomic_materialization() {
    let fixture = TempDirectory::new("interrupted-checkout");
    let (repository, commit) = repository(fixture.path(), "repository", "fn recovered() {}\n");
    let case = code_eval_case("recovered", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let final_root = materializer.root_for(&case);
    let stale_root = final_root.parent().unwrap().join(format!(
        ".{}.tmp",
        final_root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir(&stale_root).unwrap();
    fs::write(stale_root.join("partial-checkout"), b"interrupted").unwrap();

    let root = materializer.materialize(&case).unwrap();

    assert_eq!(root.root(), final_root);
    assert!(root.repository_root().join("source.rs").is_file());
    assert!(stale_root.is_dir());
}

#[test]
fn rejects_missing_pinned_subdirectory() {
    let fixture = TempDirectory::new("missing-subdirectory");
    let (repository, commit) = repository(fixture.path(), "repository", "fn root() {}\n");
    let materialization_hash =
        compute_materialization_hash(&repository, Some("missing/subtree")).unwrap();
    let case = code_eval_case_with_repository_pin(
        "missing-subdirectory",
        &repository,
        &commit,
        Some("missing/subtree".to_owned()),
        materialization_hash,
    );
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();

    let error = materializer.materialize(&case).unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::MissingSubdirectory { .. }
    ));
}

#[test]
fn rejects_tampered_audit_content_hash() {
    let fixture = TempDirectory::new("metadata-content-hash");
    let (repository, commit) = repository(fixture.path(), "repository", "fn metadata() {}\n");
    let case = code_eval_case("metadata-content-hash", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    let metadata_path = root.root().join(".spur-code-eval-materialization.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["materialization_hash"] = json!(format!("sha256:{}", "f".repeat(64)));
    fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::ContentHashMismatch { .. }
    ));
}

#[test]
fn rejects_tampered_audit_origin() {
    let fixture = TempDirectory::new("metadata-origin");
    let (repository, commit) = repository(fixture.path(), "repository", "fn metadata() {}\n");
    let case = code_eval_case("metadata-origin", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = materializer.materialize(&case).unwrap();
    let metadata_path = root.root().join(".spur-code-eval-materialization.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["repository_uri"] = json!("https://example.invalid/tampered-origin.git");
    fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let error = materializer
        .verify_existing(&case, root.root())
        .unwrap_err();

    assert!(matches!(error, MaterializeError::OriginMismatch { .. }));
}

#[cfg(unix)]
#[test]
fn rejects_pinned_subdirectory_symlink_outside_repository() {
    use std::os::unix::fs::symlink;

    let fixture = TempDirectory::new("escaping-subdirectory");
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("outside.rs"), "fn outside() {}\n").unwrap();
    let (repository, _) = repository(fixture.path(), "repository", "fn root() {}\n");
    symlink(&outside, repository.join("subtree")).unwrap();
    git(&repository, &["add", "--", "subtree"]);
    git(&repository, &["commit", "-m", "add escaping subtree"]);
    let commit = git(&repository, &["rev-parse", "HEAD"]);
    let materialization_hash = compute_materialization_hash(&repository, Some("subtree")).unwrap();
    let case = code_eval_case_with_repository_pin(
        "escaping-subdirectory",
        &repository,
        &commit,
        Some("subtree".to_owned()),
        materialization_hash,
    );
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();

    let error = materializer.materialize(&case).unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::SubdirectoryEscapesRoot { .. }
    ));
}

#[test]
fn rejects_parent_traversal_subdirectory_before_git() {
    let fixture = TempDirectory::new("parent-traversal");
    let (repository, commit) = repository(fixture.path(), "repository", "fn safe() {}\n");
    let case = code_eval_case_with_repository_pin(
        "parent-traversal",
        &repository,
        &commit,
        Some("../outside".to_owned()),
        "sha256:not-reached".to_owned(),
    );
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();

    let error = materializer.materialize(&case).unwrap_err();

    assert!(matches!(
        error,
        MaterializeError::InvalidSubdirectory { .. }
    ));
}

#[test]
fn materializes_the_pinned_subdirectory_as_the_source_root() {
    let fixture = TempDirectory::new("pinned-subdirectory");
    let (repository, _) = repository(fixture.path(), "repository", "fn root() {}\n");
    fs::create_dir(repository.join("nested")).unwrap();
    fs::write(repository.join("nested/lib.rs"), "fn nested() {}\n").unwrap();
    git(&repository, &["add", "--", "nested/lib.rs"]);
    git(&repository, &["commit", "-m", "add nested source"]);
    let commit = git(&repository, &["rev-parse", "HEAD"]);
    let materialization_hash = compute_materialization_hash(&repository, Some("nested")).unwrap();
    let case = code_eval_case_with_repository_pin(
        "pinned-subdirectory",
        &repository,
        &commit,
        Some("nested".to_owned()),
        materialization_hash,
    );
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();

    let root = materializer.materialize(&case).unwrap();

    assert_eq!(
        root.source_root(),
        root.repository_root()
            .join("nested")
            .canonicalize()
            .unwrap()
    );
    assert!(root.source_root().join("lib.rs").is_file());
}

#[test]
fn tracked_tree_hash_is_deterministic_and_content_sensitive() {
    let fixture = TempDirectory::new("tree-hash");
    let (repository, _) = repository(fixture.path(), "repository", "fn first() {}\n");
    let first = compute_materialization_hash(&repository, None).unwrap();
    let repeated = compute_materialization_hash(&repository, None).unwrap();
    fs::write(repository.join("source.rs"), "fn second() {}\n").unwrap();
    git(&repository, &["commit", "-am", "change tracked bytes"]);
    let second = compute_materialization_hash(&repository, None).unwrap();

    assert_eq!(first, repeated);
    assert_ne!(first, second);
}

#[test]
fn missing_and_invalid_audit_metadata_are_typed() {
    let fixture = TempDirectory::new("metadata-errors");
    let (repository, commit) = repository(fixture.path(), "repository", "fn metadata() {}\n");
    let case = code_eval_case("metadata-errors", &repository, &commit);
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let root = fixture.path().join("arbitrary-root");
    fs::create_dir(&root).unwrap();

    let missing = materializer.verify_existing(&case, &root).unwrap_err();
    assert!(matches!(missing, MaterializeError::MissingMetadata { .. }));

    fs::write(
        root.join(".spur-code-eval-materialization.json"),
        b"not-json",
    )
    .unwrap();
    let invalid = materializer.verify_existing(&case, &root).unwrap_err();
    assert!(matches!(invalid, MaterializeError::InvalidMetadata { .. }));
}
