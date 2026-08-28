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
const ISOLATED_TEST_MODE: &str = "SPUR_CODE_EVAL_ISOLATED_TEST_MODE";

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

fn is_isolated_test(mode: &str) -> bool {
    matches!(std::env::var(ISOLATED_TEST_MODE), Ok(value) if value == mode)
}

fn required_env_path(name: &str) -> PathBuf {
    PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("missing isolated-test path {name}")),
    )
}

fn required_env_string(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing isolated-test value {name}"))
}

#[cfg(unix)]
fn run_concurrent_materialization_child(
    mode: &str,
    base_env: &str,
    commit_env: &str,
    hash_env: &str,
    repository_env: &str,
) {
    use std::sync::{Arc, Barrier};

    let repository = required_env_path(repository_env);
    let commit = required_env_string(commit_env);
    let case = Arc::new(code_eval_case_with_hash(
        mode,
        &repository,
        &commit,
        required_env_string(hash_env),
    ));
    let materializer = Arc::new(Materializer::new(required_env_path(base_env)).unwrap());
    let start = Arc::new(Barrier::new(3));
    let callers = (0..2)
        .map(|_| {
            let case = Arc::clone(&case);
            let materializer = Arc::clone(&materializer);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                materializer.materialize(&case)
            })
        })
        .collect::<Vec<_>>();

    start.wait();
    let roots = callers
        .into_iter()
        .map(|caller| caller.join().unwrap().unwrap())
        .collect::<Vec<_>>();
    let expected_root = materializer.root_for(&case);

    assert_eq!(
        roots[0].root(),
        expected_root,
        "the first caller must return the promoted root"
    );
    assert_eq!(
        roots[1].root(),
        expected_root,
        "the second caller must validate and return the same root"
    );
    let entries = fs::read_dir(expected_root.parent().unwrap())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "the losing temporary root must be removed"
    );
    assert_eq!(
        entries[0].path(),
        expected_root,
        "only the promoted root may remain below the base"
    );
}

#[test]
fn git_commands_ignore_ambient_repository_selection() {
    const MODE: &str = "ambient-git";
    const BASE_ENV: &str = "SPUR_CODE_EVAL_TEST_BASE";
    const COMMIT_ENV: &str = "SPUR_CODE_EVAL_TEST_COMMIT";
    const HASH_ENV: &str = "SPUR_CODE_EVAL_TEST_HASH";
    const REPOSITORY_ENV: &str = "SPUR_CODE_EVAL_TEST_REPOSITORY";
    const ROOT_ENV: &str = "SPUR_CODE_EVAL_TEST_ROOT";

    if is_isolated_test(MODE) {
        let repository = required_env_path(REPOSITORY_ENV);
        let commit = required_env_string(COMMIT_ENV);
        let case =
            code_eval_case_with_hash(MODE, &repository, &commit, required_env_string(HASH_ENV));
        let materializer = Materializer::new(required_env_path(BASE_ENV)).unwrap();
        let expected_root = required_env_path(ROOT_ENV).canonicalize().unwrap();

        let root = materializer
            .verify_existing(&case, required_env_path(ROOT_ENV))
            .unwrap();

        assert_eq!(root.root(), expected_root);
        return;
    }

    let fixture = TempDirectory::new("ambient-git");
    let (source_repository, commit) = repository(
        fixture.path(),
        "repository",
        "fn selected_repository() {}\n",
    );
    let materialization_hash = compute_materialization_hash(&source_repository, None).unwrap();
    let case = code_eval_case_with_hash(
        MODE,
        &source_repository,
        &commit,
        materialization_hash.clone(),
    );
    let base = fixture.path().join("materialized");
    let materializer = Materializer::new(&base).unwrap();
    let root = materializer.materialize(&case).unwrap();
    let (hostile_repository, _) = repository(
        fixture.path(),
        "hostile-repository",
        "fn unrelated_worktree() {}\n",
    );
    git(
        &hostile_repository,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/hostile.git",
        ],
    );

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "git_commands_ignore_ambient_repository_selection",
            "--nocapture",
        ])
        .env(ISOLATED_TEST_MODE, MODE)
        .env(BASE_ENV, &base)
        .env(COMMIT_ENV, &commit)
        .env(HASH_ENV, &materialization_hash)
        .env(REPOSITORY_ENV, &source_repository)
        .env(ROOT_ENV, root.root())
        .env("GIT_DIR", hostile_repository.join(".git"))
        .env("GIT_WORK_TREE", &hostile_repository)
        .env("GIT_COMMON_DIR", hostile_repository.join(".git"))
        .env("GIT_INDEX_FILE", hostile_repository.join(".git/index"))
        .env(
            "GIT_OBJECT_DIRECTORY",
            hostile_repository.join(".git/objects"),
        )
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            hostile_repository.join(".git/objects"),
        )
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "remote.origin.url")
        .env("GIT_CONFIG_VALUE_0", "https://example.invalid/injected.git")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "isolated ambient-Git regression failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn concurrent_same_case_materialization_returns_valid_winner() {
    use std::os::unix::fs::PermissionsExt as _;

    const MODE: &str = "concurrent-same-case";
    const BASE_ENV: &str = "SPUR_CODE_EVAL_TEST_BASE";
    const COMMIT_ENV: &str = "SPUR_CODE_EVAL_TEST_COMMIT";
    const HASH_ENV: &str = "SPUR_CODE_EVAL_TEST_HASH";
    const REPOSITORY_ENV: &str = "SPUR_CODE_EVAL_TEST_REPOSITORY";

    if is_isolated_test(MODE) {
        run_concurrent_materialization_child(MODE, BASE_ENV, COMMIT_ENV, HASH_ENV, REPOSITORY_ENV);
        return;
    }

    let fixture = TempDirectory::new("concurrent-same-case");
    let (source_repository, commit) = repository(
        fixture.path(),
        "repository",
        "fn concurrent_materialization() {}\n",
    );
    let materialization_hash = compute_materialization_hash(&source_repository, None).unwrap();
    let binary_directory = fixture.path().join("bin");
    let rendezvous = fixture.path().join("git-rendezvous");
    fs::create_dir(&binary_directory).unwrap();
    fs::create_dir(&rendezvous).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git executable must be present on PATH");
    let git_shim = binary_directory.join("git");
    fs::write(
        &git_shim,
        format!(
            "#!/bin/sh\nfor argument in \"$@\"; do\n  if [ \"$argument\" = clone ]; then\n    if mkdir '{rendezvous}/first' 2>/dev/null; then\n      while [ ! -e '{rendezvous}/second' ]; do sleep 0.01; done\n    else\n      : > '{rendezvous}/second'\n    fi\n    break\n  fi\ndone\nexec '{real_git}' \"$@\"\n",
            rendezvous = rendezvous.display(),
            real_git = real_git.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&git_shim, fs::Permissions::from_mode(0o755)).unwrap();
    let child_path = std::env::join_paths(
        std::iter::once(binary_directory)
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let base = fixture.path().join("materialized");

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "concurrent_same_case_materialization_returns_valid_winner",
            "--nocapture",
        ])
        .env(ISOLATED_TEST_MODE, MODE)
        .env(BASE_ENV, &base)
        .env(COMMIT_ENV, &commit)
        .env(HASH_ENV, &materialization_hash)
        .env(REPOSITORY_ENV, &source_repository)
        .env("PATH", child_path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "isolated concurrent-materialization regression failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rendezvous.join("first").is_dir());
    assert!(rendezvous.join("second").is_file());
}

#[cfg(unix)]
#[test]
fn rejects_materialization_root_symlink_outside_base() {
    use std::os::unix::fs::symlink;

    let fixture = TempDirectory::new("root-outside-base");
    let (repository, commit) = repository(
        fixture.path(),
        "repository",
        "fn externally_materialized() {}\n",
    );
    let case = code_eval_case("root-outside-base", &repository, &commit);
    let external_materializer =
        Materializer::new(fixture.path().join("external-materializations")).unwrap();
    let external_root = external_materializer.materialize(&case).unwrap();
    let materializer = Materializer::new(fixture.path().join("materialized")).unwrap();
    let linked_root = materializer.root_for(&case);
    symlink(external_root.root(), &linked_root).unwrap();

    let error = materializer
        .verify_existing(&case, &linked_root)
        .unwrap_err();

    assert!(matches!(error, MaterializeError::RootOutsideBase { .. }));
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
    let root = materializer.root_for(&case);
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
