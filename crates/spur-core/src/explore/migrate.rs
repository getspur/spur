use anyhow::{bail, Context};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::explore::catalog::Catalog;
use crate::explore::store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub dry_run: bool,
    pub source_root: PathBuf,
    pub target_root: PathBuf,
    pub manifest_preserved: bool,
    pub operations: Vec<MigrationOperation>,
    pub conflicts: Vec<MigrationConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOperation {
    pub kind: MigrationKind,
    pub action: MigrationAction,
    pub path: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationConflict {
    pub kind: MigrationKind,
    pub path: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationKind {
    Catalog,
    Cache,
    Pool,
}

impl MigrationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Cache => "cache",
            Self::Pool => "pool",
        }
    }
}

impl fmt::Display for MigrationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationAction {
    Merge,
    Copy,
    Deduplicate,
}

impl MigrationAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Copy => "copy",
            Self::Deduplicate => "dedupe",
        }
    }
}

impl fmt::Display for MigrationAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub fn migrate_global(
    repo_root: &Path,
    global_store_root: &Path,
    dry_run: bool,
) -> anyhow::Result<MigrationReport> {
    let source_root = store::local_root(repo_root);
    let mut report = MigrationReport {
        dry_run,
        source_root: source_root.clone(),
        target_root: global_store_root.to_path_buf(),
        manifest_preserved: store::local_manifest_path(repo_root).is_file(),
        operations: Vec::new(),
        conflicts: Vec::new(),
    };

    if !source_root.exists() {
        return Ok(report);
    }

    migrate_catalog(&source_root, global_store_root, dry_run, &mut report)?;
    migrate_cache(&source_root, global_store_root, dry_run, &mut report)?;
    migrate_pool(&source_root, global_store_root, dry_run, &mut report)?;

    if !dry_run {
        remove_empty_dir(&source_root.join("index"))?;
        remove_empty_dir(&source_root.join("cache"))?;
        remove_empty_dir(&source_root.join("pool"))?;
        remove_empty_dir(&source_root)?;
    }

    Ok(report)
}

fn migrate_catalog(
    source_root: &Path,
    global_store_root: &Path,
    dry_run: bool,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    let source = store::catalog_path_in_store(source_root);
    if !source.exists() {
        return Ok(());
    }

    let destination = store::catalog_path_in_store(global_store_root);
    report.operations.push(MigrationOperation {
        kind: MigrationKind::Catalog,
        action: MigrationAction::Merge,
        path: relative_path(source_root, &source),
        source: source.clone(),
        destination: destination.clone(),
    });

    if dry_run {
        return Ok(());
    }

    let local = Catalog::load_from_store(source_root)?;
    let mut merged = Catalog::load_from_store(global_store_root)?;
    merge_catalog(&mut merged, local.clone());
    merged.save_to_store(global_store_root)?;

    let verified = Catalog::load_from_store(global_store_root)?;
    if verified.synced_at_epoch != merged.synced_at_epoch || verified.entries != merged.entries {
        bail!(
            "global catalog verification failed after merging {}",
            source.display()
        );
    }

    std::fs::remove_file(&source).with_context(|| format!("remove {}", source.display()))?;
    Ok(())
}

fn migrate_cache(
    source_root: &Path,
    global_store_root: &Path,
    dry_run: bool,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    migrate_direct_children(
        source_root,
        &source_root.join("cache"),
        &global_store_root.join("cache"),
        MigrationKind::Cache,
        dry_run,
        report,
    )
}

fn migrate_pool(
    source_root: &Path,
    global_store_root: &Path,
    dry_run: bool,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    let source_pool = source_root.join("pool");
    if !source_pool.exists() {
        return Ok(());
    }

    for owner in sorted_entries(&source_pool)? {
        let owner_path = owner.path();
        let destination_owner = global_store_root.join("pool").join(owner.file_name());
        if owner
            .file_type()
            .with_context(|| format!("inspect {}", owner_path.display()))?
            .is_dir()
        {
            migrate_direct_children(
                source_root,
                &owner_path,
                &destination_owner,
                MigrationKind::Pool,
                dry_run,
                report,
            )?;
            if !dry_run {
                remove_empty_dir(&owner_path)?;
            }
        } else {
            migrate_entry(
                source_root,
                &owner_path,
                &destination_owner,
                MigrationKind::Pool,
                dry_run,
                report,
            )?;
        }
    }

    Ok(())
}

fn migrate_direct_children(
    source_root: &Path,
    source_dir: &Path,
    destination_dir: &Path,
    kind: MigrationKind,
    dry_run: bool,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    if !source_dir.exists() {
        return Ok(());
    }

    for entry in sorted_entries(source_dir)? {
        let source = entry.path();
        let destination = destination_dir.join(entry.file_name());
        migrate_entry(source_root, &source, &destination, kind, dry_run, report)?;
    }

    if !dry_run {
        remove_empty_dir(source_dir)?;
    }

    Ok(())
}

fn migrate_entry(
    source_root: &Path,
    source: &Path,
    destination: &Path,
    kind: MigrationKind,
    dry_run: bool,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    if destination.exists() {
        if same_content(source, destination)? {
            report.operations.push(MigrationOperation {
                kind,
                action: MigrationAction::Deduplicate,
                path: relative_path(source_root, source),
                source: source.to_path_buf(),
                destination: destination.to_path_buf(),
            });
            if !dry_run {
                remove_path(source)?;
            }
        } else {
            report.conflicts.push(MigrationConflict {
                kind,
                path: relative_path(source_root, source),
                source: source.to_path_buf(),
                destination: destination.to_path_buf(),
                reason: "destination exists with different content".to_string(),
            });
        }
        return Ok(());
    }

    report.operations.push(MigrationOperation {
        kind,
        action: MigrationAction::Copy,
        path: relative_path(source_root, source),
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
    });

    if dry_run {
        return Ok(());
    }

    copy_path(source, destination)?;
    if !same_content(source, destination)? {
        bail!(
            "content verification failed after copying {} to {}",
            source.display(),
            destination.display()
        );
    }
    remove_path(source)
}

fn merge_catalog(merged: &mut Catalog, local: Catalog) {
    let synced_at_epoch = local.synced_at_epoch;
    for entry in local.entries {
        if let Some(existing) = merged
            .entries
            .iter_mut()
            .find(|existing| existing.name == entry.name)
        {
            *existing = entry;
        } else {
            merged.entries.push(entry);
        }
    }
    merged.synced_at_epoch = synced_at_epoch.or(merged.synced_at_epoch);
}

fn same_content(left: &Path, right: &Path) -> anyhow::Result<bool> {
    if !right.exists() {
        return Ok(false);
    }
    let left_hash = crate::explore::content_hash(left)?;
    let right_hash = crate::explore::content_hash(right)?;
    Ok(left_hash == right_hash)
}

fn copy_path(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata =
        std::fs::metadata(source).with_context(|| format!("stat {}", source.display()))?;
    if metadata.is_dir() {
        copy_dir(source, destination)
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::copy(source, destination)
            .with_context(|| format!("copy {} to {}", source.display(), destination.display()))?;
        Ok(())
    } else {
        bail!("unsupported explore store path type: {}", source.display());
    }
}

fn copy_dir(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create dir {}", destination.display()))?;
    for entry in sorted_entries(source)? {
        let source_path = entry.path();
        copy_path(&source_path, &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("remove dir {}", path.display()))
    } else {
        std::fs::remove_file(path).with_context(|| format!("remove file {}", path.display()))
    }
}

fn remove_empty_dir(path: &Path) -> anyhow::Result<()> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                std::fs::remove_dir(path)
                    .with_context(|| format!("remove empty dir {}", path.display()))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("read dir {}", path.display())),
    }
}

fn sorted_entries(path: &Path) -> anyhow::Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("read dir {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("read dir entry {}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::explore::catalog::{Catalog, CatalogEntry, ItemKind};
    use crate::explore::pool::{GateRecord, Manifest, ManifestItem};
    use crate::explore::store;

    const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn migrate_global_merges_catalog_and_moves_pool_cache_without_manifest() {
        let repo = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();

        write_catalog(
            repo.path(),
            vec![
                entry("shared", "local/source"),
                entry("local-only", "local/source"),
            ],
        );
        Catalog {
            synced_at_epoch: Some(1),
            entries: vec![
                entry("shared", "global/source"),
                entry("global-only", "global/source"),
            ],
        }
        .save_to_store(global.path())
        .unwrap();

        fs::create_dir_all(repo.path().join(".spur/explore/cache/local-source")).unwrap();
        fs::write(
            repo.path()
                .join(".spur/explore/cache/local-source/SKILL.md"),
            "cache body",
        )
        .unwrap();
        write_local_pool_item(repo.path(), "local-only", "pool body");
        Manifest {
            sources: Vec::new(),
            items: vec![manifest_item("local-only")],
        }
        .save(repo.path())
        .unwrap();

        let report = super::migrate_global(repo.path(), global.path(), false).unwrap();

        assert!(report.manifest_preserved);
        assert!(report
            .operations
            .iter()
            .any(|op| op.path.ends_with("index/catalog.json")));
        assert!(global.path().join("cache/local-source/SKILL.md").is_file());
        assert!(global_pool_item(global.path(), "local-only")
            .join("SKILL.md")
            .is_file());
        assert!(!repo
            .path()
            .join(".spur/explore/index/catalog.json")
            .exists());
        assert!(!repo
            .path()
            .join(".spur/explore/cache/local-source")
            .exists());
        assert!(!local_pool_item(repo.path(), "local-only").exists());
        assert!(store::local_manifest_path(repo.path()).is_file());

        let merged = Catalog::load_from_store(global.path()).unwrap();
        assert_eq!(
            merged
                .entries
                .iter()
                .find(|entry| entry.name == "shared")
                .unwrap()
                .source,
            "local/source"
        );
        assert!(merged
            .entries
            .iter()
            .any(|entry| entry.name == "global-only"));
        assert!(merged
            .entries
            .iter()
            .any(|entry| entry.name == "local-only"));

        let second = super::migrate_global(repo.path(), global.path(), false).unwrap();
        assert!(second.operations.is_empty());
        assert!(store::local_manifest_path(repo.path()).is_file());
    }

    #[test]
    fn migrate_global_merges_duplicate_local_names_with_last_entry_winning() {
        let repo = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();

        write_catalog(
            repo.path(),
            vec![
                entry("shared", "first/local-source"),
                entry("shared", "second/local-source"),
            ],
        );
        Catalog {
            synced_at_epoch: Some(1),
            entries: vec![
                entry("shared", "global/source"),
                entry("global-only", "global/source"),
            ],
        }
        .save_to_store(global.path())
        .unwrap();

        super::migrate_global(repo.path(), global.path(), false).unwrap();

        let merged = Catalog::load_from_store(global.path()).unwrap();
        let shared = merged
            .entries
            .iter()
            .filter(|entry| entry.name == "shared")
            .collect::<Vec<_>>();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].source, "second/local-source");
        assert!(merged
            .entries
            .iter()
            .any(|entry| entry.name == "global-only"));
        assert!(!store::local_catalog_path(repo.path()).exists());
    }

    #[test]
    fn migrate_global_dry_run_reports_without_mutating() {
        let repo = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        write_catalog(repo.path(), vec![entry("api-design", "local/source")]);
        fs::create_dir_all(repo.path().join(".spur/explore/cache/local-source")).unwrap();
        fs::write(
            repo.path()
                .join(".spur/explore/cache/local-source/SKILL.md"),
            "cache body",
        )
        .unwrap();
        write_local_pool_item(repo.path(), "api-design", "pool body");

        let report = super::migrate_global(repo.path(), global.path(), true).unwrap();

        assert!(report.dry_run);
        assert!(!report.operations.is_empty());
        assert!(repo
            .path()
            .join(".spur/explore/index/catalog.json")
            .is_file());
        assert!(repo
            .path()
            .join(".spur/explore/cache/local-source/SKILL.md")
            .is_file());
        assert!(local_pool_item(repo.path(), "api-design")
            .join("SKILL.md")
            .is_file());
        assert!(!store::catalog_path_in_store(global.path()).exists());
        assert!(!global.path().join("cache/local-source").exists());
        assert!(!global_pool_item(global.path(), "api-design").exists());
    }

    fn write_catalog(repo_root: &Path, entries: Vec<CatalogEntry>) {
        Catalog {
            synced_at_epoch: Some(2),
            entries,
        }
        .save(repo_root)
        .unwrap();
    }

    fn write_local_pool_item(repo_root: &Path, name: &str, body: &str) {
        let dir = local_pool_item(repo_root, name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn local_pool_item(repo_root: &Path, name: &str) -> std::path::PathBuf {
        store::local_pool_dir(repo_root, "local/source", name, COMMIT)
    }

    fn global_pool_item(global_root: &Path, name: &str) -> std::path::PathBuf {
        store::pool_dir_in_store(global_root, "local/source", name, COMMIT)
    }

    fn manifest_item(name: &str) -> ManifestItem {
        ManifestItem {
            name: name.to_string(),
            kind: ItemKind::Skill,
            source: "local/source".to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: COMMIT.to_string(),
            content_sha256: "0".repeat(64),
            license: None,
            gate: GateRecord {
                verdict: "clean".to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        }
    }

    fn entry(name: &str, source: &str) -> CatalogEntry {
        CatalogEntry {
            kind: ItemKind::Skill,
            name: name.to_string(),
            source: source.to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: COMMIT.to_string(),
            description: format!("{name} description"),
            license: None,
            content_sha256: "0".repeat(64),
        }
    }
}
