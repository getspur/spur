use std::{
    collections::BTreeMap,
    fs,
    io::{self, Cursor},
    path::{Path, PathBuf},
};

use arrow_array::RecordBatch;
use arrow_ipc::{reader::FileReader, writer::FileWriter};
use arrow_schema::{ArrowError, Schema, SchemaRef};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Error)]
pub enum PortStoreError {
    #[error("home directory is unavailable")]
    HomeUnavailable,
    #[error("port name cannot be empty")]
    EmptyPortName,
    #[error("port name '{port}' is not valid for an on-disk port file")]
    InvalidPortName { port: String },
    #[error("port '{0}' has not been written")]
    MissingPort(String),
    #[error("I/O error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("manifest JSON error at {}: {source}", path.display())]
    ManifestJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Arrow IPC error: {0}")]
    Arrow(#[from] ArrowError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortEntry {
    pub path: PathBuf,
    pub version: u64,
    pub schema: Schema,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortManifest {
    pub ports: BTreeMap<String, PortEntry>,
}

#[derive(Debug)]
pub struct PortRead {
    pub path: PathBuf,
    pub version: u64,
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
    pub ipc_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub enum PortPayload<'a> {
    RecordBatch(&'a RecordBatch),
    IpcBytes(&'a [u8]),
}

impl<'a> From<&'a RecordBatch> for PortPayload<'a> {
    fn from(batch: &'a RecordBatch) -> Self {
        Self::RecordBatch(batch)
    }
}

impl<'a> From<&'a [u8]> for PortPayload<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        Self::IpcBytes(bytes)
    }
}

impl<'a> From<&'a Vec<u8>> for PortPayload<'a> {
    fn from(bytes: &'a Vec<u8>) -> Self {
        Self::IpcBytes(bytes)
    }
}

#[derive(Debug, Clone)]
pub struct PortStore {
    notebook_root: PathBuf,
    ports_dir: PathBuf,
    manifest_path: PathBuf,
    manifest: PortManifest,
}

impl PortStore {
    pub fn open(notebook_id: &str) -> Result<Self, PortStoreError> {
        validate_path_segment(notebook_id)?;
        let home = BaseDirs::new()
            .ok_or(PortStoreError::HomeUnavailable)?
            .home_dir()
            .to_path_buf();
        Self::open_at(home.join(".spur/notebooks").join(notebook_id))
    }

    pub fn open_at(notebook_root: impl AsRef<Path>) -> Result<Self, PortStoreError> {
        let notebook_root = notebook_root.as_ref().to_path_buf();
        let ports_dir = notebook_root.join("ports");
        fs::create_dir_all(&ports_dir).map_err(|source| io_error(&ports_dir, source))?;

        let manifest_path = ports_dir.join(MANIFEST_FILE);
        let manifest = Self::load_manifest(&manifest_path)?;

        Ok(Self {
            notebook_root,
            ports_dir,
            manifest_path,
            manifest,
        })
    }

    pub fn open_read_only_at(notebook_root: impl AsRef<Path>) -> Result<Self, PortStoreError> {
        let notebook_root = notebook_root.as_ref().to_path_buf();
        let ports_dir = notebook_root.join("ports");
        let manifest_path = ports_dir.join(MANIFEST_FILE);
        let manifest = Self::load_manifest(&manifest_path)?;

        Ok(Self {
            notebook_root,
            ports_dir,
            manifest_path,
            manifest,
        })
    }

    pub fn notebook_root(&self) -> &Path {
        &self.notebook_root
    }

    pub fn manifest(&self) -> &BTreeMap<String, PortEntry> {
        &self.manifest.ports
    }

    pub fn put<'a>(
        &mut self,
        port: &str,
        payload: impl Into<PortPayload<'a>>,
    ) -> Result<PortEntry, PortStoreError> {
        validate_path_segment(port)?;

        let payload = payload.into();
        let ipc_bytes = match payload {
            PortPayload::RecordBatch(batch) => ipc_bytes_for_batch(batch)?,
            PortPayload::IpcBytes(bytes) => bytes.to_vec(),
        };
        let (schema, _) = read_ipc(&ipc_bytes)?;

        let version = self
            .manifest
            .ports
            .get(port)
            .map_or(1, |entry| entry.version + 1);
        let path = self.ports_dir.join(format!("{port}@v{version}.arrow"));

        fs::write(&path, &ipc_bytes).map_err(|source| io_error(&path, source))?;

        let entry = PortEntry {
            path,
            version,
            schema: schema.as_ref().clone(),
        };
        let mut next_manifest = self.manifest.clone();
        next_manifest.ports.insert(port.to_owned(), entry.clone());
        self.persist_manifest(&next_manifest)?;
        self.manifest = next_manifest;

        Ok(entry)
    }

    pub fn get(&self, port: &str) -> Result<PortRead, PortStoreError> {
        let entry = self
            .manifest
            .ports
            .get(port)
            .ok_or_else(|| PortStoreError::MissingPort(port.to_owned()))?;
        let ipc_bytes = fs::read(&entry.path).map_err(|source| io_error(&entry.path, source))?;
        let (schema, batches) = read_ipc(&ipc_bytes)?;

        Ok(PortRead {
            path: entry.path.clone(),
            version: entry.version,
            schema,
            batches,
            ipc_bytes,
        })
    }

    fn load_manifest(path: &Path) -> Result<PortManifest, PortStoreError> {
        match fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| PortStoreError::ManifestJson {
                    path: path.to_path_buf(),
                    source,
                })
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(PortManifest::default()),
            Err(source) => Err(io_error(path, source)),
        }
    }

    fn persist_manifest(&self, manifest: &PortManifest) -> Result<(), PortStoreError> {
        let bytes =
            serde_json::to_vec_pretty(manifest).map_err(|source| PortStoreError::ManifestJson {
                path: self.manifest_path.clone(),
                source,
            })?;
        let tmp_path = self
            .ports_dir
            .join(format!("{MANIFEST_FILE}.{}.tmp", std::process::id()));
        fs::write(&tmp_path, bytes).map_err(|source| io_error(&tmp_path, source))?;
        fs::rename(&tmp_path, &self.manifest_path)
            .map_err(|source| io_error(&self.manifest_path, source))
    }
}

fn ipc_bytes_for_batch(batch: &RecordBatch) -> Result<Vec<u8>, PortStoreError> {
    let mut bytes = Vec::new();
    {
        let mut writer = FileWriter::try_new(&mut bytes, batch.schema().as_ref())?;
        writer.write(batch)?;
        writer.finish()?;
    }
    Ok(bytes)
}

fn read_ipc(bytes: &[u8]) -> Result<(SchemaRef, Vec<RecordBatch>), PortStoreError> {
    let cursor = Cursor::new(bytes);
    let mut reader = FileReader::try_new(cursor, None)?;
    let schema = reader.schema();
    let batches = reader.by_ref().collect::<Result<Vec<_>, ArrowError>>()?;
    Ok((schema, batches))
}

fn validate_path_segment(value: &str) -> Result<(), PortStoreError> {
    if value.is_empty() {
        return Err(PortStoreError::EmptyPortName);
    }

    if value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(PortStoreError::InvalidPortName {
            port: value.to_owned(),
        });
    }

    Ok(())
}

fn io_error(path: &Path, source: io::Error) -> PortStoreError {
    PortStoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use arrow_array::{Array as _, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    fn batch(ids: Vec<i64>, labels: Vec<&str>) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("label", DataType::Utf8, false),
        ]));

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(labels)),
            ],
        )
        .expect("test batch is valid")
    }

    fn assert_batch_eq(expected: &RecordBatch, actual: &RecordBatch) {
        assert_eq!(expected.schema().as_ref(), actual.schema().as_ref());
        assert_eq!(expected.num_rows(), actual.num_rows());

        let expected_ids = expected
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("expected id column");
        let actual_ids = actual
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("actual id column");
        assert_eq!(expected_ids.values(), actual_ids.values());

        let expected_labels = expected
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("expected label column");
        let actual_labels = actual
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("actual label column");
        assert_eq!(expected_labels.len(), actual_labels.len());
        for row in 0..expected_labels.len() {
            assert_eq!(expected_labels.value(row), actual_labels.value(row));
        }
    }

    #[test]
    fn put_get_round_trip_preserves_arrow_data_and_bumps_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = PortStore::open_at(dir.path()).expect("open store");

        let first = batch(vec![1, 2], vec!["alpha", "beta"]);
        let first_entry = store.put("sales", &first).expect("put first batch");
        assert_eq!(1, first_entry.version);
        assert_eq!(dir.path().join("ports/sales@v1.arrow"), first_entry.path);

        let first_read = store.get("sales").expect("get first batch");
        assert_eq!(1, first_read.version);
        assert_eq!(first.schema().as_ref(), first_read.schema.as_ref());
        assert_eq!(1, first_read.batches.len());
        assert_batch_eq(&first, &first_read.batches[0]);

        let second = batch(vec![3, 4, 5], vec!["gamma", "delta", "epsilon"]);
        let second_entry = store.put("sales", &second).expect("put second batch");
        assert_eq!(2, second_entry.version);
        assert_eq!(dir.path().join("ports/sales@v2.arrow"), second_entry.path);

        let reloaded = PortStore::open_at(dir.path()).expect("reload persisted manifest");
        let manifest_entry = reloaded.manifest().get("sales").expect("manifest entry");
        assert_eq!(2, manifest_entry.version);
        assert_eq!(dir.path().join("ports/sales@v2.arrow"), manifest_entry.path);
        assert_eq!(second.schema().as_ref(), &manifest_entry.schema);

        let second_read = reloaded.get("sales").expect("get latest batch");
        assert_eq!(2, second_read.version);
        assert_eq!(second.schema().as_ref(), second_read.schema.as_ref());
        assert_eq!(1, second_read.batches.len());
        assert_batch_eq(&second, &second_read.batches[0]);
    }
}
