use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context as _};
use globset::Glob;

use crate::search::{compare_symbols, limited_search_result, matches_filters, matches_query};
use crate::{
    CandidateRow, CodeSelectorResolution, GraphFileArtifact, GraphFileManifestEntry,
    GraphIndexArtifact, GraphSymbolArtifact, ResolvedSymbol, SearchOptions, SearchResult,
    SearchSymbol, SelectorResolution, CODE_SYMBOL_URI_PREFIX,
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

#[derive(Debug)]
pub struct OverlayFileSegment {
    file: GraphFileArtifact,
    manifest: Option<GraphFileManifestEntry>,
    symbols: Arc<[GraphSymbolArtifact]>,
    search_symbols: Arc<[SearchSymbol]>,
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
}

#[derive(Debug)]
struct BaseGeneration {
    artifact: Arc<GraphIndexArtifact>,
    segments: BTreeMap<String, Arc<OverlayFileSegment>>,
    symbol_owners: HashMap<String, String>,
}

#[derive(Debug)]
pub struct OverlayGeneration {
    base: Arc<BaseGeneration>,
    identity: Option<OverlayGenerationIdentity>,
    path_state: Arc<BTreeMap<String, OverlayPathState>>,
    overrides: BTreeMap<String, Option<Arc<OverlayFileSegment>>>,
    delta_symbol_owners: HashMap<String, String>,
    rebuilt_paths: BTreeSet<String>,
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

        Ok(Self {
            base: Arc::new(BaseGeneration {
                artifact: base,
                segments,
                symbol_owners,
            }),
            identity: None,
            path_state: Arc::new(BTreeMap::new()),
            overrides: BTreeMap::new(),
            delta_symbol_owners: HashMap::new(),
            rebuilt_paths: BTreeSet::new(),
        })
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

        Ok(Self {
            base: Arc::clone(&previous.base),
            identity: Some(identity),
            path_state: Arc::new(path_state.clone()),
            overrides,
            delta_symbol_owners,
            rebuilt_paths,
        })
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

    pub fn file_segment(&self, path: &str) -> Option<Arc<OverlayFileSegment>> {
        if self.path_state.contains_key(path) {
            return self.overrides.get(path).and_then(Clone::clone);
        }
        self.base.segments.get(path).cloned()
    }

    pub fn search_symbols(&self, options: &SearchOptions) -> anyhow::Result<SearchResult> {
        let glob = options
            .filters
            .file_glob
            .as_deref()
            .and_then(|pattern| Glob::new(pattern).ok())
            .map(|glob| glob.compile_matcher());
        let mut candidates = self
            .visible_search_symbols()
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
        let owner = self
            .delta_symbol_owners
            .get(sid)
            .or_else(|| self.base.symbol_owners.get(sid));
        let Some(owner) = owner else {
            return Ok(None);
        };
        if !self.delta_symbol_owners.contains_key(sid) && self.path_state.contains_key(owner) {
            return Ok(None);
        }
        Ok(self.file_segment(owner).and_then(|segment| {
            segment
                .symbols()
                .iter()
                .find(|symbol| symbol.stable_symbol_id == sid)
                .cloned()
        }))
    }

    pub fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        Ok(self
            .file_segment(path)
            .map(|segment| {
                segment
                    .symbols()
                    .iter()
                    .filter(|symbol| self.symbol_is_visible(symbol))
                    .cloned()
                    .collect()
            })
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
            .file_segment(path)
            .and_then(|segment| segment.manifest().cloned()))
    }

    pub fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        Ok(self.file_segment(path).is_some())
    }

    pub fn base_artifact(&self) -> &Arc<GraphIndexArtifact> {
        &self.base.artifact
    }

    fn visible_segments(&self) -> impl Iterator<Item = &OverlayFileSegment> {
        let base = self
            .base
            .segments
            .iter()
            .filter(|(path, _)| !self.path_state.contains_key(path.as_str()))
            .map(|(_, segment)| segment.as_ref());
        let overrides = self.overrides.values().filter_map(Option::as_deref);
        base.chain(overrides)
    }

    fn visible_symbols(&self) -> impl Iterator<Item = &GraphSymbolArtifact> {
        self.visible_segments()
            .flat_map(|segment| segment.symbols().iter())
            .filter(|symbol| self.symbol_is_visible(symbol))
    }

    fn visible_search_symbols(&self) -> impl Iterator<Item = &SearchSymbol> {
        self.visible_segments()
            .flat_map(|segment| segment.search_symbols.iter())
            .filter(|symbol| self.search_symbol_is_visible(symbol))
    }

    fn symbol_is_visible(&self, symbol: &GraphSymbolArtifact) -> bool {
        self.stable_id_owner_is_path(&symbol.stable_symbol_id, &symbol.file_path)
    }

    fn search_symbol_is_visible(&self, symbol: &SearchSymbol) -> bool {
        self.stable_id_owner_is_path(&symbol.stable_symbol_id, &symbol.file_path)
    }

    fn stable_id_owner_is_path(&self, stable_symbol_id: &str, path: &str) -> bool {
        if let Some(owner) = self.delta_symbol_owners.get(stable_symbol_id) {
            return owner == path;
        }
        !self.path_state.contains_key(path)
            && self
                .base
                .symbol_owners
                .get(stable_symbol_id)
                .is_some_and(|owner| owner == path)
    }

    fn visible_file_paths(&self) -> impl Iterator<Item = &str> {
        let base = self
            .base
            .segments
            .keys()
            .filter(|path| !self.path_state.contains_key(path.as_str()))
            .map(String::as_str);
        let overrides = self
            .overrides
            .iter()
            .filter(|(_, segment)| segment.is_some())
            .map(|(path, _)| path.as_str());
        base.chain(overrides)
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
            let resolution = resolution_from_symbols(
                self.visible_symbols()
                    .filter(|symbol| symbol.qualified_name == selector)
                    .collect(),
            );
            if !matches!(resolution, SelectorResolution::NotFound) {
                return resolution;
            }
        }
        if selector.contains("::") {
            return SelectorResolution::NotFound;
        }
        resolution_from_symbols(
            self.visible_symbols()
                .filter(|symbol| symbol.entity_name == selector)
                .collect(),
        )
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

#[derive(Default)]
struct SegmentBuilder {
    file: Option<GraphFileArtifact>,
    manifest: Option<GraphFileManifestEntry>,
    symbols: Vec<GraphSymbolArtifact>,
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
