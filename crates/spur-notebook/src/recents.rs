use std::{
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use ts_rs::TS;

const MAX_UNPINNED_RECENTS: usize = 50;

static RECENTS_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecentEntry {
    #[ts(type = "string")]
    pub path: PathBuf,
    #[ts(type = "string")]
    pub last_opened: DateTime<Utc>,
    pub is_scratch: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RecentEntriesFile {
    #[serde(default)]
    entries: Vec<RecentEntry>,
}

pub async fn record_open(path: &Path) -> Result<()> {
    record_open_at(&recents_record_path()?, &scratch_dir()?, path).await
}

pub async fn list_recents() -> Result<Vec<RecentEntry>> {
    list_recents_at(&recents_record_path()?).await
}

pub async fn remove_from_recents(path: &Path) -> Result<()> {
    remove_from_recents_at(&recents_record_path()?, path).await
}

pub async fn set_pinned(path: &Path, pinned: bool) -> Result<()> {
    set_pinned_at(&recents_record_path()?, path, pinned).await
}

fn notebooks_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".spur").join("notebooks"))
}

fn scratch_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs.home_dir().join(".spur").join("scratch"))
}

fn recents_record_path() -> Result<PathBuf> {
    Ok(notebooks_dir()?.join("recents.json"))
}

async fn record_open_at(record_path: &Path, scratch_dir: &Path, path: &Path) -> Result<()> {
    let path = canonicalize_or_normalize(path).await?;
    let scratch_dir = canonicalize_or_normalize(scratch_dir).await?;
    let is_scratch = path.starts_with(&scratch_dir);
    let now = Utc::now();

    let _guard = RECENTS_LOCK.lock().await;
    let mut record = read_recents_at(record_path).await?;
    match record.entries.iter_mut().find(|entry| entry.path == path) {
        Some(entry) => {
            entry.last_opened = now;
            entry.is_scratch = is_scratch;
        }
        None => record.entries.push(RecentEntry {
            path,
            last_opened: now,
            is_scratch,
            pinned: false,
        }),
    }
    sort_recents(&mut record.entries);
    prune_unpinned_cap(&mut record.entries);
    write_recents_at(record_path, &record).await
}

async fn list_recents_at(record_path: &Path) -> Result<Vec<RecentEntry>> {
    let _guard = RECENTS_LOCK.lock().await;
    let record = read_recents_at(record_path).await?;
    let original_len = record.entries.len();
    let mut entries = Vec::with_capacity(original_len);
    for entry in record.entries {
        if tokio::fs::try_exists(&entry.path)
            .await
            .with_context(|| format!("failed to inspect {}", entry.path.display()))?
        {
            entries.push(entry);
        }
    }
    sort_recents(&mut entries);
    if entries.len() != original_len {
        write_recents_at(
            record_path,
            &RecentEntriesFile {
                entries: entries.clone(),
            },
        )
        .await?;
    }
    Ok(entries)
}

async fn remove_from_recents_at(record_path: &Path, path: &Path) -> Result<()> {
    let path = canonicalize_or_normalize(path).await?;
    let _guard = RECENTS_LOCK.lock().await;
    let mut record = read_recents_at(record_path).await?;
    let original_len = record.entries.len();
    record.entries.retain(|entry| entry.path != path);
    if record.entries.len() != original_len {
        write_recents_at(record_path, &record).await?;
    }
    Ok(())
}

async fn set_pinned_at(record_path: &Path, path: &Path, pinned: bool) -> Result<()> {
    let path = canonicalize_or_normalize(path).await?;
    let _guard = RECENTS_LOCK.lock().await;
    let mut record = read_recents_at(record_path).await?;
    let mut changed = false;
    if let Some(entry) = record.entries.iter_mut().find(|entry| entry.path == path) {
        if entry.pinned != pinned {
            entry.pinned = pinned;
            changed = true;
        }
    }
    if changed {
        sort_recents(&mut record.entries);
        prune_unpinned_cap(&mut record.entries);
        write_recents_at(record_path, &record).await?;
    }
    Ok(())
}

async fn read_recents_at(record_path: &Path) -> Result<RecentEntriesFile> {
    let bytes = match tokio::fs::read(record_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecentEntriesFile {
                entries: Vec::new(),
            })
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", record_path.display()))
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", record_path.display()))
}

async fn write_recents_at(record_path: &Path, record: &RecentEntriesFile) -> Result<()> {
    if let Some(parent) = record_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let bytes = serde_json::to_vec_pretty(record)?;
    let temp_path = record_path.with_file_name(format!(
        ".{}.{}.tmp",
        record_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recents.json"),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp_path, bytes)
        .await
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if let Err(error) = tokio::fs::rename(&temp_path, record_path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to rename {}", record_path.display()));
    }
    Ok(())
}

async fn canonicalize_or_normalize(path: &Path) -> Result<PathBuf> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => lexical_normalize(path),
        Err(error) => {
            Err(error).with_context(|| format!("failed to canonicalize {}", path.display()))
        }
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !is_root_or_prefix_only(&normalized) {
                    normalized.pop();
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

fn is_root_or_prefix_only(path: &Path) -> bool {
    let mut saw_anchor = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => saw_anchor = true,
            _ => return false,
        }
    }
    saw_anchor
}

fn sort_recents(entries: &mut [RecentEntry]) {
    entries.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.last_opened.cmp(&left.last_opened))
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn prune_unpinned_cap(entries: &mut Vec<RecentEntry>) {
    let mut unpinned_seen = 0usize;
    entries.retain(|entry| {
        if entry.pinned {
            return true;
        }
        unpinned_seen += 1;
        unpinned_seen <= MAX_UNPINNED_RECENTS
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::collections::HashSet;

    fn tempdir(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("temp dir")
    }

    async fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("parent dir writes");
        }
        tokio::fs::write(path, b"{}").await.expect("file writes");
    }

    #[tokio::test]
    async fn record_open_canonicalizes_path_and_marks_scratch() {
        let temp_dir = tempdir("spur-recents-canonical-");
        let record_path = temp_dir.path().join("recents.json");
        let scratch_dir = temp_dir.path().join("scratch");
        let notebook_path = scratch_dir.join("nested").join("analysis.ipynb");
        touch(&notebook_path).await;

        let redundant_path = scratch_dir
            .join("nested")
            .join("..")
            .join("nested")
            .join("analysis.ipynb");

        record_open_at(&record_path, &scratch_dir, &redundant_path)
            .await
            .expect("record writes");
        record_open_at(&record_path, &scratch_dir, &notebook_path)
            .await
            .expect("record upserts");

        let entries = list_recents_at(&record_path)
            .await
            .expect("recents list reads");
        let canonical_path = tokio::fs::canonicalize(&notebook_path)
            .await
            .expect("canonical path");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, canonical_path);
        assert!(entries[0].is_scratch);
        assert!(!entries[0].pinned);
    }

    #[tokio::test]
    async fn record_open_caps_unpinned_entries_but_keeps_pinned_entries() {
        let temp_dir = tempdir("spur-recents-cap-");
        let record_path = temp_dir.path().join("recents.json");
        let scratch_dir = temp_dir.path().join("scratch");
        let notebooks_dir = temp_dir.path().join("notebooks");
        let pinned_a = notebooks_dir.join("pinned-a.ipynb");
        let pinned_b = notebooks_dir.join("pinned-b.ipynb");
        touch(&pinned_a).await;
        touch(&pinned_b).await;

        record_open_at(&record_path, &scratch_dir, &pinned_a)
            .await
            .expect("first pinned candidate records");
        record_open_at(&record_path, &scratch_dir, &pinned_b)
            .await
            .expect("second pinned candidate records");
        set_pinned_at(&record_path, &pinned_a, true)
            .await
            .expect("first entry pins");
        set_pinned_at(&record_path, &pinned_b, true)
            .await
            .expect("second entry pins");

        for index in 0..55 {
            let path = notebooks_dir.join(format!("regular-{index}.ipynb"));
            touch(&path).await;
            record_open_at(&record_path, &scratch_dir, &path)
                .await
                .expect("regular entry records");
        }

        let entries = list_recents_at(&record_path)
            .await
            .expect("recents list reads");
        let pinned_paths = entries
            .iter()
            .filter(|entry| entry.pinned)
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        let unpinned_count = entries.iter().filter(|entry| !entry.pinned).count();

        assert_eq!(entries.len(), 52);
        assert_eq!(unpinned_count, 50);
        assert!(pinned_paths.contains(
            &tokio::fs::canonicalize(&pinned_a)
                .await
                .expect("canonical pinned path")
        ));
        assert!(pinned_paths.contains(
            &tokio::fs::canonicalize(&pinned_b)
                .await
                .expect("canonical pinned path")
        ));
    }

    #[tokio::test]
    async fn list_recents_sorts_and_prunes_missing_entries() {
        let temp_dir = tempdir("spur-recents-list-");
        let record_path = temp_dir.path().join("recents.json");
        let older = temp_dir.path().join("older.ipynb");
        let newer = temp_dir.path().join("newer.ipynb");
        let pinned = temp_dir.path().join("pinned.ipynb");
        let missing = temp_dir.path().join("missing.ipynb");
        touch(&older).await;
        touch(&newer).await;
        touch(&pinned).await;

        let now = Utc::now();
        write_recents_at(
            &record_path,
            &RecentEntriesFile {
                entries: vec![
                    RecentEntry {
                        path: missing.clone(),
                        last_opened: now,
                        is_scratch: false,
                        pinned: false,
                    },
                    RecentEntry {
                        path: older.clone(),
                        last_opened: now - ChronoDuration::minutes(10),
                        is_scratch: false,
                        pinned: false,
                    },
                    RecentEntry {
                        path: pinned.clone(),
                        last_opened: now - ChronoDuration::hours(1),
                        is_scratch: false,
                        pinned: true,
                    },
                    RecentEntry {
                        path: newer.clone(),
                        last_opened: now,
                        is_scratch: false,
                        pinned: false,
                    },
                ],
            },
        )
        .await
        .expect("recents writes");

        let entries = list_recents_at(&record_path)
            .await
            .expect("recents list reads");
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            vec![pinned.as_path(), newer.as_path(), older.as_path()]
        );

        let persisted = read_recents_at(&record_path)
            .await
            .expect("rewritten recents read");
        assert!(!persisted.entries.iter().any(|entry| entry.path == missing));
    }

    #[tokio::test]
    async fn concurrent_record_open_of_same_path_writes_one_entry() {
        let temp_dir = tempdir("spur-recents-concurrent-");
        let record_path = temp_dir.path().join("recents.json");
        let scratch_dir = temp_dir.path().join("scratch");
        let notebook_path = temp_dir.path().join("analysis.ipynb");
        touch(&notebook_path).await;

        let (first, second) = tokio::join!(
            record_open_at(&record_path, &scratch_dir, &notebook_path),
            record_open_at(&record_path, &scratch_dir, &notebook_path)
        );
        first.expect("first record writes");
        second.expect("second record writes");

        let entries = list_recents_at(&record_path)
            .await
            .expect("recents list reads");
        assert_eq!(entries.len(), 1);
    }
}
