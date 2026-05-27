use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;

pub struct ArtifactStagingDir {
    staging_path: PathBuf,
    final_path: PathBuf,
    committed: bool,
}

impl ArtifactStagingDir {
    pub fn new(canonical_root: &Path, content_hash: &str) -> anyhow::Result<Self> {
        fs::create_dir_all(canonical_root)
            .with_context(|| format!("failed to create `{}`", canonical_root.display()))?;

        let final_path = canonical_root.join(format!("{content_hash}.parquet"));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_nanos();
        let staging_path = canonical_root.join(format!(
            "{content_hash}.parquet.tmp.{}.{}",
            std::process::id(),
            nonce
        ));

        if staging_path.exists() {
            fs::remove_dir_all(&staging_path).with_context(|| {
                format!(
                    "failed to remove stale staging dir `{}`",
                    staging_path.display()
                )
            })?;
        }
        fs::create_dir(&staging_path)
            .with_context(|| format!("failed to create `{}`", staging_path.display()))?;

        Ok(Self {
            staging_path,
            final_path,
            committed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.staging_path
    }

    pub fn commit(mut self) -> anyhow::Result<PathBuf> {
        if self.final_path.exists() {
            fs::remove_dir_all(&self.final_path)
                .with_context(|| format!("failed to remove `{}`", self.final_path.display()))?;
        }

        fs::rename(&self.staging_path, &self.final_path).with_context(|| {
            format!(
                "failed to atomically rename `{}` to `{}`",
                self.staging_path.display(),
                self.final_path.display()
            )
        })?;
        fsync_dir(
            self.final_path
                .parent()
                .expect("artifact final path must have a parent"),
        )?;

        self.committed = true;
        Ok(self.final_path.clone())
    }
}

impl Drop for ArtifactStagingDir {
    fn drop(&mut self) {
        if !self.committed && self.staging_path.exists() {
            let _ = fs::remove_dir_all(&self.staging_path);
        }
    }
}

fn fsync_dir(path: &Path) -> anyhow::Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory `{}` for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to fsync directory `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_removes_uncommitted_staging_dir() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let staging_path = {
            let staging = ArtifactStagingDir::new(tempdir.path(), "content-hash")?;
            let path = staging.path().to_path_buf();
            fs::write(path.join("partial"), b"partial")?;
            path
        };

        assert!(!staging_path.exists());
        Ok(())
    }

    #[test]
    fn commit_renames_staging_to_final_dir() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let staging = ArtifactStagingDir::new(tempdir.path(), "content-hash")?;
        fs::write(staging.path().join("manifest.json"), b"{}")?;

        let final_path = staging.commit()?;

        assert_eq!(final_path, tempdir.path().join("content-hash.parquet"));
        assert!(final_path.join("manifest.json").is_file());
        Ok(())
    }
}
