//! `OutcomeStore` impl backed by git blobs in a SPUR worktree.
//!
//! Lives here (not in `spur-blob-store`) because this crate already owns
//! `git update-ref` / `git cat-file` plumbing.
//!
//! Ref namespace (Round 11 MF1+MF2):
//!   refs/spur/outcomes/<session-id>/<delegation-id>-<attempt>.blob   # content
//!   refs/spur/outcomes/<session-id>/<delegation-id>-<attempt>.meta   # OutcomeMetadata JSON
//!
//! Both refs are leaves under the namespace — no D/F conflict with the
//! legacy `refs/spur/artifacts/<session-id>` ref (which remains
//! read-only during transition).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use spur_blob_store::{
    BackendTag, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, OutcomeStore, Section,
    StoreError, SweepReport,
};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct GitBlobOutcomeStore {
    repo_root: Arc<PathBuf>,
}

impl GitBlobOutcomeStore {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root: Arc::new(repo_root),
        }
    }

    fn validate_uuid(value: &str, field: &str) -> Result<(), StoreError> {
        if value.len() != 36 {
            return Err(StoreError::Backend(format!(
                "non-uuid {field}: wrong length ({})",
                value.len()
            )));
        }
        for (i, c) in value.chars().enumerate() {
            let ok = match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            };
            if !ok {
                return Err(StoreError::Backend(format!(
                    "non-uuid {field}: bad char at position {i}"
                )));
            }
        }
        Ok(())
    }

    fn brain_session_str(session: &BrainSessionId) -> &str {
        session.as_session_id().0.as_str()
    }

    fn blob_ref(key: &OutcomeKey) -> String {
        format!(
            "refs/spur/outcomes/{}/{}-{}.blob",
            Self::brain_session_str(&key.brain_session_id),
            key.delegation_id.as_str(),
            key.attempt,
        )
    }

    fn meta_ref(key: &OutcomeKey) -> String {
        format!(
            "refs/spur/outcomes/{}/{}-{}.meta",
            Self::brain_session_str(&key.brain_session_id),
            key.delegation_id.as_str(),
            key.attempt,
        )
    }

    fn session_ref_glob(session: &BrainSessionId) -> String {
        format!("refs/spur/outcomes/{}/", Self::brain_session_str(session))
    }

    async fn run_git(&self, args: &[&str]) -> Result<Vec<u8>, StoreError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if !output.status.success() {
            return Err(StoreError::Backend(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }

    async fn run_git_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<Vec<u8>, StoreError> {
        use tokio::io::AsyncWriteExt;
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&*self.repo_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if let Some(mut sin) = child.stdin.take() {
            sin.write_all(stdin)
                .await
                .map_err(|e| StoreError::Backend(format!("git stdin: {e}")))?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| StoreError::Backend(format!("git wait: {e}")))?;
        if !output.status.success() {
            return Err(StoreError::Backend(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }

    async fn read_meta(&self, key: &OutcomeKey) -> Result<Option<OutcomeMetadata>, StoreError> {
        let meta_ref = Self::meta_ref(key);
        let rev_parse = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &meta_ref])
            .current_dir(&*self.repo_root)
            .output()
            .await
            .map_err(|e| StoreError::Backend(format!("git spawn: {e}")))?;
        if !rev_parse.status.success() {
            return Ok(None);
        }
        let sha = String::from_utf8_lossy(&rev_parse.stdout)
            .trim()
            .to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        let raw = self.run_git(&["cat-file", "-p", &sha]).await?;
        let meta: OutcomeMetadata = serde_json::from_slice(&raw)
            .map_err(|e| StoreError::Backend(format!("corrupt meta sidecar: {e}")))?;
        Ok(Some(meta))
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("hex write infallible");
    }
    hex
}

#[async_trait]
impl OutcomeStore for GitBlobOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Self::validate_uuid(
            Self::brain_session_str(&key.brain_session_id),
            "brain_session_id",
        )?;
        Self::validate_uuid(key.delegation_id.as_str(), "delegation_id")?;

        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        if let Some(existing) = self.read_meta(key).await? {
            if existing.sha256 == new_sha {
                return Ok(OutcomeRef {
                    key: key.clone(),
                    sha256: new_sha,
                    byte_size: existing.stored_byte_size,
                    backend: BackendTag::GitBlob,
                });
            }
            return Err(StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: existing.sha256,
                new_sha,
            });
        }

        // Write the content blob.
        let blob_sha_bytes = self
            .run_git_with_stdin(&["hash-object", "-w", "--stdin"], content)
            .await?;
        let blob_sha = String::from_utf8_lossy(&blob_sha_bytes).trim().to_string();
        let blob_ref = Self::blob_ref(key);
        self.run_git(&["update-ref", &blob_ref, &blob_sha]).await?;

        // Write the meta blob.
        let meta_bytes = serde_json::to_vec(metadata)
            .map_err(|e| StoreError::Backend(format!("metadata serialize: {e}")))?;
        let meta_blob_sha_bytes = self
            .run_git_with_stdin(&["hash-object", "-w", "--stdin"], &meta_bytes)
            .await?;
        let meta_blob_sha = String::from_utf8_lossy(&meta_blob_sha_bytes)
            .trim()
            .to_string();
        let meta_ref = Self::meta_ref(key);
        self.run_git(&["update-ref", &meta_ref, &meta_blob_sha])
            .await?;

        Ok(OutcomeRef {
            key: key.clone(),
            sha256: new_sha,
            byte_size: metadata.stored_byte_size,
            backend: BackendTag::GitBlob,
        })
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let meta = match self.read_meta(key).await? {
            Some(m) => m,
            None => return Err(StoreError::NotFound(key.clone())),
        };
        let blob_ref = Self::blob_ref(key);
        let blob_sha_out = self.run_git(&["rev-parse", "--verify", &blob_ref]).await?;
        let blob_sha = String::from_utf8_lossy(&blob_sha_out).trim().to_string();
        let bytes = self.run_git(&["cat-file", "-p", &blob_sha]).await?;
        Ok(OutcomeContent {
            bytes,
            metadata: meta,
        })
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        Self::validate_uuid(
            Self::brain_session_str(brain_session_id),
            "brain_session_id",
        )?;
        let prefix = Self::session_ref_glob(brain_session_id);
        let pattern = prefix.trim_end_matches('/');

        // List all refs under the namespace.
        let listing = self
            .run_git(&["for-each-ref", "--format=%(refname)", pattern])
            .await?;
        let listing_str = String::from_utf8_lossy(&listing);
        let refs: Vec<&str> = listing_str.lines().filter(|l| !l.is_empty()).collect();

        // Each (blob,meta) pair is one logical blob.
        let mut count = 0usize;
        for r in &refs {
            // Run update-ref -d for each ref (rare batched op; loop is fine).
            self.run_git(&["update-ref", "-d", r]).await?;
            if r.ends_with(".meta") {
                count += 1;
            }
        }

        // Also clean up the legacy ref for this session if present
        // (Round 11 MF2: legacy is read-only during transition; deleting
        // on namespace teardown removes the pre-Plan-5 debt).
        let legacy = format!(
            "refs/spur/artifacts/{}",
            Self::brain_session_str(brain_session_id)
        );
        let _ = self.run_git(&["update-ref", "-d", &legacy]).await;

        Ok(count)
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(0));

        // For each session subnamespace, find the newest .meta sidecar.
        // git for-each-ref outputs everything under refs/spur/outcomes/.
        let listing = self
            .run_git(&["for-each-ref", "--format=%(refname)", "refs/spur/outcomes/"])
            .await?;
        let listing_str = String::from_utf8_lossy(&listing);

        // Group refs by session (refs/spur/outcomes/<session>/...).
        use std::collections::BTreeMap;
        let mut by_session: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in listing_str.lines().filter(|l| !l.is_empty()) {
            if let Some(rest) = line.strip_prefix("refs/spur/outcomes/") {
                if let Some((session, _)) = rest.split_once('/') {
                    by_session
                        .entry(session.to_string())
                        .or_default()
                        .push(line.to_string());
                }
            }
        }

        let mut report = SweepReport {
            effective_ttl: ttl,
            ..Default::default()
        };

        for (_session, refs) in by_session {
            let mut newest: Option<DateTime<Utc>> = None;
            let mut total_bytes = 0u64;
            let mut blob_count = 0usize;
            for r in &refs {
                if !r.ends_with(".meta") {
                    continue;
                }
                let sha_out = match self.run_git(&["rev-parse", r]).await {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let sha = String::from_utf8_lossy(&sha_out).trim().to_string();
                let raw = match self.run_git(&["cat-file", "-p", &sha]).await {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let meta: OutcomeMetadata = match serde_json::from_slice(&raw) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                blob_count += 1;
                total_bytes += meta.stored_byte_size;
                newest = Some(match newest {
                    Some(prev) if prev > meta.created_at => prev,
                    _ => meta.created_at,
                });
            }
            if let Some(ts) = newest {
                if ts < cutoff {
                    for r in &refs {
                        let _ = self.run_git(&["update-ref", "-d", r]).await;
                    }
                    report.namespaces_swept += 1;
                    report.blobs_swept += blob_count;
                    report.bytes_freed += total_bytes;
                }
            }
        }
        Ok(report)
    }
}
