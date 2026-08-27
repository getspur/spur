//! Arrow/Parquet source-text sidecar construction.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Array as _, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::{ArrowWriter, ProjectionMask};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) const SOURCE_SIDECAR_FILENAME: &str = "source_files.parquet";

#[derive(Debug, Error)]
pub(crate) enum SourceSidecarError {
    #[error("{0}")]
    Build(String),
}

#[derive(Debug)]
struct SourceSidecarRow {
    file_path: String,
    content_oid: String,
    source_text: String,
}

pub(crate) fn write_source_sidecar(
    artifact_dir: &Path,
    source_root: &Path,
) -> Result<String, SourceSidecarError> {
    let rows = source_sidecar_rows(artifact_dir, source_root)?;
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_path", DataType::Utf8, false),
        Field::new("content_oid", DataType::Utf8, false),
        Field::new("source_text", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.file_path.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.content_oid.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.source_text.as_str()),
            )),
        ],
    )
    .map_err(|error| build_error(format!("build source sidecar batch: {error}")))?;

    let final_path = artifact_dir.join(SOURCE_SIDECAR_FILENAME);
    let temp_path = artifact_dir.join(format!("{SOURCE_SIDECAR_FILENAME}.tmp"));
    let file = fs::File::create(&temp_path).map_err(|error| {
        build_error(format!(
            "create source sidecar temp `{}`: {error}",
            temp_path.display()
        ))
    })?;
    let mut writer = ArrowWriter::try_new(file, schema, None)
        .map_err(|error| build_error(format!("create source sidecar writer: {error}")))?;
    writer
        .write(&batch)
        .map_err(|error| build_error(format!("write source sidecar: {error}")))?;
    writer
        .close()
        .map_err(|error| build_error(format!("close source sidecar: {error}")))?;
    fs::File::open(&temp_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            build_error(format!(
                "fsync source sidecar temp `{}`: {error}",
                temp_path.display()
            ))
        })?;
    let sha256 = sha256_file(&temp_path)?;
    fs::rename(&temp_path, &final_path).map_err(|error| {
        build_error(format!(
            "rename source sidecar `{}` to `{}`: {error}",
            temp_path.display(),
            final_path.display()
        ))
    })?;
    fs::File::open(artifact_dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| {
            build_error(format!(
                "fsync silver artifact directory `{}`: {error}",
                artifact_dir.display()
            ))
        })?;
    Ok(sha256)
}

fn source_sidecar_rows(
    artifact_dir: &Path,
    source_root: &Path,
) -> Result<Vec<SourceSidecarRow>, SourceSidecarError> {
    let manifest_path = artifact_dir.join("file_manifests.parquet");
    let file = fs::File::open(&manifest_path).map_err(|error| {
        build_error(format!(
            "open graph file manifest `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|error| {
        build_error(format!(
            "read graph file manifest metadata `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let path_index = builder
        .schema()
        .index_of("path")
        .map_err(|error| build_error(format!("graph file manifest missing path: {error}")))?;
    let content_oid_index = builder.schema().index_of("content_oid").map_err(|error| {
        build_error(format!("graph file manifest missing content_oid: {error}"))
    })?;
    let projection =
        ProjectionMask::roots(builder.parquet_schema(), [path_index, content_oid_index]);
    let reader = builder
        .with_projection(projection)
        .build()
        .map_err(|error| {
            build_error(format!(
                "open graph file manifest reader `{}`: {error}",
                manifest_path.display()
            ))
        })?;

    let mut rows = Vec::new();
    let mut identities = BTreeSet::new();
    for batch in reader {
        let batch = batch.map_err(|error| {
            build_error(format!(
                "read graph file manifest `{}`: {error}",
                manifest_path.display()
            ))
        })?;
        let file_path = required_source_manifest_column(&batch, "path")?;
        let content_oid = required_source_manifest_column(&batch, "content_oid")?;
        for row in 0..batch.num_rows() {
            if file_path.is_null(row) || content_oid.is_null(row) {
                return Err(build_error(
                    "graph file manifest path and content_oid must be present",
                ));
            }
            let file_path = file_path.value(row).to_owned();
            let content_oid = content_oid.value(row).to_owned();
            validate_source_manifest_path(&file_path)?;
            if file_path.is_empty() || content_oid.is_empty() {
                return Err(build_error(
                    "graph file manifest path and content_oid must be non-empty",
                ));
            }
            if !identities.insert((file_path.clone(), content_oid.clone())) {
                return Err(build_error(format!(
                    "duplicate graph file manifest identity `({file_path}, {content_oid})`"
                )));
            }
            let source_path = source_root.join(path_from_manifest_relative(&file_path));
            let source_text = fs::read_to_string(&source_path).map_err(|error| {
                build_error(format!(
                    "read referenced source `{}` as UTF-8: {error}",
                    source_path.display()
                ))
            })?;
            rows.push(SourceSidecarRow {
                file_path,
                content_oid,
                source_text,
            });
        }
    }
    rows.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then(left.content_oid.cmp(&right.content_oid))
    });
    Ok(rows)
}

fn required_source_manifest_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, SourceSidecarError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| build_error(format!("graph file manifest missing {name}")))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| build_error(format!("graph file manifest {name} must be a UTF-8 column")))
}

fn validate_source_manifest_path(path: &str) -> Result<(), SourceSidecarError> {
    if path.trim().is_empty() || path.contains('\\') {
        return Err(build_error(format!(
            "invalid graph file manifest path `{path}`"
        )));
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(build_error(format!(
            "graph file manifest path must be relative: `{path}`"
        )));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(build_error(format!(
                    "graph file manifest path escapes source root: `{path}`"
                )));
            }
        }
    }
    Ok(())
}

fn path_from_manifest_relative(relative_path: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for part in relative_path.split('/') {
        path.push(part);
    }
    path
}

fn sha256_file(path: &Path) -> Result<String, SourceSidecarError> {
    let mut file = fs::File::open(path).map_err(|error| {
        build_error(format!(
            "failed to open source sidecar `{}`: {error}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            build_error(format!(
                "failed to read source sidecar `{}`: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn build_error(message: impl Into<String>) -> SourceSidecarError {
    SourceSidecarError::Build(message.into())
}
