:dep arrow = "55"
:dep serde_json = "1"

#[derive(Clone, Debug)]
struct _Spur {
    root: String,
}

use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use arrow::record_batch::RecordBatch;
use serde_json::{Map, Number, Value};

const PORT_MIME: &str = "application/vnd.spur.port+json";
const PORTS_DIR_NAME: &str = "ports";
const PORT_FILE_VERSION_SEPARATOR: &str = "@v";
const PORT_ARROW_FILE_EXTENSION: &str = "arrow";
const PORT_MANIFEST_FILE_NAME: &str = "manifest.json";
const PORT_MANIFEST_TEMP_PREFIX: &str = "manifest.json.";
const PORT_TEMP_FILE_SUFFIX: &str = ".tmp";
const PORT_MANIFEST_PORTS_KEY: &str = "ports";
const PORT_MANIFEST_PATH_KEY: &str = "path";
const PORT_MANIFEST_VERSION_KEY: &str = "version";
const PORT_MANIFEST_SCHEMA_KEY: &str = "schema";
const PORT_INITIAL_VERSION: u64 = 0;
const PORT_VERSION_INCREMENT: u64 = 1;
const PORT_RESERVED_CURRENT_DIR: &str = ".";
const PORT_RESERVED_PARENT_DIR: &str = "..";
const PORT_FORBIDDEN_SLASH: &str = "/";
const PORT_FORBIDDEN_BACKSLASH: &str = "\\";
const PORT_FORBIDDEN_NUL: &str = "\0";

impl _Spur {
    pub fn new(root: impl Into<String>) -> Self {
        Self { root: root.into() }
    }

    pub fn put(
        &self,
        port: &str,
        batch: RecordBatch,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.validate_port(port)?;
        let ports_dir = self.ports_dir();
        fs::create_dir_all(&ports_dir)?;

        let mut manifest = self.load_manifest()?;
        let schema = Self::schema_json(batch.schema().as_ref(), port)?;
        let previous_version = Self::previous_version(&manifest, port);
        let version = previous_version + PORT_VERSION_INCREMENT;
        let arrow_path = ports_dir.join(Self::port_file_name(port, version));

        let file = File::create(&arrow_path)?;
        let mut writer = FileWriter::try_new(file, batch.schema().as_ref())?;
        writer.write(&batch)?;
        writer.finish()?;

        Self::set_manifest_entry(
            &mut manifest,
            port,
            arrow_path.to_string_lossy().into_owned(),
            version,
            schema.clone(),
        )?;
        self.store_manifest(&manifest)?;
        self.display_port(port, version, schema, &batch)
    }

    pub fn get(
        &self,
        port: &str,
    ) -> Result<Vec<RecordBatch>, Box<dyn std::error::Error>> {
        self.validate_port(port)?;
        let manifest = self.load_manifest()?;
        let entry = manifest
            .get(PORT_MANIFEST_PORTS_KEY)
            .and_then(|ports| ports.get(port))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("SPUR port has not been written: {port}"),
                )
            })?;
        let path = entry
            .get(PORT_MANIFEST_PATH_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("SPUR port manifest entry is missing a path: {port}"),
                )
            })?;

        let file = File::open(path)?;
        let reader = FileReader::try_new(file, None)?;
        let mut batches = Vec::new();
        for batch in reader {
            batches.push(batch?);
        }
        Ok(batches)
    }

    fn ports_dir(&self) -> PathBuf {
        Path::new(self.root.as_str()).join(PORTS_DIR_NAME)
    }

    fn manifest_path(&self) -> PathBuf {
        self.ports_dir().join(PORT_MANIFEST_FILE_NAME)
    }

    fn load_manifest(&self) -> Result<Value, Box<dyn std::error::Error>> {
        let manifest_path = self.manifest_path();
        if !manifest_path.exists() {
            return Ok(Self::empty_manifest());
        }
        let text = fs::read_to_string(manifest_path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn store_manifest(&self, manifest: &Value) -> Result<(), Box<dyn std::error::Error>> {
        let tmp_path = self.temp_manifest_path()?;
        let mut tmp = File::create(&tmp_path)?;
        serde_json::to_writer_pretty(&mut tmp, manifest)?;
        writeln!(&mut tmp)?;
        drop(tmp);
        fs::rename(&tmp_path, self.manifest_path())?;
        Ok(())
    }

    fn temp_manifest_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(self.ports_dir().join(format!(
            "{PORT_MANIFEST_TEMP_PREFIX}{nanos}{PORT_TEMP_FILE_SUFFIX}"
        )))
    }

    fn previous_version(manifest: &Value, port: &str) -> u64 {
        manifest
            .get(PORT_MANIFEST_PORTS_KEY)
            .and_then(|ports| ports.get(port))
            .and_then(|entry| entry.get(PORT_MANIFEST_VERSION_KEY))
            .and_then(Value::as_u64)
            .unwrap_or(PORT_INITIAL_VERSION)
    }

    fn port_file_name(port: &str, version: u64) -> String {
        format!(
            "{}{}{}.{}",
            port, PORT_FILE_VERSION_SEPARATOR, version, PORT_ARROW_FILE_EXTENSION
        )
    }

    fn empty_manifest() -> Value {
        let mut manifest = Map::new();
        manifest.insert(
            PORT_MANIFEST_PORTS_KEY.to_string(),
            Value::Object(Map::new()),
        );
        Value::Object(manifest)
    }

    fn set_manifest_entry(
        manifest: &mut Value,
        port: &str,
        path: String,
        version: u64,
        schema: Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let manifest_object = manifest.as_object_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SPUR port manifest must be a JSON object",
            )
        })?;
        let ports_value = manifest_object
            .entry(PORT_MANIFEST_PORTS_KEY.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let ports = ports_value.as_object_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SPUR port manifest ports must be a JSON object",
            )
        })?;

        let mut entry = Map::new();
        entry.insert(PORT_MANIFEST_PATH_KEY.to_string(), Value::String(path));
        entry.insert(
            PORT_MANIFEST_VERSION_KEY.to_string(),
            Value::Number(Number::from(version)),
        );
        entry.insert(PORT_MANIFEST_SCHEMA_KEY.to_string(), schema);
        ports.insert(port.to_string(), Value::Object(entry));
        Ok(())
    }

    fn schema_json(
        schema: &arrow::datatypes::Schema,
        port: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let mut fields = Vec::new();
        for field in schema.fields() {
            let mut field_json = Map::new();
            field_json.insert("name".to_string(), Value::String(field.name().clone()));
            field_json.insert(
                "data_type".to_string(),
                Self::data_type_json(field.data_type(), port)?,
            );
            field_json.insert("nullable".to_string(), Value::Bool(field.is_nullable()));
            field_json.insert("dict_id".to_string(), Value::Number(Number::from(0)));
            field_json.insert("dict_is_ordered".to_string(), Value::Bool(false));
            field_json.insert("metadata".to_string(), Self::metadata_json(field.metadata()));
            fields.push(Value::Object(field_json));
        }

        let mut schema_json = Map::new();
        schema_json.insert("fields".to_string(), Value::Array(fields));
        schema_json.insert("metadata".to_string(), Self::metadata_json(schema.metadata()));
        Ok(Value::Object(schema_json))
    }

    fn metadata_json(metadata: &std::collections::HashMap<String, String>) -> Value {
        let mut output = Map::new();
        for (key, value) in metadata {
            output.insert(key.clone(), Value::String(value.clone()));
        }
        Value::Object(output)
    }

    fn data_type_json(
        data_type: &arrow::datatypes::DataType,
        port: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        use arrow::datatypes::DataType;

        let value = match data_type {
            DataType::Null => "Null",
            DataType::Boolean => "Boolean",
            DataType::Int8 => "Int8",
            DataType::Int16 => "Int16",
            DataType::Int32 => "Int32",
            DataType::Int64 => "Int64",
            DataType::UInt8 => "UInt8",
            DataType::UInt16 => "UInt16",
            DataType::UInt32 => "UInt32",
            DataType::UInt64 => "UInt64",
            DataType::Float16 => "Float16",
            DataType::Float32 => "Float32",
            DataType::Float64 => "Float64",
            DataType::Utf8 => "Utf8",
            DataType::LargeUtf8 => "LargeUtf8",
            DataType::Binary => "Binary",
            DataType::LargeBinary => "LargeBinary",
            DataType::Date32 => "Date32",
            DataType::Date64 => "Date64",
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "SPUR port {port:?}: unsupported Arrow type for manifest schema: {other}"
                    ),
                )
                .into())
            }
        };
        Ok(Value::String(value.to_string()))
    }

    fn display_port(
        &self,
        port: &str,
        version: u64,
        schema: Value,
        batch: &RecordBatch,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let mut payload = Map::new();
        payload.insert("port".to_string(), Value::String(port.to_string()));
        payload.insert(
            PORT_MANIFEST_VERSION_KEY.to_string(),
            Value::Number(Number::from(version)),
        );
        payload.insert(PORT_MANIFEST_SCHEMA_KEY.to_string(), schema);
        let payload = Value::Object(payload);
        let payload_json = serde_json::to_string(&payload)?;
        println!(
            "EVCXR_BEGIN_CONTENT {}\n{}\nEVCXR_END_CONTENT",
            PORT_MIME, payload_json
        );
        println!(
            "EVCXR_BEGIN_CONTENT text/html\n{}\nEVCXR_END_CONTENT",
            self.preview_html(port, version, batch)
        );
        Ok(payload)
    }

    fn preview_html(&self, port: &str, version: u64, batch: &RecordBatch) -> String {
        let title = format!(
            "<strong>SPUR port</strong> <code>{}</code> <span>v{}</span>",
            Self::escape_html(port),
            version
        );
        format!(
            "<div>{title}<p>{} rows x {} columns</p></div>",
            batch.num_rows(),
            batch.num_columns()
        )
    }

    fn escape_html(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }

    fn validate_port(&self, port: &str) -> Result<(), Box<dyn std::error::Error>> {
        if port.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SPUR port name cannot be empty",
            )
            .into());
        }
        if port == PORT_RESERVED_CURRENT_DIR
            || port == PORT_RESERVED_PARENT_DIR
            || port.contains(PORT_FORBIDDEN_SLASH)
            || port.contains(PORT_FORBIDDEN_BACKSLASH)
            || port.contains(PORT_FORBIDDEN_NUL)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("SPUR port name is not valid for an on-disk port file: {port}"),
            )
            .into());
        }
        Ok(())
    }
}

let spur: _Spur = _Spur::new(std::env::var("SPUR_NOTEBOOK_PORT_ROOT").expect("SPUR_NOTEBOOK_PORT_ROOT is not set"));
