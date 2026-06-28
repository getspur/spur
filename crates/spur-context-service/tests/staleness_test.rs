use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use spur_context_service::staleness::{medallion_staleness_report, StalenessVersions};

#[test]
fn staleness_report_flags_silver_and_gold_version_drift() -> Result<()> {
    let conn = Connection::open_in_memory().context("open duckdb")?;
    conn.execute_batch(
        r#"
        CREATE SCHEMA silver;
        CREATE SCHEMA gold;

        CREATE TABLE silver.graph_artifacts (
            source VARCHAR,
            package VARCHAR,
            version VARCHAR,
            builder_version VARCHAR,
            build_status VARCHAR
        );

        CREATE TABLE gold.package_catalog (
            source VARCHAR,
            package VARCHAR,
            revision VARCHAR,
            index_status VARCHAR,
            translate_schema_version VARCHAR,
            embed_text_version VARCHAR
        );
        "#,
    )
    .context("create test registries")?;
    conn.execute(
        "INSERT INTO silver.graph_artifacts VALUES (?, ?, ?, ?, 'success')",
        params!["registry:crates-io", "demo", "0.1.0", "builder-v1"],
    )?;
    conn.execute(
        "INSERT INTO silver.graph_artifacts VALUES (?, ?, ?, ?, 'success')",
        params!["registry:crates-io", "current", "0.1.0", "builder-v2"],
    )?;
    conn.execute(
        "INSERT INTO gold.package_catalog VALUES (?, ?, ?, 'complete', ?, ?)",
        params![
            "registry:crates-io",
            "demo",
            "0.1.0",
            "translate-v0",
            "embed-v3"
        ],
    )?;
    conn.execute(
        "INSERT INTO gold.package_catalog VALUES (?, ?, ?, 'complete', ?, ?)",
        params![
            "registry:crates-io",
            "current",
            "0.1.0",
            "translate-v1",
            "embed-v4"
        ],
    )?;

    let report = medallion_staleness_report(
        &conn,
        &StalenessVersions {
            builder_version: "builder-v2".to_owned(),
            translate_schema_version: "translate-v1".to_owned(),
            embed_text_version: "embed-v4".to_owned(),
        },
    )?;

    let rows: Vec<_> = report
        .iter()
        .map(|row| {
            (
                row.layer.as_str(),
                row.field.as_str(),
                row.package.as_str(),
                row.observed_version.as_deref(),
                row.current_version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "gold",
                "embed_text_version",
                "demo",
                Some("embed-v3"),
                "embed-v4"
            ),
            (
                "gold",
                "translate_schema_version",
                "demo",
                Some("translate-v0"),
                "translate-v1"
            ),
            (
                "silver",
                "builder_version",
                "demo",
                Some("builder-v1"),
                "builder-v2"
            ),
        ]
    );
    Ok(())
}
