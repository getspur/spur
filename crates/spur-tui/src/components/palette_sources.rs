//! Palette data sources.
//!
//! Each source is a pure function of some view of app state (metadata store,
//! lineage, trace). Sources do not filter — ranking happens in `PaletteState`.

use crate::components::palette::PaletteResult;

pub trait PaletteSource {
    fn collect(&self) -> Vec<PaletteResult>;
}
