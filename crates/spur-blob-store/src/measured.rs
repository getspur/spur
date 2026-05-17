//! Decorator that emits `tracing` events for every `OutcomeStore`
//! operation. Wrap any inner store; preserves its behavior.
//!
//! Event target: `spur.metrics.blob_store.*` (matches Plan-4 §12.1).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use spur_acp::BrainSessionId;
use tracing::event;

use crate::trait_def::OutcomeStore;
use crate::{
    DeleteNamespaceReport, OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section,
    StoreError, SweepReport,
};

pub struct MeasuredOutcomeStore<S: OutcomeStore> {
    inner: S,
}

impl<S: OutcomeStore> std::fmt::Debug for MeasuredOutcomeStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasuredOutcomeStore")
            .finish_non_exhaustive()
    }
}

impl<S: OutcomeStore> MeasuredOutcomeStore<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<S: OutcomeStore> OutcomeStore for MeasuredOutcomeStore<S> {
    async fn put(
        &self,
        key: &OutcomeKey,
        content: &[u8],
        metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        let start = Instant::now();
        let result = self.inner.put(key, content, metadata).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        let bytes = content.len() as u64;

        match &result {
            Ok(r) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::DEBUG,
                op = "put",
                outcome = "ok",
                bytes,
                elapsed_us,
                backend = ?r.backend,
                sha256 = %r.sha256,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "put",
                outcome = "err",
                bytes,
                elapsed_us,
                error = %e,
            ),
        }
        result
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        let start = Instant::now();
        let result = self.inner.get(key, section).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(c) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::DEBUG,
                op = "get",
                outcome = "ok",
                bytes = c.bytes.len() as u64,
                elapsed_us,
                ?section,
            ),
            Err(e) => match e {
                StoreError::NotFound(_) => event!(
                    target: "spur.metrics.blob_store",
                    tracing::Level::DEBUG,
                    op = "get",
                    outcome = "err",
                    elapsed_us,
                    error = %e,
                    ?section,
                ),
                _ => event!(
                    target: "spur.metrics.blob_store",
                    tracing::Level::WARN,
                    op = "get",
                    outcome = "err",
                    elapsed_us,
                    error = %e,
                    ?section,
                ),
            },
        }
        result
    }

    async fn delete_namespace(
        &self,
        brain_session_id: &BrainSessionId,
    ) -> Result<DeleteNamespaceReport, StoreError> {
        let start = Instant::now();
        let result = self.inner.delete_namespace(brain_session_id).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(report) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::INFO,
                op = "delete_namespace",
                outcome = "ok",
                elapsed_us,
                blobs_removed = report.count,
                total_bytes = report.total_bytes,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "delete_namespace",
                outcome = "err",
                elapsed_us,
                error = %e,
            ),
        }
        result
    }

    async fn sweep_older_than(&self, ttl: Duration) -> Result<SweepReport, StoreError> {
        let start = Instant::now();
        let result = self.inner.sweep_older_than(ttl).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
        match &result {
            Ok(r) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::INFO,
                op = "sweep",
                outcome = "ok",
                elapsed_us,
                namespaces_swept = r.namespaces_swept,
                blobs_swept = r.blobs_swept,
                bytes_freed = r.bytes_freed,
            ),
            Err(e) => event!(
                target: "spur.metrics.blob_store",
                tracing::Level::WARN,
                op = "sweep",
                outcome = "err",
                elapsed_us,
                error = %e,
            ),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OutcomeBlobKind;
    use crate::{ContentType, MemoryOutcomeStore};
    use chrono::Utc;
    use sha2::{Digest, Sha256};
    use spur_acp::SessionId;

    fn sha256_hex(content: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(content);
        let d = h.finalize();
        let mut s = String::with_capacity(64);
        for b in d {
            use std::fmt::Write;
            write!(&mut s, "{b:02x}").expect("hex write infallible");
        }
        s
    }

    fn key() -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
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
    async fn measured_store_preserves_inner_behavior() {
        let inner = MemoryOutcomeStore::new();
        let store = MeasuredOutcomeStore::new(inner);
        let k = key();
        let body = b"trace me".to_vec();

        let r = store.put(&k, &body, &metadata(&body)).await.expect("put");
        assert_eq!(r.byte_size, body.len() as u64);

        let got = store.get(&k, None).await.expect("get");
        assert_eq!(got.bytes, body);
    }

    #[tokio::test]
    async fn measured_store_propagates_errors() {
        let inner = MemoryOutcomeStore::new();
        let store = MeasuredOutcomeStore::new(inner);
        let k = key();
        let err = store.get(&k, None).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }
}
