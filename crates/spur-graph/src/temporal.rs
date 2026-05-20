use std::collections::{HashMap, HashSet};

use crate::schema::{
    ChangeKind, CommitIndexArtifact, EdgeEndpoint, GraphIndexArtifact, RelationKind, RenamePrev,
    SnapshotKey, StableSymbolId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<T> {
    Found { value: T, chain: Vec<SnapshotKey> },
    Deleted { last_seen: SnapshotKey },
    Ambiguous { candidates: Vec<T> },
    Unknown { reason: ResolutionFailure },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionFailure {
    AnchorCommitNotIndexed(String),
    SymbolNotPresentAtAnchor,
    IndexCorrupt(String),
}

pub fn resolve_symbol_at(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: &str,
    anchor: &str,
    target: &str,
) -> Resolution<StableSymbolId> {
    let graph = CommitGraph::new(commits);
    if !graph.contains(anchor) {
        return Resolution::Unknown {
            reason: ResolutionFailure::AnchorCommitNotIndexed(anchor.to_string()),
        };
    }

    let Some(target_ancestors) = graph.ancestors_of(target) else {
        return Resolution::Unknown {
            reason: ResolutionFailure::IndexCorrupt(format!(
                "target commit `{target}` is not indexed"
            )),
        };
    };
    if !target_ancestors.contains(anchor) {
        return Resolution::Unknown {
            reason: ResolutionFailure::IndexCorrupt(format!(
                "anchor commit `{anchor}` is not reachable from target `{target}`"
            )),
        };
    }

    let Some(anchor_key) = code
        .symbol_snapshots
        .iter()
        .find(|snapshot| snapshot.key.stable_symbol_id == symbol && snapshot.key.commit == anchor)
        .map(|snapshot| snapshot.key.clone())
    else {
        return Resolution::Unknown {
            reason: ResolutionFailure::SymbolNotPresentAtAnchor,
        };
    };

    resolve_from_anchor_key(code, &graph, &target_ancestors, anchor_key)
}

fn resolve_from_anchor_key(
    code: &GraphIndexArtifact,
    graph: &CommitGraph<'_>,
    target_ancestors: &HashSet<String>,
    anchor_key: SnapshotKey,
) -> Resolution<StableSymbolId> {
    let mut current = anchor_key;
    let mut chain = Vec::new();
    let mut visited = HashSet::new();

    loop {
        if !visited.insert(current.clone()) {
            return Resolution::Unknown {
                reason: ResolutionFailure::IndexCorrupt(format!(
                    "cycle in rename chain at `{}`@`{}`",
                    current.stable_symbol_id, current.commit
                )),
            };
        }

        match latest_reachable_snapshots(
            code,
            graph,
            target_ancestors,
            &current.stable_symbol_id,
            &current.commit,
        ) {
            Ok(latest) => {
                if latest
                    .iter()
                    .any(|snapshot| is_deleted_snapshot(code, snapshot))
                {
                    if let [last_seen] = latest.as_slice() {
                        return Resolution::Deleted {
                            last_seen: last_seen.clone(),
                        };
                    }
                    return Resolution::Ambiguous {
                        candidates: vec![current.stable_symbol_id],
                    };
                }
            }
            Err(reason) => return Resolution::Unknown { reason },
        }

        let mut candidates = match forward_rename_candidates(
            code,
            graph,
            target_ancestors,
            &current.stable_symbol_id,
            &current.commit,
        ) {
            Ok(candidates) => candidates,
            Err(reason) => return Resolution::Unknown { reason },
        };

        if candidates.is_empty() {
            return Resolution::Found {
                value: current.stable_symbol_id,
                chain,
            };
        }

        sort_snapshot_keys(&mut candidates, graph);
        candidates.dedup();
        if candidates.len() > 1 {
            return Resolution::Ambiguous {
                candidates: stable_symbol_ids(candidates),
            };
        }

        current = candidates.remove(0);
        chain.push(current.clone());
    }
}

fn latest_reachable_snapshots(
    code: &GraphIndexArtifact,
    graph: &CommitGraph<'_>,
    target_ancestors: &HashSet<String>,
    stable_symbol_id: &str,
    after_commit: &str,
) -> Result<Vec<SnapshotKey>, ResolutionFailure> {
    let mut candidates = snapshot_keys_for_symbol(code, stable_symbol_id);
    candidates.retain(|key| {
        target_ancestors.contains(&key.commit) && graph.is_ancestor(after_commit, &key.commit)
    });
    sort_snapshot_keys(&mut candidates, graph);
    candidates.dedup();

    let mut latest = Vec::new();
    'candidate: for candidate in &candidates {
        if !graph.contains(&candidate.commit) {
            return Err(ResolutionFailure::IndexCorrupt(format!(
                "snapshot `{}` references unindexed commit `{}`",
                candidate.stable_symbol_id, candidate.commit
            )));
        }

        for other in &candidates {
            if candidate == other {
                continue;
            }
            if graph.is_ancestor(&candidate.commit, &other.commit) {
                continue 'candidate;
            }
        }
        latest.push(candidate.clone());
    }

    Ok(latest)
}

fn forward_rename_candidates(
    code: &GraphIndexArtifact,
    graph: &CommitGraph<'_>,
    target_ancestors: &HashSet<String>,
    stable_symbol_id: &str,
    after_commit: &str,
) -> Result<Vec<SnapshotKey>, ResolutionFailure> {
    let mut candidates = Vec::new();
    for edge in &code.temporal_edges {
        if edge.relation != RelationKind::Touches {
            continue;
        }

        let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &edge.change_kind else {
            continue;
        };
        if prev.stable_symbol_id != stable_symbol_id {
            continue;
        }
        if !target_ancestors.contains(&prev.commit) {
            continue;
        }
        if !graph.contains(&prev.commit) {
            return Err(ResolutionFailure::IndexCorrupt(format!(
                "rename predecessor `{}` references unindexed commit `{}`",
                prev.stable_symbol_id, prev.commit
            )));
        }
        if !graph.is_ancestor(after_commit, &prev.commit) {
            continue;
        }

        let Some(next) = rename_target(edge, prev) else {
            continue;
        };
        if !target_ancestors.contains(&next.commit) {
            continue;
        }
        if !graph.contains(&next.commit) {
            return Err(ResolutionFailure::IndexCorrupt(format!(
                "rename target `{}` references unindexed commit `{}`",
                next.stable_symbol_id, next.commit
            )));
        }
        if !graph.is_ancestor(&prev.commit, &next.commit) {
            return Err(ResolutionFailure::IndexCorrupt(format!(
                "rename target `{}`@`{}` predates predecessor `{}`@`{}`",
                next.stable_symbol_id, next.commit, prev.stable_symbol_id, prev.commit
            )));
        }

        candidates.push(next);
    }
    Ok(candidates)
}

fn rename_target(
    edge: &crate::schema::TemporalEdgeArtifact,
    prev: &SnapshotKey,
) -> Option<SnapshotKey> {
    match (&edge.source, &edge.target) {
        (EdgeEndpoint::Commit { .. }, EdgeEndpoint::Snapshot { key }) if key != prev => {
            Some(key.clone())
        }
        (EdgeEndpoint::Snapshot { key }, EdgeEndpoint::Snapshot { key: to_prev })
            if to_prev == prev =>
        {
            Some(key.clone())
        }
        _ => None,
    }
}

fn snapshot_keys_for_symbol(code: &GraphIndexArtifact, stable_symbol_id: &str) -> Vec<SnapshotKey> {
    let mut keys: Vec<_> = code
        .symbol_snapshots
        .iter()
        .filter(|snapshot| snapshot.key.stable_symbol_id == stable_symbol_id)
        .map(|snapshot| snapshot.key.clone())
        .collect();

    for edge in &code.temporal_edges {
        if let EdgeEndpoint::Snapshot { key } = &edge.source {
            if key.stable_symbol_id == stable_symbol_id {
                keys.push(key.clone());
            }
        }
        if let EdgeEndpoint::Snapshot { key } = &edge.target {
            if key.stable_symbol_id == stable_symbol_id {
                keys.push(key.clone());
            }
        }
        if let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &edge.change_kind {
            if prev.stable_symbol_id == stable_symbol_id {
                keys.push(prev.clone());
            }
        }
    }

    keys
}

fn is_deleted_snapshot(code: &GraphIndexArtifact, snapshot: &SnapshotKey) -> bool {
    code.temporal_edges.iter().any(|edge| {
        matches!(
            (&edge.source, &edge.target, &edge.change_kind),
            (
                EdgeEndpoint::Commit { .. },
                EdgeEndpoint::Snapshot { key },
                Some(ChangeKind::Deleted)
            ) if key == snapshot
        )
    })
}

fn stable_symbol_ids(mut keys: Vec<SnapshotKey>) -> Vec<StableSymbolId> {
    keys.sort_by(|left, right| left.stable_symbol_id.cmp(&right.stable_symbol_id));
    let mut ids: Vec<_> = keys.into_iter().map(|key| key.stable_symbol_id).collect();
    ids.dedup();
    ids
}

fn sort_snapshot_keys(keys: &mut [SnapshotKey], graph: &CommitGraph<'_>) {
    keys.sort_by(|left, right| {
        graph
            .position(&left.commit)
            .cmp(&graph.position(&right.commit))
            .then_with(|| left.commit.cmp(&right.commit))
            .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });
}

struct CommitGraph<'a> {
    parents: HashMap<&'a str, &'a [String]>,
    positions: HashMap<&'a str, usize>,
}

impl<'a> CommitGraph<'a> {
    fn new(commits: &'a CommitIndexArtifact) -> Self {
        Self {
            parents: commits
                .commits
                .iter()
                .map(|commit| (commit.sha.as_str(), commit.parents.as_slice()))
                .collect(),
            positions: commits
                .commits
                .iter()
                .enumerate()
                .map(|(index, commit)| (commit.sha.as_str(), index))
                .collect(),
        }
    }

    fn contains(&self, commit: &str) -> bool {
        self.positions.contains_key(commit)
    }

    fn position(&self, commit: &str) -> usize {
        self.positions.get(commit).copied().unwrap_or(usize::MAX)
    }

    fn ancestors_of(&self, commit: &str) -> Option<HashSet<String>> {
        if !self.contains(commit) {
            return None;
        }

        let mut seen = HashSet::new();
        let mut stack = vec![commit.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(parents) = self.parents.get(current.as_str()) {
                stack.extend(parents.iter().cloned());
            }
        }
        Some(seen)
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        if ancestor == descendant {
            return self.contains(ancestor);
        }
        if !self.contains(ancestor) || !self.contains(descendant) {
            return false;
        }

        let mut seen = HashSet::new();
        let mut stack = vec![descendant];
        while let Some(current) = stack.pop() {
            if current == ancestor {
                return true;
            }
            if !seen.insert(current) {
                continue;
            }
            if let Some(parents) = self.parents.get(current) {
                stack.extend(parents.iter().map(String::as_str));
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn fixture() -> (GraphIndexArtifact, CommitIndexArtifact) {
        let mut graph = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.into(),
                content_hash_blake3: None,
            },
            manifest_version: String::new(),
            graph_content_hash: String::new(),
            file_manifests: vec![],
            files: vec![],
            symbols: vec![],
            edges: vec![],
            tombstones: vec![],
            diagnostics: vec![],
            commits: vec![],
            symbol_snapshots: vec![],
            temporal_edges: vec![],
        };

        let c1 = CommitArtifact {
            sha: "c1".into(),
            parents: vec![],
            author_time: 0,
            summary: "init".into(),
        };
        let c2 = CommitArtifact {
            sha: "c2".into(),
            parents: vec!["c1".into()],
            author_time: 1,
            summary: "rename".into(),
        };
        let c3 = CommitArtifact {
            sha: "c3".into(),
            parents: vec!["c2".into()],
            author_time: 2,
            summary: "later".into(),
        };

        let snap_old = SymbolSnapshotArtifact {
            key: SnapshotKey {
                stable_symbol_id: "old".into(),
                commit: "c1".into(),
            },
            file_path: "lib.rs".into(),
            entity_name: "old".into(),
            symbol_kind: "function".into(),
            enclosing_scope: None,
            byte_range: [0, 10],
            line_range: [1, 1],
            anchor_hash: "h1".into(),
            tokens: vec![],
        };
        let snap_new = SymbolSnapshotArtifact {
            key: SnapshotKey {
                stable_symbol_id: "new".into(),
                commit: "c2".into(),
            },
            file_path: "lib.rs".into(),
            entity_name: "new".into(),
            symbol_kind: "function".into(),
            enclosing_scope: None,
            byte_range: [0, 10],
            line_range: [1, 1],
            anchor_hash: "h1".into(),
            tokens: vec![],
        };

        graph.symbol_snapshots.push(snap_old.clone());
        graph.symbol_snapshots.push(snap_new.clone());
        graph.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit { sha: "c1".into() },
            target: EdgeEndpoint::Snapshot {
                key: snap_old.key.clone(),
            },
            relation: RelationKind::Touches,
            change_kind: Some(ChangeKind::Added),
        });
        graph.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit { sha: "c2".into() },
            target: EdgeEndpoint::Snapshot {
                key: snap_new.key.clone(),
            },
            relation: RelationKind::Touches,
            change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(
                snap_old.key.clone(),
            ))),
        });
        graph.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Snapshot {
                key: snap_new.key.clone(),
            },
            target: EdgeEndpoint::Snapshot {
                key: snap_old.key.clone(),
            },
            relation: RelationKind::Touches,
            change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(
                snap_old.key.clone(),
            ))),
        });

        let commits = CommitIndexArtifact {
            schema_version: 1,
            commits: vec![c1, c2, c3],
            refs: [("main".into(), "c3".into())].into(),
            indexed_at: "2026-05-20T12:00:00Z".into(),
            walk_strategy: WalkStrategy::Reachable,
        };

        (graph, commits)
    }

    #[test]
    fn resolves_renamed_symbol_to_target() {
        let (graph, commits) = fixture();
        let resolution = resolve_symbol_at(&graph, &commits, "old", "c1", "c3");

        match resolution {
            Resolution::Found { value, chain } => {
                assert_eq!(value, "new");
                assert_eq!(chain.len(), 1);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn resolves_unknown_anchor() {
        let (graph, commits) = fixture();
        let resolution = resolve_symbol_at(&graph, &commits, "old", "nonexistent", "c3");

        assert!(matches!(resolution, Resolution::Unknown { .. }));
    }
}
