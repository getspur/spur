//! Palette data sources.
//!
//! Each source is a pure function of some view of app state (metadata store,
//! lineage, trace). Sources do not filter — ranking happens in `PaletteState`.

use crate::components::palette::PaletteResult;

pub trait PaletteSource {
    fn collect(&self) -> Vec<PaletteResult>;
}

use crate::action::{Action, ViewId};
use crate::commands::registry::CommandRegistry;
use crate::components::palette::{PaletteKind, PalettePayload};

pub struct ViewSource;

impl PaletteSource for ViewSource {
    fn collect(&self) -> Vec<PaletteResult> {
        vec![
            PaletteResult {
                kind: PaletteKind::View,
                label: "Dashboard".into(),
                subtitle: "view · switch to dashboard".into(),
                payload: PalettePayload::View {
                    action: Action::NavigateTo(ViewId::Dashboard),
                },
            },
            PaletteResult {
                kind: PaletteKind::View,
                label: "Issues".into(),
                subtitle: "view · open issue browser".into(),
                payload: PalettePayload::View {
                    action: Action::NavigateTo(ViewId::IssueBrowser),
                },
            },
            PaletteResult {
                kind: PaletteKind::View,
                label: "Sessions".into(),
                subtitle: "view · open session picker".into(),
                payload: PalettePayload::View {
                    action: Action::RequestSessions,
                },
            },
            PaletteResult {
                kind: PaletteKind::View,
                label: "Insights".into(),
                subtitle: "view · open insights".into(),
                payload: PalettePayload::View {
                    action: Action::OpenInsights,
                },
            },
        ]
    }
}

pub struct CommandSource<'a> {
    registry: &'a CommandRegistry,
}

impl<'a> CommandSource<'a> {
    pub fn new(registry: &'a CommandRegistry) -> Self {
        Self { registry }
    }
}

impl<'a> PaletteSource for CommandSource<'a> {
    fn collect(&self) -> Vec<PaletteResult> {
        self.registry
            .list()
            .iter()
            .map(|e| PaletteResult {
                kind: PaletteKind::Command,
                label: e.name.clone(),
                subtitle: format!("cmd · {}", e.description),
                payload: PalettePayload::Command {
                    name: e.name.clone(),
                },
            })
            .collect()
    }
}

use crate::session_metadata::SessionMetadata;

pub struct SessionSource {
    /// Snapshot taken at palette-open time, pre-sorted by recency
    /// (`last_opened_at` descending). Owned to avoid lifetime gymnastics.
    ///
    /// Entries with an empty `last_opened_at` (the `SessionEntry::default()`
    /// value, e.g. sessions persisted before the field was introduced) sort
    /// to the END of the list — empty string lex-sorts before any ISO-8601
    /// timestamp, so under descending sort they end up last.
    ///
    /// Tie-break for entries with identical timestamps is `BTreeMap` key
    /// order (lexicographic on `session_id`), inherited from
    /// `SessionMetadata::sessions` iteration order plus stable sort.
    entries: Vec<(String, String)>, // (session_id, display_label)
}

impl SessionSource {
    pub fn from_metadata(meta: &SessionMetadata) -> Self {
        // Capture (session_id, label, last_opened_at) so we can sort.
        let mut ranked: Vec<(String, String, String)> = meta
            .sessions
            .iter()
            .map(|(id, entry)| {
                let label = entry.title_override.clone().unwrap_or_else(|| id.clone());
                (id.clone(), label, entry.last_opened_at.clone())
            })
            .collect();
        // ISO-8601 timestamps sort correctly via lexicographic order.
        // Descending: newest first.
        ranked.sort_by(|a, b| b.2.cmp(&a.2));
        let entries = ranked
            .into_iter()
            .map(|(id, label, _ts)| (id, label))
            .collect();
        Self { entries }
    }
}

impl PaletteSource for SessionSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(id, label)| PaletteResult {
                kind: PaletteKind::Session,
                label: label.clone(),
                subtitle: format!("session · {}", id),
                payload: PalettePayload::Session {
                    session_id: id.clone(),
                },
            })
            .collect()
    }
}

use crate::components::react_trace::ReactTrace;

pub struct TraceSource {
    /// Snapshot: (entry_idx, preview_text). Preview is truncated to 80 chars.
    entries: Vec<(usize, String)>,
}

impl TraceSource {
    pub fn from_trace(trace: &ReactTrace) -> Self {
        let entries = trace
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                if !has_user_visible_text(&entry.kind) {
                    return None;
                }
                let preview = truncate_trace(entry.text.clone(), 80);
                Some((idx, preview))
            })
            .collect();
        Self { entries }
    }

    /// Empty-trace constructor for smoke tests without a full view.
    pub fn from_empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl PaletteSource for TraceSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(idx, preview)| PaletteResult {
                kind: PaletteKind::Trace,
                label: preview.clone(),
                subtitle: format!("trace · entry #{}", idx),
                payload: PalettePayload::Trace { entry_idx: *idx },
            })
            .collect()
    }
}

/// Returns true for TraceKind variants that carry user-readable text.
/// `Act`, `Delegate`, and `Permission` are skipped.
fn has_user_visible_text(kind: &crate::components::react_trace::TraceKind) -> bool {
    use crate::components::react_trace::TraceKind;
    matches!(
        kind,
        TraceKind::Think
            | TraceKind::AgentMessage { .. }
            | TraceKind::Observe { .. }
            | TraceKind::UserMessage
    )
}

fn truncate_trace(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        let taken: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", taken)
    }
}

use spur_core::lineage::projection::ExecutorLineage;

pub struct WorkerSource {
    entries: Vec<(spur_acp::SessionId, String, String)>, // (session_id, agent, phase_label)
}

impl WorkerSource {
    pub fn from_lineage(lineage: &ExecutorLineage) -> Self {
        let entries = lineage
            .nodes()
            .filter_map(|n| {
                let sid = n.current_attempt().map(|a| a.session_id.clone())?;
                Some((
                    sid,
                    n.agent.clone(),
                    format!("{:?}", n.phase).to_lowercase(),
                ))
            })
            .collect();
        Self { entries }
    }
}

impl PaletteSource for WorkerSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(sid, agent, phase)| PaletteResult {
                kind: PaletteKind::Worker,
                label: agent.clone(),
                subtitle: format!("worker · {}", phase),
                payload: PalettePayload::Worker {
                    session_id: sid.clone(),
                },
            })
            .collect()
    }
}
