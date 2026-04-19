//! Shared contract for popup-backed retrieval sources.
//!
//! Each source produces `RetrievalRow`s from a query string and, on accept,
//! returns a `RetrievalAccept` payload that the view dispatches onto the
//! `InputBar`.

use crate::input_history::InputStateSnapshot;

/// Where the popup's query string lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// `PickerShell` owns a `MiniInput` and routes keys into it. Used by
    /// history search (Ctrl+R) where the query is scratch navigation text.
    OwnedByShell,
    /// The shell reads its query from the `InputBar` trigger prefix. Used
    /// by @mention and /slash where the query is part of the outbound draft.
    /// (Phase 1 does NOT construct any source in this mode; reserved for
    /// Phase 3.)
    #[allow(dead_code)]
    ReadFromInputBar,
}

/// One displayable row in a retrieval popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalRow {
    /// Main label text.
    pub primary: String,
    /// Description / metadata shown to the right of `primary`.
    pub secondary: String,
    /// Right-aligned provenance tag, e.g. `⟨claude⟩`. Empty for no tag.
    pub tag: String,
    /// Byte ranges inside `primary` to be rendered as protected-atom spans
    /// (LightBlue + underlined). Ranges MUST be valid inside `primary`;
    /// implementors are responsible for validating against any truncation
    /// they applied before returning.
    pub atoms: Vec<(usize, usize)>,
}

/// Payload dispatched by the view when the user accepts a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalAccept {
    /// Replace the entire `InputBar` state with this snapshot. Used by history.
    ReplaceState(InputStateSnapshot),
    /// Insert a protected atom at the `InputBar` cursor. Used by @mention.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    InsertAtom {
        text: String,
        uri: String,
        name: String,
    },
    /// Replace the text between `prefix_start` and the cursor with
    /// `replacement`. Used by /slash.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    ReplaceTriggerToken {
        prefix_start: usize,
        replacement: String,
    },
}

/// A retrieval source: given a query, produces ranked rows; on accept,
/// returns a dispatchable payload.
pub trait QuerySource {
    /// Title shown in the shell header (e.g. "History · bck-i-search").
    fn title(&self) -> &str;

    /// Where the query lives.
    fn query_mode(&self) -> QueryMode;

    /// Filter+rank using the given query. Implementors MUST reuse any
    /// internal matcher state across calls; constructing a fresh
    /// `nucleo::Matcher` per call is forbidden for hot-path reasons
    /// (see Phase 2 plan).
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow>;

    /// Build the accept payload for the row at `row_idx`. Returns `None`
    /// if `row_idx` is out of bounds or the source has no state to accept.
    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_row_atoms_are_byte_ranges() {
        // Smoke test: atoms use byte offsets, so multi-byte primary stays
        // consistent. "你好@foo" — atom range for @foo is bytes 6..10.
        let r = RetrievalRow {
            primary: "你好@foo".to_string(),
            secondary: String::new(),
            tag: String::new(),
            atoms: vec![(6, 10)],
        };
        assert_eq!(&r.primary[r.atoms[0].0..r.atoms[0].1], "@foo");
    }

    #[test]
    fn replace_state_roundtrip() {
        let snap = InputStateSnapshot::from_text("hello");
        let a = RetrievalAccept::ReplaceState(snap.clone());
        match a {
            RetrievalAccept::ReplaceState(got) => assert_eq!(got.text, "hello"),
            _ => panic!("wrong variant"),
        }
    }
}
