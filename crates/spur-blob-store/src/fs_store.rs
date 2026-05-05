//! Filesystem-backed `OutcomeStore`.
//!
//! Path layout:
//!   <root>/<brain_session_id>/<delegation_id>/<attempt>.bin   # content bytes
//!   <root>/<brain_session_id>/<delegation_id>/<attempt>.meta  # OutcomeMetadata JSON
//!
//! Both `brain_session_id` and `delegation_id` accept EITHER a 36-char UUID
//! (legacy) OR a 16-char `[0-9a-f]+` short hex form (post-`bd-ttyo`):
//!   * `delegation_id` shortened by `mint_delegation_id` to fit the
//!     `br create --label` 50-char cap (60 random bits from the high 64 of a
//!     v4 UUID).
//!   * `brain_session_id` shortened by `derive_brain_session_id`
//!     (sha256-truncated from the ACP session_id) for the same cap and to
//!     make plan ownership labels deterministic across spur restarts.
//!
//! Both forms are safe against directory-traversal and shell-meta injection.
//!
//! Idempotent `put`: reads `<attempt>.meta` first, compares sha256.
//! Equal → return existing `OutcomeRef`. Different → `ContentMismatch`.
//! Missing → atomic write via tempfile-then-rename (Round 11 SF1
//! single-source-of-truth: meta.sha256 not re-hashed from disk).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Process-local monotonic counter that disambiguates concurrent `put`s
/// from different async tasks within the same process. `std::process::id()`
/// alone is not sufficient — two threads writing the same key simultaneously
/// would collide on the temp filename. Gemini Plan-2 Task 5 review.
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_tmp_nonce() -> u64 {
    TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
}

use async_trait::async_trait;
use chrono::DateTime;
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use tokio::fs;

use crate::trait_def::OutcomeStore;
use crate::{
    BackendTag, DeleteNamespaceReport, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef,
    Section, StoreError, SweepReport,
};

const MIN_TTL_SECS: u64 = 86_400;

#[derive(Debug, Clone)]
pub struct FsOutcomeStore {
    root: Arc<PathBuf>,
}

impl FsOutcomeStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
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

    fn validate_id(value: &str, field: &str) -> Result<(), StoreError> {
        match value.len() {
            16 => {
                for (i, c) in value.chars().enumerate() {
                    if !c.is_ascii_hexdigit() {
                        return Err(StoreError::Backend(format!(
                            "non-id {field}: bad char at position {i}"
                        )));
                    }
                }
                Ok(())
            }
            36 => Self::validate_uuid(value, field),
            n => Err(StoreError::Backend(format!(
                "non-id {field}: wrong length ({n}); expected 16 (short hex) or 36 (UUID)"
            ))),
        }
    }

    fn paths_for(&self, key: &OutcomeKey) -> (PathBuf, PathBuf, PathBuf) {
        let session_dir = self
            .root
            .join(key.brain_session_id.as_session_id().0.as_str());
        let delegation_dir = session_dir.join(key.delegation_id.as_str());
        let bin = delegation_dir.join(format!("{}.bin", key.attempt));
        let meta = delegation_dir.join(format!("{}.meta", key.attempt));
        (delegation_dir, bin, meta)
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
impl OutcomeStore for FsOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Self::validate_id(
            key.brain_session_id.as_session_id().0.as_str(),
            "brain_session_id",
        )?;
        Self::validate_id(key.delegation_id.as_str(), "delegation_id")?;

        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        let (dir, bin_path, meta_path) = self.paths_for(key);

        if meta_path.exists() {
            let raw = fs::read(&meta_path).await?;
            let existing_meta: OutcomeMetadata = serde_json::from_slice(&raw)
                .map_err(|e| StoreError::Backend(format!("corrupt sidecar: {e}")))?;
            if existing_meta.sha256 == new_sha {
                return Ok(OutcomeRef {
                    key: key.clone(),
                    sha256: new_sha,
                    byte_size: existing_meta.stored_byte_size,
                    backend: BackendTag::Fs,
                    git_blob_sha: None,
                });
            }
            return Err(StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: existing_meta.sha256,
                new_sha,
            });
        }

        fs::create_dir_all(&dir).await?;

        let pid = std::process::id();
        let tmp_bin = dir.join(format!(
            "{}.bin.tmp.{}.{}",
            key.attempt,
            pid,
            next_tmp_nonce()
        ));
        fs::write(&tmp_bin, content).await?;
        fs::rename(&tmp_bin, &bin_path).await?;

        let tmp_meta = dir.join(format!(
            "{}.meta.tmp.{}.{}",
            key.attempt,
            pid,
            next_tmp_nonce()
        ));
        let meta_bytes = serde_json::to_vec(metadata)
            .map_err(|e| StoreError::Backend(format!("metadata serialize: {e}")))?;
        fs::write(&tmp_meta, &meta_bytes).await?;
        fs::rename(&tmp_meta, &meta_path).await?;

        Ok(OutcomeRef {
            key: key.clone(),
            sha256: new_sha,
            byte_size: metadata.stored_byte_size,
            backend: BackendTag::Fs,
            git_blob_sha: None,
        })
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let (_, bin_path, meta_path) = self.paths_for(key);
        // No exists() pre-check — eliminates TOCTOU race with concurrent
        // delete_namespace/sweep (gemini Plan-2 Task 5 review). Map
        // io::ErrorKind::NotFound directly to StoreError::NotFound.
        let raw_meta = match fs::read(&meta_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(key.clone()));
            }
            Err(e) => return Err(StoreError::Io(e)),
        };
        let metadata: OutcomeMetadata = serde_json::from_slice(&raw_meta)
            .map_err(|e| StoreError::Backend(format!("corrupt sidecar: {e}")))?;
        let bytes = fs::read(&bin_path).await?;
        Ok(OutcomeContent { bytes, metadata })
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<DeleteNamespaceReport, StoreError> {
        Self::validate_id(
            brain_session_id.as_session_id().0.as_str(),
            "brain_session_id",
        )?;
        let session_dir = self.root.join(brain_session_id.as_session_id().0.as_str());
        if !session_dir.exists() {
            return Ok(DeleteNamespaceReport::default());
        }
        let report = collect_delete_namespace_report(&session_dir).await?;
        fs::remove_dir_all(&session_dir).await?;
        Ok(report)
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let effective_ttl = ttl.max(Duration::from_secs(MIN_TTL_SECS));
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(effective_ttl)
                .unwrap_or_else(|_| chrono::Duration::seconds(MIN_TTL_SECS as i64));

        let mut report = SweepReport {
            effective_ttl,
            ..Default::default()
        };
        if !self.root.exists() {
            return Ok(report);
        }

        let mut entries = fs::read_dir(&*self.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let session_dir = entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let newest = newest_meta_in(&session_dir).await?;
            match newest {
                Some(ts) if ts < cutoff => {
                    let stats = collect_namespace_stats(&session_dir).await?;
                    report.namespaces_swept += 1;
                    report.blobs_swept += stats.blob_count;
                    report.bytes_freed += stats.bytes;
                    fs::remove_dir_all(&session_dir).await?;
                }
                _ => continue,
            }
        }
        Ok(report)
    }
}

#[derive(Default)]
struct NamespaceStats {
    blob_count: usize,
    bytes: u64,
}

async fn collect_delete_namespace_report(
    session_dir: &Path,
) -> Result<DeleteNamespaceReport, StoreError> {
    let mut report = DeleteNamespaceReport::default();
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            let p = f.path();
            if f.file_type().await?.is_file() {
                report.total_bytes += f.metadata().await?.len();
            }
            if p.extension().and_then(|s| s.to_str()) == Some("meta") {
                report.count += 1;
            }
        }
    }
    Ok(report)
}

async fn newest_meta_in(session_dir: &Path) -> Result<Option<DateTime<chrono::Utc>>, StoreError> {
    let mut newest: Option<DateTime<chrono::Utc>> = None;
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            if f.path().extension().and_then(|s| s.to_str()) != Some("meta") {
                continue;
            }
            let raw = fs::read(f.path()).await?;
            let meta: OutcomeMetadata = match serde_json::from_slice(&raw) {
                Ok(m) => m,
                Err(_) => continue,
            };
            newest = Some(match newest {
                Some(prev) if prev > meta.created_at => prev,
                _ => meta.created_at,
            });
        }
    }
    Ok(newest)
}

async fn collect_namespace_stats(session_dir: &Path) -> Result<NamespaceStats, StoreError> {
    let mut stats = NamespaceStats::default();
    let mut delegation_dirs = fs::read_dir(session_dir).await?;
    while let Some(d) = delegation_dirs.next_entry().await? {
        if !d.path().is_dir() {
            continue;
        }
        let mut files = fs::read_dir(d.path()).await?;
        while let Some(f) = files.next_entry().await? {
            let p = f.path();
            if p.extension().and_then(|s| s.to_str()) == Some("meta") {
                let raw = fs::read(&p).await?;
                if let Ok(meta) = serde_json::from_slice::<OutcomeMetadata>(&raw) {
                    stats.blob_count += 1;
                    stats.bytes += meta.stored_byte_size;
                }
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentType;
    use chrono::Utc;
    use spur_acp::SessionId;
    use tempfile::TempDir;

    fn key(session: &str, delegation: &str, attempt: u32) -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(session.into())),
            delegation_id: delegation.into(),
            attempt,
        }
    }

    fn metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    #[tokio::test]
    async fn fs_store_put_get_roundtrip() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"hello world".to_vec();

        let r = store.put(&k, &body, &metadata(&body)).await.expect("put");
        assert_eq!(r.byte_size, body.len() as u64);
        assert_eq!(r.backend, BackendTag::Fs);

        let got = store.get(&k, Some(Section::Full)).await.expect("get");
        assert_eq!(got.bytes, body);
    }

    #[tokio::test]
    async fn fs_store_idempotent_put() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"same".to_vec();
        let m = metadata(&body);

        let a = store.put(&k, &body, &m).await.unwrap();
        let b = store.put(&k, &body, &m).await.unwrap();
        assert_eq!(a.sha256, b.sha256);
    }

    #[tokio::test]
    async fn fs_store_content_mismatch() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );

        let body_a = b"first".to_vec();
        store.put(&k, &body_a, &metadata(&body_a)).await.unwrap();

        let body_b = b"second".to_vec();
        let err = store
            .put(&k, &body_b, &metadata(&body_b))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::ContentMismatch { .. }));
    }

    #[tokio::test]
    async fn fs_store_rejects_non_uuid_session() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let bad = OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId("../etc/passwd".into())),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        };
        let body = b"x".to_vec();
        let err = store.put(&bad, &body, &metadata(&body)).await.unwrap_err();
        // brain_session_id now validates via `validate_id` (16 OR 36 char) post-bd-ttyo;
        // "../etc/passwd" is wrong-length and rejected with "non-id ... wrong length".
        assert!(
            matches!(err, StoreError::Backend(ref s) if s.contains("non-id")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn fs_store_accepts_short_hex_delegation_id() {
        // Regression: bd-ttyo's `mint_delegation_id` produces 16-char `[0-9a-f]+`
        // ids to fit the `br create --label` 50-char cap. Before this fix the
        // outcome store rejected them as "non-uuid delegation_id: wrong length
        // (16)", forcing fallback to legacy artifact storage and breaking the
        // worker-output invariant downstream.
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "d04edac4e67c4649",
            1,
        );
        let body = b"x".to_vec();
        let r = store.put(&k, &body, &metadata(&body)).await.unwrap();
        assert_eq!(r.sha256, sha256_hex(&body));
        assert!(store.get(&k, None).await.is_ok());
    }

    #[tokio::test]
    async fn fs_store_accepts_short_hex_brain_session_id() {
        // Regression: bd-ttyo Phase 2 (fd6c8947) made `brain_session_id`
        // sha256-truncated 16 hex chars (derive_brain_session_id). bd-ljsr
        // (34eb50b9) only switched the `delegation_id` validator to the
        // dual-format helper; this site still hard-required a 36-char UUID
        // for brain_session_id, surfacing as
        // "non-uuid brain_session_id: wrong length (16)" → put/delete fail →
        // worker outcome dropped. Covers both put() and delete_namespace().
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "d04edac4e67c4649",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"x".to_vec();
        let r = store.put(&k, &body, &metadata(&body)).await.unwrap();
        assert_eq!(r.sha256, sha256_hex(&body));
        assert!(store.get(&k, None).await.is_ok());

        let report = store
            .delete_namespace(&BrainSessionId::new(SessionId("d04edac4e67c4649".into())))
            .await
            .unwrap();
        assert_eq!(report.count, 1);
    }

    #[tokio::test]
    async fn fs_store_rejects_bad_short_id() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        // 15 chars (between the two valid lengths)
        let bad = OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "d04edac4e67c46".into(),
            attempt: 1,
        };
        let body = b"x".to_vec();
        let err = store.put(&bad, &body, &metadata(&body)).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(ref s) if s.contains("non-id")));
    }

    #[tokio::test]
    async fn fs_store_rejects_short_id_with_non_hex() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let bad = OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            // 16 chars but contains non-hex `g`
            delegation_id: "d04edac4e67c464g".into(),
            attempt: 1,
        };
        let body = b"x".to_vec();
        let err = store.put(&bad, &body, &metadata(&body)).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(ref s) if s.contains("non-id")));
    }

    #[tokio::test]
    async fn fs_store_namespace_isolation() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let session_a = "550e8400-e29b-41d4-a716-446655440000";
        let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let k_a = key(session_a, "deadbeef-1111-2222-3333-444455556666", 1);
        let k_b = key(session_b, "deadbeef-1111-2222-3333-bbbbbbbbbbbb", 1);
        let body = b"body".to_vec();

        store.put(&k_a, &body, &metadata(&body)).await.unwrap();
        store.put(&k_b, &body, &metadata(&body)).await.unwrap();

        let report = store
            .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
            .await
            .unwrap();
        assert_eq!(report.count, 1);

        assert!(store.get(&k_a, None).await.is_err());
        assert!(store.get(&k_b, None).await.is_ok());
    }

    #[tokio::test]
    async fn fs_store_delete_namespace_reports_total_bytes() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let session = "550e8400-e29b-41d4-a716-446655440000";
        let k = key(session, "deadbeef-1111-2222-3333-444455556666", 1);
        let bytes = b"x".repeat(1_024);
        let metadata = metadata(&bytes);

        store.put(&k, &bytes, &metadata).await.unwrap();

        let report = store
            .delete_namespace(&BrainSessionId::new(SessionId(session.into())))
            .await
            .unwrap();
        assert_eq!(report.count, 1);
        assert!(
            report.total_bytes >= 1_024,
            "expected >=1024, got {}",
            report.total_bytes
        );
    }

    #[tokio::test]
    async fn fs_store_sweep_clamps_to_one_day() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let report = store
            .sweep_older_than(Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(report.effective_ttl, Duration::from_secs(MIN_TTL_SECS));
    }

    #[tokio::test]
    async fn fs_store_sweep_drops_old_namespace() {
        let td = TempDir::new().unwrap();
        let store = FsOutcomeStore::new(td.path().to_path_buf());
        let k = key(
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
            1,
        );
        let body = b"old".to_vec();
        let mut m = metadata(&body);
        m.created_at = Utc::now() - chrono::Duration::days(2);

        store.put(&k, &body, &m).await.unwrap();
        let report = store
            .sweep_older_than(Duration::from_secs(MIN_TTL_SECS))
            .await
            .unwrap();
        assert_eq!(report.namespaces_swept, 1);
        assert_eq!(report.blobs_swept, 1);
        assert!(store.get(&k, None).await.is_err());
    }
}
