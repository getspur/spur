//! Palette data sources.
//!
//! Each source is a pure function of some view of app state (metadata store,
//! lineage, trace). Sources do not filter — ranking happens in `PaletteState`.

use crate::components::palette::PaletteResult;

pub trait PaletteSource {
    fn collect(&self) -> Vec<PaletteResult>;
}

use crate::commands::registry::CommandRegistry;
use crate::components::palette::{PaletteKind, PalettePayload};

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
                payload: PalettePayload::Command { name: e.name.clone() },
            })
            .collect()
    }
}

use crate::session_metadata::SessionMetadata;

pub struct SessionSource {
    /// Snapshot taken at palette-open time. Owned to avoid lifetime gymnastics.
    entries: Vec<(String, String)>, // (session_id, display_label)
}

impl SessionSource {
    pub fn from_metadata(meta: &SessionMetadata) -> Self {
        let entries = meta
            .sessions
            .iter()
            .map(|(id, entry)| {
                let label = entry
                    .title_override
                    .clone()
                    .unwrap_or_else(|| id.clone());
                (id.clone(), label)
            })
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
                payload: PalettePayload::Session { session_id: id.clone() },
            })
            .collect()
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
                Some((sid, n.agent.clone(), format!("{:?}", n.phase).to_lowercase()))
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
                payload: PalettePayload::Worker { session_id: sid.clone() },
            })
            .collect()
    }
}
