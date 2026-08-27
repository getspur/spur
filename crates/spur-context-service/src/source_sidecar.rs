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
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub(crate) const SOURCE_SIDECAR_FILENAME: &str = "source_files.parquet";

#[derive(Debug, Error)]
pub(crate) enum SourceSidecarError {
    #[error("{0}")]
    Build(String),
}

#[derive(Debug, Error)]
#[allow(
    dead_code,
    reason = "the reader is used through code_backend's shared module view"
)]
pub(crate) enum SourceSidecarReadError {
    #[error("source sidecar integrity mismatch")]
    IntegrityMismatch,
    #[error("source sidecar is corrupt")]
    Corrupt,
    #[error("source text is unavailable")]
    TextUnavailable,
    #[error("source content OID mismatch")]
    ContentOidMismatch,
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

#[allow(
    dead_code,
    reason = "the reader is used through code_backend's shared module view"
)]
pub(crate) fn read_verified_source(
    sidecar_path: &Path,
    expected_sha256: &str,
    expected_bytes: u64,
    file_path: &str,
    content_oid: &str,
) -> Result<String, SourceSidecarReadError> {
    let metadata =
        fs::metadata(sidecar_path).map_err(|_| SourceSidecarReadError::IntegrityMismatch)?;
    if metadata.len() != expected_bytes {
        return Err(SourceSidecarReadError::IntegrityMismatch);
    }
    let actual_sha256 =
        sha256_file(sidecar_path).map_err(|_| SourceSidecarReadError::IntegrityMismatch)?;
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(SourceSidecarReadError::IntegrityMismatch);
    }

    let file =
        fs::File::open(sidecar_path).map_err(|_| SourceSidecarReadError::IntegrityMismatch)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|_| SourceSidecarReadError::Corrupt)?;
    let file_path_index = builder
        .schema()
        .index_of("file_path")
        .map_err(|_| SourceSidecarReadError::Corrupt)?;
    let content_oid_index = builder
        .schema()
        .index_of("content_oid")
        .map_err(|_| SourceSidecarReadError::Corrupt)?;
    let source_text_index = builder
        .schema()
        .index_of("source_text")
        .map_err(|_| SourceSidecarReadError::Corrupt)?;
    let projection = ProjectionMask::roots(
        builder.parquet_schema(),
        [file_path_index, content_oid_index, source_text_index],
    );
    let reader = builder
        .with_projection(projection)
        .build()
        .map_err(|_| SourceSidecarReadError::Corrupt)?;

    let mut exact_source = None;
    let mut saw_path_with_other_oid = false;
    for batch in reader {
        let batch = batch.map_err(|_| SourceSidecarReadError::Corrupt)?;
        let file_paths = required_read_column(&batch, "file_path")?;
        let content_oids = required_read_column(&batch, "content_oid")?;
        let source_texts = required_read_column(&batch, "source_text")?;
        for row in 0..batch.num_rows() {
            if file_paths.is_null(row) || content_oids.is_null(row) || source_texts.is_null(row) {
                return Err(SourceSidecarReadError::Corrupt);
            }
            let row_path = file_paths.value(row);
            let row_oid = content_oids.value(row);
            let row_source = source_texts.value(row);
            if validate_source_manifest_path(row_path).is_err() || row_oid.is_empty() {
                return Err(SourceSidecarReadError::Corrupt);
            }
            if git_blob_oid(row_source.as_bytes()) != row_oid {
                return Err(SourceSidecarReadError::ContentOidMismatch);
            }
            if row_path != file_path {
                continue;
            }
            if row_oid != content_oid {
                saw_path_with_other_oid = true;
                continue;
            }
            if exact_source.replace(row_source.to_owned()).is_some() {
                return Err(SourceSidecarReadError::Corrupt);
            }
        }
    }

    exact_source.ok_or_else(|| {
        if saw_path_with_other_oid {
            SourceSidecarReadError::ContentOidMismatch
        } else {
            SourceSidecarReadError::TextUnavailable
        }
    })
}

#[allow(
    dead_code,
    reason = "the reader is used through code_backend's shared module view"
)]
fn required_read_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, SourceSidecarReadError> {
    batch
        .column_by_name(name)
        .ok_or(SourceSidecarReadError::Corrupt)?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(SourceSidecarReadError::Corrupt)
}

fn source_sidecar_rows(
    artifact_dir: &Path,
    source_root: &Path,
) -> Result<Vec<SourceSidecarRow>, SourceSidecarError> {
    let canonical_source_root = fs::canonicalize(source_root).map_err(|error| {
        build_error(format!(
            "canonicalize source root `{}`: {error}",
            source_root.display()
        ))
    })?;
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
            let resolved_source_path = fs::canonicalize(&source_path).map_err(|error| {
                build_error(format!(
                    "read referenced source `{}`: resolve path: {error}",
                    source_path.display()
                ))
            })?;
            if !resolved_source_path.starts_with(&canonical_source_root) {
                return Err(build_error(format!(
                    "referenced source `{file_path}` resolves outside source root `{}`: `{}`",
                    canonical_source_root.display(),
                    resolved_source_path.display()
                )));
            }
            let source_bytes = fs::read(&resolved_source_path).map_err(|error| {
                build_error(format!(
                    "read referenced source `{}`: {error}",
                    resolved_source_path.display()
                ))
            })?;
            let actual_content_oid = git_blob_oid(&source_bytes);
            if actual_content_oid != content_oid {
                return Err(build_error(format!(
                    "graph file manifest content_oid mismatch for `{file_path}`: expected `{content_oid}`, got `{actual_content_oid}`"
                )));
            }
            let source_text = String::from_utf8(source_bytes).map_err(|error| {
                build_error(format!(
                    "read referenced source `{}` as UTF-8: {error}",
                    resolved_source_path.display()
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

fn git_blob_oid(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
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
