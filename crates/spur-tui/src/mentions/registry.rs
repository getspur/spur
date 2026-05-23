use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher,
};
use spur_acp::SessionId;

use super::code_graph::source::CodeGraphMentionSource;
use super::entry::{MentionEntry, MentionKind, MentionSource};
use super::file_source::FileMentionSource;
use super::issue_source::{IssueMentionDescriptor, IssueMentionSource};
use super::worker_source::{WorkerMentionDescriptor, WorkerMentionSource};
use spur_graph::CodeMentionPayload;

const GLOBAL_SOURCE_CACHE_TTL: Duration = Duration::from_secs(600);
const SESSION_SOURCE_CACHE_TTL: Duration = Duration::from_secs(600);
pub const CODE_GRAPH_INDEX_ENV: &str = "SPUR_CODE_GRAPH_INDEX";
pub const CODE_GRAPH_MISSING_HINT: &str = "Run 'spur graph build' to enable code-graph mentions";
const CODE_GRAPH_POINTER_SCHEMA: &str = "spur-graph-pointer-v1";
const CODE_GRAPH_POINTER_PATH: &str = ".spur/graph-index.pointer.json";
const CODE_GRAPH_LEGACY_INDEX_PATH: &str = ".spur/graph-index.json";

/// Maximum number of worker rows pinned to the top of the empty-query
/// picker view. See design spec §4.4 / §10.1.
pub(super) const WORKER_PIN_CAP: usize = 4;
const FILE_CAP: usize = 6;
const ISSUE_CAP: usize = 3;
const CODE_CAP: usize = 3;

struct CachedSourceIndex {
    entries: Arc<Vec<MentionEntry>>,
    code_payloads: HashMap<String, Arc<CodeMentionPayload>>,
    built_at: Instant,
}

/// Scope for completion cache lookup. Dashboard pre-session composition
/// has no real ACP session id yet, while session-detail composition does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionScope<'a> {
    PreSession,
    Session(&'a SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CompletionScopeKey {
    PreSession,
    Session(SessionId),
}

impl From<CompletionScope<'_>> for CompletionScopeKey {
    fn from(scope: CompletionScope<'_>) -> Self {
        match scope {
            CompletionScope::PreSession => CompletionScopeKey::PreSession,
            CompletionScope::Session(session) => CompletionScopeKey::Session(session.clone()),
        }
    }
}

pub struct MentionRegistry {
    sources: Vec<Box<dyn MentionSource>>,
    cache: HashMap<&'static str, CachedSourceIndex>,
    code_graph_hint: Option<&'static str>,
    code_graph_token: Option<CodeGraphToken>,
    code_graph_auto_discovery: bool,
    matcher: Matcher,
    #[cfg(any(test, debug_assertions))]
    query_call_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodeGraphToken {
    Pointer {
        canonical_artifact_path: PathBuf,
        graph_content_hash: String,
        manifest_version: String,
        indexed_commit_oid: Option<String>,
    },
    Resolved {
        artifact_path: PathBuf,
        cache_key: spur_graph::ArtifactCacheKey,
        modified: Option<SystemTime>,
        len: u64,
    },
}

#[derive(Debug)]
struct CodeGraphCandidate {
    explicit_override: Option<PathBuf>,
    token: CodeGraphToken,
}

enum CodeGraphSourceUpdate {
    ClearAllCaches,
    ClearCodeGraphCache,
}

impl MentionRegistry {
    /// Source list for direct (single-agent) sessions. Files only.
    pub fn for_direct_session() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        }
    }

    /// Source list for brain sessions. Files + workers.
    /// `workers` is the snapshot derived from the agent registry.
    pub fn for_brain_session(workers: Vec<super::WorkerMentionDescriptor>) -> Self {
        Self {
            sources: vec![
                Box::new(FileMentionSource),
                Box::new(WorkerMentionSource::new(workers)),
            ],
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        }
    }

    pub fn with_code_graph(mut self, artifact_path: impl Into<PathBuf>) -> Self {
        self.code_graph_auto_discovery = false;
        let artifact_path = artifact_path.into();
        self.set_code_graph_source(
            None,
            Some(artifact_path),
            None,
            CodeGraphSourceUpdate::ClearAllCaches,
        );
        self.code_graph_hint = None;
        self
    }

    /// Opt-in runtime code-graph source registration.
    ///
    /// Resolution order:
    /// 1. `SPUR_CODE_GRAPH_INDEX=<path>` env var, if set and non-empty.
    /// 2. `<worktree_root>/.spur/graph/CURRENT`.
    /// 3. `<worktree_root>/.spur/graph-index.pointer.json`.
    /// 4. `<worktree_root>/.spur/graph-index.json` legacy fallback.
    pub fn with_code_graph_from_env(mut self) -> Self {
        self.code_graph_auto_discovery = true;
        let worktree_root = spur_graph::resolve_worktree_root();
        self.refresh_code_graph_registration(&worktree_root);
        self
    }

    fn refresh_code_graph_registration(&mut self, cwd: &Path) {
        if !self.code_graph_auto_discovery {
            return;
        }
        let worktree_root = spur_graph::resolve_worktree_root_from(cwd.to_path_buf());
        match discover_code_graph_candidate(&worktree_root) {
            Some(candidate) => {
                if self.code_graph_token.as_ref() != Some(&candidate.token)
                    || !self.has_code_graph_source()
                {
                    self.set_code_graph_source(
                        Some(worktree_root.clone()),
                        candidate.explicit_override,
                        Some(candidate.token),
                        CodeGraphSourceUpdate::ClearCodeGraphCache,
                    );
                }
                self.code_graph_hint = None;
            }
            None => {
                self.remove_code_graph_source();
                self.code_graph_token = None;
                self.code_graph_hint = Some(CODE_GRAPH_MISSING_HINT);
            }
        }
    }

    fn set_code_graph_source(
        &mut self,
        worktree_root: Option<PathBuf>,
        explicit_override: Option<PathBuf>,
        token: Option<CodeGraphToken>,
        cache_update: CodeGraphSourceUpdate,
    ) {
        let source: Box<dyn MentionSource> = match worktree_root {
            Some(worktree_root) => Box::new(CodeGraphMentionSource::for_worktree(
                worktree_root,
                explicit_override,
            )),
            None => Box::new(CodeGraphMentionSource::new(
                explicit_override.expect("manual code graph source requires an artifact path"),
            )),
        };
        if let Some(index) = self
            .sources
            .iter()
            .position(|source| source.name() == "code_graph")
        {
            self.sources[index] = source;
        } else {
            let insert_at = self
                .sources
                .iter()
                .position(|source| source.name() != "file")
                .unwrap_or(self.sources.len());
            self.sources.insert(insert_at, source);
        }
        self.code_graph_token = token;
        self.code_graph_hint = None;
        match cache_update {
            CodeGraphSourceUpdate::ClearAllCaches => self.clear_cache(),
            CodeGraphSourceUpdate::ClearCodeGraphCache => self.clear_cache_for("code_graph"),
        }
    }

    fn has_code_graph_source(&self) -> bool {
        self.sources
            .iter()
            .any(|source| source.name() == "code_graph")
    }

    fn remove_code_graph_source(&mut self) {
        let previous_len = self.sources.len();
        self.sources.retain(|source| source.name() != "code_graph");
        if self.sources.len() != previous_len {
            self.clear_cache_for("code_graph");
        }
    }

    /// Back-compat alias used by tests and any caller that doesn't
    /// know the session role. Equivalent to `for_direct_session()`.
    pub fn new() -> Self {
        Self::for_direct_session()
    }

    /// Drop all cached per-session indexes. Call after the agent
    /// registry reloads so the next `query()` rebuilds with the
    /// fresh worker snapshot.
    ///
    /// Currently has no caller — wired up when live config-reload
    /// support is added (out of scope for v1).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    fn clear_cache_for(&mut self, name: &'static str) {
        self.cache.remove(name);
    }

    pub fn lookup_code_payload(&self, uri: &str) -> Option<&CodeMentionPayload> {
        self.cache
            .values()
            .find_map(|cached| cached.code_payloads.get(uri))
            .map(Arc::as_ref)
    }

    pub fn code_graph_hint(&self) -> Option<&'static str> {
        self.code_graph_hint
    }

    pub fn retain_code_payloads_for_uris<'a>(&mut self, uris: impl IntoIterator<Item = &'a str>) {
        let keep: std::collections::HashSet<&str> = uris.into_iter().collect();
        for cached in self.cache.values_mut() {
            cached
                .code_payloads
                .retain(|uri, _| !is_graph_uri(uri) || keep.contains(uri.as_str()));
        }
    }

    pub fn set_issue_snapshot(&mut self, issues: Vec<IssueMentionDescriptor>) {
        // Ordering invariant: callers must pass issues newest-first.
        // Empty-query `@` preserves that order within the ISSUE_CAP slice.
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.name() == "issue")
        {
            *source = Box::new(IssueMentionSource::new(issues));
        } else {
            self.sources.push(Box::new(IssueMentionSource::new(issues)));
        }
        self.clear_cache_for("issue");
    }

    pub fn set_worker_snapshot_in_place(&mut self, workers: Vec<WorkerMentionDescriptor>) {
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.name() == "worker")
        {
            *source = Box::new(WorkerMentionSource::new(workers));
        } else {
            self.sources
                .push(Box::new(WorkerMentionSource::new(workers)));
        }
        self.clear_cache_for("worker");
    }

    pub fn query(
        &mut self,
        scope: CompletionScope<'_>,
        cwd: &Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        #[cfg(any(test, debug_assertions))]
        {
            self.query_call_count += 1;
        }
        let _scope_key = CompletionScopeKey::from(scope);
        let _span =
            tracing::debug_span!("mention_registry_query", query_len = query.len()).entered();
        self.refresh_code_graph_registration(cwd);
        let resolver_token_unchanged = self.code_graph_auto_discovery
            && matches!(
                self.code_graph_token,
                Some(CodeGraphToken::Pointer { .. })
                    | Some(CodeGraphToken::Resolved {
                        cache_key: spur_graph::ArtifactCacheKey::Parquet { .. },
                        ..
                    })
            );
        for source in &mut self.sources {
            let source_name = source.name();
            let needs_rebuild = match self.cache.get(source_name) {
                Some(_) if source_name == "code_graph" && resolver_token_unchanged => false,
                Some(cached) => cached.built_at.elapsed() > source_cache_ttl(source_name),
                None => true,
            };
            if needs_rebuild {
                tracing::debug!(source = source_name, "rebuilding mention source cache");
                if let Ok(entries) = source.build(cwd) {
                    let mut source_code_payloads = HashMap::new();
                    for (uri, payload) in source.code_payloads() {
                        source_code_payloads.insert(uri.clone(), Arc::clone(payload));
                    }
                    self.cache.insert(
                        source_name,
                        CachedSourceIndex {
                            entries: Arc::new(entries),
                            code_payloads: source_code_payloads,
                            built_at: Instant::now(),
                        },
                    );
                }
            }
        }
        let total_len: usize = self.cache.values().map(|cached| cached.entries.len()).sum();
        let mut all_entries: Vec<&MentionEntry> = Vec::with_capacity(total_len);
        for source in &self.sources {
            if let Some(cached) = self.cache.get(source.name()) {
                all_entries.extend(cached.entries.iter());
            }
        }
        dedup_file_entries_with_code_files(&mut all_entries);
        let entries = all_entries.as_slice();

        if query.is_empty() {
            let mut workers: Vec<&MentionEntry> = entries
                .iter()
                .filter(|e| e.kind == MentionKind::Worker)
                .copied()
                .collect();
            workers.sort_by(|a, b| {
                a.display
                    .len()
                    .cmp(&b.display.len())
                    .then(a.display.cmp(&b.display))
            });
            workers.truncate(WORKER_PIN_CAP);
            let workers: Vec<MentionEntry> = workers.into_iter().cloned().collect();

            let mut files: Vec<&MentionEntry> = entries
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::File | MentionKind::Directory))
                .copied()
                .collect();
            files.sort_by(|a, b| {
                path_depth(&a.display)
                    .cmp(&path_depth(&b.display))
                    .then(a.display.len().cmp(&b.display.len()))
                    .then(a.display.cmp(&b.display))
                    .then(a.uri.cmp(&b.uri))
            });
            files.truncate(FILE_CAP);
            let files: Vec<MentionEntry> = files.into_iter().cloned().collect();

            let mut issues: Vec<(usize, &MentionEntry)> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.kind == MentionKind::Issue)
                .map(|(index, e)| (index, *e))
                .collect();
            issues.sort_by(|(idx_a, a), (idx_b, b)| {
                idx_a
                    .cmp(idx_b)
                    .then(issue_id(a).cmp(issue_id(b)))
                    .then(a.uri.cmp(&b.uri))
            });
            issues.truncate(ISSUE_CAP);
            let issues: Vec<MentionEntry> =
                issues.into_iter().map(|(_, entry)| entry.clone()).collect();

            let mut code_graph: Vec<&MentionEntry> = entries
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
                .copied()
                .collect();
            code_graph.sort_by(|a, b| {
                empty_code_kind_rank(&a.kind)
                    .cmp(&empty_code_kind_rank(&b.kind))
                    .then(path_depth(&a.display).cmp(&path_depth(&b.display)))
                    .then(a.display.len().cmp(&b.display.len()))
                    .then(a.display.cmp(&b.display))
                    .then(a.uri.cmp(&b.uri))
            });
            code_graph.truncate(CODE_CAP);
            let code_graph: Vec<MentionEntry> = code_graph.into_iter().cloned().collect();

            let mut rows = Vec::new();
            append_section_rows(&mut rows, "Workers", &workers);
            append_section_rows(&mut rows, "Files", &files);
            append_section_rows(&mut rows, "Issues", &issues);
            append_section_rows(&mut rows, "Code", &code_graph);
            rows.truncate(limit);
            return rows;
        }

        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let code_pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<RankedMentionRef<'_>> = entries
            .iter()
            .filter_map(|e| {
                buf.clear();
                let rank = if matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol) {
                    code_match_rank(e, query, &code_pattern, &mut self.matcher, &mut buf)?
                } else {
                    let haystack = e.search_text.as_deref().unwrap_or(&e.display);
                    pattern.score(
                        nucleo_matcher::Utf32Str::new(haystack, &mut buf),
                        &mut self.matcher,
                    )?
                };
                Some(RankedMentionRef { rank, entry: e })
            })
            .collect();
        let max_rank = scored.iter().map(|mention| mention.rank).max().unwrap_or(0);
        scored.sort_by(|a, b| typed_query_cmp(a, b, max_rank));
        scored
            .into_iter()
            .take(limit)
            .map(|ranked| ranked.entry.clone())
            .collect()
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn query_call_count_for_test(&self) -> usize {
        self.query_call_count
    }
}

fn is_graph_uri(uri: &str) -> bool {
    uri.starts_with("graph://file/") || uri.starts_with("graph://symbol/")
}

fn dedup_file_entries_with_code_files(entries: &mut Vec<&MentionEntry>) {
    let code_file_paths: HashSet<String> = entries
        .iter()
        .filter(|entry| entry.kind == MentionKind::CodeFile)
        .map(|entry| mention_path_key(&entry.display))
        .collect();
    if code_file_paths.is_empty() {
        return;
    }

    entries.retain(|entry| {
        entry.kind != MentionKind::File
            || !code_file_paths.contains(&mention_path_key(&entry.display))
    });
}

fn mention_path_key(path: &str) -> String {
    let mut relative = path;
    while let Some(stripped) = relative.strip_prefix("./") {
        relative = stripped;
    }
    relative.replace('\\', "/")
}

#[derive(Debug, Clone)]
struct RankedMentionRef<'a> {
    rank: u32,
    entry: &'a MentionEntry,
}

fn code_match_rank(
    entry: &MentionEntry,
    query: &str,
    pattern: &Pattern,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
) -> Option<u32> {
    let path = code_entry_path(entry);
    match entry.kind {
        MentionKind::CodeSymbol => {
            if eq_ignore_ascii_case(&entry.display, query) {
                return Some(if entry.code_scope.is_none() {
                    u32::MAX
                } else {
                    u32::MAX - 1
                });
            }

            if let Some(score) = pattern_score(pattern, matcher, buf, &entry.display)
                .or_else(|| prefix_score(&entry.display, query))
            {
                return Some(score);
            }

            let path = path?;
            pattern_score(pattern, matcher, buf, path).or_else(|| path_prefix_score(path, query))
        }
        MentionKind::CodeFile => {
            let path = path?;
            if path_segment_exact(path, query) {
                return Some(u32::MAX);
            }

            pattern_score(pattern, matcher, buf, path).or_else(|| path_prefix_score(path, query))
        }
        MentionKind::File | MentionKind::Directory | MentionKind::Worker | MentionKind::Issue => {
            None
        }
    }
}

fn pattern_score(
    pattern: &Pattern,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
    haystack: &str,
) -> Option<u32> {
    buf.clear();
    pattern.score(nucleo_matcher::Utf32Str::new(haystack, buf), matcher)
}

fn prefix_score(value: &str, query: &str) -> Option<u32> {
    value
        .to_ascii_lowercase()
        .starts_with(&query.to_ascii_lowercase())
        .then_some(query.len() as u32)
}

fn path_prefix_score(path: &str, query: &str) -> Option<u32> {
    let query = query.to_ascii_lowercase();
    path_segments_and_stems(path)
        .any(|segment| segment.to_ascii_lowercase().starts_with(&query))
        .then_some(query.len() as u32)
}

fn path_segment_exact(path: &str, query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    path_segments_and_stems(path).any(|segment| segment.to_ascii_lowercase() == query)
}

fn path_segments_and_stems(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .flat_map(|segment| {
            let stem = segment.rsplit_once('.').map(|(stem, _)| stem);
            std::iter::once(segment).chain(stem)
        })
}

fn code_entry_path(entry: &MentionEntry) -> Option<&str> {
    match entry.kind {
        MentionKind::CodeFile | MentionKind::CodeSymbol => entry.code_path.as_deref(),
        MentionKind::File | MentionKind::Directory | MentionKind::Worker | MentionKind::Issue => {
            None
        }
    }
}

fn eq_ignore_ascii_case(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn path_depth(path: &str) -> usize {
    path.trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
}

fn empty_code_kind_rank(kind: &MentionKind) -> u8 {
    match kind {
        MentionKind::CodeFile => 0,
        MentionKind::CodeSymbol => 1,
        MentionKind::File | MentionKind::Directory | MentionKind::Worker | MentionKind::Issue => 2,
    }
}

fn stable_tie_key(entry: &MentionEntry) -> &str {
    if matches!(entry.kind, MentionKind::CodeFile | MentionKind::CodeSymbol) {
        entry.uri.as_str()
    } else {
        entry.display.as_str()
    }
}

fn append_section_rows(
    rows: &mut Vec<MentionEntry>,
    header: &'static str,
    section: &[MentionEntry],
) {
    if section.is_empty() {
        return;
    }
    rows.push(MentionEntry {
        section_header: Some(header),
        kind: MentionKind::File,
        uri: String::new(),
        display: format!("── {header} ──"),
        secondary: None,
        code_path: None,
        code_scope: None,
        tag: None,
        search_text: None,
        atom_text: None,
        issue_preview: None,
    });
    rows.extend(section.iter().cloned());
}

fn issue_id(entry: &MentionEntry) -> &str {
    entry
        .issue_preview
        .as_ref()
        .map(|issue| issue.id.as_str())
        .unwrap_or(entry.display.as_str())
}

fn tier_rank(kind: &MentionKind) -> u8 {
    match kind {
        MentionKind::File | MentionKind::Directory => 0,
        MentionKind::Worker => 1,
        MentionKind::Issue => 2,
        MentionKind::CodeFile | MentionKind::CodeSymbol => 3,
    }
}

fn score_bucket(score: u32, global_max: u32) -> u32 {
    if global_max == 0 {
        return 0;
    }
    let bucket_width = global_max.saturating_div(10).max(1);
    score.saturating_div(bucket_width)
}

fn typed_query_cmp(
    a: &RankedMentionRef<'_>,
    b: &RankedMentionRef<'_>,
    max_rank: u32,
) -> std::cmp::Ordering {
    // Transitive comparator: bucket rank (desc), then tier rank (asc), then stable key.
    let bucket_a = score_bucket(a.rank, max_rank);
    let bucket_b = score_bucket(b.rank, max_rank);
    bucket_b
        .cmp(&bucket_a)
        .then(tier_rank(&a.entry.kind).cmp(&tier_rank(&b.entry.kind)))
        .then(b.rank.cmp(&a.rank))
        .then(stable_tie_key(a.entry).cmp(stable_tie_key(b.entry)))
}

fn source_cache_ttl(name: &'static str) -> Duration {
    match name {
        "file" | "code" | "code_graph" => GLOBAL_SOURCE_CACHE_TTL,
        _ => SESSION_SOURCE_CACHE_TTL,
    }
}

fn discover_code_graph_candidate(worktree_root: &Path) -> Option<CodeGraphCandidate> {
    debug_assert_eq!(CODE_GRAPH_LEGACY_INDEX_PATH, ".spur/graph-index.json");

    if let Some(path) = std::env::var_os(CODE_GRAPH_INDEX_ENV).filter(|v| !v.is_empty()) {
        let explicit_override = PathBuf::from(path);
        let resolved =
            spur_graph::resolve_artifact_location(worktree_root, Some(&explicit_override)).ok()?;
        let token = token_for_resolved(worktree_root, &resolved, Some(&explicit_override));
        return Some(CodeGraphCandidate {
            explicit_override: Some(explicit_override),
            token,
        });
    }

    let resolved = spur_graph::resolve_artifact_location(worktree_root, None).ok()?;
    let token = token_for_resolved(worktree_root, &resolved, None);
    Some(CodeGraphCandidate {
        explicit_override: None,
        token,
    })
}

fn token_for_resolved(
    worktree_root: &Path,
    resolved: &spur_graph::ResolvedArtifact,
    explicit_override: Option<&Path>,
) -> CodeGraphToken {
    if explicit_override
        .and_then(|path| canonical_artifact_path(worktree_root, path))
        .as_ref()
        == Some(&resolved.path)
    {
        return resolved_token(resolved);
    }

    if !current_pointer_selected(worktree_root, resolved) {
        if let Some(pointer_token) = pointer_token(worktree_root, resolved) {
            return pointer_token;
        }
    }

    resolved_token(resolved)
}

fn resolved_token(resolved: &spur_graph::ResolvedArtifact) -> CodeGraphToken {
    let metadata = fs::metadata(&resolved.path).ok();
    CodeGraphToken::Resolved {
        artifact_path: resolved.path.clone(),
        cache_key: resolved.cache_key.clone(),
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        len: metadata.map(|metadata| metadata.len()).unwrap_or(0),
    }
}

fn current_pointer_selected(worktree_root: &Path, resolved: &spur_graph::ResolvedArtifact) -> bool {
    resolved.format == spur_graph::ArtifactFormat::Parquet
        && spur_graph::read_current_pointer(worktree_root)
            .ok()
            .as_ref()
            == Some(&resolved.path)
}

fn pointer_token(
    worktree_root: &Path,
    resolved: &spur_graph::ResolvedArtifact,
) -> Option<CodeGraphToken> {
    let pointer_path = worktree_root.join(CODE_GRAPH_POINTER_PATH);
    let bytes = fs::read(&pointer_path).ok()?;
    let pointer: spur_graph::GraphIndexPointer = serde_json::from_slice(&bytes).ok()?;
    if pointer.schema != CODE_GRAPH_POINTER_SCHEMA {
        return None;
    }
    let canonical_artifact_path =
        canonical_artifact_path(worktree_root, &pointer.canonical_artifact_path)?;
    if canonical_artifact_path != resolved.path {
        return None;
    }
    Some(CodeGraphToken::Pointer {
        canonical_artifact_path,
        graph_content_hash: pointer.graph_content_hash,
        manifest_version: pointer.manifest_version,
        indexed_commit_oid: pointer.indexed_commit_oid,
    })
}

fn canonical_artifact_path(worktree_root: &Path, path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree_root.join(path)
    };
    path.canonicalize().ok()
}

impl Default for MentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use super::*;
    use crate::mentions::{
        IssueMentionDescriptor, IssueMentionSource, MentionKind, MentionSource,
        WorkerMentionDescriptor, WorkerMentionSource,
    };
    use filetime::{set_file_mtime, FileTime};
    use spur_graph::{
        write_artifact_parquet, write_current_pointer, GraphFileArtifact, GraphFileManifestEntry,
        GraphIndexArtifact, GraphIndexHeader, GraphIndexPointer, NodeId, SourceKind, WriteOptions,
    };
    use spur_pm::PmSource;

    static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn issue(id: &str, title: &str, assignee: Option<&str>) -> IssueMentionDescriptor {
        IssueMentionDescriptor {
            id: id.to_string(),
            title: title.to_string(),
            source: PmSource::Beads,
            status: "open".to_string(),
            assignee: assignee.map(str::to_string),
            priority: None,
            issue_type: Some("task".to_string()),
            labels: vec!["mentions".to_string()],
            url: format!("https://example.test/{id}"),
            description: None,
        }
    }

    #[test]
    fn query_matches_issue_search_text_not_just_display() {
        let mut registry = MentionRegistry {
            sources: vec![Box::new(IssueMentionSource::new(vec![issue(
                "bd-1",
                "Picker rows",
                Some("alice"),
            )]))],
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        };

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "alice", 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "issue://beads/bd-1");
    }

    #[test]
    fn code_graph_pointer_appearing_after_startup_lazy_registers_source() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let _restore = ProcessEnvRestore::enter(root);

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        assert_eq!(registry.code_graph_hint(), Some(CODE_GRAPH_MISSING_HINT));

        let artifact_path = root.join(".spur/canonical/graph-a.json");
        write_graph_fixture(&artifact_path, "lazy-file", "src/lazy.rs", "hash-a");
        write_pointer(
            root,
            &artifact_path,
            "hash-a",
            "manifest-a",
            Some("commit-a"),
        );

        let hits = registry.query(CompletionScope::PreSession, root, "lazy", 10);

        assert!(
            hits.iter()
                .any(|hit| hit.kind == MentionKind::CodeFile && hit.display == "src/lazy.rs"),
            "expected lazy-registered code graph row, got {hits:?}"
        );
        assert_eq!(registry.code_graph_hint(), None);
    }

    #[test]
    fn code_graph_pointer_path_change_reloads_immediately() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let _restore = ProcessEnvRestore::enter(root);

        let artifact_a = root.join(".spur/canonical/graph-a.json");
        let artifact_b = root.join(".spur/canonical/graph-b.json");
        write_graph_fixture(&artifact_a, "first-file", "src/first.rs", "hash-a");
        write_graph_fixture(&artifact_b, "second-file", "src/second.rs", "hash-b");
        write_pointer(root, &artifact_a, "hash-a", "manifest-a", Some("commit-a"));

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        let first = registry.query(CompletionScope::PreSession, root, "first", 10);
        assert!(first.iter().any(|hit| hit.display == "src/first.rs"));

        write_pointer(root, &artifact_b, "hash-b", "manifest-b", Some("commit-b"));
        let second = registry.query(CompletionScope::PreSession, root, "second", 10);

        assert!(
            second.iter().any(|hit| hit.display == "src/second.rs"),
            "expected pointer path change to reload graph rows, got {second:?}"
        );
    }

    #[test]
    fn code_graph_pointer_hash_change_reloads_even_when_artifact_metadata_is_stable() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let _restore = ProcessEnvRestore::enter(root);

        let artifact_path = root.join(".spur/canonical/graph.json");
        let stable_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        write_graph_fixture(&artifact_path, "old-file", "src/old.rs", "hash-a");
        set_file_mtime(&artifact_path, stable_mtime).unwrap();
        let original_len = fs::metadata(&artifact_path).unwrap().len();
        write_pointer(
            root,
            &artifact_path,
            "hash-a",
            "manifest-a",
            Some("commit-a"),
        );

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        let old = registry.query(CompletionScope::PreSession, root, "old", 10);
        assert!(old.iter().any(|hit| hit.display == "src/old.rs"));

        write_graph_fixture(&artifact_path, "new-file", "src/new.rs", "hash-b");
        assert_eq!(
            fs::metadata(&artifact_path).unwrap().len(),
            original_len,
            "fixture rewrite must keep length stable to prove pointer-token invalidation"
        );
        set_file_mtime(&artifact_path, stable_mtime).unwrap();
        write_pointer(
            root,
            &artifact_path,
            "hash-b",
            "manifest-b",
            Some("commit-b"),
        );

        let new = registry.query(CompletionScope::PreSession, root, "new", 10);

        assert!(
            new.iter().any(|hit| hit.display == "src/new.rs"),
            "expected pointer hash change to force source reload, got {new:?}"
        );
    }

    #[test]
    fn code_graph_env_override_still_wins_over_pointer() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let env_artifact = root.join("env-graph.json");
        let pointer_artifact = root.join(".spur/canonical/pointer-graph.json");
        write_graph_fixture(&env_artifact, "env-file", "src/env.rs", "env-hash");
        write_graph_fixture(
            &pointer_artifact,
            "pointer-file",
            "src/pointer.rs",
            "pointer-hash",
        );
        write_pointer(
            root,
            &pointer_artifact,
            "pointer-hash",
            "manifest-pointer",
            Some("commit-pointer"),
        );
        let _restore = ProcessEnvRestore::enter(root).with_env_override(&env_artifact);

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        let hits = registry.query(CompletionScope::PreSession, root, "env", 10);

        assert!(hits.iter().any(|hit| hit.display == "src/env.rs"));
        assert!(
            !hits.iter().any(|hit| hit.display == "src/pointer.rs"),
            "env override should ignore pointer artifact rows, got {hits:?}"
        );
    }

    #[test]
    fn code_graph_without_pointer_falls_back_to_legacy_worktree_artifact() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let _restore = ProcessEnvRestore::enter(root);

        let legacy_artifact = root.join(".spur/graph-index.json");
        write_graph_fixture(
            &legacy_artifact,
            "legacy-file",
            "src/legacy.rs",
            "legacy-hash",
        );

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        let hits = registry.query(CompletionScope::PreSession, root, "legacy", 10);

        assert!(
            hits.iter()
                .any(|hit| hit.kind == MentionKind::CodeFile && hit.display == "src/legacy.rs"),
            "expected legacy worktree artifact fallback, got {hits:?}"
        );
        assert_eq!(registry.code_graph_hint(), None);
    }

    #[test]
    fn code_graph_auto_discovery_prefers_current_parquet_over_legacy_json() {
        let _guard = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".git")).unwrap();
        let _restore = ProcessEnvRestore::enter(root);

        let legacy_artifact = root.join(".spur/graph-index.json");
        write_graph_fixture(
            &legacy_artifact,
            "legacy-file",
            "src/legacy.rs",
            "legacy-hash",
        );

        let parquet_dir = write_artifact_parquet(
            &graph_artifact("parquet-file", "src/parquet.rs", "parquet-hash"),
            &root.join(".git/spur-graph/artifacts/test"),
            WriteOptions::default(),
        )
        .expect("write parquet artifact");
        write_current_pointer(root, &parquet_dir).expect("write CURRENT pointer");

        let mut registry = MentionRegistry::for_direct_session().with_code_graph_from_env();
        let hits = registry.query(CompletionScope::PreSession, root, "parquet", 10);

        assert!(
            hits.iter()
                .any(|hit| hit.kind == MentionKind::CodeFile && hit.display == "src/parquet.rs"),
            "expected resolver to prefer CURRENT parquet artifact, got {hits:?}"
        );
        assert!(
            !hits.iter().any(|hit| hit.display == "src/legacy.rs"),
            "legacy fallback should not win when CURRENT parquet is valid, got {hits:?}"
        );
        assert_eq!(registry.code_graph_hint(), None);
    }

    #[test]
    fn set_issue_snapshot_clears_cache_for_next_query() {
        let mut registry = MentionRegistry {
            sources: vec![Box::new(IssueMentionSource::new(vec![issue(
                "bd-1",
                "Old title",
                None,
            )]))],
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        };

        let first = registry.query(CompletionScope::PreSession, Path::new("."), "old", 10);
        assert_eq!(first[0].display, "bd-1 Old title");

        registry.set_issue_snapshot(vec![issue("bd-2", "New title", None)]);
        let old = registry.query(CompletionScope::PreSession, Path::new("."), "old", 10);
        let new = registry.query(CompletionScope::PreSession, Path::new("."), "new", 10);

        assert!(old.is_empty());
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].display, "bd-2 New title");
    }

    #[test]
    fn set_issue_snapshot_does_not_invalidate_file_cache() {
        let file_build_count = Arc::new(AtomicUsize::new(0));
        let mut registry = test_registry(vec![
            Box::new(CountingSource {
                name: "file",
                entries: vec![mention(MentionKind::File, 1, "src/main.rs".into())],
                build_count: Arc::clone(&file_build_count),
            }),
            Box::new(IssueMentionSource::new(vec![issue(
                "bd-1",
                "Old title",
                None,
            )])),
        ]);

        let _ = registry.query(CompletionScope::PreSession, Path::new("."), "", 16);
        assert_eq!(file_build_count.load(Ordering::Relaxed), 1);

        registry.set_issue_snapshot(vec![issue("bd-2", "New title", None)]);
        let _ = registry.query(CompletionScope::PreSession, Path::new("."), "", 16);

        assert_eq!(file_build_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn presession_to_session_transition_reuses_global_caches() {
        let file_build_count = Arc::new(AtomicUsize::new(0));
        let code_build_count = Arc::new(AtomicUsize::new(0));
        let session_id = SessionId("session-1".to_string());
        let mut registry = test_registry(vec![
            Box::new(CountingSource {
                name: "file",
                entries: vec![mention(MentionKind::File, 1, "src/lib.rs".into())],
                build_count: Arc::clone(&file_build_count),
            }),
            Box::new(CountingSource {
                name: "code",
                entries: vec![mention(MentionKind::CodeFile, 2, "src/code.rs".into())],
                build_count: Arc::clone(&code_build_count),
            }),
            Box::new(WorkerMentionSource::new(vec![WorkerMentionDescriptor {
                name: "alpha".into(),
                description: None,
                tier: None,
            }])),
        ]);

        let _ = registry.query(CompletionScope::PreSession, Path::new("."), "", 16);
        let _ = registry.query(
            CompletionScope::Session(&session_id),
            Path::new("."),
            "",
            16,
        );

        assert_eq!(file_build_count.load(Ordering::Relaxed), 1);
        assert_eq!(code_build_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn query_uses_smart_case_matching() {
        let mut registry = MentionRegistry {
            sources: vec![Box::new(IssueMentionSource::new(vec![
                issue("bd-1", "deploy prod", None),
                issue("bd-2", "Deploy Prod", None),
            ]))],
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        };

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "Deploy", 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "bd-2 Deploy Prod");
    }

    struct StaticSource {
        name: &'static str,
        entries: Vec<MentionEntry>,
    }

    impl MentionSource for StaticSource {
        fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
            Ok(self.entries.clone())
        }

        fn name(&self) -> &'static str {
            self.name
        }
    }

    struct CountingSource {
        name: &'static str,
        entries: Vec<MentionEntry>,
        build_count: Arc<AtomicUsize>,
    }

    impl MentionSource for CountingSource {
        fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
            self.build_count.fetch_add(1, Ordering::Relaxed);
            Ok(self.entries.clone())
        }

        fn name(&self) -> &'static str {
            self.name
        }
    }

    fn mention(kind: MentionKind, id: usize, display: String) -> MentionEntry {
        let code_path = (kind == MentionKind::CodeFile).then(|| display.clone());
        MentionEntry {
            section_header: None,
            kind,
            uri: format!("test://{id}"),
            display,
            secondary: None,
            code_path,
            code_scope: None,
            tag: None,
            search_text: None,
            atom_text: None,
            issue_preview: None,
        }
    }

    fn test_registry(sources: Vec<Box<dyn MentionSource>>) -> MentionRegistry {
        MentionRegistry {
            sources,
            cache: HashMap::new(),
            code_graph_hint: None,
            code_graph_token: None,
            code_graph_auto_discovery: false,
            matcher: Matcher::new(Config::DEFAULT),
            #[cfg(any(test, debug_assertions))]
            query_call_count: 0,
        }
    }

    struct ProcessEnvRestore {
        previous_cwd: PathBuf,
        previous_env: Option<std::ffi::OsString>,
    }

    impl ProcessEnvRestore {
        fn enter(cwd: &Path) -> Self {
            let restore = Self {
                previous_cwd: std::env::current_dir().unwrap(),
                previous_env: std::env::var_os(CODE_GRAPH_INDEX_ENV),
            };
            std::env::remove_var(CODE_GRAPH_INDEX_ENV);
            std::env::set_current_dir(cwd).unwrap();
            restore
        }

        fn with_env_override(self, path: &Path) -> Self {
            std::env::set_var(CODE_GRAPH_INDEX_ENV, path);
            self
        }
    }

    impl Drop for ProcessEnvRestore {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous_cwd).unwrap();
            match &self.previous_env {
                Some(value) => std::env::set_var(CODE_GRAPH_INDEX_ENV, value),
                None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
            }
        }
    }

    fn write_pointer(
        root: &Path,
        canonical_artifact_path: &Path,
        graph_content_hash: &str,
        manifest_version: &str,
        indexed_commit_oid: Option<&str>,
    ) {
        let pointer = GraphIndexPointer {
            schema: "spur-graph-pointer-v1".to_string(),
            graph_content_hash: graph_content_hash.to_string(),
            manifest_version: manifest_version.to_string(),
            source_kind: SourceKind::Git,
            indexed_commit_oid: indexed_commit_oid.map(str::to_string),
            canonical_artifact_path: canonical_artifact_path.to_path_buf(),
        };
        let path = root.join(".spur/graph-index.pointer.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_string_pretty(&pointer).unwrap()).unwrap();
    }

    fn write_graph_fixture(path: &Path, stable_file_id: &str, file_path: &str, content_hash: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let artifact = serde_json::json!({
            "header": {
                "graph_index_version": "registry-test",
                "content_hash_blake3": content_hash
            },
            "manifest_version": "registry-test-manifest",
            "graph_content_hash": content_hash,
            "files": [
                {
                    "stable_file_id": stable_file_id,
                    "file_path": file_path
                }
            ],
            "symbols": []
        });
        fs::write(path, serde_json::to_string_pretty(&artifact).unwrap()).unwrap();
    }

    fn graph_artifact(
        stable_file_id: &str,
        file_path: &str,
        content_hash: &str,
    ) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "registry-test".to_string(),
                content_hash_blake3: Some(content_hash.to_string()),
            },
            manifest_version: "registry-test-manifest".to_string(),
            graph_content_hash: content_hash.to_string(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: stable_file_id.to_string(),
                path: file_path.to_string(),
                content_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                node_ids: Vec::new(),
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: stable_file_id.to_string(),
                file_path: file_path.to_string(),
            }],
            file_node_ids: vec![NodeId(1)],
            symbols: Vec::new(),
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),

            commits: Vec::new(),

            symbol_snapshots: Vec::new(),

            temporal_edges: Vec::new(),
        }
    }

    #[test]
    fn empty_query_caps_each_kind() {
        // Limitation: this test validates section caps on large candidate sets,
        // but does not instrument exact clone counts.
        let workers = (0..100)
            .map(|i| WorkerMentionDescriptor {
                name: format!("worker-{i}"),
                description: None,
                tier: None,
            })
            .collect::<Vec<_>>();
        let files = (0..120)
            .map(|i| mention(MentionKind::File, i, format!("src/path-{i}.rs")))
            .collect::<Vec<_>>();
        let issues = (0..80)
            .map(|i| issue(&format!("bd-{i}"), &format!("Issue {i}"), None))
            .collect::<Vec<_>>();
        let code_files = (0..60)
            .map(|i| mention(MentionKind::CodeFile, 100 + i, format!("src/code-{i}.rs")))
            .collect::<Vec<_>>();
        let code_symbols = (0..60)
            .map(|i| mention(MentionKind::CodeSymbol, 200 + i, format!("symbol_{i}")))
            .collect::<Vec<_>>();

        let mut registry = test_registry(vec![
            Box::new(WorkerMentionSource::new(workers)),
            Box::new(StaticSource {
                name: "file",
                entries: files,
            }),
            Box::new(IssueMentionSource::new(issues)),
            Box::new(StaticSource {
                name: "code",
                entries: code_files.into_iter().chain(code_symbols).collect(),
            }),
        ]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "", 128);
        let content: Vec<&MentionEntry> = results
            .iter()
            .filter(|entry| entry.section_header.is_none())
            .collect();

        assert_eq!(content.len(), 16);
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::Worker)
                .count(),
            WORKER_PIN_CAP
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::File)
                .count(),
            FILE_CAP
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::Issue)
                .count(),
            ISSUE_CAP
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
                .count(),
            CODE_CAP
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::Worker)
                .count(),
            4
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::File)
                .count(),
            6
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| e.kind == MentionKind::Issue)
                .count(),
            3
        );
        assert_eq!(
            content
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
                .count(),
            3
        );
        assert!(content[..4].iter().all(|e| e.kind == MentionKind::Worker));
        assert!(content[4..10].iter().all(|e| e.kind == MentionKind::File));
        assert!(content[10..13].iter().all(|e| e.kind == MentionKind::Issue));
        assert!(content[13..16]
            .iter()
            .all(|e| matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol)));
    }

    #[test]
    fn empty_query_emits_section_headers_in_order() {
        let mut registry = test_registry(vec![
            Box::new(WorkerMentionSource::new(vec![WorkerMentionDescriptor {
                name: "alpha".into(),
                description: None,
                tier: None,
            }])),
            Box::new(StaticSource {
                name: "file",
                entries: vec![mention(MentionKind::File, 1, "src/main.rs".into())],
            }),
            Box::new(IssueMentionSource::new(vec![issue("bd-1", "Issue", None)])),
            Box::new(StaticSource {
                name: "code",
                entries: vec![mention(MentionKind::CodeFile, 2, "src/lib.rs".into())],
            }),
        ]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "", 128);
        let headers = results
            .iter()
            .filter_map(|entry| entry.section_header)
            .collect::<Vec<_>>();
        assert_eq!(headers, vec!["Workers", "Files", "Issues", "Code"]);

        let mut workers_only = test_registry(vec![Box::new(WorkerMentionSource::new(vec![
            WorkerMentionDescriptor {
                name: "alpha".into(),
                description: None,
                tier: None,
            },
        ]))]);
        let workers_only_results =
            workers_only.query(CompletionScope::PreSession, Path::new("."), "", 128);
        let workers_only_headers = workers_only_results
            .iter()
            .filter_map(|entry| entry.section_header)
            .collect::<Vec<_>>();
        assert_eq!(workers_only_headers, vec!["Workers"]);
    }

    #[test]
    fn typed_query_prefers_code_file_when_file_has_same_path() {
        let mut registry = test_registry(vec![
            Box::new(StaticSource {
                name: "file",
                entries: vec![
                    mention(MentionKind::File, 1, "src/shared.rs".into()),
                    mention(MentionKind::File, 2, "src/file_only.rs".into()),
                ],
            }),
            Box::new(StaticSource {
                name: "code",
                entries: vec![
                    mention(MentionKind::CodeFile, 3, "src/code_only.rs".into()),
                    mention(MentionKind::CodeFile, 4, "src/shared.rs".into()),
                ],
            }),
        ]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "src", 16);
        let content: Vec<&MentionEntry> = results
            .iter()
            .filter(|entry| entry.section_header.is_none())
            .collect();

        assert_eq!(
            content
                .iter()
                .filter(|entry| entry.display == "src/shared.rs")
                .count(),
            1,
            "expected same-path File/CodeFile rows to collapse into one result: {content:?}"
        );
        assert!(
            content.iter().any(
                |entry| entry.kind == MentionKind::CodeFile && entry.display == "src/shared.rs"
            ),
            "expected CodeFile to survive for duplicate path: {content:?}"
        );
        assert!(
            content
                .iter()
                .any(|entry| entry.kind == MentionKind::File
                    && entry.display == "src/file_only.rs"),
            "expected File-only path to survive: {content:?}"
        );
        assert!(
            content
                .iter()
                .any(|entry| entry.kind == MentionKind::CodeFile
                    && entry.display == "src/code_only.rs"),
            "expected CodeFile-only path to survive: {content:?}"
        );
    }

    #[test]
    fn empty_query_file_header_disappears_when_files_are_code_duplicates() {
        let mut registry = test_registry(vec![
            Box::new(StaticSource {
                name: "file",
                entries: vec![mention(MentionKind::File, 1, "src/shared.rs".into())],
            }),
            Box::new(StaticSource {
                name: "code",
                entries: vec![mention(MentionKind::CodeFile, 2, "src/shared.rs".into())],
            }),
        ]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "", 16);
        let headers = results
            .iter()
            .filter_map(|entry| entry.section_header)
            .collect::<Vec<_>>();
        let content = results
            .iter()
            .filter(|entry| entry.section_header.is_none())
            .collect::<Vec<_>>();

        assert_eq!(headers, vec!["Code"]);
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].kind, MentionKind::CodeFile);
        assert_eq!(content[0].display, "src/shared.rs");
    }

    #[test]
    fn typed_query_tier_breaks_within_window() {
        let mut registry = test_registry(vec![Box::new(StaticSource {
            name: "mixed",
            entries: vec![
                mention(MentionKind::Worker, 1, "abcd".into()),
                mention(MentionKind::File, 2, "abce".into()),
            ],
        })]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "abc", 10);
        assert_eq!(results.len(), 2);
        // For `abc` vs `abcd` / `abce`, both rows score in the same bucket;
        // tier rank is the deciding key, so File beats Worker.
        assert_eq!(results[0].kind, MentionKind::File);
    }

    #[test]
    fn typed_query_higher_bucket_beats_lower_bucket_even_if_tier_is_worse() {
        let mut registry = test_registry(vec![Box::new(StaticSource {
            name: "mixed",
            entries: vec![
                mention(MentionKind::File, 1, "needle-notes.md".into()),
                mention(MentionKind::CodeSymbol, 2, "needle".into()),
            ],
        })]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "needle", 10);
        assert_eq!(results.len(), 2);
        // Invariant under test: score bucket is the primary key. The exact
        // code-symbol hit is in a higher bucket than the weak file prefix hit.
        assert_eq!(results[0].kind, MentionKind::CodeSymbol);
    }

    #[test]
    fn code_symbol_path_ranking_uses_structured_code_path_not_secondary() {
        let mut symbol = mention(MentionKind::CodeSymbol, 1, "render_dashboard".into());
        symbol.secondary = Some("not a parseable location".into());
        symbol.code_path = Some("crates/spur-tui/src/dashboard_view.rs".into());

        let mut registry = test_registry(vec![Box::new(StaticSource {
            name: "code",
            entries: vec![symbol],
        })]);

        let results = registry.query(
            CompletionScope::PreSession,
            Path::new("."),
            "dashboard_view",
            10,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "render_dashboard");
    }

    #[test]
    fn exact_bare_code_symbol_ranks_above_exact_scoped_symbol() {
        let mut scoped = mention(MentionKind::CodeSymbol, 1, "Cache".into());
        scoped.uri = "test://a-scoped-cache".into();
        scoped.code_scope = Some("CacheStore".into());

        let mut bare = mention(MentionKind::CodeSymbol, 2, "Cache".into());
        bare.uri = "test://z-bare-cache".into();

        let mut registry = test_registry(vec![Box::new(StaticSource {
            name: "code",
            entries: vec![scoped, bare],
        })]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "cache", 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].uri, "test://z-bare-cache");
        assert_eq!(results[1].uri, "test://a-scoped-cache");
    }

    #[test]
    fn empty_query_keeps_most_recent_issues_first() {
        let mut registry = test_registry(vec![Box::new(IssueMentionSource::new(vec![
            issue("bd-5", "Issue 5", None),
            issue("bd-4", "Issue 4", None),
            issue("bd-3", "Issue 3", None),
            issue("bd-2", "Issue 2", None),
            issue("bd-1", "Issue 1", None),
        ]))]);

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "", 128);
        let issues: Vec<&MentionEntry> = results
            .iter()
            .filter(|entry| entry.kind == MentionKind::Issue)
            .collect();

        assert_eq!(issues.len(), 3);
        assert_eq!(issue_id(issues[0]), "bd-5");
        assert_eq!(issue_id(issues[1]), "bd-4");
        assert_eq!(issue_id(issues[2]), "bd-3");
    }

    #[test]
    fn comparator_is_strict_weak_ordering() {
        let entries = [
            mention(MentionKind::Worker, 1, "worker-a".into()),
            mention(MentionKind::CodeSymbol, 2, "code-b".into()),
            mention(MentionKind::File, 3, "file-c".into()),
            mention(MentionKind::Issue, 4, "issue-d".into()),
            mention(MentionKind::File, 5, "file-e".into()),
            mention(MentionKind::Worker, 6, "worker-f".into()),
        ];
        let base = vec![
            RankedMentionRef {
                rank: 95,
                entry: &entries[0],
            },
            RankedMentionRef {
                rank: 100,
                entry: &entries[1],
            },
            RankedMentionRef {
                rank: 86,
                entry: &entries[2],
            },
            RankedMentionRef {
                rank: 88,
                entry: &entries[3],
            },
            RankedMentionRef {
                rank: 45,
                entry: &entries[4],
            },
            RankedMentionRef {
                rank: 0,
                entry: &entries[5],
            },
        ];
        let max_rank = base.iter().map(|mention| mention.rank).max().unwrap_or(0);

        for a in &base {
            assert_eq!(typed_query_cmp(a, a, max_rank), std::cmp::Ordering::Equal);
        }
        for a in &base {
            for b in &base {
                let ab = typed_query_cmp(a, b, max_rank);
                let ba = typed_query_cmp(b, a, max_rank);
                assert_eq!(ab, ba.reverse());
            }
        }
        for a in &base {
            for b in &base {
                for c in &base {
                    if typed_query_cmp(a, b, max_rank) == std::cmp::Ordering::Less
                        && typed_query_cmp(b, c, max_rank) == std::cmp::Ordering::Less
                    {
                        assert_eq!(typed_query_cmp(a, c, max_rank), std::cmp::Ordering::Less);
                    }
                }
            }
        }

        let mut expected = base.clone();
        expected.sort_by(|a, b| typed_query_cmp(a, b, max_rank));
        let expected_ids: Vec<String> =
            expected.iter().map(|item| item.entry.uri.clone()).collect();

        for shift in 0..base.len() {
            let mut shuffled = base.clone();
            shuffled.rotate_left(shift);
            shuffled.sort_by(|a, b| typed_query_cmp(a, b, max_rank));
            let ids: Vec<String> = shuffled.iter().map(|item| item.entry.uri.clone()).collect();
            assert_eq!(ids, expected_ids);
        }
    }
}
