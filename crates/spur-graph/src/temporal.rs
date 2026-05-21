use std::collections::{HashMap, HashSet};

use crate::schema::{
    ChangeKind, CommitIndexArtifact, EdgeEndpoint, GraphIndexArtifact, RelationKind, RenamePrev,
    SnapshotKey, StableSymbolId, TemporalEdgeArtifact,
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

pub type GitSha = String;

#[derive(Debug)]
pub struct TemporalIndex<'a> {
    code: &'a GraphIndexArtifact,
    edges_by_stable_symbol_id: HashMap<&'a str, Vec<&'a TemporalEdgeArtifact>>,
    edges_by_commit_sha: HashMap<&'a str, Vec<&'a TemporalEdgeArtifact>>,
    commit_positions: HashMap<&'a str, usize>,
    snapshot_keys_by_stable_symbol_id: HashMap<&'a str, Vec<SnapshotKey>>,
    rename_edges: Vec<(SnapshotKey, SnapshotKey)>,
    rename_neighbors_by_snapshot: HashMap<SnapshotKey, Vec<SnapshotKey>>,
    referenced_snapshot_count: usize,
}

impl<'a> TemporalIndex<'a> {
    pub fn new(code: &'a GraphIndexArtifact) -> Self {
        let mut edges_by_stable_symbol_id: HashMap<&'a str, Vec<&'a TemporalEdgeArtifact>> =
            HashMap::new();
        let mut edges_by_commit_sha: HashMap<&'a str, Vec<&'a TemporalEdgeArtifact>> =
            HashMap::new();
        let commit_positions = code
            .commits
            .iter()
            .enumerate()
            .map(|(index, commit)| (commit.sha.as_str(), index))
            .collect();
        let mut snapshot_key_sets: HashMap<&'a str, HashSet<SnapshotKey>> = HashMap::new();
        let mut referenced_snapshot_keys = HashSet::new();
        let mut rename_edges = Vec::new();

        for snapshot in &code.symbol_snapshots {
            insert_snapshot_key(
                &mut snapshot_key_sets,
                snapshot.key.stable_symbol_id.as_str(),
                snapshot.key.clone(),
                &mut referenced_snapshot_keys,
            );
        }

        for edge in &code.temporal_edges {
            let mut stable_symbol_ids = Vec::new();
            index_endpoint(
                &edge.source,
                &mut stable_symbol_ids,
                &mut snapshot_key_sets,
                &mut referenced_snapshot_keys,
            );
            index_endpoint(
                &edge.target,
                &mut stable_symbol_ids,
                &mut snapshot_key_sets,
                &mut referenced_snapshot_keys,
            );

            if let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &edge.change_kind {
                push_unique_stable_symbol_id(&mut stable_symbol_ids, &prev.stable_symbol_id);
                insert_snapshot_key(
                    &mut snapshot_key_sets,
                    prev.stable_symbol_id.as_str(),
                    prev.clone(),
                    &mut referenced_snapshot_keys,
                );
                if let Some(next) = rename_target(edge, prev) {
                    rename_edges.push((prev.clone(), next));
                }
            }

            for stable_symbol_id in stable_symbol_ids {
                edges_by_stable_symbol_id
                    .entry(stable_symbol_id)
                    .or_default()
                    .push(edge);
            }
            if let EdgeEndpoint::Commit { sha } = &edge.source {
                edges_by_commit_sha.entry(sha).or_default().push(edge);
            }
            if let EdgeEndpoint::Commit { sha } = &edge.target {
                edges_by_commit_sha.entry(sha).or_default().push(edge);
            }
        }

        let mut snapshot_keys_by_stable_symbol_id = HashMap::new();
        for (stable_symbol_id, keys) in snapshot_key_sets {
            let mut keys: Vec<_> = keys.into_iter().collect();
            keys.sort_by(|left, right| {
                left.commit
                    .cmp(&right.commit)
                    .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
            });
            snapshot_keys_by_stable_symbol_id.insert(stable_symbol_id, keys);
        }

        let mut rename_neighbors_by_snapshot: HashMap<SnapshotKey, Vec<SnapshotKey>> =
            HashMap::new();
        for (prev, next) in &rename_edges {
            rename_neighbors_by_snapshot
                .entry(prev.clone())
                .or_default()
                .push(next.clone());
            rename_neighbors_by_snapshot
                .entry(next.clone())
                .or_default()
                .push(prev.clone());
        }

        Self {
            code,
            edges_by_stable_symbol_id,
            edges_by_commit_sha,
            commit_positions,
            snapshot_keys_by_stable_symbol_id,
            rename_edges,
            rename_neighbors_by_snapshot,
            referenced_snapshot_count: referenced_snapshot_keys.len(),
        }
    }

    pub fn artifact(&self) -> &'a GraphIndexArtifact {
        self.code
    }

    pub fn edges_for_stable_symbol_id(
        &self,
        stable_symbol_id: &str,
    ) -> &[&'a TemporalEdgeArtifact] {
        self.edges_by_stable_symbol_id
            .get(stable_symbol_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn edges_for_commit_sha(&self, commit_sha: &str) -> &[&'a TemporalEdgeArtifact] {
        self.edges_by_commit_sha
            .get(commit_sha)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn has_commit_positions(&self) -> bool {
        !self.commit_positions.is_empty()
    }

    fn commit_position(&self, commit: &str) -> usize {
        self.commit_positions
            .get(commit)
            .copied()
            .unwrap_or(usize::MAX)
    }
}

pub enum TemporalHistorySource<'a> {
    Artifact(&'a GraphIndexArtifact),
    Index(&'a TemporalIndex<'a>),
}

impl<'a> From<&'a GraphIndexArtifact> for TemporalHistorySource<'a> {
    fn from(value: &'a GraphIndexArtifact) -> Self {
        Self::Artifact(value)
    }
}

impl<'a> From<&'a TemporalIndex<'a>> for TemporalHistorySource<'a> {
    fn from(value: &'a TemporalIndex<'a>) -> Self {
        Self::Index(value)
    }
}

pub fn symbol_history<'a, S>(
    source: S,
    commits: &CommitIndexArtifact,
    symbol: &str,
) -> Vec<(GitSha, ChangeKind, SnapshotKey)>
where
    S: Into<TemporalHistorySource<'a>>,
{
    match source.into() {
        TemporalHistorySource::Artifact(code) => {
            let index = TemporalIndex::new(code);
            symbol_history_indexed(&index, commits, symbol)
        }
        TemporalHistorySource::Index(index) => symbol_history_indexed(index, commits, symbol),
    }
}

fn symbol_history_indexed(
    index: &TemporalIndex<'_>,
    commits: &CommitIndexArtifact,
    symbol: &str,
) -> Vec<(GitSha, ChangeKind, SnapshotKey)> {
    let mut chain_keys = seed_symbol_history_keys(index, symbol);
    if close_symbol_history_chain(index, &mut chain_keys).is_err() {
        return Vec::new();
    }

    let stable_symbol_ids: HashSet<_> = chain_keys
        .iter()
        .map(|key| key.stable_symbol_id.as_str())
        .collect();
    let mut events = Vec::new();
    for stable_symbol_id in stable_symbol_ids {
        for edge in index.edges_for_stable_symbol_id(stable_symbol_id) {
            match (&edge.source, &edge.target) {
                (EdgeEndpoint::Commit { sha }, EdgeEndpoint::Snapshot { key })
                    if chain_keys.contains(key) =>
                {
                    if let Some(change_kind) = &edge.change_kind {
                        events.push((sha.clone(), change_kind.clone(), key.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    if index.has_commit_positions() {
        sort_history_events(&mut events, |commit| index.commit_position(commit));
    } else {
        let graph = CommitGraph::new(commits);
        sort_history_events(&mut events, |commit| graph.position(commit));
    }
    events.dedup();
    events
}

fn sort_history_events(
    events: &mut [(GitSha, ChangeKind, SnapshotKey)],
    position: impl Fn(&str) -> usize,
) {
    events.sort_by(|left, right| {
        position(&left.0)
            .cmp(&position(&right.0))
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.2.stable_symbol_id.cmp(&right.2.stable_symbol_id))
            .then_with(|| left.2.commit.cmp(&right.2.commit))
    });
}

fn insert_snapshot_key<'a>(
    snapshot_key_sets: &mut HashMap<&'a str, HashSet<SnapshotKey>>,
    stable_symbol_id: &'a str,
    key: SnapshotKey,
    referenced_snapshot_keys: &mut HashSet<SnapshotKey>,
) {
    referenced_snapshot_keys.insert(key.clone());
    snapshot_key_sets
        .entry(stable_symbol_id)
        .or_default()
        .insert(key);
}

fn index_endpoint<'a>(
    endpoint: &'a EdgeEndpoint,
    stable_symbol_ids: &mut Vec<&'a str>,
    snapshot_key_sets: &mut HashMap<&'a str, HashSet<SnapshotKey>>,
    referenced_snapshot_keys: &mut HashSet<SnapshotKey>,
) {
    match endpoint {
        EdgeEndpoint::Symbol { stable_symbol_id } => {
            push_unique_stable_symbol_id(stable_symbol_ids, stable_symbol_id);
        }
        EdgeEndpoint::Snapshot { key } => {
            push_unique_stable_symbol_id(stable_symbol_ids, &key.stable_symbol_id);
            insert_snapshot_key(
                snapshot_key_sets,
                key.stable_symbol_id.as_str(),
                key.clone(),
                referenced_snapshot_keys,
            );
        }
        EdgeEndpoint::File { .. } | EdgeEndpoint::Commit { .. } => {}
    }
}

fn push_unique_stable_symbol_id<'a>(
    stable_symbol_ids: &mut Vec<&'a str>,
    stable_symbol_id: &'a str,
) {
    if !stable_symbol_ids.contains(&stable_symbol_id) {
        stable_symbol_ids.push(stable_symbol_id);
    }
}

pub fn resolve_symbol_at(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: &str,
    anchor: &str,
    target: &str,
) -> Resolution<StableSymbolId> {
    let graph = CommitGraph::new(commits);

    let Some(target_ancestors) = graph.ancestors_of(target) else {
        return Resolution::Unknown {
            reason: ResolutionFailure::IndexCorrupt(format!(
                "target commit `{target}` is not indexed"
            )),
        };
    };
    if !target_ancestors.contains(anchor) {
        if !graph.contains(anchor) {
            return Resolution::Unknown {
                reason: ResolutionFailure::AnchorCommitNotIndexed(anchor.to_string()),
            };
        }
        return Resolution::Unknown {
            reason: ResolutionFailure::IndexCorrupt(format!(
                "anchor commit `{anchor}` is not reachable from target `{target}`"
            )),
        };
    }

    let mut anchor_candidates = match latest_anchor_snapshots(code, &graph, symbol, anchor) {
        Ok(candidates) => candidates,
        Err(reason) => return Resolution::Unknown { reason },
    };
    if anchor_candidates.is_empty() {
        return Resolution::Unknown {
            reason: ResolutionFailure::SymbolNotPresentAtAnchor,
        };
    }
    if anchor_candidates.len() > 1 {
        return Resolution::Ambiguous {
            candidates: stable_symbol_ids(anchor_candidates),
        };
    }

    let anchor_key = anchor_candidates.remove(0);
    let mut reachable_chain = [anchor_key.clone()].into_iter().collect();
    if let Err(reason) = close_rename_chain(code, &mut reachable_chain) {
        return Resolution::Unknown { reason };
    }

    resolve_from_anchor_key(code, &graph, &target_ancestors, anchor_key)
}

fn seed_symbol_history_keys(index: &TemporalIndex<'_>, symbol: &str) -> HashSet<SnapshotKey> {
    snapshot_keys_for_symbol_indexed(index, symbol)
        .into_iter()
        .collect()
}

fn close_symbol_history_chain(
    index: &TemporalIndex<'_>,
    chain_keys: &mut HashSet<SnapshotKey>,
) -> Result<(), ResolutionFailure> {
    loop {
        let previous_len = chain_keys.len();
        expand_stable_symbol_snapshots(index, chain_keys);
        close_rename_chain_indexed(index, chain_keys)?;
        if chain_keys.len() == previous_len {
            break;
        }
    }
    Ok(())
}

fn expand_stable_symbol_snapshots(
    index: &TemporalIndex<'_>,
    chain_keys: &mut HashSet<SnapshotKey>,
) {
    let stable_symbol_ids: HashSet<_> = chain_keys
        .iter()
        .map(|key| key.stable_symbol_id.clone())
        .collect();

    for stable_symbol_id in stable_symbol_ids {
        chain_keys.extend(snapshot_keys_for_symbol_indexed(index, &stable_symbol_id));
    }
}

fn close_rename_chain_indexed(
    index: &TemporalIndex<'_>,
    chain_keys: &mut HashSet<SnapshotKey>,
) -> Result<(), ResolutionFailure> {
    if index.rename_edges.is_empty() {
        return Ok(());
    }

    let snapshot_count = index.referenced_snapshot_count;
    let mut component = HashSet::new();
    let mut stack: Vec<_> = chain_keys.iter().cloned().collect();

    while let Some(current) = stack.pop() {
        if !component.insert(current.clone()) {
            continue;
        }
        chain_keys.insert(current.clone());
        guard_rename_chain_bound(chain_keys.len(), snapshot_count, &current)?;

        if let Some(neighbors) = index.rename_neighbors_by_snapshot.get(&current) {
            for neighbor in neighbors {
                if !component.contains(neighbor) {
                    stack.push(neighbor.clone());
                }
            }
        }
    }

    debug_assert!(chain_keys.len() <= snapshot_count);
    detect_reachable_rename_cycle(&index.rename_edges, &component)
}

fn close_rename_chain(
    code: &GraphIndexArtifact,
    chain_keys: &mut HashSet<SnapshotKey>,
) -> Result<(), ResolutionFailure> {
    let rename_edges = rename_edges(code);
    if rename_edges.is_empty() {
        return Ok(());
    }

    let snapshot_count = referenced_snapshot_keys(code).len();
    let mut component = HashSet::new();
    let mut stack: Vec<_> = chain_keys.iter().cloned().collect();

    while let Some(current) = stack.pop() {
        if !component.insert(current.clone()) {
            continue;
        }
        chain_keys.insert(current.clone());
        guard_rename_chain_bound(chain_keys.len(), snapshot_count, &current)?;

        for (prev, next) in &rename_edges {
            if prev == &current && !component.contains(next) {
                stack.push(next.clone());
            }
            if next == &current && !component.contains(prev) {
                stack.push(prev.clone());
            }
        }
    }

    debug_assert!(chain_keys.len() <= snapshot_count);
    detect_reachable_rename_cycle(&rename_edges, &component)
}

fn rename_edges(code: &GraphIndexArtifact) -> Vec<(SnapshotKey, SnapshotKey)> {
    code.temporal_edges
        .iter()
        .filter_map(|edge| {
            let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &edge.change_kind else {
                return None;
            };
            rename_target(edge, prev).map(|next| (prev.clone(), next))
        })
        .collect()
}

fn referenced_snapshot_keys(code: &GraphIndexArtifact) -> HashSet<SnapshotKey> {
    let mut keys: HashSet<_> = code
        .symbol_snapshots
        .iter()
        .map(|snapshot| snapshot.key.clone())
        .collect();

    for edge in &code.temporal_edges {
        if let EdgeEndpoint::Snapshot { key } = &edge.source {
            keys.insert(key.clone());
        }
        if let EdgeEndpoint::Snapshot { key } = &edge.target {
            keys.insert(key.clone());
        }
        if let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &edge.change_kind {
            keys.insert(prev.clone());
        }
    }

    keys
}

fn guard_rename_chain_bound(
    chain_len: usize,
    snapshot_count: usize,
    key: &SnapshotKey,
) -> Result<(), ResolutionFailure> {
    if chain_len <= snapshot_count {
        return Ok(());
    }

    Err(ResolutionFailure::IndexCorrupt(format!(
        "rename chain exceeds snapshot count at `{}`@`{}`",
        key.stable_symbol_id, key.commit
    )))
}

fn detect_reachable_rename_cycle(
    rename_edges: &[(SnapshotKey, SnapshotKey)],
    component: &HashSet<SnapshotKey>,
) -> Result<(), ResolutionFailure> {
    let mut forward: HashMap<SnapshotKey, Vec<SnapshotKey>> = HashMap::new();
    for (prev, next) in rename_edges {
        if component.contains(prev) && component.contains(next) {
            forward.entry(prev.clone()).or_default().push(next.clone());
        }
    }

    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for key in component {
        visit_rename_chain(key, &forward, &mut visited, &mut visiting)?;
    }
    Ok(())
}

fn visit_rename_chain(
    key: &SnapshotKey,
    forward: &HashMap<SnapshotKey, Vec<SnapshotKey>>,
    visited: &mut HashSet<SnapshotKey>,
    visiting: &mut HashSet<SnapshotKey>,
) -> Result<(), ResolutionFailure> {
    if visiting.contains(key) {
        return Err(ResolutionFailure::IndexCorrupt(format!(
            "cycle in rename chain at `{}`@`{}`",
            key.stable_symbol_id, key.commit
        )));
    }
    if !visited.insert(key.clone()) {
        return Ok(());
    }

    visiting.insert(key.clone());
    if let Some(next_keys) = forward.get(key) {
        for next in next_keys {
            visit_rename_chain(next, forward, visited, visiting)?;
        }
    }
    visiting.remove(key);

    Ok(())
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
                    return Resolution::Found {
                        value: current.stable_symbol_id,
                        chain,
                    };
                }
                Err(reason) => return Resolution::Unknown { reason },
            }
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

fn latest_anchor_snapshots(
    code: &GraphIndexArtifact,
    graph: &CommitGraph<'_>,
    stable_symbol_id: &str,
    anchor: &str,
) -> Result<Vec<SnapshotKey>, ResolutionFailure> {
    let Some(anchor_ancestors) = graph.ancestors_of(anchor) else {
        return Err(ResolutionFailure::AnchorCommitNotIndexed(
            anchor.to_string(),
        ));
    };

    let mut candidates: Vec<_> = code
        .symbol_snapshots
        .iter()
        .filter(|snapshot| {
            snapshot.key.stable_symbol_id == stable_symbol_id
                && anchor_ancestors.contains(&snapshot.key.commit)
        })
        .map(|snapshot| snapshot.key.clone())
        .collect();
    sort_snapshot_keys(&mut candidates, graph);
    candidates.dedup();

    latest_from_candidates(candidates, graph)
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

    latest_from_candidates(candidates, graph)
}

fn latest_from_candidates(
    candidates: Vec<SnapshotKey>,
    graph: &CommitGraph<'_>,
) -> Result<Vec<SnapshotKey>, ResolutionFailure> {
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
        (EdgeEndpoint::Snapshot { key: from }, EdgeEndpoint::Snapshot { key: to })
            if from == prev && to != prev =>
        {
            Some(to.clone())
        }
        (EdgeEndpoint::Snapshot { key: to }, EdgeEndpoint::Snapshot { key: from })
            if from == prev && to != prev =>
        {
            Some(to.clone())
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

fn snapshot_keys_for_symbol_indexed(
    index: &TemporalIndex<'_>,
    stable_symbol_id: &str,
) -> Vec<SnapshotKey> {
    index
        .snapshot_keys_by_stable_symbol_id
        .get(stable_symbol_id)
        .cloned()
        .unwrap_or_default()
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
            parent: None,
            change_kind: Some(ChangeKind::Added),
        });
        graph.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit { sha: "c2".into() },
            target: EdgeEndpoint::Snapshot {
                key: snap_new.key.clone(),
            },
            relation: RelationKind::Touches,
            parent: None,
            change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(
                snap_old.key.clone(),
            ))),
        });
        graph.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Snapshot {
                key: snap_old.key.clone(),
            },
            target: EdgeEndpoint::Snapshot {
                key: snap_new.key.clone(),
            },
            relation: RelationKind::Touches,
            parent: None,
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

    #[test]
    fn symbol_history_returns_chronological_chain() {
        let (g, c) = fixture();
        let hist = symbol_history(&g, &c, "old");

        // old (c1, Added) -> new (c2, RenamedFrom(old))
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].0, "c1");
        assert!(matches!(hist[0].1, ChangeKind::Added));
        assert_eq!(hist[0].2.stable_symbol_id, "old");
        assert_eq!(hist[1].0, "c2");
        assert!(matches!(hist[1].1, ChangeKind::RenamedFrom(_)));
        assert_eq!(hist[1].2.stable_symbol_id, "new");
    }

    #[test]
    fn symbol_history_walks_backward_across_rename_predecessors() {
        let (g, c) = fixture();
        let hist = symbol_history(&g, &c, "new");

        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].2.stable_symbol_id, "old");
        assert_eq!(hist[1].2.stable_symbol_id, "new");
    }
}
