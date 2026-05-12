//! Watermark sentinel and import-marker helpers (§4.1, §4.2, §4.3 of
//! `docs/architecture/spur-pm-github-ingest.md`).
//!
//! The watermark is stored as a `spur-sync v1` comment appended to each
//! ingested issue. The latest such comment (by `created_at`) is the
//! current watermark. Imported remote comments carry an embedded
//! `<!-- spur-import gh:<node_id> -->` first-line marker that is the
//! per-comment dedup key.
//!
//! **Marker-removal behavior.** If a human edits the first line of an
//! imported comment and strips the `<!-- spur-import ... -->` marker,
//! the comment becomes invisible to the dedup scan. A subsequent
//! ingest of the same `RemoteComment` will import a fresh copy. The
//! existing local comment is left untouched. This is the
//! "duplicate, never corrupt" failure mode documented in §10 / T-4 —
//! the marker is fragile by design because we chose the comments
//! column over a sidecar table.

use chrono::{DateTime, Utc};

use crate::advanced::Comment;
use crate::sync::{RemoteComment, RemoteNode};

pub const SYNC_SENTINEL_HEADER: &str = "spur-sync v1";
pub const DEP_HINT_SENTINEL_HEADER: &str = "spur-dep-hint v1";
pub const IMPORT_MARKER_PREFIX: &str = "<!-- spur-import ";

/// Lifecycle state of the per-issue ingest link (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Ingest is working; last sync succeeded.
    Active,
    /// A 401 was observed against this remote node.
    NeedsAuth,
    /// A 404 / private-repo error was observed.
    Disconnected,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkState::Active => "active",
            LinkState::NeedsAuth => "needs_auth",
            LinkState::Disconnected => "disconnected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "active" => Some(LinkState::Active),
            "needs_auth" => Some(LinkState::NeedsAuth),
            "disconnected" => Some(LinkState::Disconnected),
            _ => None,
        }
    }
}

/// Parsed `spur-sync v1` sentinel body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSentinel {
    pub source_system: String,
    pub remote_id: String,
    pub remote_number: Option<u64>,
    pub remote_etag: Option<String>,
    pub remote_updated_at: DateTime<Utc>,
    pub last_synced_at: DateTime<Utc>,
    pub last_synced_remote_updated_at: DateTime<Utc>,
    pub state: LinkState,
}

/// Format a `spur-sync v1` sentinel body (§4.1).
pub fn format_sync_sentinel(s: &SyncSentinel) -> String {
    let etag = s.remote_etag.as_deref().unwrap_or("");
    let number = s.remote_number.map(|n| n.to_string()).unwrap_or_default();
    let mut out = String::with_capacity(360);
    out.push_str(SYNC_SENTINEL_HEADER);
    out.push('\n');
    push_kv(&mut out, "source_system", &s.source_system);
    push_kv(&mut out, "remote_id", &s.remote_id);
    push_kv(&mut out, "remote_number", &number);
    push_kv(&mut out, "remote_etag", etag);
    push_kv(&mut out, "remote_updated_at", &rfc3339(s.remote_updated_at));
    push_kv(&mut out, "last_synced_at", &rfc3339(s.last_synced_at));
    push_kv(
        &mut out,
        "last_synced_remote_updated_at",
        &rfc3339(s.last_synced_remote_updated_at),
    );
    push_kv(&mut out, "state", s.state.as_str());
    out
}

/// Convenience: build a sentinel from a `RemoteNode` and ambient state.
pub fn sentinel_from_node(
    node: &RemoteNode,
    source_system: &str,
    state: LinkState,
    now: DateTime<Utc>,
) -> SyncSentinel {
    SyncSentinel {
        source_system: source_system.to_string(),
        remote_id: node.remote_id.clone(),
        remote_number: node.remote_number,
        remote_etag: node.etag.clone(),
        remote_updated_at: node.updated_at,
        last_synced_at: now,
        last_synced_remote_updated_at: node.updated_at,
        state,
    }
}

/// Parse a `spur-sync v1` sentinel body. Returns `Err` on missing
/// required fields or unparseable timestamps.
pub fn parse_sync_sentinel(body: &str) -> Result<SyncSentinel, ParseError> {
    let mut lines = body.lines();
    let header = lines.next().ok_or(ParseError::MissingHeader)?;
    if header.trim() != SYNC_SENTINEL_HEADER {
        return Err(ParseError::WrongHeader(header.to_string()));
    }

    let mut source_system: Option<String> = None;
    let mut remote_id: Option<String> = None;
    let mut remote_number: Option<u64> = None;
    let mut remote_etag: Option<String> = None;
    let mut remote_updated_at: Option<DateTime<Utc>> = None;
    let mut last_synced_at: Option<DateTime<Utc>> = None;
    let mut last_synced_remote_updated_at: Option<DateTime<Utc>> = None;
    let mut state: Option<LinkState> = None;

    for line in lines {
        let (k, v) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match k {
            "source_system" => source_system = Some(v.to_string()),
            "remote_id" => remote_id = Some(v.to_string()),
            "remote_number" => {
                if !v.is_empty() {
                    remote_number = Some(
                        v.parse()
                            .map_err(|_| ParseError::BadValue("remote_number"))?,
                    );
                }
            }
            "remote_etag" => {
                if !v.is_empty() {
                    remote_etag = Some(v.to_string());
                }
            }
            "remote_updated_at" => {
                remote_updated_at =
                    Some(parse_rfc3339(v).ok_or(ParseError::BadValue("remote_updated_at"))?);
            }
            "last_synced_at" => {
                last_synced_at =
                    Some(parse_rfc3339(v).ok_or(ParseError::BadValue("last_synced_at"))?);
            }
            "last_synced_remote_updated_at" => {
                last_synced_remote_updated_at = Some(
                    parse_rfc3339(v)
                        .ok_or(ParseError::BadValue("last_synced_remote_updated_at"))?,
                );
            }
            "state" => state = LinkState::parse(v),
            _ => { /* forward-compat: ignore unknown keys */ }
        }
    }

    Ok(SyncSentinel {
        source_system: source_system.ok_or(ParseError::Missing("source_system"))?,
        remote_id: remote_id.ok_or(ParseError::Missing("remote_id"))?,
        remote_number,
        remote_etag,
        remote_updated_at: remote_updated_at.ok_or(ParseError::Missing("remote_updated_at"))?,
        last_synced_at: last_synced_at.ok_or(ParseError::Missing("last_synced_at"))?,
        last_synced_remote_updated_at: last_synced_remote_updated_at
            .ok_or(ParseError::Missing("last_synced_remote_updated_at"))?,
        state: state.unwrap_or(LinkState::Active),
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("sentinel body is empty")]
    MissingHeader,
    #[error("wrong sentinel header: {0}")]
    WrongHeader(String),
    #[error("missing required field: {0}")]
    Missing(&'static str),
    #[error("invalid value for field: {0}")]
    BadValue(&'static str),
}

/// Return the newest `spur-sync v1` sentinel across `comments` if any.
///
/// Bounded scan: O(N) over comments where N is the count for the
/// issue. R-4 in §13 of the spec sets the wall-time gate (<500ms for
/// 1k issues averaging ~50 comments each) — the dominant cost is the
/// SQLite `get_comments` call, not the parse loop itself.
pub fn latest_sync_sentinel(comments: &[Comment]) -> Option<SyncSentinel> {
    comments
        .iter()
        .filter(|c| c.body.starts_with(SYNC_SENTINEL_HEADER))
        .max_by_key(|c| c.created_at)
        .and_then(|c| parse_sync_sentinel(&c.body).ok())
}

/// Format an imported-remote-comment body (§4.2). First line is the
/// HTML-comment marker; remaining lines preserve the remote body
/// verbatim with a small attribution header.
pub fn format_import_comment(rc: &RemoteComment, html_url: &str) -> String {
    let mut out = String::with_capacity(rc.body.len() + 200);
    out.push_str(IMPORT_MARKER_PREFIX);
    out.push_str("gh:");
    out.push_str(&rc.remote_id);
    out.push_str(" -->\n");
    out.push_str("imported from ");
    out.push_str(html_url);
    out.push_str(" by gh:");
    out.push_str(&rc.author);
    out.push_str(" (");
    out.push_str(&rfc3339(rc.created_at));
    out.push_str("):\n\n");
    out.push_str(&rc.body);
    out
}

/// Parse the leading `<!-- spur-import gh:<id> -->` line if present.
/// Returns `Some(remote_id)` on match (the part after `gh:`), `None`
/// for anything else — including:
/// * lines whose marker has been stripped (the human-edit failure
///   mode the §10 / T-4 manual-marker-removal case pins as
///   "duplicate, never corrupt").
/// * lookalike text inside the body that happens to contain
///   `spur-import` (we only match the EXACT marker shape).
pub fn parse_import_marker(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(IMPORT_MARKER_PREFIX)?;
    let rest = rest.strip_prefix("gh:")?;
    let end = rest.find(" -->")?;
    let id = &rest[..end];
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Scan a comment vector for `<!-- spur-import gh:<id> -->` markers
/// on the first line of each comment body. Returns the set of remote
/// node_ids already imported.
pub fn scan_import_markers<'a, I>(comments: I) -> std::collections::HashSet<String>
where
    I: IntoIterator<Item = &'a Comment>,
{
    comments
        .into_iter()
        .filter_map(|c| c.body.lines().next().and_then(parse_import_marker))
        .map(|s| s.to_string())
        .collect()
}

fn push_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push(':');
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

fn rfc3339(ts: DateTime<Utc>) -> String {
    // Millisecond precision so a sentinel write followed by a local
    // `updated_at` bump can be ordered correctly. Seconds-only would
    // truncate `last_synced_at` to the second boundary, which broke
    // the conflict detector for any write that landed in the same
    // wall-clock second.
    ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(iso: &str) -> DateTime<Utc> {
        parse_rfc3339(iso).unwrap()
    }

    fn make_sentinel() -> SyncSentinel {
        SyncSentinel {
            source_system: "github".into(),
            remote_id: "I_kwDOExample123".into(),
            remote_number: Some(42),
            remote_etag: Some("W/\"abc\"".into()),
            remote_updated_at: ts("2026-05-10T12:00:00Z"),
            last_synced_at: ts("2026-05-12T03:00:00Z"),
            last_synced_remote_updated_at: ts("2026-05-10T12:00:00Z"),
            state: LinkState::Active,
        }
    }

    #[test]
    fn format_parse_roundtrip_active() {
        let s = make_sentinel();
        let body = format_sync_sentinel(&s);
        let parsed = parse_sync_sentinel(&body).expect("roundtrip parse");
        assert_eq!(parsed, s);
    }

    #[test]
    fn format_parse_roundtrip_disconnected_no_etag_no_number() {
        let mut s = make_sentinel();
        s.state = LinkState::Disconnected;
        s.remote_etag = None;
        s.remote_number = None;
        let body = format_sync_sentinel(&s);
        let parsed = parse_sync_sentinel(&body).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn format_parse_roundtrip_needs_auth() {
        let mut s = make_sentinel();
        s.state = LinkState::NeedsAuth;
        let body = format_sync_sentinel(&s);
        let parsed = parse_sync_sentinel(&body).unwrap();
        assert_eq!(parsed.state, LinkState::NeedsAuth);
    }

    #[test]
    fn parse_rejects_wrong_header() {
        let err = parse_sync_sentinel("not-a-sentinel v9\nfoo: bar").unwrap_err();
        assert!(matches!(err, ParseError::WrongHeader(_)));
    }

    #[test]
    fn parse_rejects_missing_required() {
        let err = parse_sync_sentinel("spur-sync v1\nsource_system: github").unwrap_err();
        assert!(matches!(err, ParseError::Missing(_)));
    }

    #[test]
    fn parse_rejects_bad_timestamp() {
        let bad = "spur-sync v1\nsource_system: github\nremote_id: x\nremote_number: \nremote_etag: \nremote_updated_at: not-a-date\nlast_synced_at: 2026-05-12T03:00:00Z\nlast_synced_remote_updated_at: 2026-05-12T03:00:00Z\nstate: active\n";
        let err = parse_sync_sentinel(bad).unwrap_err();
        assert!(matches!(err, ParseError::BadValue("remote_updated_at")));
    }

    #[test]
    fn latest_picks_newest_by_created_at() {
        let s_old = make_sentinel();
        let s_new = {
            let mut s = make_sentinel();
            s.last_synced_at = ts("2026-05-15T03:00:00Z");
            s.state = LinkState::NeedsAuth;
            s
        };

        let comments = vec![
            Comment {
                id: "1".into(),
                body: format_sync_sentinel(&s_old),
                actor: "spur".into(),
                created_at: ts("2026-05-12T03:00:00Z"),
            },
            Comment {
                id: "2".into(),
                body: "unrelated body".into(),
                actor: "alice".into(),
                created_at: ts("2026-05-14T03:00:00Z"),
            },
            Comment {
                id: "3".into(),
                body: format_sync_sentinel(&s_new),
                actor: "spur".into(),
                created_at: ts("2026-05-15T03:00:00Z"),
            },
        ];

        let latest = latest_sync_sentinel(&comments).expect("found");
        assert_eq!(latest.state, LinkState::NeedsAuth);
    }

    #[test]
    fn latest_returns_none_when_no_sentinel() {
        let comments = vec![Comment {
            id: "1".into(),
            body: "just chatter".into(),
            actor: "alice".into(),
            created_at: ts("2026-05-12T03:00:00Z"),
        }];
        assert!(latest_sync_sentinel(&comments).is_none());
    }

    #[test]
    fn import_marker_roundtrip() {
        let rc = RemoteComment {
            remote_id: "IC_kwDOC123".into(),
            author: "alice".into(),
            body: "hello world".into(),
            created_at: ts("2026-05-12T03:00:00Z"),
            updated_at: ts("2026-05-12T03:00:00Z"),
        };
        let formatted =
            format_import_comment(&rc, "https://github.com/o/r/issues/1#issuecomment-1");
        let first_line = formatted.lines().next().unwrap();
        let id = parse_import_marker(first_line).expect("marker parsed");
        assert_eq!(id, "IC_kwDOC123");
    }

    #[test]
    fn import_marker_rejects_lookalikes() {
        assert!(parse_import_marker("body mentioning spur-import inline").is_none());
        assert!(parse_import_marker("<!-- spur-import gh: -->").is_none());
        assert!(parse_import_marker("<!-- something else -->").is_none());
        assert!(parse_import_marker("<!-- spur-import gh:abc").is_none());
    }

    #[test]
    fn scan_import_markers_returns_unique_ids() {
        let comments = vec![
            Comment {
                id: "1".into(),
                body: "<!-- spur-import gh:A1 -->\nbody1".into(),
                actor: "spur".into(),
                created_at: ts("2026-05-12T03:00:00Z"),
            },
            Comment {
                id: "2".into(),
                body: "no marker".into(),
                actor: "alice".into(),
                created_at: ts("2026-05-13T03:00:00Z"),
            },
            Comment {
                id: "3".into(),
                body: "<!-- spur-import gh:B2 -->\nbody2".into(),
                actor: "spur".into(),
                created_at: ts("2026-05-14T03:00:00Z"),
            },
        ];
        let set = scan_import_markers(&comments);
        assert_eq!(set.len(), 2);
        assert!(set.contains("A1"));
        assert!(set.contains("B2"));
    }
}
