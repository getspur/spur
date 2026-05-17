//! In-process `OutcomeStore` for tests and dev.
//!
//! Same contract as `FsOutcomeStore` for idempotence and
//! `ContentMismatch`, just held in a `HashMap` instead of on disk.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use spur_acp::BrainSessionId;
use tokio::sync::RwLock;

use crate::trait_def::OutcomeStore;
use crate::{
    BackendTag, DeleteNamespaceReport, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef,
    Section, StoreError, SweepReport,
};

type MemoryStoreMap = HashMap<OutcomeKey, (Vec<u8>, OutcomeMetadata)>;

#[derive(Debug, Default, Clone)]
pub struct MemoryOutcomeStore {
    inner: Arc<RwLock<MemoryStoreMap>>,
}

impl MemoryOutcomeStore {
    pub fn new() -> Self {
        Self::default()
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
impl OutcomeStore for MemoryOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        let new_sha = sha256_hex(content);
        if new_sha != metadata.sha256 {
            return Err(StoreError::Backend(format!(
                "metadata.sha256 ({}) does not match hashed content ({})",
                metadata.sha256, new_sha
            )));
        }

        let mut map = self.inner.write().await;
        if let Some((_, existing_meta)) = map.get(key) {
            if existing_meta.sha256 != new_sha {
                return Err(StoreError::ContentMismatch {
                    key: key.clone(),
                    existing_sha: existing_meta.sha256.clone(),
                    new_sha,
                });
            }
            return Ok(OutcomeRef {
                key: key.clone(),
                sha256: existing_meta.sha256.clone(),
                byte_size: existing_meta.stored_byte_size,
                backend: BackendTag::Fs,
                git_blob_sha: None,
            });
        }

        map.insert(key.clone(), (content.to_vec(), metadata.clone()));
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
        let map = self.inner.read().await;
        match map.get(key) {
            Some((bytes, meta)) => Ok(OutcomeContent {
                bytes: bytes.clone(),
                metadata: meta.clone(),
            }),
            None => Err(StoreError::NotFound(key.clone())),
        }
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<DeleteNamespaceReport, StoreError> {
        let mut map = self.inner.write().await;
        let mut report = DeleteNamespaceReport::default();
        map.retain(|k, (bytes, _)| {
            if &k.brain_session_id == brain_session_id {
                report.count += 1;
                report.total_bytes += bytes.len() as u64;
                false
            } else {
                true
            }
        });
        Ok(report)
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::seconds(0));

        let mut map = self.inner.write().await;
        let mut report = SweepReport {
            effective_ttl: ttl,
            ..Default::default()
        };
        let mut sessions_swept: std::collections::HashSet<BrainSessionId> =
            std::collections::HashSet::new();

        map.retain(|k, (bytes, meta)| {
            if meta.created_at < cutoff {
                report.blobs_swept += 1;
                report.bytes_freed += bytes.len() as u64;
                sessions_swept.insert(k.brain_session_id.clone());
                false
            } else {
                true
            }
        });
        report.namespaces_swept = sessions_swept.len();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentType, OutcomeBlobKind};
    use chrono::Utc;
    use spur_acp::SessionId;

    fn key(session: &str, delegation: &str, attempt: u32) -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(session.into())),
            delegation_id: delegation.into(),
            attempt,
            kind: OutcomeBlobKind::ResultJson,
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
    async fn memory_store_put_get_roundtrip() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"hello world".to_vec();
        let meta = metadata(&body);

        let ref_a = store.put(&k, &body, &meta).await.expect("put");
        assert_eq!(ref_a.byte_size, body.len() as u64);
        assert_eq!(ref_a.sha256, sha256_hex(&body));

        let got = store.get(&k, Some(Section::Full)).await.expect("get");
        assert_eq!(got.bytes, body);
        assert_eq!(got.metadata.sha256, sha256_hex(&body));
    }

    #[tokio::test]
    async fn memory_store_idempotent_put_same_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"same".to_vec();
        let meta = metadata(&body);

        let ref_a = store.put(&k, &body, &meta).await.expect("first put");
        let ref_b = store.put(&k, &body, &meta).await.expect("second put");
        assert_eq!(ref_a.sha256, ref_b.sha256);
        assert_eq!(ref_a.byte_size, ref_b.byte_size);
    }

    #[tokio::test]
    async fn memory_store_content_mismatch_on_diff_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);

        let body_a = b"first".to_vec();
        let meta_a = metadata(&body_a);
        store.put(&k, &body_a, &meta_a).await.expect("first put");

        let body_b = b"second".to_vec();
        let meta_b = metadata(&body_b);
        let err = store.put(&k, &body_b, &meta_b).await.unwrap_err();
        match err {
            StoreError::ContentMismatch {
                key: ek,
                existing_sha,
                new_sha,
            } => {
                assert_eq!(ek, k);
                assert_eq!(existing_sha, meta_a.sha256);
                assert_eq!(new_sha, meta_b.sha256);
            }
            other => panic!("expected ContentMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn memory_store_get_not_found() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-missing", 1);
        let err = store.get(&k, None).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(ref nf) if nf == &k));
    }

    #[tokio::test]
    async fn memory_store_delete_namespace_removes_only_that_session() {
        let store = MemoryOutcomeStore::new();
        let session_a = "550e8400-e29b-41d4-a716-446655440000";
        let session_b = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let k_a = key(session_a, "d-a", 1);
        let k_b = key(session_b, "d-b", 1);
        let body = b"body".to_vec();
        let meta = metadata(&body);

        store.put(&k_a, &body, &meta).await.unwrap();
        store.put(&k_b, &body, &meta).await.unwrap();

        let report = store
            .delete_namespace(&BrainSessionId::new(SessionId(session_a.into())))
            .await
            .unwrap();
        assert_eq!(report.count, 1);
        assert_eq!(report.total_bytes, body.len() as u64);

        assert!(store.get(&k_a, None).await.is_err());
        assert!(store.get(&k_b, None).await.is_ok());
    }

    #[tokio::test]
    async fn memory_store_metadata_sha_must_match_content() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"body".to_vec();
        let mut meta = metadata(&body);
        meta.sha256 = "0".repeat(64);

        let err = store.put(&k, &body, &meta).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Backend(_)),
            "expected Backend error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn memory_store_sweep_drops_old_namespaces() {
        let store = MemoryOutcomeStore::new();
        let k = key("550e8400-e29b-41d4-a716-446655440000", "d-1", 1);
        let body = b"x".to_vec();
        let mut meta = metadata(&body);
        meta.created_at = Utc::now() - chrono::Duration::seconds(10);

        store.put(&k, &body, &meta).await.unwrap();
        let report = store
            .sweep_older_than(Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(report.blobs_swept, 1);
        assert_eq!(report.namespaces_swept, 1);
    }
}
