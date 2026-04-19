use std::collections::HashMap;
use std::time::{Duration, Instant};

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};
use spur_acp::SessionId;

use super::entry::{MentionEntry, MentionSource};
use super::file_source::FileMentionSource;

const CACHE_TTL: Duration = Duration::from_secs(60);

struct CachedIndex {
    entries: Vec<MentionEntry>,
    built_at: Instant,
}

pub struct MentionRegistry {
    sources: Vec<Box<dyn MentionSource>>,
    cache: HashMap<String, CachedIndex>,
}

impl MentionRegistry {
    /// Source list for direct (single-agent) sessions. Files only.
    pub fn for_direct_session() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
        }
    }

    /// Source list for brain sessions. Files + workers.
    /// `workers` is the snapshot derived from the agent registry.
    pub fn for_brain_session(workers: Vec<super::WorkerMentionDescriptor>) -> Self {
        Self {
            sources: vec![
                Box::new(FileMentionSource),
                Box::new(super::WorkerMentionSource::new(workers)),
            ],
            cache: HashMap::new(),
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
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub fn query(
        &mut self,
        session: &SessionId,
        cwd: &std::path::Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        // … existing body unchanged in this task; ranking changes happen in Task 4.
        let key = session_key(session);
        let needs_rebuild = match self.cache.get(&key) {
            Some(c) => c.built_at.elapsed() > CACHE_TTL,
            None => true,
        };
        if needs_rebuild {
            let mut all = Vec::new();
            for s in &mut self.sources {
                if let Ok(mut entries) = s.build(cwd) {
                    all.append(&mut entries);
                }
            }
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
            let mut out: Vec<MentionEntry> = entries.iter().take(limit).cloned().collect();
            out.sort_by_key(|e| e.display.len());
            return out.into_iter().take(limit).collect();
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, MentionEntry)> = entries
            .iter()
            .filter_map(|e| {
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&e.display, &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, e.clone()))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.display.len().cmp(&b.1.display.len()))
        });
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

impl Default for MentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn session_key(session: &SessionId) -> String {
    session.0.clone()
}
