use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context as _};
use globset::Glob;

use crate::search::{compare_symbols, limited_search_result, matches_filters, matches_query};
use crate::temporal::TemporalIndex;
use crate::{
    CandidateRow, CodeSelectorResolution, GraphEdgeArtifact, GraphFileArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphQueryClient, GraphSymbolArtifact,
    OwnedCalleeRecord, OwnedCallerRecord, RelationKind, ResolvedSymbol, SearchOptions,
    SearchResult, SearchSymbol, SelectorResolution, CODE_SYMBOL_URI_PREFIX,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OverlayGenerationIdentity {
    pub canonical_worktree: PathBuf,
    pub indexed_graph_content_hash: String,
    pub indexed_head_oid: Option<String>,
    pub current_head_oid: String,
    pub index_identity: String,
    pub normalized_changed_set_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlayPathState {
    Tracked(String),
    Untracked(String),
    Deleted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OverlayGenerationQueryMeasurements {
    pub path_visibility_filters: u64,
    pub result_layer_merges: u64,
    pub stable_id_dedup_checks: u64,
}

impl OverlayGenerationQueryMeasurements {
    pub fn total(self) -> u64 {
        self.path_visibility_filters + self.result_layer_merges + self.stable_id_dedup_checks
    }
}

#[derive(Debug)]
pub struct OverlayFileSegment {
    file: GraphFileArtifact,
    manifest: Option<GraphFileManifestEntry>,
    symbols: Arc<[GraphSymbolArtifact]>,
    search_symbols: Arc<[SearchSymbol]>,
    edges: Arc<[GraphEdgeArtifact]>,
}

impl OverlayFileSegment {
    pub fn path(&self) -> &str {
        &self.file.file_path
    }

    pub fn file(&self) -> &GraphFileArtifact {
        &self.file
    }

    pub fn manifest(&self) -> Option<&GraphFileManifestEntry> {
        self.manifest.as_ref()
    }

    pub fn symbols(&self) -> &[GraphSymbolArtifact] {
        &self.symbols
    }

    fn edges(&self) -> &[GraphEdgeArtifact] {
        &self.edges
    }
}

#[derive(Debug)]
pub struct OverlayAdjacencySegment {
    callers: Arc<[OwnedCallerRecord]>,
    callees: Arc<[OwnedCalleeRecord]>,
}

impl OverlayAdjacencySegment {
    pub fn callers(&self) -> &[OwnedCallerRecord] {
        &self.callers
    }

    pub fn callees(&self) -> &[OwnedCalleeRecord] {
        &self.callees
    }
}

#[derive(Debug)]
struct VisibleFileSlot {
    segment: Arc<OverlayFileSegment>,
    visible_symbol_indices: Arc<[usize]>,
}

impl VisibleFileSlot {
    fn new(segment: Arc<OverlayFileSegment>, symbol_owners: &PersistentMap<String>) -> Self {
        let visible_symbol_indices = segment
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| {
                symbol_owners
                    .get(&symbol.stable_symbol_id)
                    .is_some_and(|owner| owner == segment.path())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
            .into();
        Self {
            segment,
            visible_symbol_indices,
        }
    }

    fn symbols(&self) -> impl Iterator<Item = &GraphSymbolArtifact> {
        self.visible_symbol_indices
            .iter()
            .map(|index| &self.segment.symbols[*index])
    }

    fn search_symbols(&self) -> impl Iterator<Item = &SearchSymbol> {
        self.visible_symbol_indices
            .iter()
            .map(|index| &self.segment.search_symbols[*index])
    }
}

#[derive(Debug, Clone)]
struct PersistentMap<V> {
    chunks: Arc<[Option<Arc<BTreeMap<String, V>>>; 256]>,
    len: usize,
}

impl<V> Default for PersistentMap<V> {
    fn default() -> Self {
        Self {
            chunks: Arc::new(std::array::from_fn(|_| None)),
            len: 0,
        }
    }
}

impl<V: Clone> PersistentMap<V> {
    fn get(&self, key: &str) -> Option<&V> {
        self.chunks[persistent_chunk_index(key)]
            .as_deref()
            .and_then(|chunk| chunk.get(key))
    }

    fn insert(&self, key: String, value: V) -> Self {
        let index = persistent_chunk_index(&key);
        let mut chunks = self.chunks.as_ref().clone();
        let mut chunk = chunks[index].as_deref().cloned().unwrap_or_default();
        let inserted = chunk.insert(key, value).is_none();
        chunks[index] = Some(Arc::new(chunk));
        Self {
            chunks: Arc::new(chunks),
            len: self.len + usize::from(inserted),
        }
    }

    fn remove(&self, key: &str) -> Self {
        let index = persistent_chunk_index(key);
        let Some(existing) = self.chunks[index].as_deref() else {
            return self.clone();
        };
        let mut chunk = existing.clone();
        if chunk.remove(key).is_none() {
            return self.clone();
        }
        let mut chunks = self.chunks.as_ref().clone();
        chunks[index] = (!chunk.is_empty()).then(|| Arc::new(chunk));
        Self {
            chunks: Arc::new(chunks),
            len: self.len - 1,
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.chunks
            .iter()
            .filter_map(|chunk| chunk.as_deref())
            .flat_map(|chunk| chunk.iter().map(|(key, value)| (key.as_str(), value)))
    }

    fn populated_chunk_count(&self) -> usize {
        self.chunks.iter().filter(|chunk| chunk.is_some()).count()
    }

    fn shared_chunk_count(&self, previous: &Self) -> usize {
        self.chunks
            .iter()
            .zip(previous.chunks.iter())
            .filter(|(current, previous)| match (current, previous) {
                (Some(current), Some(previous)) => Arc::ptr_eq(current, previous),
                _ => false,
            })
            .count()
    }
}

fn persistent_chunk_index(key: &str) -> usize {
    usize::from(blake3::hash(key.as_bytes()).as_bytes()[0])
}

#[derive(Debug)]
struct BaseGeneration {
    artifact: Arc<GraphIndexArtifact>,
    temporal_index: Arc<TemporalIndex>,
    segments: BTreeMap<String, Arc<OverlayFileSegment>>,
    symbol_owners: HashMap<String, String>,
}

#[derive(Debug)]
pub struct OverlayGeneration {
    base: Arc<BaseGeneration>,
    identity: Option<OverlayGenerationIdentity>,
    path_state: Arc<BTreeMap<String, OverlayPathState>>,
    overrides: BTreeMap<String, Option<Arc<OverlayFileSegment>>>,
    visible_files: PersistentMap<Arc<VisibleFileSlot>>,
    visible_symbol_owners: PersistentMap<String>,
    selector_index: PersistentMap<Arc<[String]>>,
    remap: PersistentMap<String>,
    remappable_labels: Arc<HashSet<String>>,
    adjacency: PersistentMap<Arc<OverlayAdjacencySegment>>,
    unresolved_callers_by_label: PersistentMap<Arc<[OwnedCallerRecord]>>,
    rebuilt_paths: BTreeSet<String>,
    rewritten_query_paths: BTreeSet<String>,
    rebuilt_adjacency_symbols: BTreeSet<String>,
}

impl OverlayGeneration {
    pub fn seed(base: Arc<GraphIndexArtifact>) -> anyhow::Result<Self> {
        let paths = artifact_paths(&base);
        let segments = build_segments_for_paths(&base, &paths)?;
        let mut symbol_owners = HashMap::new();
        for symbol in &base.symbols {
            if segments.contains_key(&symbol.file_path) {
                symbol_owners
                    .entry(symbol.stable_symbol_id.clone())
                    .or_insert_with(|| symbol.file_path.clone());
            }
        }

        let mut visible_symbol_owners = PersistentMap::default();
        let mut sorted_symbol_owners = symbol_owners.iter().collect::<Vec<_>>();
        sorted_symbol_owners.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (stable_symbol_id, path) in sorted_symbol_owners {
            visible_symbol_owners =
                visible_symbol_owners.insert(stable_symbol_id.clone(), path.clone());
        }
        let mut visible_files = PersistentMap::default();
        for (path, segment) in &segments {
            visible_files = visible_files.insert(
                path.clone(),
                Arc::new(VisibleFileSlot::new(
                    Arc::clone(segment),
                    &visible_symbol_owners,
                )),
            );
        }

        let temporal_index = Arc::new(TemporalIndex::new(Arc::clone(&base)));
        let mut generation = Self {
            base: Arc::new(BaseGeneration {
                artifact: base,
                temporal_index,
                segments,
                symbol_owners,
            }),
            identity: None,
            path_state: Arc::new(BTreeMap::new()),
            overrides: BTreeMap::new(),
            visible_files,
            visible_symbol_owners,
            selector_index: PersistentMap::default(),
            remap: PersistentMap::default(),
            remappable_labels: Arc::new(HashSet::new()),
            adjacency: PersistentMap::default(),
            unresolved_callers_by_label: PersistentMap::default(),
            rebuilt_paths: BTreeSet::new(),
            rewritten_query_paths: BTreeSet::new(),
            rebuilt_adjacency_symbols: BTreeSet::new(),
        };
        generation.selector_index = build_selector_index(&generation);
        let (adjacency, unresolved_callers_by_label) = build_full_adjacency(&generation);
        generation.adjacency = adjacency;
        generation.unresolved_callers_by_label = unresolved_callers_by_label;
        Ok(generation)
    }

    pub fn update(
        previous: &Arc<Self>,
        identity: OverlayGenerationIdentity,
        path_state: &BTreeMap<String, OverlayPathState>,
        delta: Arc<GraphIndexArtifact>,
    ) -> anyhow::Result<Self> {
        if identity.indexed_graph_content_hash != previous.base.artifact.graph_content_hash {
            bail!(
                "overlay generation base hash mismatch: identity={}, base={}",
                identity.indexed_graph_content_hash,
                previous.base.artifact.graph_content_hash
            );
        }

        let mut rebuilt_paths = previous
            .path_state
            .keys()
            .filter(|path| !path_state.contains_key(*path))
            .cloned()
            .collect::<BTreeSet<_>>();
        let rebuild_segments = path_state
            .iter()
            .filter(|(path, state)| {
                !matches!(state, OverlayPathState::Deleted)
                    && previous.path_state.get(*path) != Some(*state)
            })
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();
        let mut built_segments = build_segments_for_paths(&delta, &rebuild_segments)?;
        let mut overrides = BTreeMap::new();

        for (path, state) in path_state {
            if previous.path_state.get(path) == Some(state) {
                overrides.insert(path.clone(), previous.file_segment(path));
                continue;
            }

            rebuilt_paths.insert(path.clone());
            let segment = match state {
                OverlayPathState::Deleted => None,
                OverlayPathState::Tracked(_) | OverlayPathState::Untracked(_) => Some(
                    built_segments
                        .remove(path)
                        .with_context(|| format!("delta is missing changed path `{path}`"))?,
                ),
            };
            overrides.insert(path.clone(), segment);
        }

        let mut delta_symbol_owners = HashMap::new();
        for (path, segment) in &overrides {
            let Some(segment) = segment else {
                continue;
            };
            for symbol in segment.symbols() {
                delta_symbol_owners
                    .entry(symbol.stable_symbol_id.clone())
                    .or_insert_with(|| path.clone());
            }
        }

        let mut affected_stable_ids = BTreeSet::new();
        for path in &rebuilt_paths {
            collect_segment_stable_ids(
                previous.overrides.get(path).and_then(Option::as_deref),
                &mut affected_stable_ids,
            );
            collect_segment_stable_ids(
                previous.base.segments.get(path).map(Arc::as_ref),
                &mut affected_stable_ids,
            );
            collect_segment_stable_ids(
                current_file_segment(&previous.base, path_state, &overrides, path).as_deref(),
                &mut affected_stable_ids,
            );
        }

        let mut visible_symbol_owners = previous.visible_symbol_owners.clone();
        let mut rewritten_query_paths = rebuilt_paths.clone();
        for stable_symbol_id in affected_stable_ids {
            let previous_owner = previous
                .visible_symbol_owners
                .get(&stable_symbol_id)
                .cloned();
            let current_owner = current_symbol_owner(
                &stable_symbol_id,
                &delta_symbol_owners,
                &previous.base,
                path_state,
            );
            if previous_owner == current_owner {
                continue;
            }
            if let Some(path) = previous_owner {
                rewritten_query_paths.insert(path);
            }
            if let Some(path) = current_owner.as_ref() {
                rewritten_query_paths.insert(path.clone());
                visible_symbol_owners =
                    visible_symbol_owners.insert(stable_symbol_id, path.clone());
            } else {
                visible_symbol_owners = visible_symbol_owners.remove(&stable_symbol_id);
            }
        }

        let mut visible_files = previous.visible_files.clone();
        for path in &rewritten_query_paths {
            let segment = current_file_segment(&previous.base, path_state, &overrides, path);
            visible_files = match segment {
                Some(segment) => visible_files.insert(
                    path.clone(),
                    Arc::new(VisibleFileSlot::new(segment, &visible_symbol_owners)),
                ),
                None => visible_files.remove(path),
            };
        }

        let selector_index = update_selector_index(
            previous,
            &visible_files,
            &visible_symbol_owners,
            &rewritten_query_paths,
        );
        let current_remap = build_generation_remap(&previous.base, path_state, &overrides);
        let remap = update_string_index(&previous.remap, &current_remap);
        let remappable_labels = Arc::new(changed_definition_labels(path_state, &overrides));
        let mut generation = Self {
            base: Arc::clone(&previous.base),
            identity: Some(identity),
            path_state: Arc::new(path_state.clone()),
            overrides,
            visible_files,
            visible_symbol_owners,
            selector_index,
            remap,
            remappable_labels,
            adjacency: previous.adjacency.clone(),
            unresolved_callers_by_label: previous.unresolved_callers_by_label.clone(),
            rebuilt_paths,
            rewritten_query_paths,
            rebuilt_adjacency_symbols: BTreeSet::new(),
        };
        rebuild_changed_adjacency(previous, &mut generation);
        Ok(generation)
    }

    pub fn identity(&self) -> Option<&OverlayGenerationIdentity> {
        self.identity.as_ref()
    }

    pub fn path_state(&self) -> &BTreeMap<String, OverlayPathState> {
        &self.path_state
    }

    pub fn rebuilt_paths(&self) -> &BTreeSet<String> {
        &self.rebuilt_paths
    }

    pub fn rewritten_query_paths(&self) -> &BTreeSet<String> {
        &self.rewritten_query_paths
    }

    pub fn query_chunk_count(&self) -> usize {
        self.visible_files.populated_chunk_count()
    }

    pub fn shared_query_chunk_count(&self, previous: &Self) -> usize {
        self.visible_files
            .shared_chunk_count(&previous.visible_files)
    }

    pub fn file_segment(&self, path: &str) -> Option<Arc<OverlayFileSegment>> {
        self.visible_files
            .get(path)
            .map(|slot| Arc::clone(&slot.segment))
    }

    pub fn adjacency_segment(&self, sid: &str) -> Option<Arc<OverlayAdjacencySegment>> {
        self.adjacency.get(sid).cloned()
    }

    pub fn rebuilt_adjacency_symbols(&self) -> &BTreeSet<String> {
        &self.rebuilt_adjacency_symbols
    }

    pub fn search_symbols(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        self.search_symbols_counted(options)
    }

    pub fn search_symbols_with_measurements(
        &self,
        options: &SearchOptions,
        _measurements: &mut OverlayGenerationQueryMeasurements,
    ) -> anyhow::Result<SearchResult> {
        self.search_symbols_counted(options)
    }

    fn search_symbols_counted(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        let glob = options
            .filters
            .file_glob
            .as_deref()
            .and_then(|pattern| Glob::new(pattern).ok())
            .map(|glob| glob.compile_matcher());
        let mut candidates = self
            .visible_files
            .iter()
            .flat_map(|(_, slot)| slot.search_symbols())
            .filter(|symbol| matches_query(symbol, options))
            .filter(|symbol| matches_filters(symbol, &options.filters, glob.as_ref()))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| compare_symbols(left, right, options));
        let total_matches = candidates.len();
        Ok(limited_search_result(
            candidates,
            total_matches,
            options.limit,
        ))
    }

    pub fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution> {
        Ok(self.resolve_selector_inner(selector))
    }

    pub fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        let owner = self.visible_symbol_owners.get(sid);
        let Some(owner) = owner else {
            return Ok(None);
        };
        Ok(self.visible_files.get(owner).and_then(|slot| {
            slot.symbols()
                .find(|symbol| symbol.stable_symbol_id == sid)
                .cloned()
        }))
    }

    pub fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        Ok(self
            .visible_files
            .get(path)
            .map(|slot| slot.symbols().cloned().collect())
            .unwrap_or_default())
    }

    pub fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        Ok(self
            .symbols_by_file(path)?
            .into_iter()
            .filter(|symbol| symbol.entity_name == name || symbol.qualified_name == name)
            .collect())
    }

    pub fn file_manifest_by_path(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        Ok(self
            .visible_files
            .get(path)
            .and_then(|slot| slot.segment.manifest().cloned()))
    }

    pub fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        Ok(self.visible_files.get(path).is_some())
    }

    pub fn base_artifact(&self) -> &Arc<GraphIndexArtifact> {
        &self.base.artifact
    }

    fn visible_symbols(&self) -> impl Iterator<Item = &GraphSymbolArtifact> {
        self.visible_files
            .iter()
            .flat_map(|(_, slot)| slot.symbols())
    }

    fn visible_file_paths(&self) -> impl Iterator<Item = &str> {
        self.visible_files.iter().map(|(path, _)| path)
    }

    fn resolve_selector_inner(&self, selector: &str) -> SelectorResolution {
        let selector = selector.trim();
        if selector.is_empty() {
            return SelectorResolution::NotFound;
        }

        if let Some(symbol_id) = selector.strip_prefix(CODE_SYMBOL_URI_PREFIX) {
            return self.resolved_symbol_by_id(symbol_id);
        }
        if is_bare_stable_symbol_id(selector) {
            let resolution = self.resolved_symbol_by_id(selector);
            if !matches!(resolution, SelectorResolution::NotFound) {
                return resolution;
            }
        }
        if let Some(file_scoped) = selector
            .strip_prefix("file:")
            .or_else(|| selector.strip_prefix("path:"))
        {
            return self.resolve_file_scoped(file_scoped);
        }
        if let Some(resolution) = self.resolve_line_locator(selector) {
            return resolution;
        }
        if let Some(resolution) = self.resolve_file_qualified(selector) {
            return resolution;
        }
        if !first_token_contains_path_separator(selector) {
            let resolution = self.resolution_from_selector_key(&selector_index_key(
                SelectorIndexKind::Qualified,
                selector,
            ));
            if !matches!(resolution, SelectorResolution::NotFound) {
                return resolution;
            }
        }
        if selector.contains("::") {
            return SelectorResolution::NotFound;
        }
        self.resolution_from_selector_key(&selector_index_key(SelectorIndexKind::Entity, selector))
    }

    fn resolution_from_selector_key(&self, key: &str) -> SelectorResolution {
        let symbols = self
            .selector_index
            .get(key)
            .into_iter()
            .flat_map(|symbol_ids| symbol_ids.iter())
            .filter_map(|symbol_id| self.symbol_by_id(symbol_id).ok().flatten())
            .collect::<Vec<_>>();
        resolution_from_owned_symbols(symbols)
    }

    fn resolved_symbol_by_id(&self, symbol_id: &str) -> SelectorResolution {
        if symbol_id.is_empty() {
            return SelectorResolution::NotFound;
        }
        match self.symbol_by_id(symbol_id) {
            Ok(Some(symbol)) => SelectorResolution::Resolved(resolved_symbol(&symbol)),
            Ok(None) | Err(_) => SelectorResolution::NotFound,
        }
    }

    fn resolve_file_scoped(&self, selector: &str) -> SelectorResolution {
        self.resolve_line_locator(selector)
            .or_else(|| self.resolve_file_qualified(selector))
            .unwrap_or(SelectorResolution::NotFound)
    }

    fn resolve_line_locator(&self, selector: &str) -> Option<SelectorResolution> {
        let (file_path, line) = self.split_file_prefix(selector, ":")?;
        if line.starts_with(':')
            || line.is_empty()
            || !line.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let Ok(line) = line.parse::<usize>() else {
            return None;
        };
        let symbol = self
            .visible_symbols()
            .filter(|symbol| symbol.file_path == file_path)
            .filter(|symbol| symbol.line_range[0] <= line && line <= symbol.line_range[1])
            .max_by(compare_innermost);
        Some(
            symbol
                .map(resolved_symbol)
                .map(SelectorResolution::Resolved)
                .unwrap_or(SelectorResolution::NotFound),
        )
    }

    fn resolve_file_qualified(&self, selector: &str) -> Option<SelectorResolution> {
        let (file_path, chain) = self
            .split_file_prefix(selector, "::")
            .or_else(|| self.split_file_prefix(selector, ":"))?;
        let resolution = resolution_from_symbols(
            self.visible_symbols()
                .filter(|symbol| symbol.file_path == file_path && symbol.qualified_name == chain)
                .collect(),
        );
        if !matches!(resolution, SelectorResolution::NotFound) {
            return Some(resolution);
        }

        Some(resolution_from_symbols(
            self.visible_symbols()
                .filter(|symbol| symbol.file_path == file_path)
                .filter(|symbol| enclosing_scope_entity_name(symbol).as_deref() == Some(chain))
                .collect(),
        ))
    }

    fn split_file_prefix<'generation, 'selector>(
        &'generation self,
        selector: &'selector str,
        separator: &str,
    ) -> Option<(&'generation str, &'selector str)> {
        self.visible_file_paths()
            .filter_map(|file_path| {
                selector
                    .strip_prefix(file_path)
                    .and_then(|tail| tail.strip_prefix(separator))
                    .map(|tail| (file_path, tail))
            })
            .max_by_key(|(file_path, _)| file_path.len())
    }
}

impl GraphQueryClient for Arc<OverlayGeneration> {
    fn search_symbols(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        OverlayGeneration::search_symbols(self.as_ref(), options)
    }

    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord> {
        self.adjacency
            .get(sid)
            .map(|segment| segment.callers().to_vec())
            .unwrap_or_default()
    }

    fn find_unresolved_caller_edges_by_labels(
        &self,
        target_labels: &HashSet<String>,
    ) -> Vec<OwnedCallerRecord> {
        let mut records = Vec::new();
        let mut seen = HashSet::new();
        for label in target_labels {
            let Some(label_records) = self.unresolved_callers_by_label.get(label) else {
                continue;
            };
            for record in label_records.iter().cloned() {
                if seen.insert(edge_dedupe_key(record.edge())) {
                    records.push(record);
                }
            }
        }
        sort_caller_records(&mut records);
        records
    }

    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord> {
        self.adjacency
            .get(sid)
            .map(|segment| segment.callees().to_vec())
            .unwrap_or_default()
    }

    fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution> {
        OverlayGeneration::resolve_selector(self.as_ref(), selector)
    }

    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        OverlayGeneration::symbol_by_id(self.as_ref(), sid)
    }

    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        OverlayGeneration::symbols_by_file(self.as_ref(), path)
    }

    fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        OverlayGeneration::symbols_by_path_name(self.as_ref(), path, name)
    }

    fn file_manifest_by_path(&self, path: &str) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        OverlayGeneration::file_manifest_by_path(self.as_ref(), path)
    }

    fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        OverlayGeneration::file_exists(self.as_ref(), path)
    }

    fn temporal_index(&self) -> Arc<TemporalIndex> {
        Arc::clone(&self.base.temporal_index)
    }
}

#[derive(Clone, Copy)]
enum SelectorIndexKind {
    Qualified,
    Entity,
}

fn selector_index_key(kind: SelectorIndexKind, label: &str) -> String {
    let prefix = match kind {
        SelectorIndexKind::Qualified => "qualified",
        SelectorIndexKind::Entity => "entity",
    };
    format!("{prefix}\0{label}")
}

fn selector_keys(symbol: &GraphSymbolArtifact) -> [String; 2] {
    [
        selector_index_key(SelectorIndexKind::Qualified, &symbol.qualified_name),
        selector_index_key(SelectorIndexKind::Entity, &symbol.entity_name),
    ]
}

fn build_selector_index(generation: &OverlayGeneration) -> PersistentMap<Arc<[String]>> {
    let mut rows = BTreeMap::<String, Vec<String>>::new();
    for symbol in generation.visible_symbols() {
        for key in selector_keys(symbol) {
            rows.entry(key)
                .or_default()
                .push(symbol.stable_symbol_id.clone());
        }
    }

    let mut index = PersistentMap::default();
    for (key, mut symbol_ids) in rows {
        symbol_ids.sort();
        symbol_ids.dedup();
        index = index.insert(key, symbol_ids.into());
    }
    index
}

fn update_selector_index(
    previous: &OverlayGeneration,
    visible_files: &PersistentMap<Arc<VisibleFileSlot>>,
    _visible_symbol_owners: &PersistentMap<String>,
    rewritten_paths: &BTreeSet<String>,
) -> PersistentMap<Arc<[String]>> {
    let mut affected_keys = BTreeSet::new();
    for path in rewritten_paths {
        if let Some(slot) = previous.visible_files.get(path) {
            for symbol in slot.symbols() {
                affected_keys.extend(selector_keys(symbol));
            }
        }
        if let Some(slot) = visible_files.get(path) {
            for symbol in slot.symbols() {
                affected_keys.extend(selector_keys(symbol));
            }
        }
    }

    let mut index = previous.selector_index.clone();
    for key in affected_keys {
        let mut symbol_ids = previous
            .selector_index
            .get(&key)
            .map(|ids| ids.to_vec())
            .unwrap_or_default();
        symbol_ids.retain(|symbol_id| {
            previous
                .visible_symbol_owners
                .get(symbol_id)
                .is_some_and(|path| !rewritten_paths.contains(path))
        });
        for path in rewritten_paths {
            let Some(slot) = visible_files.get(path) else {
                continue;
            };
            symbol_ids.extend(
                slot.symbols()
                    .filter(|symbol| selector_keys(symbol).contains(&key))
                    .map(|symbol| symbol.stable_symbol_id.clone()),
            );
        }
        symbol_ids.sort();
        symbol_ids.dedup();
        index = if symbol_ids.is_empty() {
            index.remove(&key)
        } else {
            index.insert(key, symbol_ids.into())
        };
    }
    index
}

fn update_string_index(
    previous: &PersistentMap<String>,
    current: &HashMap<String, String>,
) -> PersistentMap<String> {
    let mut keys = previous
        .iter()
        .map(|(key, _)| key.to_owned())
        .collect::<BTreeSet<_>>();
    keys.extend(current.keys().cloned());
    let mut index = previous.clone();
    for key in keys {
        index = match current.get(&key) {
            Some(value) if previous.get(&key) == Some(value) => index,
            Some(value) => index.insert(key, value.clone()),
            None => index.remove(&key),
        };
    }
    index
}

fn build_generation_remap(
    base: &BaseGeneration,
    path_state: &BTreeMap<String, OverlayPathState>,
    overrides: &BTreeMap<String, Option<Arc<OverlayFileSegment>>>,
) -> HashMap<String, String> {
    let mut remap = HashMap::new();
    for (path, state) in path_state {
        if matches!(state, OverlayPathState::Deleted) {
            continue;
        }
        let Some(current) = overrides.get(path).and_then(Option::as_deref) else {
            continue;
        };
        let base_symbols = base
            .segments
            .get(path)
            .map(|segment| segment.symbols())
            .unwrap_or_default();
        for current_symbol in current.symbols() {
            let matching_name = |name: &str| {
                base_symbols
                    .iter()
                    .filter(|base_symbol| {
                        base_symbol.entity_name == name || base_symbol.qualified_name == name
                    })
                    .collect::<Vec<_>>()
            };
            let mut candidates = matching_name(&current_symbol.qualified_name);
            if candidates.is_empty() && current_symbol.qualified_name != current_symbol.entity_name
            {
                candidates = matching_name(&current_symbol.entity_name);
            }
            if candidates.is_empty() {
                candidates = base_symbols
                    .iter()
                    .filter(|base_symbol| base_symbol.symbol_kind == current_symbol.symbol_kind)
                    .filter(|base_symbol| {
                        ranges_overlap(base_symbol.line_range, current_symbol.line_range)
                    })
                    .collect();
            }
            candidates.sort_by(|left, right| {
                line_distance(left.line_range, current_symbol.line_range)
                    .cmp(&line_distance(right.line_range, current_symbol.line_range))
                    .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
            });
            for base_symbol in candidates {
                if base_symbol.stable_symbol_id != current_symbol.stable_symbol_id {
                    remap.insert(
                        base_symbol.stable_symbol_id.clone(),
                        current_symbol.stable_symbol_id.clone(),
                    );
                }
            }
        }
    }
    remap
}

fn changed_definition_labels(
    path_state: &BTreeMap<String, OverlayPathState>,
    overrides: &BTreeMap<String, Option<Arc<OverlayFileSegment>>>,
) -> HashSet<String> {
    path_state
        .keys()
        .filter_map(|path| overrides.get(path).and_then(Option::as_deref))
        .flat_map(OverlayFileSegment::symbols)
        .flat_map(symbol_labels)
        .collect()
}

fn symbol_labels(symbol: &GraphSymbolArtifact) -> [String; 3] {
    [
        symbol.stable_symbol_id.clone(),
        symbol.entity_name.clone(),
        symbol.qualified_name.clone(),
    ]
}

fn build_full_adjacency(
    generation: &OverlayGeneration,
) -> (
    PersistentMap<Arc<OverlayAdjacencySegment>>,
    PersistentMap<Arc<[OwnedCallerRecord]>>,
) {
    let symbol_ids = generation
        .visible_symbol_owners
        .iter()
        .map(|(symbol_id, _)| symbol_id.to_owned())
        .collect::<Vec<_>>();
    let mut callees_by_source = BTreeMap::new();
    let mut callers_by_target = BTreeMap::<String, Vec<OwnedCallerRecord>>::new();
    let mut unresolved_by_label = BTreeMap::<String, Vec<OwnedCallerRecord>>::new();
    for source_id in &symbol_ids {
        let callees = current_callees_for_source(generation, source_id);
        if let Ok(Some(source)) = generation.symbol_by_id(source_id) {
            for callee in &callees {
                let caller = caller_record_from_callee(&source, callee);
                match callee {
                    OwnedCalleeRecord::Resolved { symbol, .. } => callers_by_target
                        .entry(symbol.stable_symbol_id.clone())
                        .or_default()
                        .push(caller),
                    OwnedCalleeRecord::Unresolved { target_label, .. } => unresolved_by_label
                        .entry(target_label.clone())
                        .or_default()
                        .push(caller),
                }
            }
        }
        callees_by_source.insert(source_id.clone(), callees);
    }

    let mut adjacency = PersistentMap::default();
    for symbol_id in symbol_ids {
        let mut callers = callers_by_target.remove(&symbol_id).unwrap_or_default();
        let mut callees = callees_by_source.remove(&symbol_id).unwrap_or_default();
        sort_caller_records(&mut callers);
        sort_callee_records(&mut callees);
        adjacency = adjacency.insert(
            symbol_id,
            Arc::new(OverlayAdjacencySegment {
                callers: callers.into(),
                callees: callees.into(),
            }),
        );
    }

    let mut unresolved_index = PersistentMap::default();
    for (label, mut callers) in unresolved_by_label {
        dedupe_caller_records(&mut callers);
        unresolved_index = unresolved_index.insert(label, callers.into());
    }
    (adjacency, unresolved_index)
}

fn rebuild_changed_adjacency(previous: &OverlayGeneration, current: &mut OverlayGeneration) {
    let mut affected_ids = BTreeSet::new();
    let mut affected_labels = BTreeSet::new();
    for path in &current.rewritten_query_paths {
        if let Some(slot) = previous.visible_files.get(path) {
            for symbol in slot.symbols() {
                affected_ids.insert(symbol.stable_symbol_id.clone());
                affected_labels.extend(symbol_labels(symbol));
            }
        }
        if let Some(slot) = current.visible_files.get(path) {
            for symbol in slot.symbols() {
                affected_ids.insert(symbol.stable_symbol_id.clone());
                affected_labels.extend(symbol_labels(symbol));
            }
        }
    }
    let mut remap_keys = previous
        .remap
        .iter()
        .map(|(old, _)| old.to_owned())
        .collect::<BTreeSet<_>>();
    remap_keys.extend(current.remap.iter().map(|(old, _)| old.to_owned()));
    for old_id in remap_keys {
        let previous_new = previous.remap.get(&old_id);
        let current_new = current.remap.get(&old_id);
        if previous_new == current_new {
            continue;
        }
        affected_ids.insert(old_id);
        if let Some(symbol_id) = previous_new {
            affected_ids.insert(symbol_id.clone());
        }
        if let Some(symbol_id) = current_new {
            affected_ids.insert(symbol_id.clone());
        }
    }

    let mut affected_sources = affected_ids.clone();
    for target_id in &affected_ids {
        if let Some(segment) = previous.adjacency.get(target_id) {
            affected_sources.extend(
                segment
                    .callers()
                    .iter()
                    .map(|record| record.edge().source_stable_symbol_id.clone()),
            );
        }
    }
    for label in &affected_labels {
        if let Some(records) = previous.unresolved_callers_by_label.get(label) {
            affected_sources.extend(
                records
                    .iter()
                    .map(|record| record.edge().source_stable_symbol_id.clone()),
            );
        }
    }

    let mut new_callees = BTreeMap::<String, Vec<OwnedCalleeRecord>>::new();
    let mut affected_targets = affected_ids;
    let mut unresolved_labels = BTreeSet::new();
    for source_id in &affected_sources {
        if let Some(segment) = previous.adjacency.get(source_id) {
            for record in segment.callees() {
                match record {
                    OwnedCalleeRecord::Resolved { symbol, .. } => {
                        affected_targets.insert(symbol.stable_symbol_id.clone());
                    }
                    OwnedCalleeRecord::Unresolved { target_label, .. } => {
                        unresolved_labels.insert(target_label.clone());
                    }
                }
            }
        }
        let records = current_callees_for_source(current, source_id);
        for record in &records {
            match record {
                OwnedCalleeRecord::Resolved { symbol, .. } => {
                    affected_targets.insert(symbol.stable_symbol_id.clone());
                }
                OwnedCalleeRecord::Unresolved { target_label, .. } => {
                    unresolved_labels.insert(target_label.clone());
                }
            }
        }
        new_callees.insert(source_id.clone(), records);
    }

    let mut new_callers_by_target = BTreeMap::<String, Vec<OwnedCallerRecord>>::new();
    let mut new_unresolved_by_label = BTreeMap::<String, Vec<OwnedCallerRecord>>::new();
    for (source_id, callees) in &new_callees {
        let Ok(Some(source)) = current.symbol_by_id(source_id) else {
            continue;
        };
        for callee in callees {
            let caller = caller_record_from_callee(&source, callee);
            match callee {
                OwnedCalleeRecord::Resolved { symbol, .. } => new_callers_by_target
                    .entry(symbol.stable_symbol_id.clone())
                    .or_default()
                    .push(caller),
                OwnedCalleeRecord::Unresolved { target_label, .. } => new_unresolved_by_label
                    .entry(target_label.clone())
                    .or_default()
                    .push(caller),
            }
        }
    }

    for label in unresolved_labels {
        let mut records = previous
            .unresolved_callers_by_label
            .get(&label)
            .map(|records| records.to_vec())
            .unwrap_or_default();
        records.retain(|record| !affected_sources.contains(&record.edge().source_stable_symbol_id));
        records.extend(new_unresolved_by_label.remove(&label).unwrap_or_default());
        dedupe_caller_records(&mut records);
        current.unresolved_callers_by_label = if records.is_empty() {
            current.unresolved_callers_by_label.remove(&label)
        } else {
            current
                .unresolved_callers_by_label
                .insert(label, records.into())
        };
    }

    let mut callers_by_target = BTreeMap::<String, Vec<OwnedCallerRecord>>::new();
    for target_id in &affected_targets {
        let mut records = previous
            .adjacency
            .get(target_id)
            .map(|segment| segment.callers().to_vec())
            .unwrap_or_default();
        records.retain(|record| !affected_sources.contains(&record.edge().source_stable_symbol_id));
        records.extend(new_callers_by_target.remove(target_id).unwrap_or_default());
        dedupe_caller_records(&mut records);
        callers_by_target.insert(target_id.clone(), records);
    }

    let mut rebuilt = affected_sources.clone();
    rebuilt.extend(affected_targets.iter().cloned());
    for symbol_id in &rebuilt {
        if current.symbol_by_id(symbol_id).ok().flatten().is_none() {
            current.adjacency = current.adjacency.remove(symbol_id);
            continue;
        }
        let callers = if affected_targets.contains(symbol_id) {
            callers_by_target.remove(symbol_id).unwrap_or_default()
        } else {
            previous
                .adjacency
                .get(symbol_id)
                .map(|segment| segment.callers().to_vec())
                .unwrap_or_default()
        };
        let callees = if affected_sources.contains(symbol_id) {
            new_callees.remove(symbol_id).unwrap_or_default()
        } else {
            previous
                .adjacency
                .get(symbol_id)
                .map(|segment| segment.callees().to_vec())
                .unwrap_or_default()
        };
        current.adjacency = current.adjacency.insert(
            symbol_id.clone(),
            Arc::new(OverlayAdjacencySegment {
                callers: callers.into(),
                callees: callees.into(),
            }),
        );
    }
    current.rebuilt_adjacency_symbols = rebuilt;
}

fn current_callees_for_source(
    generation: &OverlayGeneration,
    source_id: &str,
) -> Vec<OwnedCalleeRecord> {
    let Ok(Some(source)) = generation.symbol_by_id(source_id) else {
        return Vec::new();
    };
    let Some(slot) = generation.visible_files.get(&source.file_path) else {
        return Vec::new();
    };
    let source_is_changed = generation.path_state.contains_key(&source.file_path);
    let mut records = slot
        .segment
        .edges()
        .iter()
        .filter(|edge| is_caller_relation(edge.relation))
        .filter(|edge| edge.source_stable_symbol_id == source_id)
        .filter_map(|edge| resolve_callee_edge(generation, edge.clone(), source_is_changed))
        .collect::<Vec<_>>();
    sort_callee_records(&mut records);
    records
}

fn resolve_callee_edge(
    generation: &OverlayGeneration,
    mut edge: GraphEdgeArtifact,
    source_is_changed: bool,
) -> Option<OwnedCalleeRecord> {
    if let Some(target_id) = edge.target_stable_symbol_id.clone() {
        if let Some(remapped) = generation.remap.get(&target_id) {
            edge.target_stable_symbol_id = Some(remapped.clone());
        }
        if let Some(symbol) = edge
            .target_stable_symbol_id
            .as_deref()
            .and_then(|target_id| generation.symbol_by_id(target_id).ok().flatten())
        {
            return Some(OwnedCalleeRecord::Resolved { symbol, edge });
        }
    }

    let target_label = edge.target_label.clone()?;
    let label_can_remap = source_is_changed
        || generation.remappable_labels.contains(&target_label)
        || edge
            .target_stable_symbol_id
            .as_deref()
            .is_some_and(|target_id| generation.remap.get(target_id).is_some());
    if label_can_remap {
        if let Some(symbol) = resolve_label_to_visible_symbol(generation, &target_label) {
            edge.target_stable_symbol_id = Some(symbol.stable_symbol_id.clone());
            return Some(OwnedCalleeRecord::Resolved { symbol, edge });
        }
    }
    edge.target_stable_symbol_id = None;
    Some(OwnedCalleeRecord::Unresolved { edge, target_label })
}

fn resolve_label_to_visible_symbol(
    generation: &OverlayGeneration,
    label: &str,
) -> Option<GraphSymbolArtifact> {
    match generation.resolve_selector_inner(label) {
        SelectorResolution::Resolved(resolved) => generation
            .symbol_by_id(&resolved.stable_symbol_id)
            .ok()
            .flatten(),
        SelectorResolution::Ambiguous { .. } | SelectorResolution::NotFound => None,
    }
}

fn caller_record_from_callee(
    source: &GraphSymbolArtifact,
    callee: &OwnedCalleeRecord,
) -> OwnedCallerRecord {
    match callee {
        OwnedCalleeRecord::Resolved { edge, .. } => OwnedCallerRecord::Resolved {
            caller: source.clone(),
            edge: edge.clone(),
        },
        OwnedCalleeRecord::Unresolved { edge, target_label } => OwnedCallerRecord::Unresolved {
            caller: source.clone(),
            edge: edge.clone(),
            target_label: target_label.clone(),
        },
    }
}

fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

fn edge_dedupe_key(edge: &GraphEdgeArtifact) -> String {
    format!(
        "{}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}\0{:?}",
        edge.source_stable_symbol_id,
        edge.target_stable_symbol_id,
        edge.target_label,
        edge.import_path,
        edge.receiver_text,
        edge.scope_text,
        edge.relation,
        edge.edge_kind,
        edge.bind_method,
    )
}

fn dedupe_caller_records(records: &mut Vec<OwnedCallerRecord>) {
    sort_caller_records(records);
    records.dedup_by(|left, right| edge_dedupe_key(left.edge()) == edge_dedupe_key(right.edge()));
}

fn sort_caller_records(records: &mut [OwnedCallerRecord]) {
    records.sort_by_key(|record| edge_dedupe_key(record.edge()));
}

fn sort_callee_records(records: &mut [OwnedCalleeRecord]) {
    records.sort_by_key(|record| edge_dedupe_key(record.edge()));
}

fn ranges_overlap(left: [usize; 2], right: [usize; 2]) -> bool {
    left[0] <= right[1] && right[0] <= left[1]
}

fn line_distance(left: [usize; 2], right: [usize; 2]) -> usize {
    left[0].abs_diff(right[0]) + left[1].abs_diff(right[1])
}

fn resolution_from_owned_symbols(symbols: Vec<GraphSymbolArtifact>) -> SelectorResolution {
    resolution_from_symbols(symbols.iter().collect())
}

fn collect_segment_stable_ids(
    segment: Option<&OverlayFileSegment>,
    stable_symbol_ids: &mut BTreeSet<String>,
) {
    let Some(segment) = segment else {
        return;
    };
    stable_symbol_ids.extend(
        segment
            .symbols()
            .iter()
            .map(|symbol| symbol.stable_symbol_id.clone()),
    );
}

fn current_file_segment(
    base: &BaseGeneration,
    path_state: &BTreeMap<String, OverlayPathState>,
    overrides: &BTreeMap<String, Option<Arc<OverlayFileSegment>>>,
    path: &str,
) -> Option<Arc<OverlayFileSegment>> {
    if path_state.contains_key(path) {
        return overrides.get(path).and_then(Clone::clone);
    }
    base.segments.get(path).cloned()
}

fn current_symbol_owner(
    stable_symbol_id: &str,
    delta_symbol_owners: &HashMap<String, String>,
    base: &BaseGeneration,
    path_state: &BTreeMap<String, OverlayPathState>,
) -> Option<String> {
    if let Some(owner) = delta_symbol_owners.get(stable_symbol_id) {
        return Some(owner.clone());
    }
    base.symbol_owners
        .get(stable_symbol_id)
        .filter(|owner| !path_state.contains_key(owner.as_str()))
        .cloned()
}

#[derive(Default)]
struct SegmentBuilder {
    file: Option<GraphFileArtifact>,
    manifest: Option<GraphFileManifestEntry>,
    symbols: Vec<GraphSymbolArtifact>,
    edges: Vec<GraphEdgeArtifact>,
    observed: bool,
}

fn artifact_paths(artifact: &GraphIndexArtifact) -> BTreeSet<String> {
    artifact
        .files
        .iter()
        .map(|file| file.file_path.clone())
        .chain(
            artifact
                .file_manifests
                .iter()
                .map(|manifest| manifest.path.clone()),
        )
        .chain(
            artifact
                .symbols
                .iter()
                .map(|symbol| symbol.file_path.clone()),
        )
        .collect()
}

fn build_segments_for_paths(
    artifact: &GraphIndexArtifact,
    paths: &BTreeSet<String>,
) -> anyhow::Result<BTreeMap<String, Arc<OverlayFileSegment>>> {
    let mut builders = paths
        .iter()
        .cloned()
        .map(|path| (path, SegmentBuilder::default()))
        .collect::<BTreeMap<_, _>>();
    for file in &artifact.files {
        if let Some(builder) = builders.get_mut(&file.file_path) {
            builder.file = Some(file.clone());
            builder.observed = true;
        }
    }
    for manifest in &artifact.file_manifests {
        if let Some(builder) = builders.get_mut(&manifest.path) {
            builder.manifest = Some(manifest.clone());
            builder.observed = true;
        }
    }
    for symbol in &artifact.symbols {
        if let Some(builder) = builders.get_mut(&symbol.file_path) {
            builder.symbols.push(symbol.clone());
            builder.observed = true;
        }
    }
    let mut source_paths = HashMap::new();
    for symbol in &artifact.symbols {
        source_paths
            .entry(symbol.stable_symbol_id.as_str())
            .or_insert(symbol.file_path.as_str());
    }
    for edge in &artifact.edges {
        let Some(path) = source_paths.get(edge.source_stable_symbol_id.as_str()) else {
            continue;
        };
        if let Some(builder) = builders.get_mut(*path) {
            builder.edges.push(edge.clone());
            builder.observed = true;
        }
    }

    builders
        .into_iter()
        .map(|(path, mut builder)| {
            if !builder.observed {
                bail!("artifact does not contain path `{path}`");
            }
            let file = builder.file.unwrap_or_else(|| GraphFileArtifact {
                stable_file_id: builder
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.stable_file_id.clone())
                    .unwrap_or_else(|| format!("file:{path}")),
                file_path: path.clone(),
            });
            let mut seen = HashSet::new();
            builder
                .symbols
                .retain(|symbol| seen.insert(symbol.stable_symbol_id.clone()));
            let search_symbols = builder
                .symbols
                .iter()
                .map(SearchSymbol::from)
                .collect::<Vec<_>>();
            Ok((
                path,
                Arc::new(OverlayFileSegment {
                    file,
                    manifest: builder.manifest,
                    symbols: builder.symbols.into(),
                    search_symbols: search_symbols.into(),
                    edges: builder.edges.into(),
                }),
            ))
        })
        .collect()
}

fn is_bare_stable_symbol_id(selector: &str) -> bool {
    selector.len() >= 16
        && selector
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn first_token_contains_path_separator(selector: &str) -> bool {
    selector
        .split("::")
        .next()
        .is_some_and(|token| token.contains('/'))
}

fn compare_innermost(left: &&GraphSymbolArtifact, right: &&GraphSymbolArtifact) -> Ordering {
    left.line_range[0]
        .cmp(&right.line_range[0])
        .then_with(|| right.line_range[1].cmp(&left.line_range[1]))
        .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
}

fn resolution_from_symbols(symbols: Vec<&GraphSymbolArtifact>) -> SelectorResolution {
    let mcp_tool_matches = symbols
        .iter()
        .copied()
        .filter(|symbol| symbol.symbol_kind == "mcp_tool")
        .collect::<Vec<_>>();
    if let [symbol] = mcp_tool_matches.as_slice() {
        return SelectorResolution::Resolved(resolved_symbol(symbol));
    }
    match symbols.as_slice() {
        [] => SelectorResolution::NotFound,
        [symbol] => SelectorResolution::Resolved(resolved_symbol(symbol)),
        _ => SelectorResolution::Ambiguous {
            candidates: candidate_rows(symbols),
        },
    }
}

fn resolved_symbol(symbol: &GraphSymbolArtifact) -> ResolvedSymbol {
    ResolvedSymbol {
        stable_symbol_id: symbol.stable_symbol_id.clone(),
    }
}

fn candidate_rows(symbols: Vec<&GraphSymbolArtifact>) -> Vec<CandidateRow> {
    let mut candidates = symbols.into_iter().map(candidate_row).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
            .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
}

fn candidate_row(symbol: &GraphSymbolArtifact) -> CandidateRow {
    let uri = format!("{CODE_SYMBOL_URI_PREFIX}{}", symbol.stable_symbol_id);
    let selector = if symbol.qualified_name.is_empty() {
        uri.clone()
    } else {
        format!("{}::{}", symbol.file_path, symbol.qualified_name)
    };
    CandidateRow {
        selector,
        uri,
        id: symbol.stable_symbol_id.clone(),
        entity_name: symbol.entity_name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
    }
}

fn enclosing_scope_entity_name(symbol: &GraphSymbolArtifact) -> Option<String> {
    symbol
        .enclosing_scope
        .as_ref()
        .map(|scope| format!("{scope}::{}", symbol.entity_name))
}
