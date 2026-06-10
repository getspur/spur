use std::{
    collections::BTreeMap,
    fs,
    io::{self, Cursor},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::Arc,
};

use arrow_array::RecordBatch;
use arrow_buffer::Buffer;
use arrow_ipc::{
    convert::fb_to_schema,
    reader::{read_footer_length, FileDecoder, FileReader},
    root_as_footer,
    writer::FileWriter,
};
use arrow_schema::{ArrowError, Schema, SchemaRef};
use directories::BaseDirs;
use memmap2::Mmap;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
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

#[derive(Debug, Clone, PartialEq)]
pub enum PortKind {
    Arrow(Schema),
    Media {
        mime: String,
        size: u64,
        /// Duration in seconds for media ports. Optional; `None` if the producer
        /// did not supply a duration.
        duration_sec: Option<f64>,
    },
}

// Manual Eq impl because f64 doesn't implement Eq.
impl Eq for PortKind {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEntry {
    pub path: PathBuf,
    pub version: u64,
    pub kind: PortKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortManifest {
    pub ports: BTreeMap<String, PortEntry>,
}

#[derive(Debug)]
pub enum PortRead {
    Arrow {
        path: PathBuf,
        version: u64,
        schema: SchemaRef,
        batches: Vec<RecordBatch>,
        /// Raw Arrow IPC File bytes, backed by the memory-mapped port file. The
        /// decoded `batches` reference this same buffer, so reads are zero-copy and
        /// the buffer can be re-shipped to other consumers without re-encoding.
        ipc_bytes: Buffer,
    },
    Media {
        path: PathBuf,
        version: u64,
        mime: String,
        bytes: Vec<u8>,
        /// Duration in seconds as recorded in the port manifest, if present.
        duration_sec: Option<f64>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum PortPayload<'a> {
    RecordBatch(&'a RecordBatch),
    IpcBytes(&'a [u8]),
    MediaBlob {
        bytes: &'a [u8],
        mime: &'a str,
        /// Optional duration in seconds; forwarded to the wire manifest entry.
        duration_sec: Option<f64>,
    },
}

impl Serialize for PortEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct WireEntry<'a> {
            path: &'a Path,
            version: u64,
            kind: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            schema: Option<&'a Schema>,
            #[serde(skip_serializing_if = "Option::is_none")]
            mime: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            size: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            duration_sec: Option<f64>,
        }

        let wire = match &self.kind {
            PortKind::Arrow(schema) => WireEntry {
                path: &self.path,
                version: self.version,
                kind: "arrow",
                schema: Some(schema),
                mime: None,
                size: None,
                duration_sec: None,
            },
            PortKind::Media {
                mime,
                size,
                duration_sec,
            } => WireEntry {
                path: &self.path,
                version: self.version,
                kind: "media",
                schema: None,
                mime: Some(mime),
                size: Some(*size),
                duration_sec: *duration_sec,
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PortEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireEntry {
            path: PathBuf,
            version: u64,
            #[serde(default)]
            kind: Option<String>,
            #[serde(default)]
            schema: Option<Schema>,
            #[serde(default)]
            mime: Option<String>,
            #[serde(default)]
            size: Option<u64>,
            /// Optional duration; absent in older manifests — defaults to `None`.
            #[serde(default)]
            duration_sec: Option<f64>,
        }

        let wire = WireEntry::deserialize(deserializer)?;
        let kind = match wire.kind.as_deref().unwrap_or("arrow") {
            "arrow" | "Arrow" => PortKind::Arrow(
                wire.schema
                    .ok_or_else(|| de::Error::missing_field("schema"))?,
            ),
            "media" | "Media" => PortKind::Media {
                mime: wire.mime.ok_or_else(|| de::Error::missing_field("mime"))?,
                size: wire.size.ok_or_else(|| de::Error::missing_field("size"))?,
                duration_sec: wire.duration_sec,
            },
            other => {
                return Err(de::Error::unknown_variant(other, &["arrow", "media"]));
            }
        };

        Ok(Self {
            path: wire.path,
            version: wire.version,
            kind,
        })
    }
}

impl PortRead {
    pub fn version(&self) -> u64 {
        match self {
            Self::Arrow { version, .. } | Self::Media { version, .. } => *version,
        }
    }
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

        let (bytes, kind, extension) = match payload.into() {
            PortPayload::RecordBatch(batch) => {
                let ipc_bytes = ipc_bytes_for_batch(batch)?;
                let (schema, _) = read_ipc(&ipc_bytes)?;
                (ipc_bytes, PortKind::Arrow(schema.as_ref().clone()), "arrow")
            }
            PortPayload::IpcBytes(bytes) => {
                let ipc_bytes = bytes.to_vec();
                let (schema, _) = read_ipc(&ipc_bytes)?;
                (ipc_bytes, PortKind::Arrow(schema.as_ref().clone()), "arrow")
            }
            PortPayload::MediaBlob {
                bytes,
                mime,
                duration_sec,
            } => (
                bytes.to_vec(),
                PortKind::Media {
                    mime: mime.to_owned(),
                    size: bytes.len() as u64,
                    duration_sec,
                },
                "media",
            ),
        };

        let version = self
            .manifest
            .ports
            .get(port)
            .map_or(1, |entry| entry.version + 1);
        let path = self
            .ports_dir
            .join(format!("{port}@v{version}.{extension}"));

        fs::write(&path, &bytes).map_err(|source| io_error(&path, source))?;

        let entry = PortEntry {
            path,
            version,
            kind,
        };
        let mut next_manifest = self.manifest.clone();
        next_manifest.ports.insert(port.to_owned(), entry.clone());
        self.persist_manifest(&next_manifest)?;
        self.manifest = next_manifest;

        Ok(entry)
    }

    #[expect(
        unsafe_code,
        reason = "memmap2::Mmap::map maps a write-once, atomically-renamed port file"
    )]
    pub fn get(&self, port: &str) -> Result<PortRead, PortStoreError> {
        let entry = self
            .manifest
            .ports
            .get(port)
            .ok_or_else(|| PortStoreError::MissingPort(port.to_owned()))?;
        if entry.path.extension().and_then(|value| value.to_str()) == Some("media") {
            let bytes = fs::read(&entry.path).map_err(|source| io_error(&entry.path, source))?;
            let (mime, duration_sec) = match &entry.kind {
                PortKind::Media {
                    mime, duration_sec, ..
                } => (mime.clone(), *duration_sec),
                PortKind::Arrow(_) => ("application/octet-stream".to_owned(), None),
            };
            return Ok(PortRead::Media {
                path: entry.path.clone(),
                version: entry.version,
                mime,
                bytes,
                duration_sec,
            });
        }

        let file = fs::File::open(&entry.path).map_err(|source| io_error(&entry.path, source))?;
        // SAFETY: port files are written once via create + atomic rename and are
        // never mutated in place (each `put` writes a new versioned file), so the
        // mapped region stays valid and immutable for the lifetime of the mapping.
        let mmap = unsafe { Mmap::map(&file).map_err(|source| io_error(&entry.path, source))? };
        let ipc_bytes = mmap_to_buffer(mmap);
        let (schema, batches) = decode_ipc_file(&ipc_bytes)?;

        Ok(PortRead::Arrow {
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

/// Wrap a memory-mapped region in an Arrow [`Buffer`] without copying. The
/// `Mmap` is moved into the buffer's allocation owner, keeping the mapping alive
/// for exactly as long as the buffer (and any arrays sliced from it) are used.
#[expect(
    unsafe_code,
    reason = "Buffer::from_custom_allocation wraps the mmap and keeps the Mmap owner alive"
)]
fn mmap_to_buffer(mmap: Mmap) -> Buffer {
    let len = mmap.len();
    if len == 0 {
        return Buffer::from_vec::<u8>(Vec::new());
    }
    let ptr = NonNull::new(mmap.as_ptr().cast_mut()).expect("mmap pointer is non-null");
    // SAFETY: `ptr`/`len` describe the live mapped region, and the `Mmap` owner
    // moved into the `Arc` backs that allocation for the buffer's lifetime.
    unsafe { Buffer::from_custom_allocation(ptr, len, Arc::new(mmap)) }
}

/// Decode an Arrow IPC File from an in-memory [`Buffer`] without copying the
/// column buffers: each [`RecordBatch`] slices directly into `buffer`. This is
/// the zero-copy counterpart to [`read_ipc`], used for the mmap read path.
fn decode_ipc_file(buffer: &Buffer) -> Result<(SchemaRef, Vec<RecordBatch>), PortStoreError> {
    const ARROW_FOOTER_TRAILER_LEN: usize = 10;
    if buffer.len() < ARROW_FOOTER_TRAILER_LEN {
        return Err(arrow_parse_error(
            "Arrow IPC file is smaller than its trailer",
        ));
    }

    let trailer_start = buffer.len() - ARROW_FOOTER_TRAILER_LEN;
    let footer_len = read_footer_length(
        buffer[trailer_start..]
            .try_into()
            .expect("slice is exactly the trailer length"),
    )?;
    if footer_len > trailer_start {
        return Err(arrow_parse_error("Arrow IPC footer length exceeds file"));
    }
    let footer = root_as_footer(&buffer[trailer_start - footer_len..trailer_start])
        .map_err(|err| arrow_parse_error(format!("invalid Arrow IPC footer: {err}")))?;

    let schema: SchemaRef =
        Arc::new(fb_to_schema(footer.schema().ok_or_else(|| {
            arrow_parse_error("Arrow IPC footer is missing a schema")
        })?));

    let mut decoder = FileDecoder::new(Arc::clone(&schema), footer.version());
    if let Some(dictionaries) = footer.dictionaries() {
        for block in dictionaries {
            let data = block_slice(buffer, block);
            decoder.read_dictionary(block, &data)?;
        }
    }

    let mut batches = Vec::new();
    if let Some(record_batches) = footer.recordBatches() {
        for block in record_batches {
            let data = block_slice(buffer, block);
            if let Some(batch) = decoder.read_record_batch(block, &data)? {
                batches.push(batch);
            }
        }
    }

    Ok((schema, batches))
}

fn block_slice(buffer: &Buffer, block: &arrow_ipc::Block) -> Buffer {
    let len = block.bodyLength() as usize + block.metaDataLength() as usize;
    buffer.slice_with_length(block.offset() as usize, len)
}

fn arrow_parse_error(message: impl Into<String>) -> PortStoreError {
    PortStoreError::Arrow(ArrowError::ParseError(message.into()))
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
    use arrow_schema::{DataType, Field, Schema, TimeUnit};

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
    fn js_manifest_shape_deserializes_into_port_manifest() {
        let manifest_json = r#"{
  "ports": {
    "sales": {
      "path": "/tmp/spur-notebook/ports/sales@v1.arrow",
      "version": 1,
      "schema": {
        "fields": [
          {
            "name": "id",
            "data_type": "Int64",
            "nullable": false,
            "dict_id": 0,
            "dict_is_ordered": false,
            "metadata": {}
          }
        ],
        "metadata": {}
      }
    }
  }
}"#;

        let manifest: PortManifest =
            serde_json::from_str(manifest_json).expect("JS manifest shape deserializes");
        let entry = manifest.ports.get("sales").expect("sales port entry");

        assert_eq!(
            PathBuf::from("/tmp/spur-notebook/ports/sales@v1.arrow"),
            entry.path
        );
        assert_eq!(1, entry.version);
        assert_eq!(
            PortKind::Arrow(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
            entry.kind
        );
    }

    #[test]
    fn legacy_manifest_without_kind_defaults_to_arrow_schema() {
        let manifest_json = r#"{
  "ports": {
    "legacy": {
      "path": "/tmp/spur-notebook/ports/legacy@v4.arrow",
      "version": 4,
      "schema": {
        "fields": [
          {
            "name": "value",
            "data_type": "Utf8",
            "nullable": false,
            "dict_id": 0,
            "dict_is_ordered": false,
            "metadata": {}
          }
        ],
        "metadata": {}
      }
    }
  }
}"#;

        let manifest: PortManifest =
            serde_json::from_str(manifest_json).expect("legacy manifest deserializes");
        let entry = manifest.ports.get("legacy").expect("legacy port entry");

        assert_eq!(4, entry.version);
        assert_eq!(
            PortKind::Arrow(Schema::new(vec![Field::new(
                "value",
                DataType::Utf8,
                false
            )])),
            entry.kind
        );
    }

    #[test]
    fn arrow_data_type_serde_json_oracle() {
        let cases = [
            (DataType::Boolean, serde_json::json!("Boolean")),
            (DataType::Int8, serde_json::json!("Int8")),
            (DataType::Int16, serde_json::json!("Int16")),
            (DataType::Int32, serde_json::json!("Int32")),
            (DataType::Int64, serde_json::json!("Int64")),
            (DataType::UInt8, serde_json::json!("UInt8")),
            (DataType::UInt16, serde_json::json!("UInt16")),
            (DataType::UInt32, serde_json::json!("UInt32")),
            (DataType::UInt64, serde_json::json!("UInt64")),
            (DataType::Float16, serde_json::json!("Float16")),
            (DataType::Float32, serde_json::json!("Float32")),
            (DataType::Float64, serde_json::json!("Float64")),
            (DataType::Utf8, serde_json::json!("Utf8")),
            (DataType::LargeUtf8, serde_json::json!("LargeUtf8")),
            (DataType::Binary, serde_json::json!("Binary")),
            (DataType::LargeBinary, serde_json::json!("LargeBinary")),
            (DataType::Null, serde_json::json!("Null")),
            (DataType::Date32, serde_json::json!("Date32")),
            (DataType::Date64, serde_json::json!("Date64")),
            (
                DataType::Timestamp(TimeUnit::Microsecond, None),
                serde_json::json!({ "Timestamp": ["Microsecond", null] }),
            ),
            (
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                serde_json::json!({ "Timestamp": ["Microsecond", "UTC"] }),
            ),
            (
                DataType::Time64(TimeUnit::Nanosecond),
                serde_json::json!({ "Time64": "Nanosecond" }),
            ),
            (
                DataType::Decimal128(12, 4),
                serde_json::json!({ "Decimal128": [12, 4] }),
            ),
            (
                DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                serde_json::json!({ "Dictionary": ["Int32", "Utf8"] }),
            ),
        ];

        for (data_type, expected) in cases {
            assert_eq!(
                expected,
                serde_json::to_value(&data_type).expect("DataType serializes"),
                "serde spelling for {data_type:?}"
            );
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
        assert_eq!(
            PortKind::Arrow(first.schema().as_ref().clone()),
            first_entry.kind
        );

        let first_read = store.get("sales").expect("get first batch");
        let PortRead::Arrow {
            version,
            schema,
            batches,
            ..
        } = first_read
        else {
            panic!("expected Arrow port read");
        };
        assert_eq!(1, version);
        assert_eq!(first.schema().as_ref(), schema.as_ref());
        assert_eq!(1, batches.len());
        assert_batch_eq(&first, &batches[0]);

        let second = batch(vec![3, 4, 5], vec!["gamma", "delta", "epsilon"]);
        let second_entry = store.put("sales", &second).expect("put second batch");
        assert_eq!(2, second_entry.version);
        assert_eq!(dir.path().join("ports/sales@v2.arrow"), second_entry.path);

        let reloaded = PortStore::open_at(dir.path()).expect("reload persisted manifest");
        let manifest_entry = reloaded.manifest().get("sales").expect("manifest entry");
        assert_eq!(2, manifest_entry.version);
        assert_eq!(dir.path().join("ports/sales@v2.arrow"), manifest_entry.path);
        assert_eq!(
            PortKind::Arrow(second.schema().as_ref().clone()),
            manifest_entry.kind
        );

        let second_read = reloaded.get("sales").expect("get latest batch");
        let PortRead::Arrow {
            version,
            schema,
            batches,
            ..
        } = second_read
        else {
            panic!("expected Arrow port read");
        };
        assert_eq!(2, version);
        assert_eq!(second.schema().as_ref(), schema.as_ref());
        assert_eq!(1, batches.len());
        assert_batch_eq(&second, &batches[0]);
    }

    #[test]
    fn put_get_round_trip_preserves_media_blob_and_mime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = PortStore::open_at(dir.path()).expect("open store");
        let webm = b"webm bytes";

        let entry = store
            .put(
                "frame-1",
                PortPayload::MediaBlob {
                    bytes: webm,
                    mime: "video/webm",
                    duration_sec: None,
                },
            )
            .expect("put media blob");

        assert_eq!(1, entry.version);
        assert_eq!(dir.path().join("ports/frame-1@v1.media"), entry.path);
        assert_eq!(
            PortKind::Media {
                mime: "video/webm".to_string(),
                size: webm.len() as u64,
                duration_sec: None,
            },
            entry.kind
        );

        let read = store.get("frame-1").expect("get media blob");
        let PortRead::Media {
            version,
            mime,
            bytes,
            ..
        } = read
        else {
            panic!("expected media port read");
        };
        assert_eq!(1, version);
        assert_eq!("video/webm", mime);
        assert_eq!(webm, bytes.as_slice());
    }

    // ── T3: port-store fixtures + duration_sec ────────────────────────────────

    #[test]
    fn put_get_round_trip_preserves_media_blob_and_duration_sec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = PortStore::open_at(dir.path()).expect("open store");
        let webm = b"webm bytes";

        let entry = store
            .put(
                "capture",
                PortPayload::MediaBlob {
                    bytes: webm,
                    mime: "video/webm",
                    duration_sec: Some(60.0),
                },
            )
            .expect("put media blob with duration");

        assert_eq!(
            PortKind::Media {
                mime: "video/webm".to_string(),
                size: webm.len() as u64,
                duration_sec: Some(60.0),
            },
            entry.kind
        );

        // Re-open the store from disk to verify the manifest was persisted.
        let store2 = PortStore::open_at(dir.path()).expect("re-open store");
        let reloaded = store2.manifest().get("capture").expect("capture entry");
        assert_eq!(
            PortKind::Media {
                mime: "video/webm".to_string(),
                size: webm.len() as u64,
                duration_sec: Some(60.0),
            },
            reloaded.kind
        );

        // get() must also surface duration_sec in PortRead.
        let read = store2.get("capture").expect("get media blob");
        let PortRead::Media { duration_sec, .. } = read else {
            panic!("expected media port read");
        };
        assert_eq!(Some(60.0), duration_sec);
    }

    #[test]
    fn media_port_without_duration_sec_defaults_to_none_on_deserialization() {
        // Old manifests written before duration_sec existed must still parse.
        let manifest_json = r#"{
  "ports": {
    "frame": {
      "path": "/tmp/ports/frame@v1.media",
      "version": 1,
      "kind": "media",
      "mime": "video/webm",
      "size": 42
    }
  }
}"#;
        let manifest: PortManifest =
            serde_json::from_str(manifest_json).expect("old manifest deserializes");
        let entry = manifest.ports.get("frame").expect("frame entry");
        assert_eq!(
            PortKind::Media {
                mime: "video/webm".to_string(),
                size: 42,
                duration_sec: None,
            },
            entry.kind,
            "old manifest without duration_sec must default to None"
        );
    }

    #[test]
    fn port_store_golden_fixture_round_trips() {
        // Load the canonical fixture files from the on-disk port-store fixture
        // directory.  The fixture manifest uses paths relative to the fixture
        // dir so we resolve them before checking.
        let fixture_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/port-store");

        let manifest_path = fixture_dir.join("manifest.json");
        let manifest_json = std::fs::read_to_string(&manifest_path)
            .expect("fixture manifest.json must exist — run `git lfs pull` if missing");

        // Parse with paths as-written in the fixture.
        let manifest: PortManifest =
            serde_json::from_str(&manifest_json).expect("fixture manifest.json deserializes");

        // Arrow entry: sales
        let sales = manifest.ports.get("sales").expect("sales entry in fixture");
        assert_eq!(1, sales.version);
        assert!(
            matches!(&sales.kind, PortKind::Arrow(schema) if schema.field(0).name() == "id"),
            "sales entry must be an Arrow port with an 'id' field"
        );

        // Media entry with duration_sec: spur-ad-capture
        let capture = manifest
            .ports
            .get("spur-ad-capture")
            .expect("spur-ad-capture entry in fixture");
        assert_eq!(1, capture.version);
        assert_eq!(
            PortKind::Media {
                mime: "video/webm".to_string(),
                size: 10,
                duration_sec: Some(60.0),
            },
            capture.kind,
            "fixture media entry must carry duration_sec = 60.0"
        );

        // The physical media file referenced in the fixture must exist.
        let media_path = fixture_dir.join("spur-ad-capture@v1.media");
        assert!(
            media_path.exists(),
            "fixture media file {} must exist",
            media_path.display()
        );

        // Round-trip: serialize back to JSON and re-parse — must be identical.
        let re_serialized = serde_json::to_string_pretty(&manifest).expect("manifest serializes");
        let round_tripped: PortManifest =
            serde_json::from_str(&re_serialized).expect("re-serialized manifest deserializes");
        assert_eq!(manifest, round_tripped, "manifest must round-trip cleanly");
    }
}
