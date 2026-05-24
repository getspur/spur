use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use spur_graph::schema::GraphIndexArtifact;
use tokio::sync::{Mutex, OnceCell};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RebuildKey {
    head_oid: String,
    dirty_oid_set_hash: u64,
}

impl RebuildKey {
    pub fn from(head_oid: &str, dirty: &BTreeMap<PathBuf, [u8; 20]>) -> Self {
        let mut hasher = DefaultHasher::new();
        dirty.hash(&mut hasher);

        Self {
            head_oid: head_oid.to_string(),
            dirty_oid_set_hash: hasher.finish(),
        }
    }
}

type RebuildCell = OnceCell<Arc<GraphIndexArtifact>>;

pub(super) struct RebuildCoordinator {
    cells: Mutex<HashMap<RebuildKey, Weak<RebuildCell>>>,
}

impl RebuildCoordinator {
    pub fn new() -> Self {
        Self {
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the graph artifact for the requested rebuild key.
    ///
    /// The coordinator stores only weak cell handles. Concurrent callers share
    /// the same strong cell while awaiting the build, and dead weak entries are
    /// collected on the next access.
    pub async fn get_or_build<F, Fut>(
        &self,
        key: RebuildKey,
        build: F,
    ) -> anyhow::Result<Arc<GraphIndexArtifact>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Arc<GraphIndexArtifact>>>,
    {
        let cell = {
            let mut cells = self.cells.lock().await;
            cells.retain(|_, cell| cell.strong_count() > 0);

            if let Some(cell) = cells.get(&key).and_then(Weak::upgrade) {
                cell
            } else {
                let cell = Arc::new(RebuildCell::new());
                cells.insert(key, Arc::downgrade(&cell));
                cell
            }
        };

        let artifact = cell.get_or_try_init(build).await?;
        Ok(Arc::clone(artifact))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Poll;

    use spur_graph::schema::{GraphIndexArtifact, GraphIndexHeader};

    use super::{RebuildCoordinator, RebuildKey};

    fn key() -> RebuildKey {
        let mut dirty = BTreeMap::new();
        dirty.insert(PathBuf::from("src/lib.rs"), [7; 20]);
        RebuildKey::from("head-a", &dirty)
    }

    fn artifact(hash: &str) -> Arc<GraphIndexArtifact> {
        Arc::new(GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: hash.to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            file_node_ids: Vec::new(),
            symbols: Vec::new(),
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        })
    }

    async fn assert_pending<F>(mut future: Pin<&mut F>)
    where
        F: Future,
    {
        futures::future::poll_fn(move |cx| match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("future completed before test released the build"),
        })
        .await;
    }

    #[tokio::test]
    async fn same_key_concurrent_calls_invoke_build_once() {
        let coordinator = RebuildCoordinator::new();
        let builds = Arc::new(AtomicUsize::new(0));
        let (release_build, wait_for_release) = tokio::sync::oneshot::channel();

        let first = coordinator.get_or_build(key(), {
            let builds = Arc::clone(&builds);
            move || async move {
                builds.fetch_add(1, Ordering::SeqCst);
                wait_for_release.await.expect("release build");
                Ok(artifact("first"))
            }
        });
        tokio::pin!(first);
        assert_pending(first.as_mut()).await;

        let second = coordinator.get_or_build(key(), {
            let builds = Arc::clone(&builds);
            move || async move {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(artifact("second"))
            }
        });
        tokio::pin!(second);
        assert_pending(second.as_mut()).await;

        release_build.send(()).expect("send release");
        let first_artifact = first.await.expect("first build succeeds");
        let second_artifact = second.await.expect("second build succeeds");

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first_artifact, &second_artifact));
        assert_eq!(first_artifact.graph_content_hash, "first");
    }

    #[tokio::test]
    async fn dropped_completed_entry_is_rebuilt_on_next_access() {
        let coordinator = RebuildCoordinator::new();
        let builds = Arc::new(AtomicUsize::new(0));

        let first_artifact = coordinator
            .get_or_build(key(), {
                let builds = Arc::clone(&builds);
                move || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(artifact("first"))
                }
            })
            .await
            .expect("first build succeeds");

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(first_artifact.graph_content_hash, "first");
        drop(first_artifact);

        let second_artifact = coordinator
            .get_or_build(key(), {
                let builds = Arc::clone(&builds);
                move || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(artifact("second"))
                }
            })
            .await
            .expect("second build succeeds");

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(second_artifact.graph_content_hash, "second");
    }
}
