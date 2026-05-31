//! SPUR port bootstrap helpers injected into notebook cells before dispatch.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// MIME type used for SPUR port display payloads.
pub const PORT_MIME: &str = "application/vnd.spur.port+json";

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
        self._ports_dir = self._root / "ports"
        self._manifest_path = self._ports_dir / "manifest.json"
        self._ports_dir.mkdir(parents=True, exist_ok=True)

    def put(self, port, value):
        self._validate_port(port)
        import pyarrow as _spur_pa
        import pyarrow.ipc as _spur_ipc

        table = self._to_table(value, _spur_pa)
        manifest = self._load_manifest()
        version = int(manifest.get("ports", {{}}).get(port, {{}}).get("version", 0)) + 1
        arrow_path = self._ports_dir / f"{{port}}@v{{version}}.arrow"

        with _spur_ipc.new_file(str(arrow_path), table.schema) as writer:
            writer.write_table(table)

        schema = self._schema_json(table.schema, port)
        manifest.setdefault("ports", {{}})[port] = {{
            "path": str(arrow_path),
            "version": version,
            "schema": schema,
        }}
        self._store_manifest(manifest)

        bundle = {{
            self._MIME: {{
                "port": port,
                "version": version,
                "schema": schema,
            }},
            "text/html": self._preview_html(port, version, table),
        }}
        from IPython.display import display as _spur_display
        _spur_display(bundle, raw=True)
        return {{"port": port, "version": version, "schema": schema}}

    def get(self, port):
        self._validate_port(port)
        import pyarrow.ipc as _spur_ipc

        entry = self._load_manifest().get("ports", {{}}).get(port)
        if entry is None:
            raise KeyError(f"SPUR port has not been written: {{port}}")
        with _spur_ipc.open_file(entry["path"]) as reader:
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
            return {{"ports": {{}}}}
        return _spur_json.loads(self._manifest_path.read_text(encoding="utf-8"))

    def _store_manifest(self, manifest):
        fd, tmp_name = _spur_tempfile.mkstemp(
            prefix="manifest.json.",
            suffix=".tmp",
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
        if port in (".", "..") or "/" in port or "\\" in port or "\0" in port:
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
        root = root_literal
    )
}

/// JavaScript/Deno bootstrap source that installs the `globalThis.spur` helper for one cell.
pub fn javascript_bootstrap(notebook_root: impl AsRef<Path>) -> String {
    let root = notebook_root.as_ref().display().to_string();
    let ports_dir = notebook_root.as_ref().join("ports").display().to_string();
    let manifest_path = notebook_root
        .as_ref()
        .join("ports")
        .join("manifest.json")
        .display()
        .to_string();
    let root_literal = serde_json::to_string(&root).expect("path string serializes");
    let ports_literal = serde_json::to_string(&ports_dir).expect("path string serializes");
    let manifest_literal = serde_json::to_string(&manifest_path).expect("path string serializes");
    let mime_literal = serde_json::to_string(PORT_MIME).expect("mime string serializes");

    r#"
// --- SPUR port helper bootstrap ---
const _spurArrow = await import("npm:apache-arrow");
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
        prefix: "manifest.json.",
        suffix: ".tmp",
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
    const entry = this._loadManifest().ports?.[port];
    if (entry === undefined) {
      throw new Error(`SPUR port has not been written: ${port}`);
    }
    return tableFromIPC(this._runtime.fs.readBytes(entry.path));
  }

  put(port, value) {
    this._validatePort(port);
    const table = this._toTable(value);
    const manifest = this._loadManifest();
    const version = Number(manifest.ports?.[port]?.version ?? 0) + 1;
    const arrowPath = `${this._portsDir}/${port}@v${version}.arrow`;

    this._runtime.fs.writeBytes(arrowPath, tableToIPC(table, "file"));

    const schema = this._schemaJson(table.schema, port);
    manifest.ports ??= {};
    manifest.ports[port] = {
      path: arrowPath,
      version,
      schema,
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
      return { ports: {} };
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
      port === "." ||
      port === ".." ||
      port.includes("/") ||
      port.includes("\\") ||
      port.includes("\0")
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
"#
    .replace("__SPUR_ROOT__", &root_literal)
    .replace("__SPUR_PORTS_DIR__", &ports_literal)
    .replace("__SPUR_MANIFEST_PATH__", &manifest_literal)
    .replace("__SPUR_PORT_MIME__", &mime_literal)
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
        assert!(wrapped.contains("npm:apache-arrow"));
        assert!(wrapped.contains(PORT_MIME));
        assert!(wrapped.contains(&root_literal));
        assert!(wrapped.contains(&ports_literal));
        assert!(root.display().to_string().contains("/nb-"));
        assert!(wrapped.ends_with("await spur.put('sales', [{ id: 1 }]);"));
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
