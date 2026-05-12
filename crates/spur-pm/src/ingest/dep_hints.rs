//! Dependency-hint extraction and sentinel format (§5.6, §7.5).
//!
//! Pure functions, no I/O. Two extractors compose into the final
//! `Vec<DepHint>`:
//!
//! E-1 Body extractor — regex-driven over `RemoteNode.body`.
//! E-2 Timeline extractor — walks the GraphQL `timelineItems.nodes`
//!     via the `TimelineRef` adapter type below (PR-4 populates it
//!     from `octocrab` types; PR-3 ships the walker and the canonical
//!     form helpers).
//!
//! Refs are always normalized to `<owner>/<repo>#<number>`. Bare
//! `#42` body refs are expanded with the issue's own `source_repo`.
//!
//! Hints **never** mutate the local DAG — the CI grep gate A-8
//! enforces no `add_dependency` call sites that originate from a
//! `DepHint`.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::ingest::watermark::DEP_HINT_SENTINEL_HEADER;
use crate::sync::{DepHint, DepHintKind, DepHintSource};

// ─── Timeline adapter type (populated by github/mapping.rs in PR-4) ───

/// Timeline-event shape the extractor consumes. Two cases mirror
/// GitHub's GraphQL schema (`ingest_repo.graphql` in PR-4):
///
/// - `CrossReferenced` — another issue/PR cross-references *this*
///   node. When the source is a PR whose `closingIssuesReferences`
///   contains us, the relationship is `closes`; otherwise it's
///   `depends-on`.
/// - `Closed` — `ClosedEvent` on this node; if `closer_pr` is set the
///   issue was closed *by* a PR (DepHint kind: `closes`, on the
///   issue side, ref points at the PR).
#[derive(Debug, Clone)]
pub enum TimelineRef {
    CrossReferenced {
        source_kind: TimelineSourceKind,
        repo: String,
        number: u64,
        node_id: String,
        /// True iff the source is a PR that has us in its
        /// `closingIssuesReferences` list.
        closes_this: bool,
    },
    Closed {
        reason: Option<String>,
        /// Present iff the closer was a PR.
        closer_pr: Option<TimelineCloserPr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineSourceKind {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone)]
pub struct TimelineCloserPr {
    pub repo: String,
    pub number: u64,
    pub node_id: String,
}

// ─── Body regex set (§7.5 E-1) ──────────────────────────────────────

/// `\b(close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+(<owner>/<repo>)?#<n>`,
/// case-insensitive.
static PATTERN_CLOSING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?P<kw>close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+(?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)",
    )
    .expect("PATTERN_CLOSING compiles")
});

/// `^\s*(depends on|blocked by|blocks)\s*:?\s+<ref>`, multiline,
/// case-insensitive.
static PATTERN_DEPENDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^\s*(?P<kw>depends\s+on|blocked\s+by|blocks)\s*:?\s+(?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)",
    )
    .expect("PATTERN_DEPENDS compiles")
});

/// `^\s*- [ ] <ref>` and `^\s*- [x] <ref>`, multiline.
static PATTERN_TASKLIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*-\s*\[\s*[ xX]\s*\]\s*(?P<ref>(?:[\w.-]+/[\w.-]+)?#\d+)")
        .expect("PATTERN_TASKLIST compiles")
});

fn closing_keyword_kind(kw: &str) -> DepHintKind {
    let lower = kw.to_ascii_lowercase();
    if lower.starts_with("close") {
        DepHintKind::Closes
    } else if lower.starts_with("fix") {
        DepHintKind::Fixes
    } else if lower.starts_with("resolve") {
        DepHintKind::Resolves
    } else {
        DepHintKind::Closes
    }
}

fn depends_keyword_kind(kw: &str) -> DepHintKind {
    let normalized: String = kw
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match normalized.as_str() {
        "depends on" => DepHintKind::DependsOn,
        "blocked by" => DepHintKind::BlockedBy,
        "blocks" => DepHintKind::Blocks,
        _ => DepHintKind::DependsOn,
    }
}

/// Canonicalize `#42` (bare numeric) or `owner/repo#42` to
/// `owner/repo#42`. Returns `None` if the ref shape is not
/// recognized.
pub fn canonicalize_ref(raw: &str, current_repo: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('#') {
        if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Some(format!("{current_repo}#{rest}"));
        }
        return None;
    }
    let (repo, num) = raw.split_once('#')?;
    if repo.is_empty() || num.is_empty() {
        return None;
    }
    if !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !repo.contains('/') {
        return None;
    }
    Some(format!("{repo}#{num}"))
}

/// Extract dep hints from a body + timeline, canonicalizing all refs
/// against `current_repo` and deduplicating by `(remote_ref, kind)`.
///
/// Timeline-sourced hints take precedence over body-sourced ones for
/// the same `(remote_ref, kind)` because GitHub's already-resolved
/// view is more authoritative than a regex match.
pub fn extract(body: &str, timeline: &[TimelineRef], current_repo: &str) -> Vec<DepHint> {
    let body_hints = extract_from_body(body, current_repo);
    let timeline_hints = extract_from_timeline(timeline, current_repo);
    dedupe_prefer_timeline(body_hints, timeline_hints)
}

fn extract_from_body(body: &str, current_repo: &str) -> Vec<DepHint> {
    let mut out = Vec::new();

    for cap in PATTERN_CLOSING.captures_iter(body) {
        let kw = cap.name("kw").map(|m| m.as_str()).unwrap_or("");
        let r = cap.name("ref").map(|m| m.as_str()).unwrap_or("");
        let Some(canonical) = canonicalize_ref(r, current_repo) else {
            continue;
        };
        out.push(DepHint {
            kind: closing_keyword_kind(kw),
            remote_keyword: kw.to_string(),
            remote_ref: canonical,
            remote_node_id: None,
            raw_span: cap
                .get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            source: DepHintSource::Body,
        });
    }

    for cap in PATTERN_DEPENDS.captures_iter(body) {
        let kw = cap.name("kw").map(|m| m.as_str()).unwrap_or("");
        let r = cap.name("ref").map(|m| m.as_str()).unwrap_or("");
        let Some(canonical) = canonicalize_ref(r, current_repo) else {
            continue;
        };
        out.push(DepHint {
            kind: depends_keyword_kind(kw),
            remote_keyword: kw.to_string(),
            remote_ref: canonical,
            remote_node_id: None,
            raw_span: cap
                .get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            source: DepHintSource::Body,
        });
    }

    for cap in PATTERN_TASKLIST.captures_iter(body) {
        let r = cap.name("ref").map(|m| m.as_str()).unwrap_or("");
        let Some(canonical) = canonicalize_ref(r, current_repo) else {
            continue;
        };
        out.push(DepHint {
            kind: DepHintKind::TaskList,
            remote_keyword: "- [ ]".to_string(),
            remote_ref: canonical,
            remote_node_id: None,
            raw_span: cap
                .get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
            source: DepHintSource::Body,
        });
    }

    out
}

fn extract_from_timeline(timeline: &[TimelineRef], _current_repo: &str) -> Vec<DepHint> {
    let mut out = Vec::new();
    for item in timeline {
        match item {
            TimelineRef::CrossReferenced {
                repo,
                number,
                node_id,
                closes_this,
                source_kind: _,
            } => {
                let canonical = format!("{repo}#{number}");
                let (kind, kw) = if *closes_this {
                    (DepHintKind::Closes, "ClosedByPR")
                } else {
                    (DepHintKind::DependsOn, "CrossReferenced")
                };
                out.push(DepHint {
                    kind,
                    remote_keyword: kw.to_string(),
                    remote_ref: canonical,
                    remote_node_id: Some(node_id.clone()),
                    raw_span: format!("{repo}#{number}"),
                    source: DepHintSource::TimelineItem,
                });
            }
            TimelineRef::Closed {
                reason: _,
                closer_pr: Some(pr),
            } => {
                let canonical = format!("{}#{}", pr.repo, pr.number);
                out.push(DepHint {
                    kind: DepHintKind::Closes,
                    remote_keyword: "ClosedByPR".to_string(),
                    remote_ref: canonical,
                    remote_node_id: Some(pr.node_id.clone()),
                    raw_span: format!("{}#{}", pr.repo, pr.number),
                    source: DepHintSource::TimelineItem,
                });
            }
            TimelineRef::Closed {
                closer_pr: None, ..
            } => {
                // Closed by a human commit or manually — no hint to emit.
            }
        }
    }
    out
}

fn dedupe_prefer_timeline(body: Vec<DepHint>, timeline: Vec<DepHint>) -> Vec<DepHint> {
    let mut seen: HashSet<(String, DepHintKind)> = HashSet::new();
    let mut out = Vec::with_capacity(body.len() + timeline.len());

    // Timeline first so it wins on (ref, kind) ties.
    for h in timeline {
        if seen.insert((h.remote_ref.clone(), h.kind)) {
            out.push(h);
        }
    }
    for h in body {
        if seen.insert((h.remote_ref.clone(), h.kind)) {
            out.push(h);
        }
    }
    out
}

// ─── Sentinel format/parse for `spur-dep-hint v1` comments ─────────

pub fn format_dep_hint_sentinel(hint: &DepHint) -> String {
    let kind_str = dep_hint_kind_str(hint.kind);
    let node_id = hint.remote_node_id.as_deref().unwrap_or("");
    let source = match hint.source {
        DepHintSource::Body => "body",
        DepHintSource::TimelineItem => "timeline_item",
    };
    let mut out = String::with_capacity(256);
    out.push_str(DEP_HINT_SENTINEL_HEADER);
    out.push('\n');
    push_kv(&mut out, "kind", kind_str);
    push_kv(&mut out, "remote_keyword", &hint.remote_keyword);
    push_kv(&mut out, "remote_ref", &hint.remote_ref);
    push_kv(&mut out, "remote_node", node_id);
    push_kv(&mut out, "source", source);
    let safe_raw: String = hint
        .raw_span
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    push_kv(&mut out, "raw_span", &safe_raw);
    out
}

pub fn parse_dep_hint_sentinel(body: &str) -> Option<DepHint> {
    let mut lines = body.lines();
    let header = lines.next()?;
    if header.trim() != DEP_HINT_SENTINEL_HEADER {
        return None;
    }
    let mut kind: Option<DepHintKind> = None;
    let mut kw: Option<String> = None;
    let mut r: Option<String> = None;
    let mut node: Option<String> = None;
    let mut source: Option<DepHintSource> = None;
    let mut raw: Option<String> = None;

    for line in lines {
        let (k, v) = line.split_once(':')?;
        let v = v.trim();
        match k.trim() {
            "kind" => kind = parse_dep_hint_kind(v),
            "remote_keyword" => kw = Some(v.to_string()),
            "remote_ref" => r = Some(v.to_string()),
            "remote_node" => {
                node = if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                };
            }
            "source" => {
                source = match v {
                    "body" => Some(DepHintSource::Body),
                    "timeline_item" => Some(DepHintSource::TimelineItem),
                    _ => None,
                }
            }
            "raw_span" => raw = Some(v.to_string()),
            _ => {}
        }
    }

    Some(DepHint {
        kind: kind?,
        remote_keyword: kw.unwrap_or_default(),
        remote_ref: r?,
        remote_node_id: node,
        raw_span: raw.unwrap_or_default(),
        source: source.unwrap_or(DepHintSource::Body),
    })
}

fn dep_hint_kind_str(k: DepHintKind) -> &'static str {
    match k {
        DepHintKind::Closes => "closes",
        DepHintKind::Fixes => "fixes",
        DepHintKind::Resolves => "resolves",
        DepHintKind::DependsOn => "depends-on",
        DepHintKind::Blocks => "blocks",
        DepHintKind::BlockedBy => "blocked-by",
        DepHintKind::TaskList => "task-list",
    }
}

fn parse_dep_hint_kind(s: &str) -> Option<DepHintKind> {
    match s {
        "closes" => Some(DepHintKind::Closes),
        "fixes" => Some(DepHintKind::Fixes),
        "resolves" => Some(DepHintKind::Resolves),
        "depends-on" => Some(DepHintKind::DependsOn),
        "blocks" => Some(DepHintKind::Blocks),
        "blocked-by" => Some(DepHintKind::BlockedBy),
        "task-list" => Some(DepHintKind::TaskList),
        _ => None,
    }
}

fn push_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push(':');
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_bare_ref_uses_current_repo() {
        assert_eq!(
            canonicalize_ref("#42", "getspur/spur").as_deref(),
            Some("getspur/spur#42")
        );
    }

    #[test]
    fn canonicalize_qualified_ref_is_preserved() {
        assert_eq!(
            canonicalize_ref("other/repo#7", "getspur/spur").as_deref(),
            Some("other/repo#7")
        );
    }

    #[test]
    fn canonicalize_rejects_malformed() {
        assert_eq!(canonicalize_ref("not-a-ref", "getspur/spur"), None);
        assert_eq!(canonicalize_ref("#abc", "getspur/spur"), None);
        assert_eq!(canonicalize_ref("#", "getspur/spur"), None);
        assert_eq!(canonicalize_ref("noOwner#1", "getspur/spur"), None);
    }

    #[test]
    fn body_extracts_closing_keywords() {
        let body = "This PR closes #1 and fixes other/repo#2. Also resolves #3.";
        let hints = extract(body, &[], "owner/repo");
        let kinds: Vec<_> = hints
            .iter()
            .map(|h| (h.kind, h.remote_ref.clone()))
            .collect();
        assert!(kinds.contains(&(DepHintKind::Closes, "owner/repo#1".to_string())));
        assert!(kinds.contains(&(DepHintKind::Fixes, "other/repo#2".to_string())));
        assert!(kinds.contains(&(DepHintKind::Resolves, "owner/repo#3".to_string())));
    }

    #[test]
    fn body_extracts_depends_keywords_with_optional_colon() {
        let body = "\nDepends on #5\nBlocked by: other/repo#6\nblocks #7\n";
        let hints = extract(body, &[], "owner/repo");
        let kinds: Vec<_> = hints
            .iter()
            .map(|h| (h.kind, h.remote_ref.clone()))
            .collect();
        assert!(kinds.contains(&(DepHintKind::DependsOn, "owner/repo#5".to_string())));
        assert!(kinds.contains(&(DepHintKind::BlockedBy, "other/repo#6".to_string())));
        assert!(kinds.contains(&(DepHintKind::Blocks, "owner/repo#7".to_string())));
    }

    #[test]
    fn body_extracts_task_list() {
        let body = "- [ ] #1\n- [x] other/repo#2\n";
        let hints = extract(body, &[], "owner/repo");
        let refs: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == DepHintKind::TaskList)
            .map(|h| h.remote_ref.clone())
            .collect();
        assert!(refs.contains(&"owner/repo#1".to_string()));
        assert!(refs.contains(&"other/repo#2".to_string()));
    }

    #[test]
    fn timeline_closed_by_pr_emits_closes_hint() {
        let timeline = vec![TimelineRef::Closed {
            reason: Some("COMPLETED".into()),
            closer_pr: Some(TimelineCloserPr {
                repo: "owner/repo".into(),
                number: 99,
                node_id: "PR_kwDO_99".into(),
            }),
        }];
        let hints = extract("", &timeline, "owner/repo");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].kind, DepHintKind::Closes);
        assert_eq!(hints[0].remote_ref, "owner/repo#99");
        assert_eq!(hints[0].remote_node_id.as_deref(), Some("PR_kwDO_99"));
        assert!(matches!(hints[0].source, DepHintSource::TimelineItem));
    }

    #[test]
    fn dedupe_prefers_timeline_over_body_for_same_pair() {
        let body = "Closes #1";
        let timeline = vec![TimelineRef::CrossReferenced {
            source_kind: TimelineSourceKind::PullRequest,
            repo: "owner/repo".into(),
            number: 1,
            node_id: "PR_kwDO_1".into(),
            closes_this: true,
        }];
        let hints = extract(body, &timeline, "owner/repo");
        let closes: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == DepHintKind::Closes && h.remote_ref == "owner/repo#1")
            .collect();
        assert_eq!(closes.len(), 1);
        assert!(closes[0].remote_node_id.is_some());
        assert!(matches!(closes[0].source, DepHintSource::TimelineItem));
    }

    #[test]
    fn sentinel_roundtrip() {
        let h = DepHint {
            kind: DepHintKind::Closes,
            remote_keyword: "Closes".into(),
            remote_ref: "owner/repo#1".into(),
            remote_node_id: Some("PR_kwDO_1".into()),
            raw_span: "Closes #1".into(),
            source: DepHintSource::TimelineItem,
        };
        let body = format_dep_hint_sentinel(&h);
        let parsed = parse_dep_hint_sentinel(&body).expect("parses");
        assert_eq!(parsed.kind, h.kind);
        assert_eq!(parsed.remote_ref, h.remote_ref);
        assert_eq!(parsed.remote_node_id, h.remote_node_id);
        assert!(matches!(parsed.source, DepHintSource::TimelineItem));
    }

    proptest::proptest! {
        #[test]
        fn extract_never_panics_on_arbitrary_utf8(s in "\\PC{0,200}") {
            let _ = extract(&s, &[], "owner/repo");
        }
    }
}
