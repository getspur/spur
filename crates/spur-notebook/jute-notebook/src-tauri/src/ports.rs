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
pub fn python_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.py")
}

/// JavaScript/Deno bootstrap source that installs the `globalThis.spur` helper for one cell.
pub fn javascript_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.js")
}

/// Prepend the Python SPUR port bootstrap to user code.
pub fn wrap_python_cell(_notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = python_bootstrap().to_string();
    wrapped.push('\n');
    wrapped.push_str(code);
    wrapped
}

/// Prepend the JavaScript/Deno SPUR port bootstrap to user code.
pub fn wrap_js_cell(_notebook_root: impl AsRef<Path>, code: &str) -> String {
    let mut wrapped = javascript_bootstrap().to_string();
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
        assert!(wrapped.contains(r#"if "spur" not in globals()"#));
        assert!(wrapped.contains(r#"_spur_os.environ["SPUR_NOTEBOOK_PORT_ROOT"]"#));
        assert!(wrapped.contains(PORT_MIME));
        assert!(!wrapped.contains("/tmp/demo-root"));
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
        assert!(wrapped.contains("globalThis.spur ??="));
        assert!(wrapped.contains(r#"Deno.env.get("SPUR_NOTEBOOK_PORT_ROOT")"#));
        assert!(wrapped.contains("npm:apache-arrow@21.1.0"));
        assert!(wrapped.contains(PORT_MIME));
        assert!(!wrapped.contains(&root_literal));
        assert!(!wrapped.contains(&ports_literal));
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
        let python = python_bootstrap();
        assert!(python.contains("unsupported Arrow type for manifest schema"));
        assert!(!python.contains(r#"scalars.get(type_name, "Utf8")"#));

        let javascript = javascript_bootstrap();
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
        let bootstrap = python_bootstrap();
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
            .env("SPUR_NOTEBOOK_PORT_ROOT", dir.path())
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
