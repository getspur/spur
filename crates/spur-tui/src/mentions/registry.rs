use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher,
};
use spur_acp::SessionId;

use super::code_graph::source::CodeGraphMentionSource;
use super::code_graph::CodeMentionPayload;
use super::entry::{MentionEntry, MentionKind, MentionSource};
use super::file_source::FileMentionSource;
use super::issue_source::{IssueMentionDescriptor, IssueMentionSource};
use super::worker_source::{WorkerMentionDescriptor, WorkerMentionSource};

const CACHE_TTL: Duration = Duration::from_secs(60);
pub const CODE_GRAPH_INDEX_ENV: &str = "SPUR_CODE_GRAPH_INDEX";

/// Maximum number of worker rows pinned to the top of the empty-query
/// picker view. See design spec §4.4 / §10.1.
pub(super) const WORKER_PIN_CAP: usize = 6;

/// Multiplicative boost numerator for worker entries in the typed-query
/// branch. With `WORKER_SCORE_DEN = 4` this yields a +25 % bias, enough
/// to surface workers above tied file matches without overriding strong
/// file-specific matches. Empirically validated; see design spec §10.1.
pub(super) const WORKER_SCORE_NUM: u32 = 5;
pub(super) const WORKER_SCORE_DEN: u32 = 4;

const EMPTY_CODE_GRAPH_CAP: usize = 8;

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
    matcher: Matcher,
}

impl MentionRegistry {
    /// Source list for direct (single-agent) sessions. Files only.
    pub fn for_direct_session() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
            code_payloads: HashMap::new(),
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
        self.clear_cache();
        self
    }

    /// Opt-in runtime code-graph source registration.
    ///
    /// SPUR intentionally uses an environment variable instead of a persisted
    /// config field for v1 so the TUI only consumes an explicit local artifact
    /// path when the user launches it with `SPUR_CODE_GRAPH_INDEX=<path>`.
    /// Unset or empty means "do not load", preserving the §9.2 empty-source
    /// behavior and avoiding accidental live parsing.
    pub fn with_code_graph_from_env(self) -> Self {
        match std::env::var_os(CODE_GRAPH_INDEX_ENV).filter(|value| !value.is_empty()) {
            Some(path) => self.with_code_graph(PathBuf::from(path)),
            None => self,
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
        self.code_payloads.clear();
    }

    pub fn lookup_code_payload(&self, uri: &str) -> Option<&CodeMentionPayload> {
        self.code_payloads.get(uri)
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
            // Empty-query branch: pin up to WORKER_PIN_CAP workers, then
            // fill remaining slots with issue rows, regular file rows, and a
            // bounded code-graph sample. Code symbols are intentionally capped
            // so an empty `@` never becomes a full symbol-table dump.
            // perf: two alloc+sort passes over cached entries on every
            // empty-query call. Acceptable at expected index scale (≤10k);
            // revisit if profiling shows picker latency.
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
            workers.truncate(WORKER_PIN_CAP.min(limit));

            let remaining = limit.saturating_sub(workers.len());
            let mut issues: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| e.kind == MentionKind::Issue)
                .cloned()
                .collect();
            issues.sort_by(|a, b| {
                a.display
                    .len()
                    .cmp(&b.display.len())
                    .then(a.display.cmp(&b.display))
                    .then(a.uri.cmp(&b.uri))
            });
            issues.truncate(remaining);

            let remaining = remaining.saturating_sub(issues.len());
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
            files.truncate(remaining);

            let remaining = remaining.saturating_sub(files.len());
            let code_cap = EMPTY_CODE_GRAPH_CAP.min(remaining);
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
            code_graph.truncate(code_cap);

            workers.extend(issues);
            workers.extend(files);
            workers.extend(code_graph);
            return workers;
        }

        // Typed-query branch: nucleo score with a +25 % boost for workers.
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
                    let raw = pattern.score(
                        nucleo_matcher::Utf32Str::new(haystack, &mut buf),
                        &mut self.matcher,
                    )?;
                    let boosted = if e.kind == MentionKind::Worker {
                        // Ceiling division so small scores still receive at least
                        // a +1 boost; otherwise floor truncation made the +25%
                        // a no-op for raw scores < 4.
                        raw.saturating_mul(WORKER_SCORE_NUM)
                            .div_ceil(WORKER_SCORE_DEN)
                    } else {
                        raw
                    };
                    MatchRank {
                        class: legacy_match_class(&e.kind),
                        score: boosted,
                    }
                };
                Some(RankedMention {
                    rank,
                    entry: e.clone(),
                })
            })
            .collect();
        scored.sort_by(|a, b| {
            a.rank
                .class
                .cmp(&b.rank.class)
                .then(b.rank.score.cmp(&a.rank.score))
                .then(stable_tie_key(&a.entry).cmp(&stable_tie_key(&b.entry)))
        });
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchRank {
    class: u8,
    score: u32,
}

struct RankedMention {
    rank: MatchRank,
    entry: MentionEntry,
}

fn legacy_match_class(kind: &MentionKind) -> u8 {
    match kind {
        MentionKind::Worker | MentionKind::Issue => 4,
        MentionKind::File | MentionKind::Directory => 5,
        MentionKind::CodeFile | MentionKind::CodeSymbol => {
            unreachable!("code rows are classified separately")
        }
    }
}

fn code_match_rank(
    entry: &MentionEntry,
    query: &str,
    pattern: &Pattern,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
) -> Option<MatchRank> {
    let path = code_entry_path(entry);
    match entry.kind {
        MentionKind::CodeSymbol => {
            if eq_ignore_ascii_case(&entry.display, query) {
                return Some(MatchRank {
                    class: 0,
                    score: u32::MAX,
                });
            }

            if let Some(score) = pattern_score(pattern, matcher, buf, &entry.display)
                .or_else(|| prefix_score(&entry.display, query))
            {
                return Some(MatchRank { class: 2, score });
            }

            let path = path?;
            pattern_score(pattern, matcher, buf, path)
                .or_else(|| path_prefix_score(path, query))
                .map(|score| MatchRank { class: 3, score })
        }
        MentionKind::CodeFile => {
            let path = path?;
            if path_segment_exact(path, query) {
                return Some(MatchRank {
                    class: 1,
                    score: u32::MAX,
                });
            }

            pattern_score(pattern, matcher, buf, path)
                .or_else(|| path_prefix_score(path, query))
                .map(|score| MatchRank { class: 3, score })
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

impl Default for MentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mentions::{IssueMentionDescriptor, IssueMentionSource};
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
            matcher: Matcher::new(Config::DEFAULT),
        };

        let results = registry.query(CompletionScope::PreSession, Path::new("."), "Deploy", 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display, "bd-2 Deploy Prod");
    }
}
