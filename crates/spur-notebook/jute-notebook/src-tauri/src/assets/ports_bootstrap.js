
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

const _spurRoot = Deno.env.get("SPUR_NOTEBOOK_PORT_ROOT");
if (_spurRoot === undefined) {
  throw new Error("SPUR_NOTEBOOK_PORT_ROOT is not set");
}

globalThis.spur ??= new _Spur({
  root: _spurRoot,
  portsDir: `${_spurRoot}/ports`,
  manifestPath: `${_spurRoot}/ports/manifest.json`,
  mime: "application/vnd.spur.port+json",
  runtime: _spurDenoRuntime,
});
// --- end SPUR port helper bootstrap ---
}
