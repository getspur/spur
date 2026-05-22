use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use spur_graph::store::cache::{lookup_canonical, write_with_dedup};
use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts, git,
    read_artifact_header_parquet, read_current_pointer, BuildMode, GraphIndexArtifact,
    GraphIndexPointer, SourceKind,
};
use tempfile::TempDir;

#[test]
fn discovery_uses_git_when_available() {
    let repo = GitRepo::new();
    repo.write(".hidden.rs", "pub fn hidden() {}\n");
    repo.write("src/visible.rs", "pub fn visible() {}\n");
    repo.git(&["add", ".hidden.rs", "src/visible.rs"]);
    repo.git(&["commit", "-m", "add tracked files"]);

    let artifact = build_full(repo.path());

    assert_manifest_paths(&artifact, &[".hidden.rs", "src/visible.rs"]);
}

#[test]
fn discovery_filters_symlink_gitlink_sparse() {
    let repo = GitRepo::new();
    repo.write("src/keep.rs", "pub fn keep() {}\n");
    repo.write("src/sparse.rs", "pub fn sparse() {}\n");
    repo.git(&["add", "src/keep.rs", "src/sparse.rs"]);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("src/keep.rs", repo.path().join("link.rs"))
            .expect("create symlink");
        repo.git(&["add", "link.rs"]);
    }

    let submodule_oid = gitlink_target_oid("submodule one");
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        &submodule_oid,
        "vendor/submodule.rs",
    ]);
    repo.git(&["commit", "-m", "add mixed entries"]);
    repo.git(&["update-index", "--skip-worktree", "src/sparse.rs"]);

    let artifact = build_full(repo.path());

    assert!(manifest_entry(&artifact, "src/keep.rs").is_some());
    assert!(manifest_entry(&artifact, "src/sparse.rs").is_none());
    #[cfg(unix)]
    assert!(manifest_entry(&artifact, "link.rs").is_none());
    assert_eq!(
        manifest_entry(&artifact, "vendor/submodule.rs")
            .expect("gitlink manifest")
            .content_oid,
        format!("gitlink:{submodule_oid}")
    );
}

#[test]
fn content_oid_replaces_mtime_size() {
    let repo = GitRepo::new();
    let content = "pub fn stable() {}\n";
    repo.write("src/lib.rs", content);
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "add lib"]);

    let first = build_full(repo.path());
    let first_manifest = manifest_entry(&first, "src/lib.rs").expect("first manifest");
    assert_eq!(
        first_manifest.content_oid,
        git_hash_object(content.as_bytes())
    );

    repo.write("src/lib.rs", content);
    let second = build_full(repo.path());
    let second_manifest = manifest_entry(&second, "src/lib.rs").expect("second manifest");

    assert_eq!(first_manifest.content_oid, second_manifest.content_oid);
    assert_eq!(first.graph_content_hash, second.graph_content_hash);

    let json = serde_json::to_string(&second).expect("serialize artifact");
    assert!(!json.contains("mtime"));
    assert!(!json.contains("size_bytes"));
}

#[test]
fn inrust_manifest_diff_handles_add_modify_delete() {
    let repo = GitRepo::new();
    repo.write("src/a.rs", "pub fn alpha() {}\n");
    repo.write("src/b.rs", "pub fn beta() {}\n");
    repo.git(&["add", "src/a.rs", "src/b.rs"]);
    repo.git(&["commit", "-m", "baseline"]);
    let baseline = build_full(repo.path());
    let removed_id = manifest_entry(&baseline, "src/b.rs")
        .expect("baseline b")
        .stable_file_id
        .clone();

    repo.write("src/a.rs", "pub fn alpha_changed() {}\n");
    repo.write("src/c.rs", "pub fn gamma() {}\n");
    fs::remove_file(repo.path().join("src/b.rs")).expect("remove b.rs");
    let (next, mode) =
        artifact_from_facts_incremental(&baseline, repo.path()).expect("incremental build");

    assert_eq!(mode, BuildMode::Incremental);
    assert!(next
        .symbols
        .iter()
        .any(|symbol| symbol.entity_name == "alpha_changed"));
    assert!(manifest_entry(&next, "src/c.rs").is_some());
    assert!(manifest_entry(&next, "src/b.rs").is_none());
    assert_eq!(next.tombstones.len(), 1);
    assert_eq!(next.tombstones[0].path, "src/b.rs");
    assert_eq!(next.tombstones[0].stable_file_id, removed_id);
}

#[test]
fn provenance_lives_in_pointer_not_artifact() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn lib() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "add lib"]);

    let artifact = build_full(repo.path());
    write_git_cache(repo.path(), &artifact);

    let current = read_current_pointer(repo.path()).expect("read CURRENT");
    let manifest = read_artifact_header_parquet(&current).expect("read parquet manifest");
    assert_eq!(manifest.indexed_commit_oid, None);

    let pointer = read_pointer(repo.path());
    assert_eq!(pointer.source_kind, SourceKind::Git);
    assert_eq!(pointer.graph_content_hash, artifact.graph_content_hash);
    assert_eq!(
        pointer.indexed_commit_oid.as_deref(),
        Some(repo.head().as_str())
    );
}

#[test]
#[cfg(unix)]
fn current_pointer_targets_canonical_parquet_directory() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn lib() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "add lib"]);

    let artifact = build_full(repo.path());
    write_git_cache(repo.path(), &artifact);
    let canonical = canonical_artifact_path(repo.path(), &artifact);
    let current = read_current_pointer(repo.path()).expect("read CURRENT");

    assert_eq!(current, canonical);
}

#[test]
fn dirty_then_commit_collides_with_clean_key() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn baseline() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "baseline"]);

    let bytes = b"pub fn same_after_commit() {}\n";
    fs::write(repo.path().join("src/lib.rs"), bytes).expect("write dirty lib");
    let dirty = build_full(repo.path());

    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "commit same bytes"]);
    let clean = build_full(repo.path());

    assert_eq!(
        manifest_entry(&dirty, "src/lib.rs")
            .expect("dirty manifest")
            .content_oid,
        git_hash_object(bytes)
    );
    assert_eq!(dirty.graph_content_hash, clean.graph_content_hash);
}

#[test]
fn head_change_during_build_only_affects_provenance() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn stable() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "add lib"]);

    let artifact = build_full(repo.path());
    write_git_cache(repo.path(), &artifact);
    let first_pointer = read_pointer(repo.path());

    repo.git(&["commit", "--allow-empty", "-m", "metadata only"]);
    write_git_cache(repo.path(), &artifact);
    let second_pointer = read_pointer(repo.path());

    assert_eq!(
        first_pointer.graph_content_hash,
        second_pointer.graph_content_hash
    );
    assert_eq!(
        first_pointer.canonical_artifact_path,
        second_pointer.canonical_artifact_path
    );
    assert_ne!(
        first_pointer.indexed_commit_oid,
        second_pointer.indexed_commit_oid
    );
    assert_eq!(
        second_pointer.indexed_commit_oid.as_deref(),
        Some(repo.head().as_str())
    );
}

#[test]
fn delete_emits_value_level_tombstone() {
    let repo = GitRepo::new();
    repo.write("src/delete_me.rs", "pub fn delete_me() {}\n");
    repo.write("src/keep.rs", "pub fn keep() {}\n");
    repo.git(&["add", "src/delete_me.rs", "src/keep.rs"]);
    repo.git(&["commit", "-m", "baseline"]);
    let baseline = build_full(repo.path());

    fs::remove_file(repo.path().join("src/delete_me.rs")).expect("remove delete_me.rs");
    let (next, mode) =
        artifact_from_facts_incremental(&baseline, repo.path()).expect("incremental delete");

    assert_eq!(mode, BuildMode::Incremental);
    assert_eq!(next.tombstones.len(), 1);
    let tombstones_json = serde_json::to_string(&next.tombstones).expect("serialize tombstones");
    assert!(tombstones_json.contains("delete_me.rs"));
    assert!(!tombstones_json.contains("content_oid"));
    assert!(!tombstones_json.contains("indexed_commit_oid"));
}

#[test]
fn non_git_uses_git_blob_oid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"\xef\xbb\xbfpub fn crlf() {\r\n    let _ = \"bytes\";\r\n}\r\n";
    fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    fs::write(dir.path().join("src/lib.rs"), bytes).expect("write lib.rs");

    let artifact = build_full(dir.path());

    assert_eq!(
        manifest_entry(&artifact, "src/lib.rs")
            .expect("manifest")
            .content_oid,
        git_hash_object(bytes)
    );
}

#[test]
fn legacy_artifact_triggers_full_rebuild() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn lib() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "add lib"]);

    let mut legacy = build_full(repo.path());
    legacy.manifest_version = "legacy-manifest-version".to_string();
    let (rebuilt, mode) =
        artifact_from_facts_incremental(&legacy, repo.path()).expect("legacy rebuild");

    assert_eq!(mode, BuildMode::Full);
    assert_ne!(rebuilt.manifest_version, "legacy-manifest-version");
    assert!(manifest_entry(&rebuilt, "src/lib.rs").is_some());
}

#[test]
fn two_worktrees_with_same_content_share_cache() {
    let repo = GitRepo::new();
    repo.git(&["commit", "--allow-empty", "-m", "base"]);
    let base = repo.head();

    repo.git(&["checkout", "-b", "order-ab", &base]);
    repo.write("src/a.rs", "pub fn alpha() {}\n");
    repo.git(&["add", "src/a.rs"]);
    repo.git(&["commit", "-m", "add a"]);
    repo.write("src/b.rs", "pub fn beta() {}\n");
    repo.git(&["add", "src/b.rs"]);
    repo.git(&["commit", "-m", "add b"]);

    repo.git(&["checkout", "-b", "order-ba", &base]);
    repo.write("src/b.rs", "pub fn beta() {}\n");
    repo.git(&["add", "src/b.rs"]);
    repo.git(&["commit", "-m", "add b"]);
    repo.write("src/a.rs", "pub fn alpha() {}\n");
    repo.git(&["add", "src/a.rs"]);
    repo.git(&["commit", "-m", "add a"]);

    repo.git(&["checkout", &base]);
    let wt_ab = repo.temp.path().join("wt-ab");
    let wt_ba = repo.temp.path().join("wt-ba");
    repo.git(&["worktree", "add", path_str(&wt_ab), "order-ab"]);
    repo.git(&["worktree", "add", path_str(&wt_ba), "order-ba"]);

    let artifact_ab = build_full(&wt_ab);
    write_git_cache(&wt_ab, &artifact_ab);
    let artifact_ba = build_full(&wt_ba);
    write_git_cache(&wt_ba, &artifact_ba);

    let pointer_ab = read_pointer(&wt_ab);
    let pointer_ba = read_pointer(&wt_ba);
    assert_ne!(pointer_ab.indexed_commit_oid, pointer_ba.indexed_commit_oid);
    assert_eq!(
        artifact_ab.graph_content_hash,
        artifact_ba.graph_content_hash
    );
    assert_eq!(
        pointer_ab.canonical_artifact_path,
        pointer_ba.canonical_artifact_path
    );
}

#[test]
#[ignore = "requires a real cross-device canonical/worktree pair; store unit test injects EXDEV"]
fn cross_fs_write_falls_back_to_copy() {}

#[test]
fn submodule_pointer_change_invalidates() {
    let repo = GitRepo::new();
    let first_oid = gitlink_target_oid("submodule one");
    let second_oid = gitlink_target_oid("submodule two");

    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        &first_oid,
        "vendor/submodule.rs",
    ]);
    repo.git(&["commit", "-m", "add submodule pointer"]);
    let first = build_full(repo.path());

    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        &second_oid,
        "vendor/submodule.rs",
    ]);
    repo.git(&["commit", "-m", "move submodule pointer"]);
    let second = build_full(repo.path());

    assert_eq!(
        manifest_entry(&first, "vendor/submodule.rs")
            .expect("first gitlink")
            .content_oid,
        format!("gitlink:{first_oid}")
    );
    assert_eq!(
        manifest_entry(&second, "vendor/submodule.rs")
            .expect("second gitlink")
            .content_oid,
        format!("gitlink:{second_oid}")
    );
    assert_ne!(first.graph_content_hash, second.graph_content_hash);
}

#[test]
fn crlf_bom_dirty_hash_is_bytewise() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn baseline() {}\n");
    repo.git(&["add", "src/lib.rs"]);
    repo.git(&["commit", "-m", "baseline"]);

    let bytes = b"\xef\xbb\xbfpub fn dirty() {\r\n    let bytes = [0, 1, 2];\r\n}\r\n";
    fs::write(repo.path().join("src/lib.rs"), bytes).expect("write dirty bytes");
    let artifact = build_full(repo.path());

    assert_eq!(
        manifest_entry(&artifact, "src/lib.rs")
            .expect("dirty manifest")
            .content_oid,
        git_hash_object(bytes)
    );
}

struct GitRepo {
    temp: TempDir,
    root: PathBuf,
}

impl GitRepo {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        fs::create_dir(&root).expect("create repo dir");
        run_git(&root, &["init"]);
        run_git(
            &root,
            &["config", "user.email", "spur-graph@example.invalid"],
        );
        run_git(&root, &["config", "user.name", "Spur Graph Test"]);
        Self { temp, root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir parent");
        }
        fs::write(path, content).expect("write file");
    }

    fn git(&self, args: &[&str]) {
        run_git(&self.root, args);
    }

    fn head(&self) -> String {
        git_stdout(&self.root, &["rev-parse", "HEAD"])
            .trim_end()
            .to_string()
    }
}

fn build_full(root: &Path) -> GraphIndexArtifact {
    let (facts, _counts) = build_facts(root).expect("build facts");
    artifact_from_facts(&facts, root).expect("build artifact")
}

fn write_git_cache(root: &Path, artifact: &GraphIndexArtifact) {
    let ctx = git::detect(root).expect("git context");
    write_with_dedup(artifact, root, &ctx).expect("write git cache");
}

fn canonical_artifact_path(root: &Path, artifact: &GraphIndexArtifact) -> PathBuf {
    let ctx = git::detect(root).expect("git context");
    lookup_canonical(
        &ctx.git_common_dir,
        &artifact.manifest_version,
        &artifact.graph_content_hash,
    )
    .expect("canonical artifact")
}

fn read_pointer(root: &Path) -> GraphIndexPointer {
    let bytes = fs::read(root.join(".spur/graph-index.pointer.json")).expect("read pointer");
    serde_json::from_slice(&bytes).expect("parse pointer")
}

fn manifest_entry<'a>(
    artifact: &'a GraphIndexArtifact,
    path: &str,
) -> Option<&'a spur_graph::GraphFileManifestEntry> {
    artifact
        .file_manifests
        .iter()
        .find(|entry| entry.path == path)
}

fn assert_manifest_paths(artifact: &GraphIndexArtifact, expected: &[&str]) {
    let mut actual = artifact
        .file_manifests
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, expected);
}

fn gitlink_target_oid(content: &str) -> String {
    let repo = GitRepo::new();
    repo.write("README.md", content);
    repo.git(&["add", "README.md"]);
    repo.git(&["commit", "-m", "submodule target"]);
    repo.head()
}

fn git_hash_object(bytes: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn git hash-object");
    child
        .stdin
        .as_mut()
        .expect("hash-object stdin")
        .write_all(bytes)
        .expect("write hash-object stdin");
    let output = child.wait_with_output().expect("wait for git hash-object");
    assert!(
        output.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("hash-object stdout UTF-8")
        .trim_end()
        .to_string()
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout UTF-8")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("path UTF-8")
}
