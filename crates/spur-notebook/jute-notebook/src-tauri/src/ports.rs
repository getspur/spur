//! SPUR port bootstrap helpers injected into notebook cells before dispatch.

use std::path::{Path, PathBuf};

use crate::identity::NotebookId;

/// MIME type used for SPUR port display payloads.
pub const PORT_MIME: &str = "application/vnd.spur.port+json";

/// Stable notebook id derived from the notebook path.
pub fn notebook_id_for_path(path: impl AsRef<Path>) -> String {
    NotebookId::for_saved_path(path).store_key().to_owned()
}

/// Per-notebook directory used to store SPUR port files and the manifest.
pub fn notebook_port_root(path: impl AsRef<Path>) -> PathBuf {
    NotebookId::for_saved_path(path).port_root()
}

/// Python bootstrap source that installs the `spur` helper for one cell.
pub fn python_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.py")
}

/// JavaScript/Deno bootstrap source that installs the `globalThis.spur` helper for one cell.
pub fn javascript_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.js")
}

/// Rust/evcxr bootstrap source that installs the `spur` helper for the kernel session.
pub fn rust_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.rs")
}

/// Go/gonb bootstrap source that installs the `spur` helper for the kernel session.
pub fn go_bootstrap() -> &'static str {
    include_str!("assets/ports_bootstrap.go")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::process::{Command, Stdio};

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
    fn python_bootstrap_pins_extended_schema_dialect() {
        let bootstrap = python_bootstrap();

        assert!(bootstrap.contains(r#"return {"Timestamp": ["#));
        assert!(
            bootstrap.contains(r#"return {"Decimal128": [data_type.precision, data_type.scale]}"#)
        );
        assert!(bootstrap.contains(r#""Dictionary": ["#));
        assert!(bootstrap.contains(r#""data_type": self._data_type_json(field.type, port)"#));
    }

    #[test]
    fn rust_bootstrap_pins_narrow_schema_dialect() {
        let bootstrap = rust_bootstrap();

        assert!(bootstrap.contains(r#"DataType::Float64 => "Float64""#));
        assert!(bootstrap.contains(r#"DataType::Date64 => "Date64""#));
        assert!(bootstrap.contains("unsupported Arrow type for manifest schema: {other}"));
        assert!(!bootstrap.contains("DataType::Timestamp"));
        assert!(!bootstrap.contains("DataType::Decimal128"));
        assert!(!bootstrap.contains("DataType::Dictionary"));
    }

    #[test]
    fn javascript_bootstrap_exposes_local_anywidget_helper() {
        let bootstrap = javascript_bootstrap();

        assert!(bootstrap.contains("async anywidget()"));
        assert!(bootstrap.contains("function _spurAnywidgetWidget"));
        assert!(bootstrap.contains("application/vnd.jupyter.widget-view+json"));
        assert!(!bootstrap.contains("jsr:@anywidget/deno"));
        assert!(!bootstrap.contains("npm:@anywidget/deno"));
        assert!(!bootstrap.contains("esm.sh/jsr/@anywidget/deno"));
    }

    #[test]
    fn javascript_bootstrap_strips_typescript_from_anywidget_render_esm() {
        let Some(deno) = deno_binary_for_test() else {
            eprintln!("skipping JS anywidget ESM parse test: deno binary is not available");
            return;
        };

        let bootstrap = javascript_bootstrap();
        let helper_start = bootstrap
            .find("function _spurAnywidgetToEsm(")
            .expect("anywidget ESM helper present");
        let helper_end = bootstrap
            .find("function _spurAnywidgetWidget(")
            .expect("anywidget widget helper present");
        let helper_source = &bootstrap[helper_start..helper_end];
        let render_source = r#"render({ model, el, experimental }: any): void {
  const text = ((value: string): string => value)(String(model.get("html") ?? ""));
  const pluck = ({ value }: { value: string }) => value;
  el.innerHTML = pluck({ value: text });
}"#;
        let script = format!(
            r#"
{helper_source}

const esm = _spurAnywidgetToEsm({{
  imports: "",
  render: {{
    toString() {{
      return {render_source};
    }},
  }},
}});
if (esm.includes(": any") || esm.includes(": void") || esm.includes(": string")) {{
  throw new Error(`expected stripped TypeScript annotations, got: ${{esm}}`);
}}
const moduleUrl = `data:text/javascript,${{encodeURIComponent(esm)}}`;
const module = await import(moduleUrl);
if (typeof module.default.render !== "function") {{
  throw new Error(`expected exported render function, got ${{typeof module.default.render}}`);
}}
"#,
            helper_source = helper_source,
            render_source = serde_json::to_string(render_source).unwrap(),
        );

        let output = Command::new(deno)
            .arg("eval")
            .arg("--ext=js")
            .arg(script)
            .output()
            .expect("deno eval runs");

        assert!(
            output.status.success(),
            "anywidget ESM helper failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn rust_bootstrap_pins_arrow_and_reads_root_from_env() {
        let bootstrap = rust_bootstrap();
        let arrow_dep = format!(
            r#":dep arrow = "{}""#,
            crate::kernel_provision::EVCXR_ARROW_CRATE_VERSION
        );

        assert!(bootstrap.contains(&arrow_dep));
        assert!(bootstrap.contains(r#":dep serde_json = "1""#));
        assert!(bootstrap.contains("struct _Spur"));
        assert!(bootstrap.contains(PORT_MIME));
        assert!(bootstrap.contains(r#""ports""#));
        assert!(bootstrap.contains(r#""manifest.json""#));
        assert!(bootstrap.contains(r#"const PORT_FILE_VERSION_SEPARATOR: &str = "@v";"#));
        assert!(bootstrap.contains("FileWriter::try_new"));
        assert!(bootstrap.contains("FileReader::try_new"));
        // Root is bound once per kernel session from the env the daemon set,
        // not formatted into the cell body.
        assert!(bootstrap.contains(r#"std::env::var("SPUR_NOTEBOOK_PORT_ROOT")"#));
    }

    #[test]
    fn go_bootstrap_pins_arrow_go_and_reads_root_from_env() {
        let bootstrap = go_bootstrap();
        let arrow_dep = format!("!*go get {}", crate::kernel_provision::GONB_ARROW_GO_MODULE);
        let arrow_ipc_import = format!(
            "import \"{}/arrow/ipc\"",
            crate::kernel_provision::GONB_ARROW_GO_MODULE
        );

        assert!(bootstrap.contains(&arrow_dep));
        assert!(bootstrap.contains(&arrow_ipc_import));
        assert!(bootstrap.contains("func (s *spurPorts) Put"));
        assert!(bootstrap.contains("func (s *spurPorts) Get"));
        assert!(bootstrap.contains("var spur = newSpurPorts"));
        assert!(bootstrap.contains(PORT_MIME));
        assert!(bootstrap.contains(r#""ports""#));
        assert!(bootstrap.contains(r#""manifest.json""#));
        assert!(bootstrap.contains(r#"const portFileVersionSeparator = "@v""#));
        assert!(bootstrap.contains("ipc.NewFileWriter"));
        assert!(bootstrap.contains("ipc.NewFileReader"));
        // Root is bound once per kernel session from the env the daemon set.
        assert!(bootstrap.contains(r#"os.Getenv("SPUR_NOTEBOOK_PORT_ROOT")"#));
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

    fn deno_binary_for_test() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("DENO_PATH").map(PathBuf::from) {
            if path.is_absolute() && path.exists() {
                return Some(path);
            }
        }

        let paths = std::env::var_os("PATH")?;
        let names = if cfg!(windows) {
            vec!["deno.exe", "deno"]
        } else {
            vec!["deno"]
        };
        for dir in std::env::split_paths(&paths) {
            for name in &names {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}
