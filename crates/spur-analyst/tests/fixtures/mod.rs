use std::fs;
use std::path::{Path, PathBuf};

pub fn crate_path(relative: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

pub fn repo_root() -> PathBuf {
    crate_path("")
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under crates/spur-analyst")
        .to_path_buf()
}

pub fn read_repo_file(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).unwrap_or_else(|error| {
        panic!("failed to read {path}: {error}");
    })
}

pub fn line_count(source: &str) -> usize {
    source.lines().count()
}
