use anyhow::{Context as _, Result};
use std::sync::OnceLock;

static LANCE_INSTALLED: OnceLock<()> = OnceLock::new();
// duckpgq is distributed via DuckDB's community repo (not the core repo) and is
// only published for DuckDB <= 1.4.4. Install it once per process; the LOAD is
// best-effort in general query paths and fallible in graph-path paths.
static DUCKPGQ_INSTALLED: OnceLock<()> = OnceLock::new();

pub(crate) fn load_analyst_icu_extension(conn: &duckdb::Connection) {
    // The analyst scorecard can depend on TIMESTAMPTZ arithmetic whose overloads
    // live in DuckDB's ICU extension. Keep this best-effort and let query
    // preparation surface genuine failures in the existing per-stage shape.
    let _ = conn.execute_batch("INSTALL icu; LOAD icu;");
}

pub(crate) fn load_analyst_lance_extension(conn: &duckdb::Connection) {
    // Hybrid retrieval uses DuckDB's Lance extension when available. Keep this
    // best-effort so missing extension binaries degrade to BM25-only search.
    LANCE_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL lance;");
    });
    let _ = conn.execute_batch("LOAD lance;");
}

pub(crate) fn load_analyst_duckpgq_extension(conn: &duckdb::Connection) -> Result<()> {
    DUCKPGQ_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL duckpgq FROM community;");
    });
    conn.execute_batch("LOAD duckpgq;")
        .context("failed to load DuckPGQ extension")?;
    Ok(())
}
