//! Durable on-disk registry of pgids for orphan reaping.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgidRecord {
    pub spur_pid: i32,
    /// Unix epoch seconds — canonical form, `i64`. Mac uses
    /// `proc_bsdinfo.pbi_start_tvsec`; Linux derives from
    /// `/proc/<pid>/stat` field 22 + `/proc/uptime`.
    pub spur_pid_start_time: i64,
    pub agent_name: String,
    /// Full command line (argv joined with spaces).
    pub cmd: String,
    pub pgid: i32,
    pub pgid_leader_start_time: i64,
    pub spawned_at: i64,
}

pub struct PgidRegistry {
    root: PathBuf,
}

impl PgidRegistry {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn path_for(&self, pgid: i32) -> PathBuf {
        self.root.join(format!("{pgid}.toml"))
    }

    pub fn write(&self, rec: &PgidRecord) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create {}", self.root.display()))?;
        let path = self.path_for(rec.pgid);
        let body = toml::to_string(rec).context("serialize PgidRecord")?;
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn delete(&self, pgid: i32) -> Result<()> {
        let path = self.path_for(pgid);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete {}", path.display())),
        }
    }

    /// Load every parseable `.toml` in the directory. Unparseable files
    /// emit a `warn!` and are skipped (mid-write crash defense).
    pub fn load_all(&self) -> Result<Vec<PgidRecord>> {
        let mut out = Vec::new();
        let read = match std::fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in read {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&path).and_then(|body| {
                toml::from_str::<PgidRecord>(&body)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            }) {
                Ok(rec) => out.push(rec),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "orphan_registry: skipping unparseable record"
                    );
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_load_round_trips() {
        let dir = tempdir().expect("tmpdir");
        let registry = PgidRegistry::new(dir.path());
        let rec = PgidRecord {
            spur_pid: 81282,
            spur_pid_start_time: 1_745_825_534,
            agent_name: "claude-code".into(),
            cmd: "/opt/homebrew/bin/npm exec @anthropic-ai/claude-agent-acp@0.26.0".into(),
            pgid: 8801,
            pgid_leader_start_time: 1_745_825_534,
            spawned_at: 1_745_825_534,
        };
        registry.write(&rec).expect("write");

        let loaded = registry.load_all().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pgid, 8801);
        assert_eq!(loaded[0].cmd, rec.cmd);
    }

    #[test]
    fn delete_removes_record() {
        let dir = tempdir().expect("tmpdir");
        let registry = PgidRegistry::new(dir.path());
        let rec = PgidRecord {
            spur_pid: 1,
            spur_pid_start_time: 0,
            agent_name: "a".into(),
            cmd: "c".into(),
            pgid: 9001,
            pgid_leader_start_time: 0,
            spawned_at: 0,
        };
        registry.write(&rec).expect("write");
        registry.delete(rec.pgid).expect("delete");
        assert_eq!(registry.load_all().expect("load").len(), 0);
    }

    #[test]
    fn corrupted_toml_is_skipped_not_panicked() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("9999.toml"), "this is not toml [[[")
            .expect("write garbage");
        let registry = PgidRegistry::new(dir.path());
        // Must not panic. Garbage record yields a warning + skip.
        let loaded = registry.load_all().expect("load");
        assert_eq!(loaded.len(), 0);
    }
}
