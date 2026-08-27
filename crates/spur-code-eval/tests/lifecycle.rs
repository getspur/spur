use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use spur_code_eval::{
    content_sha256, ArtifactError, ArtifactKind, ArtifactStore, RunManifest, RunPhase,
};

static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "spur-code-eval-artifacts-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run_root(&self) -> PathBuf {
        self.0.join("run")
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TreeEntry {
    relative_path: PathBuf,
    is_directory: bool,
    len: u64,
    readonly: bool,
    modified: Option<SystemTime>,
    bytes: Vec<u8>,
}

fn snapshot_tree(root: &Path) -> Vec<TreeEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<TreeEntry>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let is_directory = metadata.is_dir();
        entries.push(TreeEntry {
            relative_path: path.strip_prefix(root).unwrap().to_path_buf(),
            is_directory,
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
            modified: metadata.modified().ok(),
            bytes: if metadata.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            },
        });
        if is_directory {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

fn store(root: &Path) -> ArtifactStore {
    ArtifactStore::create(root, RunManifest::new("fixture-run").unwrap()).unwrap()
}

#[test]
fn artifact_kinds_have_stable_canonical_paths() {
    assert_eq!(ArtifactKind::Manifest.relative_path(), "manifest.json");
    assert_eq!(ArtifactKind::Validation.relative_path(), "validation.json");
    assert_eq!(ArtifactKind::Rankings.relative_path(), "rankings.jsonl");
    assert_eq!(ArtifactKind::Contexts.relative_path(), "contexts.jsonl");
    assert_eq!(
        ArtifactKind::CallGraphs.relative_path(),
        "call-graphs.jsonl"
    );
    assert_eq!(ArtifactKind::Metrics.relative_path(), "metrics.json");
    assert_eq!(
        ArtifactKind::ModelRecords.relative_path(),
        "model-records.jsonl"
    );
    assert_eq!(ArtifactKind::Logs.relative_path(), "run.log");
    assert_eq!(ArtifactKind::Checksums.relative_path(), "checksums.json");
}

#[test]
fn legal_lifecycle_reaches_model_scored_and_seals_each_stage() {
    let fixture = TempDirectory::new("legal-lifecycle");
    let root = fixture.run_root();
    let mut store = store(&root);

    assert_eq!(store.manifest().phase(), RunPhase::Prepared);
    store
        .write_atomic(ArtifactKind::Rankings, b"{\"case\":\"one\"}\n")
        .unwrap();
    store.freeze().unwrap();
    assert_eq!(store.manifest().phase(), RunPhase::Frozen);

    store
        .write_atomic(ArtifactKind::Metrics, b"{\"hit_at_1\":1.0}\n")
        .unwrap();
    store.transition(RunPhase::DeterministicScored).unwrap();
    assert_eq!(store.manifest().phase(), RunPhase::DeterministicScored);

    store
        .write_atomic(
            ArtifactKind::ModelRecords,
            b"{\"case\":\"one\",\"status\":\"complete\"}\n",
        )
        .unwrap();
    store.transition(RunPhase::ModelScored).unwrap();
    assert_eq!(store.manifest().phase(), RunPhase::ModelScored);

    for kind in [
        ArtifactKind::Rankings,
        ArtifactKind::Metrics,
        ArtifactKind::ModelRecords,
    ] {
        assert!(store.manifest().artifact(kind).unwrap().is_frozen());
        assert!(fs::metadata(store.artifact_path(kind))
            .unwrap()
            .permissions()
            .readonly());
    }
}

#[test]
fn out_of_order_transition_rejects_before_tree_metadata_changes() {
    let fixture = TempDirectory::new("out-of-order");
    let root = fixture.run_root();
    let mut store = store(&root);
    let before = snapshot_tree(&root);

    let error = store.transition(RunPhase::DeterministicScored).unwrap_err();

    assert!(matches!(
        error,
        ArtifactError::InvalidTransition {
            from: RunPhase::Prepared,
            to: RunPhase::DeterministicScored,
        }
    ));
    assert_eq!(snapshot_tree(&root), before);
}

#[test]
fn every_out_of_order_transition_rejects_without_changing_the_tree() {
    for (phase, rejected) in [
        (
            RunPhase::Prepared,
            vec![
                RunPhase::Prepared,
                RunPhase::DeterministicScored,
                RunPhase::ModelScored,
            ],
        ),
        (
            RunPhase::Frozen,
            vec![RunPhase::Prepared, RunPhase::Frozen, RunPhase::ModelScored],
        ),
        (
            RunPhase::DeterministicScored,
            vec![
                RunPhase::Prepared,
                RunPhase::Frozen,
                RunPhase::DeterministicScored,
            ],
        ),
        (
            RunPhase::ModelScored,
            vec![
                RunPhase::Prepared,
                RunPhase::Frozen,
                RunPhase::DeterministicScored,
                RunPhase::ModelScored,
            ],
        ),
    ] {
        let fixture = TempDirectory::new("all-out-of-order");
        let root = fixture.run_root();
        let mut store = store(&root);
        advance_to(&mut store, phase);
        let before = snapshot_tree(&root);

        for target in rejected {
            let error = store.transition(target).unwrap_err();
            assert!(matches!(
                error,
                ArtifactError::InvalidTransition { from, to }
                    if from == phase && to == target
            ));
            assert_eq!(snapshot_tree(&root), before);
        }

        if phase != RunPhase::Prepared {
            let error = store.freeze().unwrap_err();
            assert!(matches!(
                error,
                ArtifactError::InvalidTransition {
                    from,
                    to: RunPhase::Frozen,
                } if from == phase
            ));
            assert_eq!(snapshot_tree(&root), before);
        }
    }
}

fn advance_to(store: &mut ArtifactStore, phase: RunPhase) {
    if phase >= RunPhase::Frozen {
        store.freeze().unwrap();
    }
    if phase >= RunPhase::DeterministicScored {
        store.write_atomic(ArtifactKind::Metrics, b"{}\n").unwrap();
        store.transition(RunPhase::DeterministicScored).unwrap();
    }
    if phase >= RunPhase::ModelScored {
        store
            .write_atomic(ArtifactKind::ModelRecords, b"{}\n")
            .unwrap();
        store.transition(RunPhase::ModelScored).unwrap();
    }
}

#[test]
fn every_frozen_deterministic_input_rejects_mutation_without_tree_changes() {
    let fixture = TempDirectory::new("immutable-ranking");
    let root = fixture.run_root();
    let mut store = store(&root);
    for (kind, bytes) in [
        (ArtifactKind::Validation, b"validation\n".as_slice()),
        (ArtifactKind::Rankings, b"rankings\n".as_slice()),
        (ArtifactKind::Contexts, b"contexts\n".as_slice()),
        (ArtifactKind::CallGraphs, b"call graphs\n".as_slice()),
        (ArtifactKind::Logs, b"logs\n".as_slice()),
    ] {
        store.write_atomic(kind, bytes).unwrap();
    }
    store.freeze().unwrap();
    let before = snapshot_tree(&root);

    for kind in [
        ArtifactKind::Validation,
        ArtifactKind::Rankings,
        ArtifactKind::Contexts,
        ArtifactKind::CallGraphs,
        ArtifactKind::Logs,
    ] {
        let error = store.write_atomic(kind, b"replacement\n").unwrap_err();
        assert!(matches!(
            error,
            ArtifactError::MutationForbidden {
                kind: rejected,
                phase: RunPhase::Frozen,
            } if rejected == kind
        ));
        assert_eq!(snapshot_tree(&root), before);
    }
    assert_eq!(
        fs::read(store.artifact_path(ArtifactKind::Rankings)).unwrap(),
        b"rankings\n"
    );
}

#[test]
fn opening_after_a_crash_recovers_a_manifest_recorded_pending_artifact() {
    let fixture = TempDirectory::new("open-recovers");
    let root = fixture.run_root();
    let mut store = store(&root);
    let contents = b"{\"case\":\"pending-after-manifest\"}\n";
    let digest = content_sha256(contents);
    store
        .write_atomic(ArtifactKind::Rankings, contents)
        .unwrap();
    let target = store.artifact_path(ArtifactKind::Rankings);
    let pending = store.pending_path(ArtifactKind::Rankings, &digest).unwrap();
    fs::rename(&target, &pending).unwrap();
    drop(store);

    let recovered = ArtifactStore::open(&root).unwrap();

    assert_eq!(
        fs::read(recovered.artifact_path(ArtifactKind::Rankings)).unwrap(),
        contents
    );
    assert!(!pending.exists());
    assert_eq!(
        recovered
            .manifest()
            .artifact(ArtifactKind::Rankings)
            .unwrap()
            .sha256(),
        digest
    );
}

#[test]
fn freeze_content_addresses_and_open_verified_reopens_read_only() {
    let fixture = TempDirectory::new("verified-open");
    let root = fixture.run_root();
    let mut store = store(&root);
    let rankings = b"{\"case\":\"one\",\"rank\":1}\n";
    let expected_digest = content_sha256(rankings);
    store
        .write_atomic(ArtifactKind::Rankings, rankings)
        .unwrap();
    store.freeze().unwrap();

    let record = store.manifest().artifact(ArtifactKind::Rankings).unwrap();
    assert_eq!(record.sha256(), expected_digest);
    assert_eq!(
        record.content_address(),
        format!("sha256:{expected_digest}")
    );
    assert_eq!(
        record.relative_path(),
        ArtifactKind::Rankings.relative_path()
    );

    let mut file = store.open_verified(ArtifactKind::Rankings).unwrap();
    let mut reopened = Vec::new();
    io::copy(&mut file, &mut reopened).unwrap();
    assert_eq!(reopened, rankings);
    assert!(file.metadata().unwrap().permissions().readonly());
}

#[test]
fn open_verified_rejects_checksum_mismatch() {
    let fixture = TempDirectory::new("checksum-mismatch");
    let root = fixture.run_root();
    let mut store = store(&root);
    store
        .write_atomic(ArtifactKind::Rankings, b"original\n")
        .unwrap();
    store.freeze().unwrap();
    let path = store.artifact_path(ArtifactKind::Rankings);
    make_writable(&path);
    fs::write(&path, b"tampered\n").unwrap();

    let error = store.open_verified(ArtifactKind::Rankings).unwrap_err();

    assert!(matches!(
        error,
        ArtifactError::ChecksumMismatch {
            kind: ArtifactKind::Rankings,
            ..
        }
    ));
}

#[test]
fn recovery_promotes_only_complete_content_matching_temp_artifact() {
    let fixture = TempDirectory::new("recover-complete");
    let root = fixture.run_root();
    let mut store = store(&root);
    let contents = b"{\"case\":\"recovered\"}\n";
    let digest = content_sha256(contents);
    let pending = store.pending_path(ArtifactKind::Rankings, &digest).unwrap();
    fs::write(&pending, contents).unwrap();

    store.recover().unwrap();

    assert!(!pending.exists());
    assert_eq!(
        fs::read(store.artifact_path(ArtifactKind::Rankings)).unwrap(),
        contents
    );
    assert_eq!(
        store
            .manifest()
            .artifact(ArtifactKind::Rankings)
            .unwrap()
            .sha256(),
        digest
    );
}

#[test]
fn recovery_rejects_partial_temp_without_changing_prior_artifact() {
    let fixture = TempDirectory::new("recover-partial");
    let root = fixture.run_root();
    let mut store = store(&root);
    let prior = b"{\"case\":\"prior\"}\n";
    store.write_atomic(ArtifactKind::Rankings, prior).unwrap();
    let intended = b"{\"case\":\"replacement\"}\n";
    let digest = content_sha256(intended);
    let pending = store.pending_path(ArtifactKind::Contexts, &digest).unwrap();
    fs::write(&pending, &intended[..8]).unwrap();
    let before = snapshot_tree(&root);

    let error = store.recover().unwrap_err();

    assert!(matches!(
        error,
        ArtifactError::RecoveryChecksumMismatch {
            kind: ArtifactKind::Contexts,
            ..
        }
    ));
    assert_eq!(snapshot_tree(&root), before);
    assert_eq!(
        fs::read(store.artifact_path(ArtifactKind::Rankings)).unwrap(),
        prior
    );
}

#[test]
fn recovery_rejects_noncanonical_temp_path_without_entry_changes() {
    let fixture = TempDirectory::new("recover-path-mismatch");
    let root = fixture.run_root();
    let mut store = store(&root);
    let contents = b"complete but under the wrong path\n";
    let digest = content_sha256(contents);
    let pending = root
        .join(".pending")
        .join(format!("not-an-artifact.{digest}.tmp"));
    fs::write(&pending, contents).unwrap();
    let before = snapshot_tree(&root);

    let error = store.recover().unwrap_err();

    assert!(matches!(error, ArtifactError::RecoveryPathMismatch { .. }));
    assert_eq!(snapshot_tree(&root), before);
}

#[test]
fn recovery_rejects_conflict_without_corrupting_prior_artifact() {
    let fixture = TempDirectory::new("recover-conflict");
    let root = fixture.run_root();
    let mut store = store(&root);
    let prior = b"{\"case\":\"prior\"}\n";
    store.write_atomic(ArtifactKind::Rankings, prior).unwrap();
    let replacement = b"{\"case\":\"replacement\"}\n";
    let digest = content_sha256(replacement);
    let pending = store.pending_path(ArtifactKind::Rankings, &digest).unwrap();
    fs::write(&pending, replacement).unwrap();
    let before = snapshot_tree(&root);

    let error = store.recover().unwrap_err();

    assert!(matches!(
        error,
        ArtifactError::RecoveryConflict {
            kind: ArtifactKind::Rankings,
            ..
        }
    ));
    assert_eq!(snapshot_tree(&root), before);
    assert_eq!(
        fs::read(store.artifact_path(ArtifactKind::Rankings)).unwrap(),
        prior
    );
}

#[test]
fn recovery_rejects_ambiguous_complete_candidates_before_promoting_either() {
    let fixture = TempDirectory::new("recover-ambiguous");
    let root = fixture.run_root();
    let mut store = store(&root);
    let first = b"{\"case\":\"first\"}\n";
    let second = b"{\"case\":\"second\"}\n";
    let first_pending = store
        .pending_path(ArtifactKind::Rankings, &content_sha256(first))
        .unwrap();
    let second_pending = store
        .pending_path(ArtifactKind::Rankings, &content_sha256(second))
        .unwrap();
    fs::write(&first_pending, first).unwrap();
    fs::write(&second_pending, second).unwrap();
    let before = snapshot_tree(&root);

    let error = store.recover().unwrap_err();

    assert!(matches!(
        error,
        ArtifactError::RecoveryConflict {
            kind: ArtifactKind::Rankings,
            ..
        }
    ));
    assert_eq!(snapshot_tree(&root), before);
    assert!(!store.artifact_path(ArtifactKind::Rankings).exists());
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}
