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
