use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use jute::{
    backend::notebook::{Cell, MultilineString, NotebookRoot},
    notebook_store::{DeltaKind, NotebookDelta},
    state::State,
};
use spur_graph::extract::GraphFacts;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

const DEFAULT_REINDEX_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone)]
struct IndexedNotebook {
    facts: GraphFacts,
    cell_hashes: HashMap<String, [u8; 32]>,
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    inner: RwLock<BTreeMap<PathBuf, IndexedNotebook>>,
}

impl SymbolIndex {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn facts_for(&self, path: &Path) -> Option<GraphFacts> {
        self.inner
            .read()
            .expect("symbol index lock poisoned")
            .get(path)
            .map(|notebook| notebook.facts.clone())
    }

    pub fn reindex(&self, path: &Path, root: &NotebookRoot) -> Result<bool> {
        let cell_hashes = cell_hashes(root);
        if self
            .inner
            .read()
            .expect("symbol index lock poisoned")
            .get(path)
            .is_some_and(|indexed| indexed.cell_hashes == cell_hashes)
        {
            return Ok(false);
        }

        let bytes = serde_json::to_vec(root).context("failed to serialize notebook snapshot")?;
        let root_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let facts = spur_graph::extract_notebook_facts(root_dir, path, &bytes)
            .with_context(|| format!("failed to extract notebook facts for {}", path.display()))?;

        self.inner
            .write()
            .expect("symbol index lock poisoned")
            .insert(path.to_path_buf(), IndexedNotebook { facts, cell_hashes });
        Ok(true)
    }

    pub fn spawn_updater(index: Arc<Self>, state: Arc<State>) -> JoinHandle<()> {
        let mut deltas = state.subscribe_notebook_deltas();
        tokio::spawn(async move {
            let mut pending = BTreeSet::<PathBuf>::new();
            loop {
                tokio::select! {
                    received = deltas.recv() => {
                        match received {
                            Ok(delta) if is_structural_delta(&delta) => {
                                if let Some(path) = delta.path.as_ref() {
                                    pending.insert(PathBuf::from(path));
                                }
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                debug!(skipped, "symbol index delta subscriber lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    () = tokio::time::sleep(DEFAULT_REINDEX_DEBOUNCE), if !pending.is_empty() => {
                        let paths = std::mem::take(&mut pending);
                        for path in paths {
                            let store = state.notebook_for_path(&path);
                            let (root, _) = store.snapshot();
                            match index.reindex(&path, &root) {
                                Ok(true) => debug!(path = %path.display(), "symbol index rebuilt notebook facts"),
                                Ok(false) => debug!(path = %path.display(), "symbol index skipped unchanged notebook facts"),
                                Err(error) => warn!(%error, path = %path.display(), "symbol index reindex failed"),
                            }
                        }
                    }
                }
            }
        })
    }
}

fn is_structural_delta(delta: &NotebookDelta) -> bool {
    matches!(
        delta.kind,
        DeltaKind::Loaded { .. }
            | DeltaKind::CellWritten { .. }
            | DeltaKind::CellInserted { .. }
            | DeltaKind::CellDeleted { .. }
    )
}

fn cell_hashes(root: &NotebookRoot) -> HashMap<String, [u8; 32]> {
    root.cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let id = cell_id(cell).unwrap_or_else(|| format!("cell-{index}"));
            let source = cell_source(cell);
            (id, *blake3::hash(source.as_bytes()).as_bytes())
        })
        .collect()
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_source(cell: &Cell) -> String {
    match cell {
        Cell::Raw(cell) => multiline_to_string(&cell.source),
        Cell::Markdown(cell) => multiline_to_string(&cell.source),
        Cell::Code(cell) => multiline_to_string(&cell.source),
    }
}

fn multiline_to_string(source: &MultilineString) -> String {
    match source {
        MultilineString::Single(source) => source.clone(),
        MultilineString::Multi(lines) => lines.join(""),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use jute::{
        backend::notebook::{
            Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, NotebookRoot,
        },
        state::State,
    };

    use super::SymbolIndex;

    #[tokio::test]
    async fn index_updates_on_structural_delta() {
        let state = Arc::new(State::new());
        let index = SymbolIndex::shared();
        SymbolIndex::spawn_updater(Arc::clone(&index), Arc::clone(&state));

        let path = PathBuf::from("/tmp/idx.ipynb");
        state.notebook_for_path(&path).load(&path, test_root());

        tokio::time::sleep(Duration::from_millis(300)).await;

        let facts = index.facts_for(&path).expect("indexed");
        assert!(facts.nodes.iter().any(|node| node.label.contains("load")));
    }

    #[test]
    fn reindex_skips_when_cell_hashes_are_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("idx.ipynb");
        let index = SymbolIndex::default();
        let root = test_root();

        assert!(index.reindex(&path, &root).unwrap());
        assert!(!index.reindex(&path, &root).unwrap());
    }

    fn test_root() -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some("load-cell".to_string()),
                metadata: CellMetadata {
                    spur: None,
                    jute_deck: None,
                    other: Default::default(),
                },
                source: MultilineString::Single("def load():\n    return 1\n".to_string()),
                execution_count: None,
                outputs: Vec::new(),
            })],
        }
    }
}
