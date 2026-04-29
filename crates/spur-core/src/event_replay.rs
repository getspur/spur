//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

#![allow(dead_code)] // Filled in incrementally across the bd-1vnk task series.
