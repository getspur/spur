//! Medallion version-staleness reporting.

use anyhow::{Context as _, Result};
use duckdb::{params, Connection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessVersions {
    pub builder_version: String,
    pub translate_schema_version: String,
    pub embed_text_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessReportRow {
    pub source: String,
    pub package: String,
    pub version: String,
    pub layer: String,
    pub field: String,
    pub observed_version: Option<String>,
    pub current_version: String,
}

pub fn medallion_staleness_sql() -> &'static str {
    r#"
    SELECT
        source,
        package,
        version,
        'silver' AS layer,
        'builder_version' AS field,
        builder_version AS observed_version,
        ? AS current_version
    FROM silver.graph_artifacts
    WHERE COALESCE(build_status, 'success') = 'success'
      AND builder_version IS DISTINCT FROM ?

    UNION ALL

    SELECT
        source,
        package,
        revision AS version,
        'gold' AS layer,
        'translate_schema_version' AS field,
        translate_schema_version AS observed_version,
        ? AS current_version
    FROM gold.package_catalog
    WHERE COALESCE(index_status, 'complete') = 'complete'
      AND translate_schema_version IS DISTINCT FROM ?

    UNION ALL

    SELECT
        source,
        package,
        revision AS version,
        'gold' AS layer,
        'embed_text_version' AS field,
        embed_text_version AS observed_version,
        ? AS current_version
    FROM gold.package_catalog
    WHERE COALESCE(index_status, 'complete') = 'complete'
      AND embed_text_version IS DISTINCT FROM ?

    ORDER BY layer, field, source, package, version
    "#
}

pub fn medallion_staleness_report(
    conn: &Connection,
    versions: &StalenessVersions,
) -> Result<Vec<StalenessReportRow>> {
    let mut stmt = conn
        .prepare(medallion_staleness_sql())
        .context("prepare medallion staleness report")?;
    let rows = stmt
        .query_map(
            params![
                versions.builder_version.as_str(),
                versions.builder_version.as_str(),
                versions.translate_schema_version.as_str(),
                versions.translate_schema_version.as_str(),
                versions.embed_text_version.as_str(),
                versions.embed_text_version.as_str(),
            ],
            |row| {
                Ok(StalenessReportRow {
                    source: row.get(0)?,
                    package: row.get(1)?,
                    version: row.get(2)?,
                    layer: row.get(3)?,
                    field: row.get(4)?,
                    observed_version: row.get(5)?,
                    current_version: row.get(6)?,
                })
            },
        )
        .context("query medallion staleness report")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("collect medallion staleness report")
}
