use crate::explore::catalog::{CatalogEntry, ItemKind};
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};

#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "explore phase plan locks this public derive set"
)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceSpec {
    pub repo: String,
    pub url: Option<String>,
    pub pin: String,
}

#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "explore phase plan locks this public derive set"
)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GateRecord {
    pub verdict: String,
    pub justification: Option<String>,
    pub decided_at_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManifestItem {
    pub name: String,
    pub kind: ItemKind,
    pub source: String,
    pub rel_path: String,
    pub pinned_commit: String,
    pub content_sha256: String,
    pub license: Option<String>,
    pub gate: GateRecord,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceSpec>,
    #[serde(default, rename = "item")]
    pub items: Vec<ManifestItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusReport {
    pub ok: Vec<String>,
    pub missing: Vec<String>,
    pub sha_mismatch: Vec<String>,
}

impl Manifest {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = manifest_path(root);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let path = manifest_path(root);
        let raw = toml::to_string_pretty(self).context("serialize explore manifest")?;
        atomic_write(&path, raw.as_bytes())
    }
}

pub fn pool_dir(root: &Path, source: &str, name: &str, pinned_commit: &str) -> PathBuf {
    let owner = source.split('/').next().unwrap_or(source);
    let sha7 = pinned_commit.get(..7).unwrap_or(pinned_commit);
    root.join(".spur/explore/pool")
        .join(owner)
        .join(format!("{name}@{sha7}"))
}

pub fn vendor(root: &Path, checkout: &Path, entry: &CatalogEntry) -> anyhow::Result<()> {
    let source = checkout.join(&entry.rel_path);
    let dest_dir = pool_dir(root, &entry.source, &entry.name, &entry.pinned_commit);
    remove_existing_path(&dest_dir)?;

    if source.is_dir() {
        copy_dir_all(&source, &dest_dir)?;
        verify_hash(&dest_dir, entry)?;
    } else if source.is_file() {
        std::fs::create_dir_all(&dest_dir)
            .with_context(|| format!("create dir {}", dest_dir.display()))?;
        let file_name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("source has no file name: {}", source.display()))?;
        let dest = dest_dir.join(file_name);
        std::fs::copy(&source, &dest)
            .with_context(|| format!("copy {} to {}", source.display(), dest.display()))?;
        verify_hash(&dest, entry)?;
    } else {
        bail!("missing vendor source {}", source.display());
    }

    Ok(())
}

pub fn status(root: &Path, manifest: &Manifest) -> StatusReport {
    let mut report = StatusReport::default();

    for item in &manifest.items {
        let dir = pool_dir(root, &item.source, &item.name, &item.pinned_commit);
        if !dir.exists() {
            report.missing.push(item.name.clone());
            continue;
        }

        let hash_path = match item.kind {
            ItemKind::Skill => dir.clone(),
            ItemKind::Agent => dir.join(
                Path::new(&item.rel_path)
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new(&item.name)),
            ),
        };

        if !hash_path.exists() {
            report.missing.push(item.name.clone());
            continue;
        }

        match crate::explore::content_hash(&hash_path) {
            Ok(hash) if hash == item.content_sha256 => report.ok.push(item.name.clone()),
            _ => report.sha_mismatch.push(item.name.clone()),
        }
    }

    report
}

pub fn item_from_entry(entry: &CatalogEntry, gate: GateRecord) -> ManifestItem {
    ManifestItem {
        name: entry.name.clone(),
        kind: entry.kind,
        source: entry.source.clone(),
        rel_path: entry.rel_path.clone(),
        pinned_commit: entry.pinned_commit.clone(),
        content_sha256: entry.content_sha256.clone(),
        license: entry.license.clone(),
        gate,
    }
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(".spur/explore.toml")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    use std::io::Write as _;
    tmp.write_all(bytes)
        .with_context(|| format!("write {}", tmp.path().display()))?;
    tmp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist {}", path.display()))?;
    Ok(())
}

fn copy_dir_all(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("create dir {}", dest.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("read dir {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("read dir entry {}", source.display()))?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", source_path.display()))?;
        if file_type.is_dir() {
            copy_dir_all(&source_path, &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &dest_path).with_context(|| {
                format!("copy {} to {}", source_path.display(), dest_path.display())
            })?;
        }
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            std::fs::remove_dir_all(path).with_context(|| format!("remove dir {}", path.display()))
        }
        Ok(_) => std::fs::remove_file(path).with_context(|| format!("remove {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn verify_hash(path: &Path, entry: &CatalogEntry) -> anyhow::Result<()> {
    let actual = crate::explore::content_hash(path)?;
    if actual != entry.content_sha256 {
        bail!(
            "vendored content hash mismatch for {}: expected {}, got {}",
            entry.name,
            entry.content_sha256,
            actual
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::catalog::{CatalogEntry, ItemKind};

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
            kind: ItemKind::Skill,
            name: "api-design".to_string(),
            source: "acme/repo".to_string(),
            rel_path: "skills/api-design".to_string(),
            pinned_commit: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            description: "REST heuristics".to_string(),
            license: Some("MIT".to_string()),
            content_sha256: "0".repeat(64),
        }
    }

    fn sample_item(name: &str, verdict: &str) -> ManifestItem {
        let entry = CatalogEntry {
            name: name.to_string(),
            ..sample_entry()
        };
        item_from_entry(
            &entry,
            GateRecord {
                verdict: verdict.to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        )
    }

    #[test]
    fn manifest_roundtrips_toml() {
        let manifest = Manifest {
            sources: vec![SourceSpec {
                repo: "acme/repo".to_string(),
                url: None,
                pin: "main".to_string(),
            }],
            items: vec![sample_item("api-design", "clean")],
        };
        let td = tempfile::tempdir().unwrap();

        manifest.save(td.path()).unwrap();

        let loaded = Manifest::load(td.path()).unwrap();
        assert_eq!(loaded, manifest);

        let empty_dir = tempfile::tempdir().unwrap();
        let empty = Manifest::load(empty_dir.path()).unwrap();
        assert!(empty.items.is_empty());
        assert!(empty.sources.is_empty());
    }

    #[test]
    fn vendor_copies_item_and_status_detects_tamper() {
        let td = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let sk = src.path().join("skills/api-design");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: api-design\ndescription: d\n---\nbody",
        )
        .unwrap();
        let entry = CatalogEntry {
            content_sha256: crate::explore::content_hash(&sk).unwrap(),
            ..sample_entry()
        };

        vendor(td.path(), src.path(), &entry).unwrap();

        let pdir = pool_dir(td.path(), "acme/repo", "api-design", &entry.pinned_commit);
        assert!(pdir.join("SKILL.md").exists());

        let mut manifest = Manifest::default();
        manifest.items.push(item_from_entry(
            &entry,
            GateRecord {
                verdict: "clean".to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        ));
        let st = status(td.path(), &manifest);
        assert!(st.sha_mismatch.is_empty());
        assert!(st.missing.is_empty());
        assert_eq!(st.ok, vec!["api-design".to_string()]);

        std::fs::write(pdir.join("SKILL.md"), "tampered").unwrap();
        let st = status(td.path(), &manifest);
        assert_eq!(st.sha_mismatch, vec!["api-design".to_string()]);
    }
}
