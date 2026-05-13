use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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

const CACHE_TTL: Duration = Duration::from_secs(60);
pub const CODE_GRAPH_INDEX_ENV: &str = "SPUR_CODE_GRAPH_INDEX";
pub const CODE_GRAPH_MISSING_HINT: &str = "Run 'spur graph build' to enable code-graph mentions";

/// Maximum number of worker rows pinned to the top of the empty-query
/// picker view. See design spec §4.4 / §10.1.
pub(super) const WORKER_PIN_CAP: usize = 4;
const FILE_CAP: usize = 6;
const ISSUE_CAP: usize = 3;
const CODE_CAP: usize = 3;

struct CachedIndex {
    entries: Vec<MentionEntry>,
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
    cache: HashMap<CompletionScopeKey, CachedIndex>,
    code_payloads: HashMap<String, CodeMentionPayload>,
    code_graph_hint: Option<&'static str>,
    matcher: Matcher,
}

impl MentionRegistry {
    /// Source list for direct (single-agent) sessions. Files only.
    pub fn for_direct_session() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
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
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn with_code_graph(mut self, artifact_path: impl Into<PathBuf>) -> Self {
        let source: Box<dyn MentionSource> =
            Box::new(CodeGraphMentionSource::new(artifact_path.into()));
        let insert_at = self
            .sources
            .iter()
            .position(|source| source.name() != "file")
            .unwrap_or(self.sources.len());
        self.sources.insert(insert_at, source);
        self.code_graph_hint = None;
        self.clear_cache();
        self
    }

    /// Opt-in runtime code-graph source registration.
    ///
    /// Resolution order:
    /// 1. `SPUR_CODE_GRAPH_INDEX=<path>` env var, if set and non-empty.
    /// 2. `<worktree_root>/.spur/graph-index.json` — the path `spur graph build`
    ///    writes by default. This makes the TUI work out of the box once a
    ///    user has built the index.
    pub fn with_code_graph_from_env(self) -> Self {
        if let Some(path) = std::env::var_os(CODE_GRAPH_INDEX_ENV).filter(|v| !v.is_empty()) {
            let path = PathBuf::from(path);
            return if path.is_file() {
                self.with_code_graph(path)
            } else {
                self.with_code_graph_hint()
            };
        }
        let default_path = spur_graph::resolve_worktree_root().join(".spur/graph-index.json");
        if default_path.is_file() {
            self.with_code_graph(default_path)
        } else {
            self.with_code_graph_hint()
        }
    }

    fn with_code_graph_hint(mut self) -> Self {
        self.code_graph_hint = Some(CODE_GRAPH_MISSING_HINT);
        self
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
        self.code_payloads.clear();
    }

    pub fn lookup_code_payload(&self, uri: &str) -> Option<&CodeMentionPayload> {
        self.code_payloads.get(uri)
    }

    pub fn code_graph_hint(&self) -> Option<&'static str> {
        self.code_graph_hint
    }

    pub fn retain_code_payloads_for_uris<'a>(&mut self, uris: impl IntoIterator<Item = &'a str>) {
        let keep: std::collections::HashSet<&str> = uris.into_iter().collect();
        self.code_payloads
            .retain(|uri, _| !is_graph_uri(uri) || keep.contains(uri.as_str()));
    }

    pub fn set_issue_snapshot(&mut self, issues: Vec<IssueMentionDescriptor>) {
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.name() == "issue")
        {
            *source = Box::new(IssueMentionSource::new(issues));
        } else {
            self.sources.push(Box::new(IssueMentionSource::new(issues)));
        }
        self.clear_cache();
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
        self.clear_cache();
    }

    pub fn query(
        &mut self,
        scope: CompletionScope<'_>,
        cwd: &std::path::Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        let key = CompletionScopeKey::from(scope);
        let needs_rebuild = match self.cache.get(&key) {
            Some(c) => c.built_at.elapsed() > CACHE_TTL,
            None => true,
        };
        if needs_rebuild {
            let mut all = Vec::new();
            let mut code_payloads = HashMap::new();
            for s in &mut self.sources {
                if let Ok(mut entries) = s.build(cwd) {
                    all.append(&mut entries);
                    code_payloads.extend(s.code_payloads());
                }
            }
            self.code_payloads = code_payloads;
            self.cache.insert(
                key.clone(),
                CachedIndex {
                    entries: all,
                    built_at: Instant::now(),
                },
            );
        }
        let entries = &self.cache[&key].entries;

        if query.is_empty() {
            let mut workers: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| e.kind == MentionKind::Worker)
                .cloned()
                .collect();
            workers.sort_by(|a, b| {
                a.display
                    .len()
                    .cmp(&b.display.len())
                    .then(a.display.cmp(&b.display))
            });
            workers.truncate(WORKER_PIN_CAP);

            let mut files: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::File | MentionKind::Directory))
                .cloned()
                .collect();
            files.sort_by(|a, b| {
                path_depth(&a.display)
                    .cmp(&path_depth(&b.display))
                    .then(a.display.len().cmp(&b.display.len()))
                    .then(a.display.cmp(&b.display))
                    .then(a.uri.cmp(&b.uri))
            });
            files.truncate(FILE_CAP);

            let mut issues: Vec<(usize, MentionEntry)> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.kind == MentionKind::Issue)
                .map(|(index, e)| (index, e.clone()))
                .collect();
            issues.sort_by(|(idx_a, a), (idx_b, b)| {
                idx_a
                    .cmp(idx_b)
                    .then(issue_id(a).cmp(issue_id(b)))
                    .then(a.uri.cmp(&b.uri))
            });
            issues.truncate(ISSUE_CAP);
            let issues: Vec<MentionEntry> = issues.into_iter().map(|(_, entry)| entry).collect();

            let mut code_graph: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| matches!(e.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
                .cloned()
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
        let mut scored: Vec<RankedMention> = entries
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
                Some(RankedMention {
                    rank,
                    entry: e.clone(),
                })
            })
            .collect();
        scored.sort_by(typed_query_cmp);
        scored
            .into_iter()
            .take(limit)
            .map(|ranked| ranked.entry)
            .collect()
    }
}

fn is_graph_uri(uri: &str) -> bool {
    uri.starts_with("graph://file/") || uri.starts_with("graph://symbol/")
}

#[derive(Debug, Clone)]
struct RankedMention {
    rank: u32,
    entry: MentionEntry,
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
                return Some(u32::MAX);
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
        MentionKind::CodeFile => Some(entry.display.as_str()),
        MentionKind::CodeSymbol => entry.secondary.as_deref().and_then(symbol_secondary_path),
        MentionKind::File | MentionKind::Directory | MentionKind::Worker | MentionKind::Issue => {
            None
        }
    }
}

fn symbol_secondary_path(secondary: &str) -> Option<&str> {
    secondary.split_whitespace().nth(1).and_then(|location| {
        let path_end = location.rfind(':')?;
        Some(&location[..path_end])
    })
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

fn scores_within_window(a: u32, b: u32) -> bool {
    let max_score = a.max(b);
    if max_score == 0 {
        return true;
    }
    a.abs_diff(b).saturating_mul(100) <= max_score.saturating_mul(10)
}

fn typed_query_cmp(a: &RankedMention, b: &RankedMention) -> std::cmp::Ordering {
    if scores_within_window(a.rank, b.rank) {
        let tier_cmp = tier_rank(&a.entry.kind).cmp(&tier_rank(&b.entry.kind));
        if tier_cmp != std::cmp::Ordering::Equal {
            return tier_cmp;
        }
    }
    b.rank
        .cmp(&a.rank)
        .then(stable_tie_key(&a.entry).cmp(&stable_tie_key(&b.entry)))
}

impl Default for MentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mentions::{
        IssueMentionDescriptor, IssueMentionSource, MentionKind, MentionSource,
        WorkerMentionDescriptor, WorkerMentionSource,
    };
    use spur_pm::PmSource;

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
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
        };

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "alice", 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "issue://beads/bd-1");
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
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
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
    fn query_uses_smart_case_matching() {
        let mut registry = MentionRegistry {
            sources: vec![Box::new(IssueMentionSource::new(vec![
                issue("bd-1", "deploy prod", None),
                issue("bd-2", "Deploy Prod", None),
            ]))],
            cache: HashMap::new(),
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
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

    fn mention(kind: MentionKind, id: usize, display: String) -> MentionEntry {
        MentionEntry {
            section_header: None,
            kind,
            uri: format!("test://{id}"),
            display,
            secondary: None,
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
            code_payloads: HashMap::new(),
            code_graph_hint: None,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    #[test]
    fn empty_query_caps_each_kind() {
        let workers = (0..10)
            .map(|i| WorkerMentionDescriptor {
                name: format!("worker-{i}"),
                description: None,
                tier: None,
            })
            .collect::<Vec<_>>();
        let files = (0..12)
            .map(|i| mention(MentionKind::File, i, format!("src/path-{i}.rs")))
            .collect::<Vec<_>>();
        let issues = (0..8)
            .map(|i| issue(&format!("bd-{i}"), &format!("Issue {i}"), None))
            .collect::<Vec<_>>();
        let code_files = (0..6)
            .map(|i| mention(MentionKind::CodeFile, 100 + i, format!("src/code-{i}.rs")))
            .collect::<Vec<_>>();
        let code_symbols = (0..6)
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
        assert_eq!(results[0].kind, MentionKind::File);
    }

    #[test]
    fn typed_query_raw_score_beats_tier_outside_window() {
        let mut registry = test_registry(vec![Box::new(StaticSource {
            name: "mixed",
            entries: vec![
                mention(MentionKind::File, 1, "a.rs".into()),
                mention(MentionKind::CodeSymbol, 2, "verySpecificNeedle".into()),
            ],
        })]);

        let results = registry.query(
            CompletionScope::PreSession,
            Path::new("."),
            "verySpecificNeedle",
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, MentionKind::CodeSymbol);
    }
}
