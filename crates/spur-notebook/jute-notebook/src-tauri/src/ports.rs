//! SPUR port bootstrap helpers injected into notebook cells before dispatch.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::kernel_provision::{EVCXR_ARROW_CRATE_VERSION, GONB_ARROW_GO_MODULE};

/// MIME type used for SPUR port display payloads.
pub const PORT_MIME: &str = "application/vnd.spur.port+json";

/// Subdirectory under a notebook root that stores SPUR port Arrow files.
pub const PORTS_DIR_NAME: &str = "ports";
/// Separator used between a port name and its manifest version in Arrow files.
pub const PORT_FILE_VERSION_SEPARATOR: &str = "@v";
/// File extension used for SPUR port Arrow IPC files.
pub const PORT_ARROW_FILE_EXTENSION: &str = "arrow";
/// Manifest filename stored inside the SPUR ports directory.
pub const PORT_MANIFEST_FILE_NAME: &str = "manifest.json";
/// Top-level manifest object key containing per-port entries.
pub const PORT_MANIFEST_PORTS_KEY: &str = "ports";
/// Manifest entry key for the Arrow IPC file path.
pub const PORT_MANIFEST_PATH_KEY: &str = "path";
/// Manifest entry key for the current port version.
pub const PORT_MANIFEST_VERSION_KEY: &str = "version";
/// Manifest entry key for the serialized Arrow schema.
pub const PORT_MANIFEST_SCHEMA_KEY: &str = "schema";
/// Missing ports start at version 0 before the first write increments them.
pub const PORT_INITIAL_VERSION: u64 = 0;
/// Every successful put writes a fresh Arrow IPC file and bumps by one.
pub const PORT_VERSION_INCREMENT: u64 = 1;

const PORT_RESERVED_CURRENT_DIR: &str = ".";
const PORT_RESERVED_PARENT_DIR: &str = "..";
const PORT_FORBIDDEN_SLASH: &str = "/";
const PORT_FORBIDDEN_BACKSLASH: &str = "\\";
const PORT_FORBIDDEN_NUL: &str = "\0";
const PORT_TEMP_FILE_SUFFIX: &str = ".tmp";

/// Stable notebook id derived from the notebook path.
pub fn notebook_id_for_path(path: impl AsRef<Path>) -> String {
    let normalized = path.as_ref().to_string_lossy();
    let digest = blake3::hash(normalized.as_bytes()).to_hex();
    format!("nb-{}", &digest[..24])
}

/// Per-notebook directory used to store SPUR port files and the manifest.
pub fn notebook_port_root(path: impl AsRef<Path>) -> PathBuf {
    let notebook_id = notebook_id_for_path(path);
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".spur/notebooks").join(&notebook_id))
        .unwrap_or_else(|| PathBuf::from(".spur/notebooks").join(notebook_id))
}

fn ports_dir_for_root(notebook_root: impl AsRef<Path>) -> PathBuf {
    notebook_root.as_ref().join(PORTS_DIR_NAME)
}

fn manifest_path_for_root(notebook_root: impl AsRef<Path>) -> PathBuf {
    ports_dir_for_root(notebook_root).join(PORT_MANIFEST_FILE_NAME)
}

fn manifest_temp_prefix() -> String {
    format!("{PORT_MANIFEST_FILE_NAME}.")
}

fn string_literal(value: &str) -> String {
    format!("{value:?}")
}

/// Python bootstrap source that installs the `spur` helper for one cell.
pub fn python_bootstrap(notebook_root: impl AsRef<Path>) -> String {
    let root = notebook_root.as_ref().display().to_string();
    let root_literal = serde_json::to_string(&root).expect("path string serializes");
    format!(
        r#"
# --- SPUR port helper bootstrap ---
import html as _spur_html
import json as _spur_json
import os as _spur_os
import pathlib as _spur_pathlib
import tempfile as _spur_tempfile

class _Spur:
    _MIME = {mime:?}

    def __init__(self, notebook_root):
        self._root = _spur_pathlib.Path(notebook_root)
        self._ports_dir = self._root / {ports_dir_name}
        self._manifest_path = self._ports_dir / {manifest_file_name}
        self._ports_dir.mkdir(parents=True, exist_ok=True)

    def put(self, port, value):
        self._validate_port(port)
        import pyarrow as _spur_pa
        import pyarrow.ipc as _spur_ipc

        table = self._to_table(value, _spur_pa)
        manifest = self._load_manifest()
        version = int(manifest.get({manifest_ports_key}, {{}}).get(port, {{}}).get({manifest_version_key}, {initial_version})) + {version_increment}
        arrow_path = self._ports_dir / f"{{port}}{version_separator}{{version}}.{arrow_extension}"

        with _spur_ipc.new_file(str(arrow_path), table.schema) as writer:
            writer.write_table(table)

        schema = self._schema_json(table.schema, port)
        manifest.setdefault({manifest_ports_key}, {{}})[port] = {{
            {manifest_path_key}: str(arrow_path),
            {manifest_version_key}: version,
            {manifest_schema_key}: schema,
        }}
        self._store_manifest(manifest)

        bundle = {{
            self._MIME: {{
                "port": port,
                {manifest_version_key}: version,
                {manifest_schema_key}: schema,
            }},
            "text/html": self._preview_html(port, version, table),
        }}
        from IPython.display import display as _spur_display
        _spur_display(bundle, raw=True)
        return {{"port": port, "version": version, "schema": schema}}

    def get(self, port):
        self._validate_port(port)
        import pyarrow.ipc as _spur_ipc

        entry = self._load_manifest().get({manifest_ports_key}, {{}}).get(port)
        if entry is None:
            raise KeyError(f"SPUR port has not been written: {{port}}")
        with _spur_ipc.open_file(entry[{manifest_path_key}]) as reader:
            table = reader.read_all()
        try:
            return table.to_pandas()
        except Exception:
            return table

    def _to_table(self, value, pa):
        if isinstance(value, pa.Table):
            return value
        if isinstance(value, pa.RecordBatch):
            return pa.Table.from_batches([value])
        try:
            import pandas as _spur_pd
            if isinstance(value, _spur_pd.DataFrame):
                return pa.Table.from_pandas(value, preserve_index=False)
        except Exception:
            pass
        try:
            import numpy as _spur_np
            if isinstance(value, _spur_np.ndarray):
                if value.ndim == 1:
                    return pa.table({{"value": pa.array(value)}})
                if value.ndim == 2:
                    return pa.table({{f"c{{i}}": pa.array(value[:, i]) for i in range(value.shape[1])}})
        except Exception:
            pass
        if isinstance(value, dict):
            return pa.table(value)
        if isinstance(value, (list, tuple)) and value and isinstance(value[0], dict):
            return pa.Table.from_pylist(value)
        return pa.table({{"value": value}})

    def _load_manifest(self):
        if not self._manifest_path.exists():
            return {{{manifest_ports_key}: {{}}}}
        return _spur_json.loads(self._manifest_path.read_text(encoding="utf-8"))

    def _store_manifest(self, manifest):
        fd, tmp_name = _spur_tempfile.mkstemp(
            prefix={manifest_temp_prefix},
            suffix={temp_file_suffix},
            dir=str(self._ports_dir),
        )
        try:
            with _spur_os.fdopen(fd, "w", encoding="utf-8") as tmp:
                _spur_json.dump(manifest, tmp, indent=2)
            _spur_os.replace(tmp_name, self._manifest_path)
        finally:
            if _spur_os.path.exists(tmp_name):
                _spur_os.unlink(tmp_name)

    def _validate_port(self, port):
        if not isinstance(port, str) or port == "":
            raise ValueError("SPUR port name cannot be empty")
        if port in ({reserved_current_dir}, {reserved_parent_dir}) or {forbidden_slash} in port or {forbidden_backslash} in port or {forbidden_nul} in port:
            raise ValueError(f"SPUR port name is not valid for an on-disk port file: {{port}}")

    def _schema_json(self, schema, port):
        return {{
            "fields": [self._field_json(field, port) for field in schema],
            "metadata": self._metadata_json(schema.metadata),
        }}

    def _field_json(self, field, port):
        return {{
            "name": field.name,
            "data_type": self._data_type_json(field.type, port),
            "nullable": field.nullable,
            "dict_id": 0,
            "dict_is_ordered": False,
            "metadata": self._metadata_json(field.metadata),
        }}

    def _metadata_json(self, metadata):
        if not metadata:
            return {{}}
        return {{
            self._decode_metadata_key(k): self._decode_metadata_value(v)
            for k, v in metadata.items()
        }}

    def _decode_metadata_key(self, value):
        return value.decode("utf-8") if isinstance(value, bytes) else str(value)

    def _decode_metadata_value(self, value):
        return value.decode("utf-8") if isinstance(value, bytes) else str(value)

    def _data_type_json(self, data_type, port):
        import pyarrow as _spur_pa

        if _spur_pa.types.is_boolean(data_type):
            return "Boolean"
        if _spur_pa.types.is_int8(data_type):
            return "Int8"
        if _spur_pa.types.is_int16(data_type):
            return "Int16"
        if _spur_pa.types.is_int32(data_type):
            return "Int32"
        if _spur_pa.types.is_int64(data_type):
            return "Int64"
        if _spur_pa.types.is_uint8(data_type):
            return "UInt8"
        if _spur_pa.types.is_uint16(data_type):
            return "UInt16"
        if _spur_pa.types.is_uint32(data_type):
            return "UInt32"
        if _spur_pa.types.is_uint64(data_type):
            return "UInt64"
        if _spur_pa.types.is_float16(data_type):
            return "Float16"
        if _spur_pa.types.is_float32(data_type):
            return "Float32"
        if _spur_pa.types.is_float64(data_type):
            return "Float64"
        if _spur_pa.types.is_string(data_type):
            return "Utf8"
        if _spur_pa.types.is_large_string(data_type):
            return "LargeUtf8"
        if _spur_pa.types.is_binary(data_type):
            return "Binary"
        if _spur_pa.types.is_large_binary(data_type):
            return "LargeBinary"
        if _spur_pa.types.is_null(data_type):
            return "Null"
        if _spur_pa.types.is_date32(data_type):
            return "Date32"
        if _spur_pa.types.is_date64(data_type):
            return "Date64"
        if _spur_pa.types.is_timestamp(data_type):
            return {{"Timestamp": [self._time_unit_json(data_type.unit, port, data_type), data_type.tz]}}
        if _spur_pa.types.is_time32(data_type):
            return {{"Time32": self._time_unit_json(data_type.unit, port, data_type)}}
        if _spur_pa.types.is_time64(data_type):
            return {{"Time64": self._time_unit_json(data_type.unit, port, data_type)}}
        if _spur_pa.types.is_decimal128(data_type):
            return {{"Decimal128": [data_type.precision, data_type.scale]}}
        if _spur_pa.types.is_decimal256(data_type):
            return {{"Decimal256": [data_type.precision, data_type.scale]}}
        if _spur_pa.types.is_dictionary(data_type):
            return {{
                "Dictionary": [
                    self._data_type_json(data_type.index_type, port),
                    self._data_type_json(data_type.value_type, port),
                ]
            }}
        raise TypeError(f"SPUR port '{{port}}': unsupported Arrow type for manifest schema: {{data_type}}")

    def _time_unit_json(self, unit, port, data_type):
        units = {{
            "s": "Second",
            "ms": "Millisecond",
            "us": "Microsecond",
            "ns": "Nanosecond",
        }}
        try:
            return units[str(unit)]
        except KeyError:
            raise TypeError(f"SPUR port '{{port}}': unsupported Arrow time unit for manifest schema: {{data_type}}") from None

    def _preview_html(self, port, version, table):
        rows = table.slice(0, min(table.num_rows, 5)).to_pylist()
        headers = [field.name for field in table.schema]
        title = (
            f"<strong>SPUR port</strong> "
            f"<code>{{_spur_html.escape(port)}}</code> "
            f"<span>v{{version}}</span>"
        )
        if not headers:
            return f"<div>{{title}}<p>0 columns, {{table.num_rows}} rows</p></div>"
        thead = "".join(f"<th>{{_spur_html.escape(name)}}</th>" for name in headers)
        body_rows = []
        for row in rows:
            cells = "".join(
                f"<td>{{_spur_html.escape(str(row.get(name, '')))}}</td>"
                for name in headers
            )
            body_rows.append(f"<tr>{{cells}}</tr>")
        body = "".join(body_rows)
        return (
            f"<div>{{title}}<p>{{table.num_rows}} rows x {{table.num_columns}} columns</p>"
            f"<table><thead><tr>{{thead}}</tr></thead><tbody>{{body}}</tbody></table></div>"
        )

spur = _Spur({root})
# --- end SPUR port helper bootstrap ---
"#,
        mime = PORT_MIME,
        root = root_literal,
        ports_dir_name = string_literal(PORTS_DIR_NAME),
        manifest_file_name = string_literal(PORT_MANIFEST_FILE_NAME),
        manifest_ports_key = string_literal(PORT_MANIFEST_PORTS_KEY),
        manifest_path_key = string_literal(PORT_MANIFEST_PATH_KEY),
        manifest_version_key = string_literal(PORT_MANIFEST_VERSION_KEY),
        manifest_schema_key = string_literal(PORT_MANIFEST_SCHEMA_KEY),
        initial_version = PORT_INITIAL_VERSION,
        version_increment = PORT_VERSION_INCREMENT,
        version_separator = PORT_FILE_VERSION_SEPARATOR,
        arrow_extension = PORT_ARROW_FILE_EXTENSION,
        manifest_temp_prefix = string_literal(&manifest_temp_prefix()),
        temp_file_suffix = string_literal(PORT_TEMP_FILE_SUFFIX),
        reserved_current_dir = string_literal(PORT_RESERVED_CURRENT_DIR),
        reserved_parent_dir = string_literal(PORT_RESERVED_PARENT_DIR),
        forbidden_slash = string_literal(PORT_FORBIDDEN_SLASH),
        forbidden_backslash = string_literal(PORT_FORBIDDEN_BACKSLASH),
        forbidden_nul = string_literal(PORT_FORBIDDEN_NUL),
    )
}

/// JavaScript/Deno bootstrap source that installs the `globalThis.spur` helper for one cell.
pub fn javascript_bootstrap(notebook_root: impl AsRef<Path>) -> String {
    let root = notebook_root.as_ref().display().to_string();
    let ports_dir = ports_dir_for_root(notebook_root.as_ref())
        .display()
        .to_string();
    let manifest_path = manifest_path_for_root(notebook_root.as_ref())
        .display()
        .to_string();
    let root_literal = serde_json::to_string(&root).expect("path string serializes");
    let ports_literal = serde_json::to_string(&ports_dir).expect("path string serializes");
    let manifest_literal = serde_json::to_string(&manifest_path).expect("path string serializes");
    let mime_literal = serde_json::to_string(PORT_MIME).expect("mime string serializes");

    r#"
{
// --- SPUR port helper bootstrap ---
const _spurArrow = await import("npm:apache-arrow@21.1.0");
const {
  RecordBatch,
  RecordBatchFileWriter,
  Table,
  tableFromArrays,
  tableFromIPC,
  tableToIPC,
} = _spurArrow;

const _spurDenoRuntime = {
  home() {
    return Deno.env.get("HOME");
  },
  fs: {
    mkdirp(path) {
      Deno.mkdirSync(path, { recursive: true });
    },
    exists(path) {
      try {
        Deno.statSync(path);
        return true;
      } catch (error) {
        if (error instanceof Deno.errors.NotFound) {
          return false;
        }
        throw error;
      }
    },
    readBytes(path) {
      return Deno.readFileSync(path);
    },
    readText(path) {
      return Deno.readTextFileSync(path);
    },
    writeBytes(path, bytes) {
      Deno.writeFileSync(path, bytes);
    },
    writeText(path, text) {
      Deno.writeTextFileSync(path, text);
    },
    rename(from, to) {
      Deno.renameSync(from, to);
    },
    makeTempFile(dir) {
      return Deno.makeTempFileSync({
        dir,
        prefix: __SPUR_MANIFEST_TEMP_PREFIX__,
        suffix: __SPUR_TEMP_FILE_SUFFIX__,
      });
    },
    remove(path) {
      try {
        Deno.removeSync(path);
      } catch (error) {
        if (!(error instanceof Deno.errors.NotFound)) {
          throw error;
        }
      }
    },
  },
  display(bundle) {
    return Deno.jupyter.display(bundle, { raw: true });
  },
};

class _Spur {
  constructor({ root, portsDir, manifestPath, mime, runtime }) {
    this._root = this._expandHome(root, runtime);
    this._portsDir = this._expandHome(portsDir, runtime);
    this._manifestPath = this._expandHome(manifestPath, runtime);
    this._mime = mime;
    this._runtime = runtime;
    this._runtime.fs.mkdirp(this._portsDir);
  }

  get(port) {
    this._validatePort(port);
    const entry = this._loadManifest().__SPUR_MANIFEST_PORTS_KEY__?.[port];
    if (entry === undefined) {
      throw new Error(`SPUR port has not been written: ${port}`);
    }
    return tableFromIPC(this._runtime.fs.readBytes(entry.__SPUR_MANIFEST_PATH_KEY__));
  }

  put(port, value) {
    this._validatePort(port);
    const table = this._toTable(value);
    const manifest = this._loadManifest();
    const version = Number(manifest.__SPUR_MANIFEST_PORTS_KEY__?.[port]?.__SPUR_MANIFEST_VERSION_KEY__ ?? __SPUR_INITIAL_VERSION__) + __SPUR_VERSION_INCREMENT__;
    const arrowPath = `${this._portsDir}/${port}__SPUR_VERSION_SEPARATOR__${version}.__SPUR_ARROW_EXTENSION__`;

    this._runtime.fs.writeBytes(arrowPath, tableToIPC(table, "file"));

    const schema = this._schemaJson(table.schema, port);
    manifest.__SPUR_MANIFEST_PORTS_KEY__ ??= {};
    manifest.__SPUR_MANIFEST_PORTS_KEY__[port] = {
      __SPUR_MANIFEST_PATH_KEY__: arrowPath,
      __SPUR_MANIFEST_VERSION_KEY__,
      __SPUR_MANIFEST_SCHEMA_KEY__,
    };
    this._storeManifest(manifest);

    const bundle = {
      [this._mime]: {
        port,
        version,
        schema,
      },
      "text/html": this._previewHtml(port, version, table),
    };
    const display = this._runtime.display(bundle);
    if (display?.catch !== undefined) {
      display.catch((error) => {
        console.warn("SPUR port display failed", error);
      });
    }
    return { port, version, schema };
  }

  _toTable(value) {
    if (Table.isTable?.(value) || value instanceof Table) {
      return value;
    }
    if (value instanceof RecordBatch) {
      return new Table(value.schema, [value]);
    }
    if (Array.isArray(value)) {
      if (value.length > 0 && this._isPlainObject(value[0])) {
        const columns = {};
        for (const name of Object.keys(value[0])) {
          columns[name] = value.map((row) => row?.[name]);
        }
        return tableFromArrays(columns);
      }
      return tableFromArrays({ value });
    }
    if (this._isPlainObject(value)) {
      return tableFromArrays(value);
    }
    return tableFromArrays({ value: [value] });
  }

  _loadManifest() {
    if (!this._runtime.fs.exists(this._manifestPath)) {
      return { __SPUR_MANIFEST_PORTS_KEY__: {} };
    }
    return JSON.parse(this._runtime.fs.readText(this._manifestPath));
  }

  _storeManifest(manifest) {
    const tmpPath = this._runtime.fs.makeTempFile(this._portsDir);
    try {
      this._runtime.fs.writeText(tmpPath, `${JSON.stringify(manifest, null, 2)}\n`);
      this._runtime.fs.rename(tmpPath, this._manifestPath);
    } finally {
      if (this._runtime.fs.exists(tmpPath)) {
        this._runtime.fs.remove(tmpPath);
      }
    }
  }

  _validatePort(port) {
    if (typeof port !== "string" || port === "") {
      throw new Error("SPUR port name cannot be empty");
    }
    if (
      port === __SPUR_RESERVED_CURRENT_DIR__ ||
      port === __SPUR_RESERVED_PARENT_DIR__ ||
      port.includes(__SPUR_FORBIDDEN_SLASH__) ||
      port.includes(__SPUR_FORBIDDEN_BACKSLASH__) ||
      port.includes(__SPUR_FORBIDDEN_NUL__)
    ) {
      throw new Error(`SPUR port name is not valid for an on-disk port file: ${port}`);
    }
  }

  _schemaJson(schema, port) {
    return {
      fields: schema.fields.map((field) => this._fieldJson(field, port)),
      metadata: this._metadataJson(schema.metadata),
    };
  }

  _fieldJson(field, port) {
    return {
      name: field.name,
      data_type: this._dataTypeJson(field.type, port),
      nullable: Boolean(field.nullable),
      dict_id: 0,
      dict_is_ordered: false,
      metadata: this._metadataJson(field.metadata),
    };
  }

  _metadataJson(metadata) {
    if (metadata === undefined || metadata === null) {
      return {};
    }
    const entries =
      metadata instanceof Map ? metadata.entries() : Object.entries(metadata);
    const output = {};
    for (const [key, value] of entries) {
      output[this._decodeMetadata(key)] = this._decodeMetadata(value);
    }
    return output;
  }

  _decodeMetadata(value) {
    if (value instanceof Uint8Array) {
      return new TextDecoder().decode(value);
    }
    return String(value);
  }

  _dataTypeJson(dataType, port) {
    const typeName = String(dataType);
    const scalars = {
      Bool: "Boolean",
      Boolean: "Boolean",
      bool: "Boolean",
      boolean: "Boolean",
      Int8: "Int8",
      Int16: "Int16",
      Int32: "Int32",
      Int64: "Int64",
      int8: "Int8",
      int16: "Int16",
      int32: "Int32",
      int64: "Int64",
      Uint8: "UInt8",
      Uint16: "UInt16",
      Uint32: "UInt32",
      Uint64: "UInt64",
      UInt8: "UInt8",
      UInt16: "UInt16",
      UInt32: "UInt32",
      UInt64: "UInt64",
      uint8: "UInt8",
      uint16: "UInt16",
      uint32: "UInt32",
      uint64: "UInt64",
      Float: "Float32",
      Float16: "Float16",
      Float32: "Float32",
      Float64: "Float64",
      float: "Float32",
      float16: "Float16",
      float32: "Float32",
      float64: "Float64",
      Utf8: "Utf8",
      utf8: "Utf8",
      LargeUtf8: "LargeUtf8",
      largeutf8: "LargeUtf8",
      Binary: "Binary",
      binary: "Binary",
      LargeBinary: "LargeBinary",
      largebinary: "LargeBinary",
      Null: "Null",
      null: "Null",
      Date32: "Date32",
      date32: "Date32",
      Date64: "Date64",
      date64: "Date64",
    };
    const scalar = scalars[typeName] ?? scalars[typeName.toLowerCase()];
    if (scalar !== undefined) {
      return scalar;
    }

    if (this._isArrowType(dataType, "Timestamp") || typeName.startsWith("Timestamp")) {
      const timezone = dataType.timezone === undefined ? null : dataType.timezone;
      return {
        "Timestamp": [this._timeUnitJson(dataType.unit, port, dataType), timezone],
      };
    }
    if (this._isArrowType(dataType, "Date") || typeName.startsWith("Date")) {
      return this._dateTypeJson(dataType.unit, port, dataType);
    }
    if (this._isArrowType(dataType, "Time") || typeName.startsWith("Time")) {
      const key = dataType.bitWidth === 64 || typeName.startsWith("Time64") ? "Time64" : "Time32";
      return { [key]: this._timeUnitJson(dataType.unit, port, dataType) };
    }
    if (this._isArrowType(dataType, "Decimal") || typeName.startsWith("Decimal")) {
      const key = dataType.bitWidth === 256 || typeName.startsWith("Decimal256")
        ? "Decimal256"
        : "Decimal128";
      return { [key]: [dataType.precision, dataType.scale] };
    }
    if (this._isArrowType(dataType, "Dictionary") || typeName.startsWith("Dictionary")) {
      const indexType = dataType.indices ?? dataType.indexType;
      const valueType = dataType.dictionary ?? dataType.valueType;
      if (indexType === undefined || valueType === undefined) {
        throw new Error(`SPUR port '${port}': unsupported Arrow type for manifest schema: ${dataType}`);
      }
      return {
        "Dictionary": [
          this._dataTypeJson(indexType, port),
          this._dataTypeJson(valueType, port),
        ],
      };
    }

    throw new Error(`SPUR port '${port}': unsupported Arrow type for manifest schema: ${dataType}`);
  }

  _isArrowType(dataType, typeName) {
    const typeId = dataType?.typeId;
    const typeEnum = _spurArrow.Type ?? {};
    const enumIds = [typeEnum[typeName], typeEnum[typeName.toUpperCase()]]
      .filter((value) => value !== undefined);
    return (
      dataType?.constructor?.name === typeName ||
      enumIds.includes(typeId)
    );
  }

  _timeUnitJson(unit, port, dataType) {
    const timeUnit = _spurArrow.TimeUnit ?? {};
    const enumUnits = [
      [timeUnit.SECOND, "Second"],
      [timeUnit.MILLISECOND, "Millisecond"],
      [timeUnit.MICROSECOND, "Microsecond"],
      [timeUnit.NANOSECOND, "Nanosecond"],
    ].filter(([key]) => key !== undefined);
    const units = new Map([
      ...enumUnits,
      [0, "Second"],
      [1, "Millisecond"],
      [2, "Microsecond"],
      [3, "Nanosecond"],
      ["SECOND", "Second"],
      ["MILLISECOND", "Millisecond"],
      ["MICROSECOND", "Microsecond"],
      ["NANOSECOND", "Nanosecond"],
      ["second", "Second"],
      ["millisecond", "Millisecond"],
      ["microsecond", "Microsecond"],
      ["nanosecond", "Nanosecond"],
      ["s", "Second"],
      ["ms", "Millisecond"],
      ["us", "Microsecond"],
      ["ns", "Nanosecond"],
    ]);
    const serdeUnit = units.get(unit) ?? units.get(String(unit));
    if (serdeUnit === undefined) {
      throw new Error(`SPUR port '${port}': unsupported Arrow time unit for manifest schema: ${dataType}`);
    }
    return serdeUnit;
  }

  _dateTypeJson(unit, port, dataType) {
    const dateUnit = _spurArrow.DateUnit ?? {};
    const enumUnits = [
      [dateUnit.DAY, "Date32"],
      [dateUnit.MILLISECOND, "Date64"],
    ].filter(([key]) => key !== undefined);
    const units = new Map([
      ...enumUnits,
      [0, "Date32"],
      [1, "Date64"],
      ["DAY", "Date32"],
      ["MILLISECOND", "Date64"],
      ["day", "Date32"],
      ["millisecond", "Date64"],
    ]);
    const serdeType = units.get(unit) ?? units.get(String(unit));
    if (serdeType === undefined) {
      throw new Error(`SPUR port '${port}': unsupported Arrow date unit for manifest schema: ${dataType}`);
    }
    return serdeType;
  }

  _previewHtml(port, version, table) {
    const rowCount = table.numRows ?? 0;
    const columnCount = table.numCols ?? table.schema.fields.length;
    const headers = table.schema.fields.map((field) => field.name);
    const title =
      `<strong>SPUR port</strong> <code>${this._escapeHtml(port)}</code> ` +
      `<span>v${version}</span>`;
    if (headers.length === 0) {
      return `<div>${title}<p>0 columns, ${rowCount} rows</p></div>`;
    }

    const rows = table.slice(0, Math.min(rowCount, 5)).toArray();
    const thead = headers
      .map((name) => `<th>${this._escapeHtml(name)}</th>`)
      .join("");
    const body = rows
      .map((row) => {
        const cells = headers
          .map((name) => `<td>${this._escapeHtml(row?.[name] ?? "")}</td>`)
          .join("");
        return `<tr>${cells}</tr>`;
      })
      .join("");
    return (
      `<div>${title}<p>${rowCount} rows x ${columnCount} columns</p>` +
      `<table><thead><tr>${thead}</tr></thead><tbody>${body}</tbody></table></div>`
    );
  }

  _escapeHtml(value) {
    return String(value)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  _isPlainObject(value) {
    return (
      value !== null &&
      typeof value === "object" &&
      (Object.getPrototypeOf(value) === Object.prototype ||
        Object.getPrototypeOf(value) === null)
    );
  }

  _expandHome(path, runtime) {
    if (path === "~") {
      return runtime.home();
    }
    if (path.startsWith("~/")) {
      return `${runtime.home()}${path.slice(1)}`;
    }
    return path;
  }
}

globalThis.spur = new _Spur({
  root: __SPUR_ROOT__,
  portsDir: __SPUR_PORTS_DIR__,
  manifestPath: __SPUR_MANIFEST_PATH__,
  mime: __SPUR_PORT_MIME__,
  runtime: _spurDenoRuntime,
});
// --- end SPUR port helper bootstrap ---
}
"#
    .replace("__SPUR_ROOT__", &root_literal)
    .replace("__SPUR_PORTS_DIR__", &ports_literal)
    .replace("__SPUR_MANIFEST_PATH__", &manifest_literal)
    .replace("__SPUR_PORT_MIME__", &mime_literal)
    .replace(
        "__SPUR_MANIFEST_TEMP_PREFIX__",
        &string_literal(&manifest_temp_prefix()),
    )
    .replace("__SPUR_TEMP_FILE_SUFFIX__", &string_literal(PORT_TEMP_FILE_SUFFIX))
    .replace("__SPUR_MANIFEST_PORTS_KEY__", PORT_MANIFEST_PORTS_KEY)
    .replace("__SPUR_MANIFEST_PATH_KEY__", PORT_MANIFEST_PATH_KEY)
    .replace("__SPUR_MANIFEST_VERSION_KEY__", PORT_MANIFEST_VERSION_KEY)
    .replace("__SPUR_MANIFEST_SCHEMA_KEY__", PORT_MANIFEST_SCHEMA_KEY)
    .replace("__SPUR_INITIAL_VERSION__", &PORT_INITIAL_VERSION.to_string())
    .replace(
        "__SPUR_VERSION_INCREMENT__",
        &PORT_VERSION_INCREMENT.to_string(),
    )
    .replace("__SPUR_VERSION_SEPARATOR__", PORT_FILE_VERSION_SEPARATOR)
    .replace("__SPUR_ARROW_EXTENSION__", PORT_ARROW_FILE_EXTENSION)
    .replace(
        "__SPUR_RESERVED_CURRENT_DIR__",
        &string_literal(PORT_RESERVED_CURRENT_DIR),
    )
    .replace(
        "__SPUR_RESERVED_PARENT_DIR__",
        &string_literal(PORT_RESERVED_PARENT_DIR),
    )
    .replace("__SPUR_FORBIDDEN_SLASH__", &string_literal(PORT_FORBIDDEN_SLASH))
    .replace(
        "__SPUR_FORBIDDEN_BACKSLASH__",
        &string_literal(PORT_FORBIDDEN_BACKSLASH),
    )
    .replace("__SPUR_FORBIDDEN_NUL__", &string_literal(PORT_FORBIDDEN_NUL))
}

/// Go/gonb bootstrap source that installs the `spur` helper for one cell.
pub fn go_bootstrap(notebook_root: impl AsRef<Path>) -> String {
    let root = notebook_root.as_ref().display().to_string();
    let ports_dir = ports_dir_for_root(notebook_root.as_ref())
        .display()
        .to_string();
    let manifest_path = manifest_path_for_root(notebook_root.as_ref())
        .display()
        .to_string();
    let root_literal = serde_json::to_string(&root).expect("path string serializes");
    let ports_literal = serde_json::to_string(&ports_dir).expect("path string serializes");
    let manifest_literal = serde_json::to_string(&manifest_path).expect("path string serializes");
    let mime_literal = serde_json::to_string(PORT_MIME).expect("mime string serializes");

    r#"
!*go get __SPUR_GONB_ARROW_GO_MODULE__
!*go get github.com/janpfeifer/gonb/gonbui

// --- SPUR port helper bootstrap ---
import "encoding/json"
import "errors"
import "fmt"
import "html"
import "os"
import "path/filepath"
import "strings"

import "__SPUR_GONB_ARROW_GO_MODULE__/arrow"
import "__SPUR_GONB_ARROW_GO_MODULE__/arrow/ipc"
import "github.com/janpfeifer/gonb/gonbui"
import "github.com/janpfeifer/gonb/gonbui/protocol"

const portMime = __SPUR_PORT_MIME__
const portsDirName = __SPUR_PORTS_DIR_NAME__
const portFileVersionSeparator = __SPUR_VERSION_SEPARATOR__
const portArrowFileExtension = __SPUR_ARROW_EXTENSION__
const portManifestFileName = __SPUR_MANIFEST_FILE_NAME__
const portManifestTempPrefix = __SPUR_MANIFEST_TEMP_PREFIX__
const portTempFileSuffix = __SPUR_TEMP_FILE_SUFFIX__
const portManifestPortsKey = __SPUR_MANIFEST_PORTS_KEY__
const portManifestPathKey = __SPUR_MANIFEST_PATH_KEY__
const portManifestVersionKey = __SPUR_MANIFEST_VERSION_KEY__
const portManifestSchemaKey = __SPUR_MANIFEST_SCHEMA_KEY__
const portInitialVersion uint64 = __SPUR_INITIAL_VERSION__
const portVersionIncrement uint64 = __SPUR_VERSION_INCREMENT__
const portReservedCurrentDir = __SPUR_RESERVED_CURRENT_DIR__
const portReservedParentDir = __SPUR_RESERVED_PARENT_DIR__
const portForbiddenSlash = __SPUR_FORBIDDEN_SLASH__
const portForbiddenBackslash = __SPUR_FORBIDDEN_BACKSLASH__
const portForbiddenNul = __SPUR_FORBIDDEN_NUL__

type spurPorts struct {
    root string
    portsDir string
    manifestPath string
    mime string
}

type spurManifest struct {
    Ports map[string]spurManifestEntry `json:"__SPUR_MANIFEST_PORTS_KEY_RAW__"`
}

type spurManifestEntry struct {
    Path string `json:"__SPUR_MANIFEST_PATH_KEY_RAW__"`
    Version uint64 `json:"__SPUR_MANIFEST_VERSION_KEY_RAW__"`
    Schema any `json:"__SPUR_MANIFEST_SCHEMA_KEY_RAW__"`
}

func newSpurPorts(root, portsDir, manifestPath, mime string) *spurPorts {
    _ = os.MkdirAll(portsDir, 0o755)
    return &spurPorts{
        root: root,
        portsDir: portsDir,
        manifestPath: manifestPath,
        mime: mime,
    }
}

func (s *spurPorts) Put(port string, batch arrow.Record) (map[string]any, error) {
    if batch == nil {
        return nil, errors.New("SPUR port batch cannot be nil")
    }
    if err := s.validatePort(port); err != nil {
        return nil, err
    }
    if err := os.MkdirAll(s.portsDir, 0o755); err != nil {
        return nil, err
    }

    manifest, err := s.loadManifest()
    if err != nil {
        return nil, err
    }
    schema, err := spurSchemaJSON(batch.Schema(), port)
    if err != nil {
        return nil, err
    }
    previousVersion := portInitialVersion
    if entry, ok := manifest.Ports[port]; ok {
        previousVersion = entry.Version
    }
    version := previousVersion + portVersionIncrement
    arrowPath := filepath.Join(
        s.portsDir,
        fmt.Sprintf("%s%s%d.%s", port, portFileVersionSeparator, version, portArrowFileExtension),
    )

    if err := s.writeArrowFile(arrowPath, batch); err != nil {
        return nil, err
    }

    manifest.Ports[port] = spurManifestEntry{
        Path: arrowPath,
        Version: version,
        Schema: schema,
    }
    if err := s.storeManifest(manifest); err != nil {
        return nil, err
    }

    payload := map[string]any{
        "port": port,
        portManifestVersionKey: version,
        portManifestSchemaKey: schema,
    }
    s.displayPort(port, version, schema, batch)
    return payload, nil
}

func (s *spurPorts) Get(port string) ([]arrow.Record, error) {
    if err := s.validatePort(port); err != nil {
        return nil, err
    }
    manifest, err := s.loadManifest()
    if err != nil {
        return nil, err
    }
    entry, ok := manifest.Ports[port]
    if !ok {
        return nil, fmt.Errorf("SPUR port has not been written: %s", port)
    }

    file, err := os.Open(entry.Path)
    if err != nil {
        return nil, err
    }
    defer file.Close()
    reader, err := ipc.NewFileReader(file)
    if err != nil {
        return nil, err
    }
    defer reader.Close()

    records := make([]arrow.Record, 0, reader.NumRecords())
    for i := 0; i < reader.NumRecords(); i++ {
        record, err := reader.RecordAt(i)
        if err != nil {
            return nil, err
        }
        records = append(records, record)
    }
    return records, nil
}

func (s *spurPorts) writeArrowFile(path string, batch arrow.Record) error {
    file, err := os.Create(path)
    if err != nil {
        return err
    }
    writer, err := ipc.NewFileWriter(file, ipc.WithSchema(batch.Schema()))
    if err != nil {
        _ = file.Close()
        return err
    }
    if err := writer.Write(batch); err != nil {
        _ = writer.Close()
        _ = file.Close()
        return err
    }
    if err := writer.Close(); err != nil {
        _ = file.Close()
        return err
    }
    return file.Close()
}

func (s *spurPorts) loadManifest() (spurManifest, error) {
    manifest := spurManifest{Ports: map[string]spurManifestEntry{}}
    data, err := os.ReadFile(s.manifestPath)
    if errors.Is(err, os.ErrNotExist) {
        return manifest, nil
    }
    if err != nil {
        return manifest, err
    }
    if err := json.Unmarshal(data, &manifest); err != nil {
        return manifest, err
    }
    if manifest.Ports == nil {
        manifest.Ports = map[string]spurManifestEntry{}
    }
    return manifest, nil
}

func (s *spurPorts) storeManifest(manifest spurManifest) error {
    tmp, err := os.CreateTemp(
        s.portsDir,
        fmt.Sprintf("%s*%s", portManifestTempPrefix, portTempFileSuffix),
    )
    if err != nil {
        return err
    }
    tmpPath := tmp.Name()
    cleanup := true
    defer func() {
        if cleanup {
            _ = os.Remove(tmpPath)
        }
    }()

    encoder := json.NewEncoder(tmp)
    encoder.SetIndent("", "  ")
    if err := encoder.Encode(manifest); err != nil {
        _ = tmp.Close()
        return err
    }
    if err := tmp.Close(); err != nil {
        return err
    }
    if err := os.Rename(tmpPath, s.manifestPath); err != nil {
        return err
    }
    cleanup = false
    return nil
}

func (s *spurPorts) validatePort(port string) error {
    if port == "" {
        return errors.New("SPUR port name cannot be empty")
    }
    if port == portReservedCurrentDir ||
        port == portReservedParentDir ||
        strings.Contains(port, portForbiddenSlash) ||
        strings.Contains(port, portForbiddenBackslash) ||
        strings.Contains(port, portForbiddenNul) {
        return fmt.Errorf("SPUR port name is not valid for an on-disk port file: %s", port)
    }
    return nil
}

func (s *spurPorts) displayPort(port string, version uint64, schema map[string]any, batch arrow.Record) {
    gonbui.SendData(&protocol.DisplayData{
        Data: map[protocol.MIMEType]any{
            protocol.MIMEType(s.mime): map[string]any{
                "port": port,
                portManifestVersionKey: version,
                portManifestSchemaKey: schema,
            },
            protocol.MIMETextHTML: s.previewHTML(port, version, batch),
        },
    })
    gonbui.Sync()
}

func (s *spurPorts) previewHTML(port string, version uint64, batch arrow.Record) string {
    return fmt.Sprintf(
        "<div><strong>SPUR port</strong> <code>%s</code> <span>v%d</span><p>%d rows x %d columns</p></div>",
        html.EscapeString(port),
        version,
        batch.NumRows(),
        batch.NumCols(),
    )
}

func spurSchemaJSON(schema *arrow.Schema, port string) (map[string]any, error) {
    if schema == nil {
        return nil, fmt.Errorf("SPUR port %q: Arrow schema cannot be nil", port)
    }
    fields := make([]map[string]any, 0, schema.NumFields())
    for _, field := range schema.Fields() {
        dataType, err := spurDataTypeJSON(field.Type, port)
        if err != nil {
            return nil, err
        }
        fields = append(fields, map[string]any{
            "name": field.Name,
            "data_type": dataType,
            "nullable": field.Nullable,
            "dict_id": 0,
            "dict_is_ordered": false,
            "metadata": spurMetadataJSON(field.Metadata),
        })
    }
    return map[string]any{
        "fields": fields,
        "metadata": spurMetadataJSON(schema.Metadata()),
    }, nil
}

func spurMetadataJSON(metadata arrow.Metadata) map[string]string {
    output := map[string]string{}
    for key, value := range metadata.ToMap() {
        output[key] = value
    }
    return output
}

func spurDataTypeJSON(dataType arrow.DataType, port string) (any, error) {
    switch t := dataType.(type) {
    case *arrow.BooleanType:
        return "Boolean", nil
    case *arrow.Int8Type:
        return "Int8", nil
    case *arrow.Int16Type:
        return "Int16", nil
    case *arrow.Int32Type:
        return "Int32", nil
    case *arrow.Int64Type:
        return "Int64", nil
    case *arrow.Uint8Type:
        return "UInt8", nil
    case *arrow.Uint16Type:
        return "UInt16", nil
    case *arrow.Uint32Type:
        return "UInt32", nil
    case *arrow.Uint64Type:
        return "UInt64", nil
    case *arrow.Float16Type:
        return "Float16", nil
    case *arrow.Float32Type:
        return "Float32", nil
    case *arrow.Float64Type:
        return "Float64", nil
    case *arrow.StringType:
        return "Utf8", nil
    case *arrow.LargeStringType:
        return "LargeUtf8", nil
    case *arrow.BinaryType:
        return "Binary", nil
    case *arrow.LargeBinaryType:
        return "LargeBinary", nil
    case *arrow.NullType:
        return "Null", nil
    case *arrow.Date32Type:
        return "Date32", nil
    case *arrow.Date64Type:
        return "Date64", nil
    case *arrow.TimestampType:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        var timezone any
        if t.TimeZone != "" {
            timezone = t.TimeZone
        }
        return map[string]any{"Timestamp": []any{unit, timezone}}, nil
    case *arrow.Time32Type:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Time32": unit}, nil
    case *arrow.Time64Type:
        unit, err := spurTimeUnitJSON(t.Unit, port, dataType)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Time64": unit}, nil
    case *arrow.Decimal128Type:
        return map[string]any{"Decimal128": []any{t.GetPrecision(), t.GetScale()}}, nil
    case *arrow.Decimal256Type:
        return map[string]any{"Decimal256": []any{t.GetPrecision(), t.GetScale()}}, nil
    case *arrow.DictionaryType:
        index, err := spurDataTypeJSON(t.IndexType, port)
        if err != nil {
            return nil, err
        }
        value, err := spurDataTypeJSON(t.ValueType, port)
        if err != nil {
            return nil, err
        }
        return map[string]any{"Dictionary": []any{index, value}}, nil
    default:
        return nil, fmt.Errorf("SPUR port %q: unsupported Arrow type for manifest schema: %s", port, dataType)
    }
}

func spurTimeUnitJSON(unit arrow.TimeUnit, port string, dataType arrow.DataType) (string, error) {
    switch unit {
    case arrow.Second:
        return "Second", nil
    case arrow.Millisecond:
        return "Millisecond", nil
    case arrow.Microsecond:
        return "Microsecond", nil
    case arrow.Nanosecond:
        return "Nanosecond", nil
    default:
        return "", fmt.Errorf("SPUR port %q: unsupported Arrow time unit for manifest schema: %s", port, dataType)
    }
}

var spur = newSpurPorts(
    __SPUR_ROOT__,
    __SPUR_PORTS_DIR__,
    __SPUR_MANIFEST_PATH__,
    portMime,
)
// --- end SPUR port helper bootstrap ---
"#
    .replace("__SPUR_GONB_ARROW_GO_MODULE__", GONB_ARROW_GO_MODULE)
    .replace("__SPUR_ROOT__", &root_literal)
    .replace("__SPUR_PORTS_DIR__", &ports_literal)
    .replace("__SPUR_MANIFEST_PATH__", &manifest_literal)
    .replace("__SPUR_PORT_MIME__", &mime_literal)
    .replace("__SPUR_PORTS_DIR_NAME__", &string_literal(PORTS_DIR_NAME))
    .replace(
        "__SPUR_VERSION_SEPARATOR__",
        &string_literal(PORT_FILE_VERSION_SEPARATOR),
    )
    .replace(
        "__SPUR_ARROW_EXTENSION__",
        &string_literal(PORT_ARROW_FILE_EXTENSION),
    )
    .replace(
        "__SPUR_MANIFEST_FILE_NAME__",
        &string_literal(PORT_MANIFEST_FILE_NAME),
    )
    .replace(
        "__SPUR_MANIFEST_TEMP_PREFIX__",
        &string_literal(&manifest_temp_prefix()),
    )
    .replace(
        "__SPUR_TEMP_FILE_SUFFIX__",
        &string_literal(PORT_TEMP_FILE_SUFFIX),
    )
    .replace(
        "__SPUR_MANIFEST_PORTS_KEY_RAW__",
        PORT_MANIFEST_PORTS_KEY,
    )
    .replace("__SPUR_MANIFEST_PATH_KEY_RAW__", PORT_MANIFEST_PATH_KEY)
    .replace(
        "__SPUR_MANIFEST_VERSION_KEY_RAW__",
        PORT_MANIFEST_VERSION_KEY,
    )
    .replace("__SPUR_MANIFEST_SCHEMA_KEY_RAW__", PORT_MANIFEST_SCHEMA_KEY)
    .replace(
        "__SPUR_MANIFEST_PORTS_KEY__",
        &string_literal(PORT_MANIFEST_PORTS_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_PATH_KEY__",
        &string_literal(PORT_MANIFEST_PATH_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_VERSION_KEY__",
        &string_literal(PORT_MANIFEST_VERSION_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_SCHEMA_KEY__",
        &string_literal(PORT_MANIFEST_SCHEMA_KEY),
    )
    .replace(
        "__SPUR_INITIAL_VERSION__",
        &PORT_INITIAL_VERSION.to_string(),
    )
    .replace(
        "__SPUR_VERSION_INCREMENT__",
        &PORT_VERSION_INCREMENT.to_string(),
    )
    .replace(
        "__SPUR_RESERVED_CURRENT_DIR__",
        &string_literal(PORT_RESERVED_CURRENT_DIR),
    )
    .replace(
        "__SPUR_RESERVED_PARENT_DIR__",
        &string_literal(PORT_RESERVED_PARENT_DIR),
    )
    .replace(
        "__SPUR_FORBIDDEN_SLASH__",
        &string_literal(PORT_FORBIDDEN_SLASH),
    )
    .replace(
        "__SPUR_FORBIDDEN_BACKSLASH__",
        &string_literal(PORT_FORBIDDEN_BACKSLASH),
    )
    .replace(
        "__SPUR_FORBIDDEN_NUL__",
        &serde_json::to_string(PORT_FORBIDDEN_NUL).expect("nul string serializes"),
    )
}

/// Rust/evcxr bootstrap source that installs the `spur` helper for one cell.
pub fn rust_bootstrap(notebook_root: impl AsRef<Path>) -> String {
    let root = notebook_root.as_ref().display().to_string();
    let root_literal = string_literal(&root);

    r#":dep arrow = "__SPUR_EVCXR_ARROW_CRATE_VERSION__"
:dep serde_json = "1"

let spur = {
    use std::fs;
    use std::fs::File;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use arrow::ipc::reader::FileReader;
    use arrow::ipc::writer::FileWriter;
    use arrow::record_batch::RecordBatch;
    use serde_json::{Map, Number, Value};

    const PORT_MIME: &str = __SPUR_PORT_MIME__;
    const PORTS_DIR_NAME: &str = __SPUR_PORTS_DIR_NAME__;
    const PORT_FILE_VERSION_SEPARATOR: &str = __SPUR_VERSION_SEPARATOR__;
    const PORT_ARROW_FILE_EXTENSION: &str = __SPUR_ARROW_EXTENSION__;
    const PORT_MANIFEST_FILE_NAME: &str = __SPUR_MANIFEST_FILE_NAME__;
    const PORT_MANIFEST_TEMP_PREFIX: &str = __SPUR_MANIFEST_TEMP_PREFIX__;
    const PORT_TEMP_FILE_SUFFIX: &str = __SPUR_TEMP_FILE_SUFFIX__;
    const PORT_MANIFEST_PORTS_KEY: &str = __SPUR_MANIFEST_PORTS_KEY__;
    const PORT_MANIFEST_PATH_KEY: &str = __SPUR_MANIFEST_PATH_KEY__;
    const PORT_MANIFEST_VERSION_KEY: &str = __SPUR_MANIFEST_VERSION_KEY__;
    const PORT_MANIFEST_SCHEMA_KEY: &str = __SPUR_MANIFEST_SCHEMA_KEY__;
    const PORT_INITIAL_VERSION: u64 = __SPUR_INITIAL_VERSION__;
    const PORT_VERSION_INCREMENT: u64 = __SPUR_VERSION_INCREMENT__;
    const PORT_RESERVED_CURRENT_DIR: &str = __SPUR_RESERVED_CURRENT_DIR__;
    const PORT_RESERVED_PARENT_DIR: &str = __SPUR_RESERVED_PARENT_DIR__;
    const PORT_FORBIDDEN_SLASH: &str = __SPUR_FORBIDDEN_SLASH__;
    const PORT_FORBIDDEN_BACKSLASH: &str = __SPUR_FORBIDDEN_BACKSLASH__;
    const PORT_FORBIDDEN_NUL: &str = __SPUR_FORBIDDEN_NUL__;

    #[derive(Clone, Debug)]
    struct _Spur {
        root: String,
    }

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
            let schema = serde_json::to_value(batch.schema().as_ref())?;
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
            evcxr_display(PORT_MIME, &serde_json::to_string(&payload)?);
            evcxr_display("text/html", &self.preview_html(port, version, batch));
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

    _Spur::new(__SPUR_ROOT__)
};
"#
    .replace(
        "__SPUR_EVCXR_ARROW_CRATE_VERSION__",
        EVCXR_ARROW_CRATE_VERSION,
    )
    .replace("__SPUR_ROOT__", &root_literal)
    .replace("__SPUR_PORT_MIME__", &string_literal(PORT_MIME))
    .replace("__SPUR_PORTS_DIR_NAME__", &string_literal(PORTS_DIR_NAME))
    .replace(
        "__SPUR_VERSION_SEPARATOR__",
        &string_literal(PORT_FILE_VERSION_SEPARATOR),
    )
    .replace(
        "__SPUR_ARROW_EXTENSION__",
        &string_literal(PORT_ARROW_FILE_EXTENSION),
    )
    .replace(
        "__SPUR_MANIFEST_FILE_NAME__",
        &string_literal(PORT_MANIFEST_FILE_NAME),
    )
    .replace(
        "__SPUR_MANIFEST_TEMP_PREFIX__",
        &string_literal(&manifest_temp_prefix()),
    )
    .replace(
        "__SPUR_TEMP_FILE_SUFFIX__",
        &string_literal(PORT_TEMP_FILE_SUFFIX),
    )
    .replace(
        "__SPUR_MANIFEST_PORTS_KEY__",
        &string_literal(PORT_MANIFEST_PORTS_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_PATH_KEY__",
        &string_literal(PORT_MANIFEST_PATH_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_VERSION_KEY__",
        &string_literal(PORT_MANIFEST_VERSION_KEY),
    )
    .replace(
        "__SPUR_MANIFEST_SCHEMA_KEY__",
        &string_literal(PORT_MANIFEST_SCHEMA_KEY),
    )
    .replace(
        "__SPUR_INITIAL_VERSION__",
        &PORT_INITIAL_VERSION.to_string(),
    )
    .replace(
        "__SPUR_VERSION_INCREMENT__",
        &PORT_VERSION_INCREMENT.to_string(),
    )
    .replace(
        "__SPUR_RESERVED_CURRENT_DIR__",
        &string_literal(PORT_RESERVED_CURRENT_DIR),
    )
    .replace(
        "__SPUR_RESERVED_PARENT_DIR__",
        &string_literal(PORT_RESERVED_PARENT_DIR),
    )
    .replace(
        "__SPUR_FORBIDDEN_SLASH__",
        &string_literal(PORT_FORBIDDEN_SLASH),
    )
    .replace(
        "__SPUR_FORBIDDEN_BACKSLASH__",
        &string_literal(PORT_FORBIDDEN_BACKSLASH),
    )
    .replace(
        "__SPUR_FORBIDDEN_NUL__",
        &string_literal(PORT_FORBIDDEN_NUL),
    )
}

/// Prepend the Python SPUR port bootstrap to user code.
pub fn wrap_python_cell(notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = python_bootstrap(notebook_root);
    wrapped.push('\n');
    wrapped.push_str(code);
    wrapped
}

/// Prepend the JavaScript/Deno SPUR port bootstrap to user code.
pub fn wrap_js_cell(notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = javascript_bootstrap(notebook_root);
    wrapped.push('\n');
    wrapped.push_str(code);
    wrapped
}

/// Prepend the Go/gonb SPUR port bootstrap to user code.
pub fn wrap_go_cell(notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = go_bootstrap(notebook_root);
    wrapped.push('\n');
    wrapped.push_str(code);
    wrapped
}

/// Prepend the Rust/evcxr SPUR port bootstrap to user code.
pub fn wrap_rust_cell(notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = rust_bootstrap(notebook_root);
    wrapped.push('\n');
    wrapped.push_str(code);
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::{Command, Stdio};

    #[test]
    fn wrap_python_cell_installs_spur_helper_and_keeps_user_code() {
        let wrapped = wrap_python_cell("/tmp/demo-root", "spur.put('sales', [1, 2])");

        assert!(wrapped.contains("class _Spur"));
        assert!(wrapped.contains("spur = _Spur"));
        assert!(wrapped.contains(PORT_MIME));
        assert!(wrapped.ends_with("spur.put('sales', [1, 2])"));
    }

    #[test]
    fn wrap_js_cell_installs_spur_helper_and_keeps_user_code() {
        let notebook_path = "/tmp/demo-notebook.ipynb";
        let root = notebook_port_root(notebook_path);
        let wrapped = wrap_js_cell(&root, "await spur.put('sales', [{ id: 1 }]);");
        let root_literal = serde_json::to_string(&root.display().to_string()).unwrap();
        let ports_literal =
            serde_json::to_string(&root.join("ports").display().to_string()).unwrap();

        assert!(wrapped.contains("globalThis.spur"));
        assert!(wrapped.contains("npm:apache-arrow@21.1.0"));
        assert!(wrapped.contains(PORT_MIME));
        assert!(wrapped.contains(&root_literal));
        assert!(wrapped.contains(&ports_literal));
        assert!(root.display().to_string().contains("/nb-"));
        assert!(wrapped.ends_with("await spur.put('sales', [{ id: 1 }]);"));
    }

    #[test]
    fn rust_bootstrap_pulls_arrow_and_uses_shared_port_paths() {
        let bootstrap = rust_bootstrap("/tmp/demo-root");
        let wrapped = wrap_rust_cell("/tmp/demo-root", "spur.put(\"sales\", batch)?;");
        let arrow_dep = format!(
            r#":dep arrow = "{}""#,
            crate::kernel_provision::EVCXR_ARROW_CRATE_VERSION
        );

        assert!(bootstrap.contains(&arrow_dep));
        assert!(bootstrap.contains(r#":dep serde_json = "1""#));
        assert!(bootstrap.contains(PORT_MIME));
        assert!(bootstrap.contains(r#""ports""#));
        assert!(bootstrap.contains(r#""manifest.json""#));
        assert!(bootstrap.contains(r#"const PORT_FILE_VERSION_SEPARATOR: &str = "@v";"#));
        assert!(bootstrap.contains(r#"const PORT_ARROW_FILE_EXTENSION: &str = "arrow";"#));
        assert!(bootstrap.contains("FileWriter::try_new"));
        assert!(bootstrap.contains("FileReader::try_new"));
        assert!(bootstrap.contains("const PORT_VERSION_INCREMENT: u64 = 1;"));
        assert!(bootstrap.contains("previous_version + PORT_VERSION_INCREMENT"));
        assert!(wrapped.contains(&arrow_dep));
        assert!(wrapped.ends_with("spur.put(\"sales\", batch)?;"));
    }

    #[test]
    fn go_bootstrap_pulls_arrow_go_and_uses_shared_port_paths() {
        let bootstrap = go_bootstrap("/tmp/demo-root");
        let wrapped = wrap_go_cell("/tmp/demo-root", "spur.Put(\"sales\", batch)");
        let arrow_dep = format!("!*go get {}", crate::kernel_provision::GONB_ARROW_GO_MODULE);
        let arrow_ipc_import = format!(
            "import \"{}/arrow/ipc\"",
            crate::kernel_provision::GONB_ARROW_GO_MODULE
        );
        let root_literal = serde_json::to_string("/tmp/demo-root").unwrap();
        let ports_literal =
            serde_json::to_string(&ports_dir_for_root("/tmp/demo-root").display().to_string())
                .unwrap();
        let manifest_literal = serde_json::to_string(
            &manifest_path_for_root("/tmp/demo-root")
                .display()
                .to_string(),
        )
        .unwrap();

        assert!(bootstrap.contains(&arrow_dep));
        assert!(bootstrap.contains(&arrow_ipc_import));
        assert!(bootstrap.contains("func (s *spurPorts) Put"));
        assert!(bootstrap.contains("func (s *spurPorts) Get"));
        assert!(bootstrap.contains("var spur = newSpurPorts"));
        assert!(bootstrap.contains(PORT_MIME));
        assert!(bootstrap.contains(&root_literal));
        assert!(bootstrap.contains(&ports_literal));
        assert!(bootstrap.contains(&manifest_literal));
        assert!(bootstrap.contains(r#""ports""#));
        assert!(bootstrap.contains(r#""manifest.json""#));
        assert!(bootstrap.contains(r#"const portFileVersionSeparator = "@v""#));
        assert!(bootstrap.contains(r#"const portArrowFileExtension = "arrow""#));
        assert!(bootstrap.contains("ipc.NewFileWriter"));
        assert!(bootstrap.contains("ipc.NewFileReader"));
        assert!(bootstrap.contains("const portVersionIncrement uint64 = 1"));
        assert!(bootstrap.contains("previousVersion + portVersionIncrement"));
        assert!(wrapped.contains(&arrow_dep));
        assert!(wrapped.ends_with("spur.Put(\"sales\", batch)"));
    }

    #[test]
    fn generated_schema_shape_matches_arrow_schema_serde() {
        let schema: arrow_schema::Schema = serde_json::from_value(serde_json::json!({
            "fields": [{
                "name": "id",
                "data_type": "Int64",
                "nullable": false,
                "dict_id": 0,
                "dict_is_ordered": false,
                "metadata": {}
            }],
            "metadata": {}
        }))
        .expect("generated Python schema JSON matches arrow_schema serde");

        assert_eq!("id", schema.field(0).name());
    }

    #[test]
    fn generated_helpers_fail_loud_instead_of_utf8_fallback() {
        let python = python_bootstrap("/tmp/demo-root");
        assert!(python.contains("unsupported Arrow type for manifest schema"));
        assert!(!python.contains(r#"scalars.get(type_name, "Utf8")"#));

        let javascript = javascript_bootstrap("/tmp/demo-root");
        assert!(javascript.contains("unsupported Arrow type for manifest schema"));
        assert!(!javascript.contains(r#"?? "Utf8""#));
    }

    #[test]
    fn python_helper_round_trips_arrow_and_emits_display_mirror() {
        if !python_has_pyarrow() {
            eprintln!("skipping Python helper integration test: pyarrow is not installed");
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let bootstrap = python_bootstrap(dir.path());
        let script = format!(
            r#"
import json
import pathlib
import sys
import types

captured = []
display_mod = types.ModuleType("IPython.display")
display_mod.display = lambda bundle, raw=False: captured.append((bundle, raw))
ipython_mod = types.ModuleType("IPython")
ipython_mod.display = display_mod
sys.modules["IPython"] = ipython_mod
sys.modules["IPython.display"] = display_mod

{bootstrap}

import pyarrow as pa

result = spur.put("sales", pa.table({{"id": [1, 2], "label": ["alpha", "beta"]}}))
round_trip = spur.get("sales")
if hasattr(round_trip, "to_pylist"):
    rows = round_trip.to_pylist()
else:
    rows = round_trip.to_dict("records")

manifest = json.loads((pathlib.Path({root}) / "ports" / "manifest.json").read_text())
bundle, raw = captured[-1]
assert raw is True
assert "{mime}" in bundle
assert "text/html" in bundle
assert bundle["{mime}"]["port"] == "sales"
assert bundle["{mime}"]["version"] == 1
assert bundle["{mime}"]["schema"]["fields"][0]["name"] == "id"
assert result["version"] == 1
assert rows[0]["id"] == 1
assert rows[1]["label"] == "beta"
assert manifest["ports"]["sales"]["version"] == 1
assert pathlib.Path(manifest["ports"]["sales"]["path"]).exists()
"#,
            bootstrap = bootstrap,
            root = serde_json::to_string(&dir.path().display().to_string()).unwrap(),
            mime = PORT_MIME
        );

        let output = Command::new("python3")
            .arg("-c")
            .arg(script)
            .output()
            .expect("python3 runs");

        assert!(
            output.status.success(),
            "python helper failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn python_has_pyarrow() -> bool {
        Command::new("python3")
            .arg("-c")
            .arg("import pyarrow")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}
