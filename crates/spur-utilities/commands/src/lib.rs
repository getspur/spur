//! Neutral slash-command model, registry, capability synthesis, and pure
//! submit helpers (spur-utilities TUI decoupling spec §3).
//!
//! `spur-acp` is this crate's domain. Frontends (spur-tui now, the notebook
//! later) own their own meta-command tables and inject them as a
//! `LocalLayer`.
