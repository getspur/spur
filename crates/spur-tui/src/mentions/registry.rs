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

/// Maximum number of worker rows pinned to the top of the empty-query
/// picker view. See design spec §4.4 / §10.1.
pub(super) const WORKER_PIN_CAP: usize = 6;

/// Multiplicative boost numerator for worker entries in the typed-query
/// branch. With `WORKER_SCORE_DEN = 4` this yields a +25 % bias, enough
/// to surface workers above tied file matches without overriding strong
/// file-specific matches. Empirically validated; see design spec §10.1.
pub(super) const WORKER_SCORE_NUM: u32 = 5;
pub(super) const WORKER_SCORE_DEN: u32 = 4;

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
            // fill remaining slots with files sorted by display length.
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
            let mut files: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| e.kind != MentionKind::Worker)
                .cloned()
                .collect();
            files.sort_by_key(|e| e.display.len());
            files.truncate(remaining);

            workers.extend(files);
            return workers;
        }

        // Typed-query branch: nucleo score with a +25 % boost for workers.
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, MentionEntry)> = entries
            .iter()
            .filter_map(|e| {
                buf.clear();
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
                Some((boosted, e.clone()))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.display.len().cmp(&b.1.display.len()))
        });
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

fn is_graph_uri(uri: &str) -> bool {
    uri.starts_with("graph://file/") || uri.starts_with("graph://symbol/")
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
