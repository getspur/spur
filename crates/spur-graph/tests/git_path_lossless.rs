use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use spur_graph::git_walk::GitWalkConfig;
use spur_graph::schema::{EdgeEndpoint, GitPath, SymbolSnapshotArtifact};
use tempfile::TempDir;

#[test]
fn git_path_round_trips_non_utf8_bytes() {
    let original = GitPath::from_bytes(b"\xff\xfe.rs".to_vec());

    let json = serde_json::to_string(&original).unwrap();
    let decoded: GitPath = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded.as_bytes(), b"\xff\xfe.rs");
}

#[test]
#[cfg(unix)]
fn walker_preserves_non_utf8_filename_through_artifact() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    let path = b"bad-\xff.rs";
    let sha = fast_import_file(dir.path(), path, b"pub fn non_utf8_path() -> u32 { 1 }\n");

    let (graph, _commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();

    assert!(graph.temporal_edges.iter().any(|edge| {
        edge.source == (EdgeEndpoint::Commit { sha: sha.clone() })
            && matches!(&edge.target, EdgeEndpoint::File { path: file_path } if file_path.as_bytes() == path)
    }));

    let snapshot = graph
        .symbol_snapshots
        .iter()
        .find(|snapshot| snapshot.entity_name == "non_utf8_path")
        .expect("symbol snapshot for non-UTF-8 filename");
    assert_eq!(snapshot.file_path.as_bytes(), path);

    let json = serde_json::to_string(snapshot).unwrap();
    let decoded: SymbolSnapshotArtifact = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.file_path.as_bytes(), path);
}

fn init_repo(dir: &Path) {
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["config", "user.email", "t@t"].as_slice(),
        ["config", "user.name", "T"].as_slice(),
    ] {
        git(dir, args);
    }
}

#[cfg(unix)]
fn fast_import_file(dir: &Path, path: &[u8], contents: &[u8]) -> String {
    let mut script = Vec::new();
    script.extend_from_slice(b"blob\nmark :1\n");
    append_fast_import_data(&mut script, contents);
    script.extend_from_slice(b"commit refs/heads/main\nmark :2\n");
    script.extend_from_slice(b"committer T <t@t> 0 +0000\n");
    append_fast_import_data(&mut script, b"non utf8 import");
    script.extend_from_slice(b"M 100644 :1 ");
    script.extend_from_slice(path);
    script.push(b'\n');

    let mut child = Command::new("git")
        .current_dir(dir)
        .arg("fast-import")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git fast-import");
    child
        .stdin
        .as_mut()
        .expect("fast-import stdin")
        .write_all(&script)
        .expect("write fast-import stream");
    let output = child.wait_with_output().expect("wait for git fast-import");
    assert!(
        output.status.success(),
        "git fast-import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    rev_parse(dir, "refs/heads/main")
}

#[cfg(unix)]
fn append_fast_import_data(script: &mut Vec<u8>, data: &[u8]) {
    script.extend_from_slice(format!("data {}\n", data.len()).as_bytes());
    script.extend_from_slice(data);
    script.push(b'\n');
}

#[cfg(unix)]
fn rev_parse(dir: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse {rev} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
