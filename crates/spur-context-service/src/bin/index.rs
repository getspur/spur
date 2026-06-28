//! Standalone indexer: builds `DuckLake` catalog from spur-graph artifacts.
//!
//! Usage:
//!   index --package serde --revision 1.0.197 \
//!     --artifact-dir /path/to/.spur/graph \
//!     --source-root /path/to/serde/source \
//!     --catalog index.ducklake \
//!     --upload-s3 <s3://spur-context/catalog/catalog.ducklake>

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Arg, Command};

use spur_context_service::translate::{translate_artifact_to_ducklake, TranslateOptions};

fn main() -> Result<()> {
    let matches = Command::new("spur-context-indexer")
        .about("Index spur-graph artifacts into DuckLake")
        .arg(Arg::new("package").long("package").required(true))
        .arg(Arg::new("revision").long("revision").required(true))
        .arg(Arg::new("artifact-dir").long("artifact-dir").required(true))
        .arg(Arg::new("source-root").long("source-root"))
        .arg(
            Arg::new("catalog")
                .long("catalog")
                .default_value("index.ducklake"),
        )
        .arg(Arg::new("upload-s3").long("upload-s3"))
        .arg(
            Arg::new("source")
                .long("source")
                .default_value("registry:crates-io"),
        )
        .get_matches();

    let package = matches.get_one::<String>("package").unwrap();
    let revision = matches.get_one::<String>("revision").unwrap();
    let artifact_dir = PathBuf::from(matches.get_one::<String>("artifact-dir").unwrap());
    let source_root = matches.get_one::<String>("source-root").map(PathBuf::from);
    let catalog = matches.get_one::<String>("catalog").unwrap();
    let upload_s3 = matches.get_one::<String>("upload-s3").cloned();
    let source = matches.get_one::<String>("source").unwrap();

    let opts = TranslateOptions {
        source: source.clone(),
        package: package.clone(),
        revision: revision.clone(),
        revision_kind: revision_kind_for_revision(revision).to_owned(),
        artifact_dir: artifact_dir.clone(),
        artifact_manifest: None,
        source_root,
        catalog_dsn: catalog.clone(),
    };

    eprintln!("[index] translating {source}/{package}@{revision} ...");
    let stats = translate_artifact_to_ducklake(&opts)
        .with_context(|| format!("translate failed for {package}@{revision}"))?;

    eprintln!("[index] snapshot_id={}", stats.snapshot_id);
    let mut row_counts = stats.rows_inserted.iter().collect::<Vec<_>>();
    row_counts.sort_by_key(|(table, _)| table.as_str());
    for (table, rows) in row_counts {
        if *rows > 0 {
            eprintln!("[index]   {table}: {rows} rows");
        }
    }

    if let Some(s3_uri) = upload_s3 {
        eprintln!("[index] uploading catalog to {s3_uri} ...");
        let local = &catalog;
        let bucket = s3_uri.strip_prefix("s3://").unwrap_or(&s3_uri);
        let status = std::process::Command::new("aws")
            .args(["s3", "cp", local, &format!("s3://{bucket}"), "--region"])
            .arg(std::env::var("AWS_REGION").unwrap_or_else(|_| "ap-southeast-5".to_owned()))
            .status()
            .context("failed to run aws s3 cp")?;
        if !status.success() {
            anyhow::bail!("aws s3 cp failed");
        }
        eprintln!("[index] catalog uploaded to {s3_uri}");
    }

    Ok(())
}

fn revision_kind_for_revision(revision: &str) -> &'static str {
    if revision.contains('.') {
        "semver"
    } else {
        "git_sha"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_kind_for_git_sha_uses_translate_supported_value() {
        assert_eq!(revision_kind_for_revision("1.0.197"), "semver");
        assert_eq!(
            revision_kind_for_revision("b7c851407ac5d0b8eb3ef2bc597be79340f29038"),
            "git_sha"
        );
    }
}
