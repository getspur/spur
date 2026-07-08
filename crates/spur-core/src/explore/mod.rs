pub mod apply;
pub mod catalog;
pub mod gate;
pub mod materialize;
pub mod pool;
pub mod sync;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn validate_skill_names(repo_root: Option<&Path>, skills: &[String]) -> Result<(), String> {
    if skills.is_empty() {
        return Ok(());
    }
    let Some(repo_root) = repo_root else {
        return Err(format!(
            "Invalid explore skill `{}`: repository root unavailable; run `spur explore status` after opening a repository",
            skills[0]
        ));
    };
    let manifest = crate::explore::pool::Manifest::load(repo_root).map_err(|error| {
        format!(
            "Invalid explore skills: failed to load explore manifest: {error:#}; run `spur explore sync`"
        )
    })?;

    for skill in skills {
        let Some(item) = manifest.items.iter().find(|item| {
            item.kind == crate::explore::catalog::ItemKind::Skill && item.name == *skill
        }) else {
            return Err(format!(
                "Invalid explore skill `{skill}`: not found in the explore manifest; run `spur explore sync` or `spur explore add`"
            ));
        };

        if !matches!(
            item.gate.verdict.as_str(),
            "clean" | "overridden" | "replaced-bundled"
        ) {
            return Err(format!(
                "Invalid explore skill `{skill}`: gate verdict `{}` is not materializable; run `spur explore status`",
                item.gate.verdict
            ));
        }
    }

    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::validate_skill_names;
    use crate::explore::catalog::ItemKind;
    use crate::explore::pool::{GateRecord, Manifest, ManifestItem};

    fn manifest_item(name: &str, verdict: &str) -> ManifestItem {
        ManifestItem {
            name: name.to_string(),
            kind: ItemKind::Skill,
            source: "acme/skills".to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            content_sha256: "0".repeat(64),
            license: None,
            gate: GateRecord {
                verdict: verdict.to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        }
    }

    #[test]
    fn validate_skill_names_matches_delegation_semantics() {
        let root = tempfile::tempdir().unwrap();
        Manifest {
            sources: vec![],
            items: vec![manifest_item("blocked-skill", "blocked")],
        }
        .save(root.path())
        .unwrap();

        assert!(validate_skill_names(Some(root.path()), &[]).is_ok());
        assert!(validate_skill_names(None, &["x".into()])
            .unwrap_err()
            .contains("repository root unavailable"));

        let error = validate_skill_names(Some(root.path()), &["not-in-pool".into()]).unwrap_err();
        assert!(error.contains("not-in-pool") && error.contains("spur explore"));

        let error = validate_skill_names(Some(root.path()), &["blocked-skill".into()]).unwrap_err();
        assert!(error.contains("blocked-skill") && error.contains("blocked"));
    }
}
