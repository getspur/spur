use std::path::{Path, PathBuf};

use directories::BaseDirs;

pub const PORT_MIME: &str = "application/vnd.spur.port+json";

pub fn notebook_id_for_path(path: impl AsRef<Path>) -> String {
    let normalized = path.as_ref().to_string_lossy();
    let digest = blake3::hash(normalized.as_bytes()).to_hex();
    format!("nb-{}", &digest[..24])
}

pub fn notebook_port_root(path: impl AsRef<Path>) -> PathBuf {
    let notebook_id = notebook_id_for_path(path);
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".spur/notebooks").join(&notebook_id))
        .unwrap_or_else(|| PathBuf::from(".spur/notebooks").join(notebook_id))
}

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

        schema = self._schema_json(table.schema)
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

    def _schema_json(self, schema):
        return {{
            "fields": [self._field_json(field) for field in schema],
            "metadata": self._metadata_json(schema.metadata),
        }}

    def _field_json(self, field):
        return {{
            "name": field.name,
            "data_type": self._data_type_json(field.type),
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

    def _data_type_json(self, data_type):
        type_name = str(data_type)
        scalars = {{
            "bool": "Boolean",
            "int8": "Int8",
            "int16": "Int16",
            "int32": "Int32",
            "int64": "Int64",
            "uint8": "UInt8",
            "uint16": "UInt16",
            "uint32": "UInt32",
            "uint64": "UInt64",
            "float": "Float32",
            "float32": "Float32",
            "double": "Float64",
            "float64": "Float64",
            "string": "Utf8",
            "large_string": "LargeUtf8",
            "binary": "Binary",
            "large_binary": "LargeBinary",
            "null": "Null",
        }}
        return scalars.get(type_name, "Utf8")

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

pub fn wrap_python_cell(notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = python_bootstrap(notebook_root);
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
