
# --- SPUR port helper bootstrap ---
import html as _spur_html
import json as _spur_json
import os as _spur_os
import pathlib as _spur_pathlib
import tempfile as _spur_tempfile

class _Spur:
    _MIME = "application/vnd.spur.port+json"

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
        version = int(manifest.get("ports", {}).get(port, {}).get("version", 0)) + 1
        arrow_path = self._ports_dir / f"{port}@v{version}.arrow"

        with _spur_ipc.new_file(str(arrow_path), table.schema) as writer:
            writer.write_table(table)

        schema = self._schema_json(table.schema, port)
        manifest.setdefault("ports", {})[port] = {
            "path": str(arrow_path),
            "version": version,
            "schema": schema,
        }
        self._store_manifest(manifest)

        bundle = {
            self._MIME: {
                "port": port,
                "version": version,
                "schema": schema,
            },
            "text/html": self._preview_html(port, version, table),
        }
        from IPython.display import display as _spur_display
        _spur_display(bundle, raw=True)
        return {"port": port, "version": version, "schema": schema}

    def get(self, port):
        self._validate_port(port)
        import pyarrow.ipc as _spur_ipc

        entry = self._load_manifest().get("ports", {}).get(port)
        if entry is None:
            raise KeyError(f"SPUR port has not been written: {port}")
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
                    return pa.table({"value": pa.array(value)})
                if value.ndim == 2:
                    return pa.table({f"c{i}": pa.array(value[:, i]) for i in range(value.shape[1])})
        except Exception:
            pass
        if isinstance(value, dict):
            return pa.table(value)
        if isinstance(value, (list, tuple)) and value and isinstance(value[0], dict):
            return pa.Table.from_pylist(value)
        return pa.table({"value": value})

    def _load_manifest(self):
        if not self._manifest_path.exists():
            return {"ports": {}}
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
            raise ValueError(f"SPUR port name is not valid for an on-disk port file: {port}")

    def _schema_json(self, schema, port):
        return {
            "fields": [self._field_json(field, port) for field in schema],
            "metadata": self._metadata_json(schema.metadata),
        }

    def _field_json(self, field, port):
        return {
            "name": field.name,
            "data_type": self._data_type_json(field.type, port),
            "nullable": field.nullable,
            "dict_id": 0,
            "dict_is_ordered": False,
            "metadata": self._metadata_json(field.metadata),
        }

    def _metadata_json(self, metadata):
        if not metadata:
            return {}
        return {
            self._decode_metadata_key(k): self._decode_metadata_value(v)
            for k, v in metadata.items()
        }

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
            return {"Timestamp": [self._time_unit_json(data_type.unit, port, data_type), data_type.tz]}
        if _spur_pa.types.is_time32(data_type):
            return {"Time32": self._time_unit_json(data_type.unit, port, data_type)}
        if _spur_pa.types.is_time64(data_type):
            return {"Time64": self._time_unit_json(data_type.unit, port, data_type)}
        if _spur_pa.types.is_decimal128(data_type):
            return {"Decimal128": [data_type.precision, data_type.scale]}
        if _spur_pa.types.is_decimal256(data_type):
            return {"Decimal256": [data_type.precision, data_type.scale]}
        if _spur_pa.types.is_dictionary(data_type):
            return {
                "Dictionary": [
                    self._data_type_json(data_type.index_type, port),
                    self._data_type_json(data_type.value_type, port),
                ]
            }
        raise TypeError(f"SPUR port '{port}': unsupported Arrow type for manifest schema: {data_type}")

    def _time_unit_json(self, unit, port, data_type):
        units = {
            "s": "Second",
            "ms": "Millisecond",
            "us": "Microsecond",
            "ns": "Nanosecond",
        }
        try:
            return units[str(unit)]
        except KeyError:
            raise TypeError(f"SPUR port '{port}': unsupported Arrow time unit for manifest schema: {data_type}") from None

    def _preview_html(self, port, version, table):
        rows = table.slice(0, min(table.num_rows, 5)).to_pylist()
        headers = [field.name for field in table.schema]
        title = (
            f"<strong>SPUR port</strong> "
            f"<code>{_spur_html.escape(port)}</code> "
            f"<span>v{version}</span>"
        )
        if not headers:
            return f"<div>{title}<p>0 columns, {table.num_rows} rows</p></div>"
        thead = "".join(f"<th>{_spur_html.escape(name)}</th>" for name in headers)
        body_rows = []
        for row in rows:
            cells = "".join(
                f"<td>{_spur_html.escape(str(row.get(name, '')))}</td>"
                for name in headers
            )
            body_rows.append(f"<tr>{cells}</tr>")
        body = "".join(body_rows)
        return (
            f"<div>{title}<p>{table.num_rows} rows x {table.num_columns} columns</p>"
            f"<table><thead><tr>{thead}</tr></thead><tbody>{body}</tbody></table></div>"
        )

if "spur" not in globals():
    spur = _Spur(_spur_os.environ["SPUR_NOTEBOOK_PORT_ROOT"])
# --- end SPUR port helper bootstrap ---
