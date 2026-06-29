use anyhow::Result;
use serde_json::json;
use spur_context_service::medallion::{
    BronzeIdentity, GoldIdentity, SilverIdentity, SilverManifest, SilverManifestFile,
    BRONZE_PREFIX, GOLD_CATALOG_SNAPSHOT_PREFIX, GOLD_DATA_PREFIX, GOLD_PREFIX, SILVER_PREFIX,
};

#[test]
fn identity_tuples_serialize_with_medallion_lineage_field_names() -> Result<()> {
    let bronze = BronzeIdentity {
        source: "registry:crates-io".to_owned(),
        package: "serde".to_owned(),
        version: "1.0.228".to_owned(),
        content_sha256: "bronze-sha".to_owned(),
    };
    let silver = SilverIdentity {
        bronze_content_sha256: bronze.content_sha256.clone(),
        builder_version: "builder-v1".to_owned(),
        graph_content_hash: "graph-hash".to_owned(),
    };
    let gold = GoldIdentity {
        silver_graph_content_hash: silver.graph_content_hash.clone(),
        translate_schema_version: "translate-v1".to_owned(),
        embed_text_version: "embed-v4".to_owned(),
    };

    assert_eq!(
        serde_json::to_value(&bronze)?,
        json!({
            "source": "registry:crates-io",
            "package": "serde",
            "version": "1.0.228",
            "content_sha256": "bronze-sha"
        })
    );
    assert_eq!(
        serde_json::to_value(&silver)?,
        json!({
            "bronze_content_sha256": "bronze-sha",
            "builder_version": "builder-v1",
            "graph_content_hash": "graph-hash"
        })
    );
    assert_eq!(
        serde_json::to_value(&gold)?,
        json!({
            "silver_graph_content_hash": "graph-hash",
            "translate_schema_version": "translate-v1",
            "embed_text_version": "embed-v4"
        })
    );

    assert_eq!(
        serde_json::from_value::<BronzeIdentity>(serde_json::to_value(&bronze)?)?,
        bronze
    );
    assert_eq!(
        serde_json::from_value::<SilverIdentity>(serde_json::to_value(&silver)?)?,
        silver
    );
    assert_eq!(
        serde_json::from_value::<GoldIdentity>(serde_json::to_value(&gold)?)?,
        gold
    );
    Ok(())
}

#[test]
fn s3_prefix_constants_match_medallion_layout() {
    assert_eq!(BRONZE_PREFIX, "bronze");
    assert_eq!(SILVER_PREFIX, "silver");
    assert_eq!(GOLD_PREFIX, "gold");
    assert_eq!(GOLD_DATA_PREFIX, "gold/data");
    assert_eq!(GOLD_CATALOG_SNAPSHOT_PREFIX, "gold/catalog-snapshot");
}

#[test]
fn silver_manifest_round_trips_file_sizes_etags_and_schema_hash() -> Result<()> {
    let manifest = SilverManifest {
        schema_hash: "schema-sha256".to_owned(),
        files: vec![
            SilverManifestFile {
                path: "nodes.parquet".to_owned(),
                size_bytes: 1_024,
                etag: "\"nodes-etag\"".to_owned(),
            },
            SilverManifestFile {
                path: "code_symbols.lance/part.parquet".to_owned(),
                size_bytes: 2_048,
                etag: "\"symbols-etag\"".to_owned(),
            },
        ],
    };

    let encoded = serde_json::to_string(&manifest)?;
    assert_eq!(serde_json::from_str::<SilverManifest>(&encoded)?, manifest);
    assert_eq!(
        serde_json::to_value(&manifest)?,
        json!({
            "schema_hash": "schema-sha256",
            "files": [
                {
                    "path": "nodes.parquet",
                    "size_bytes": 1024,
                    "etag": "\"nodes-etag\""
                },
                {
                    "path": "code_symbols.lance/part.parquet",
                    "size_bytes": 2048,
                    "etag": "\"symbols-etag\""
                }
            ]
        })
    );
    Ok(())
}
