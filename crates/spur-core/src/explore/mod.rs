pub mod catalog;
pub mod gate;
pub mod pool;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::path::Path;

/// sha256 hex of a single file's bytes, or of a directory as
/// sha256 over sorted "rel_path\0file_sha\n" lines.
pub fn content_hash(path: &Path) -> anyhow::Result<String> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_file() {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        return Ok(sha256_hex(&bytes));
    }

    if meta.is_dir() {
        let mut files = Vec::new();
        collect_file_hashes(path, path, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut body = Vec::new();
        for (rel_path, file_hash) in files {
            body.extend_from_slice(rel_path.as_bytes());
            body.push(0);
            body.extend_from_slice(file_hash.as_bytes());
            body.push(b'\n');
        }
        return Ok(sha256_hex(&body));
    }

    bail!("unsupported path type for {}", path.display());
}

fn collect_file_hashes(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read dir entry {}", dir.display()))?;
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_file_hashes(root, &path, files)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let rel_path = path
                .strip_prefix(root)
                .with_context(|| format!("strip root from {}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((rel_path, sha256_hex(&bytes)));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}
