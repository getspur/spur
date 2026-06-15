//! Scaffolding for new Spur App directories (the U4 `init` core).
//!
//! `scaffold_app` materialises a doctor-green app structure from a named
//! template. Templates are registry entries: adding a new app model type
//! (a future Deno server template, a dashboard template, …) means adding one
//! `TemplateInfo` plus its file table here — no MCP tool or dispatch changes.
//!
//! The vendored TypeScript SDK modules are embedded at compile time from
//! `sdk/typescript/src/`, so scaffolded copies are byte-identical to the
//! canonical SDK at the compiled commit (the same lockstep invariant the
//! html_video drift-guard test pins).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{
    SpurAppManifest, SpurAppMcpServer, SpurAppSdk, SPUR_APP_ENTRY_NOTEBOOK, SPUR_APP_METADATA_KEY,
};

/// Canonical vendorable TypeScript SDK modules. Only `call_tool.ts` and
/// `wire.ts` are hermetic (no imports outside themselves); `ports.ts` needs
/// the `@std/path` import map and must not be vendored.
const VENDORED_CALL_TOOL_TS: &str = include_str!("../../../../sdk/typescript/src/call_tool.ts");
const VENDORED_WIRE_TS: &str = include_str!("../../../../sdk/typescript/src/wire.ts");

const PLACEHOLDER_APP_NAME: &str = "__APP_NAME__";
const PLACEHOLDER_TOOL_PREFIX: &str = "__TOOL_PREFIX__";
const PLACEHOLDER_SPUR_APP_REQUIREMENT: &str = "__SPUR_APP_REQUIREMENT__";

/// Errors surfaced by [`scaffold_app`].
#[derive(Debug, Error)]
pub enum ScaffoldError {
    #[error(
        "invalid app name {0:?}: must start with a lowercase letter or digit and contain only \
         lowercase letters, digits, '-' or '_' (max 64 chars)"
    )]
    InvalidName(String),
    #[error("unknown template {0:?}; available: {1}")]
    UnknownTemplate(String, String),
    #[error("refusing to scaffold: {0} already exists")]
    AppRootNotEmpty(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest serialization failed: {0}")]
    ManifestJson(#[from] serde_json::Error),
}

/// Options for [`scaffold_app`].
#[derive(Debug, Clone)]
pub struct ScaffoldOptions {
    /// Directory to scaffold into. Created if absent; individual target files
    /// must not already exist.
    pub app_root: PathBuf,
    /// App name; used as the MCP server name and the manifest `name`.
    pub name: String,
    /// Template name from [`templates`].
    pub template: String,
}

/// Result of a successful scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldedApp {
    pub app_root: PathBuf,
    /// App-root-relative paths of every file written, sorted.
    pub files: Vec<String>,
}

/// A registered scaffold template (one app model type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateInfo {
    pub name: &'static str,
    pub description: &'static str,
}

/// Registry of available templates.
pub fn templates() -> &'static [TemplateInfo] {
    &[
        TemplateInfo {
            name: "minimal",
            description: "Python MCP server on the spur_app SDK, TypeScript frontend cell on \
                          the vendored TS SDK, skill, and pytest tests via spur_app.testing.",
        },
        TemplateInfo {
            name: "frontend-only",
            description: "No MCP server: markdown + HTML-output frontend cell and skill. \
                          Fastest doctor path; add a server later by editing \
                          notebook metadata.spur_app.",
        },
    ]
}

/// Scaffold a new Spur App directory from a template.
pub fn scaffold_app(options: ScaffoldOptions) -> Result<ScaffoldedApp, ScaffoldError> {
    validate_name(&options.name)?;
    if !templates().iter().any(|t| t.name == options.template) {
        let available = templates()
            .iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ScaffoldError::UnknownTemplate(
            options.template.clone(),
            available,
        ));
    }

    let tool_prefix = options.name.replace('-', "_");
    let spur_app_requirement = python_sdk_requirement(&options.app_root);

    let manifest = manifest_for_template(&options.name, &options.template);
    let mut entries: Vec<(String, Vec<u8>)> = vec![(
        SPUR_APP_ENTRY_NOTEBOOK.to_string(),
        notebook_for_template(&options.name, &options.template, &tool_prefix, &manifest)?,
    )];
    for (path, template_body) in template_text_files(&options.template) {
        let body = template_body
            .replace(PLACEHOLDER_APP_NAME, &options.name)
            .replace(PLACEHOLDER_TOOL_PREFIX, &tool_prefix)
            .replace(PLACEHOLDER_SPUR_APP_REQUIREMENT, &spur_app_requirement);
        entries.push((path.to_string(), body.into_bytes()));
    }

    // Refuse to overwrite anything before writing the first byte.
    for (rel, _) in &entries {
        let target = options.app_root.join(rel);
        if target.exists() {
            return Err(ScaffoldError::AppRootNotEmpty(target));
        }
    }

    for (rel, bytes) in &entries {
        let target = options.app_root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, bytes)?;
    }

    let mut files: Vec<String> = entries.into_iter().map(|(rel, _)| rel).collect();
    files.sort();

    Ok(ScaffoldedApp {
        app_root: options.app_root,
        files,
    })
}

fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    let mut chars = name.chars();
    let valid_first = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let valid_rest =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if valid_first && valid_rest && name.len() <= 64 {
        Ok(())
    } else {
        Err(ScaffoldError::InvalidName(name.to_string()))
    }
}

/// Build the manifest for a template from the real model type, so generated
/// manifests can never drift from `SpurAppManifest`.
fn manifest_for_template(name: &str, template: &str) -> SpurAppManifest {
    let mut manifest = SpurAppManifest::minimal(name, SPUR_APP_ENTRY_NOTEBOOK);
    // `runtime.features` is deprecated (doctor warns when present); scaffolds
    // start clean and declare capabilities instead.
    manifest.runtime.features = Vec::new();
    manifest.skill = Some("skill/SKILL.md".to_string());

    if template == "minimal" {
        manifest.mcp_server = Some(SpurAppMcpServer {
            server_type: "python".to_string(),
            entry: "server/main.py".to_string(),
            requirements: Some("server/requirements.txt".to_string()),
            env: BTreeMap::new(),
        });
        manifest.dependencies.python = Some("server/requirements.txt".to_string());
        manifest.sdk = Some(SpurAppSdk {
            typescript: Some("sdk".to_string()),
        });
    }

    manifest
}

/// Resolve the `spur-app` line for `server/requirements.txt`.
///
/// The plugin loader installs requirements with cwd = app root, so relative
/// paths resolve against the app root (`app_gallery/html_video` uses
/// `../../sdk/python` the same way). When the app root lives inside a spur
/// checkout, emit that relative path; otherwise fall back to a comment.
fn python_sdk_requirement(app_root: &Path) -> String {
    for ancestor in app_root.ancestors().skip(1) {
        if ancestor.join("sdk/python/pyproject.toml").is_file() {
            if let Ok(rel) = app_root.strip_prefix(ancestor) {
                let ups = "../".repeat(rel.components().count());
                return format!("{ups}sdk/python");
            }
        }
    }
    "# spur-app: install from a spur checkout (pip install <checkout>/sdk/python)".to_string()
}

/// Text file table per template (app-root-relative path, body with
/// `__APP_NAME__` / `__TOOL_PREFIX__` / `__SPUR_APP_REQUIREMENT__`
/// placeholders). `app.ipynb` is generated separately.
fn template_text_files(template: &str) -> Vec<(&'static str, &'static str)> {
    match template {
        "minimal" => vec![
            ("server/main.py", SERVER_MAIN_PY),
            ("server/requirements.txt", SERVER_REQUIREMENTS_TXT),
            ("skill/SKILL.md", SKILL_MINIMAL_MD),
            ("conftest.py", CONFTEST_PY),
            ("tests/test_app.py", TEST_APP_PY),
            ("sdk/call_tool.ts", VENDORED_CALL_TOOL_TS),
            ("sdk/wire.ts", VENDORED_WIRE_TS),
        ],
        "frontend-only" => vec![("skill/SKILL.md", SKILL_FRONTEND_ONLY_MD)],
        _ => Vec::new(),
    }
}

/// Generate the entry notebook (nbformat 4.5: every cell carries an `id`).
fn notebook_for_template(
    name: &str,
    template: &str,
    tool_prefix: &str,
    manifest: &SpurAppManifest,
) -> Result<Vec<u8>, serde_json::Error> {
    let intro = serde_json::json!({
        "cell_type": "markdown",
        "id": "intro",
        "metadata": { "spur": { "version": 1 } },
        "source": markdown_source(&format!(
            "# {name}\n\nScaffolded Spur App. The dev loop:\n\n\
             1. `notebook_app_doctor` — must be green before packing.\n\
             2. Open this notebook in app mode to spawn the plugin and grant capabilities.\n\
             3. Pack with `notebook_export_spur_app` (never hand-roll a `.spurapp`).\n\n\
             Edit notebook `metadata.spur_app` to declare capabilities before \
             reading any `SPUR_*` env var — the host only injects what the \
             manifest declares."
        )),
    });

    let code_cell = if template == "minimal" {
        serde_json::json!({
            "cell_type": "code",
            "id": "sdk-client",
            "metadata": { "spur": { "version": 1, "code_type": "javascript" } },
            "execution_count": null,
            "outputs": [],
            "source": code_source(&format!(
                "// TODO(U7): replace with `import {{ callTool }} from \"jsr:@spur/app\"` once published to JSR.\n\
                 // The SDK is vendored into the app root (see metadata.spur_app \"sdk\"); the kernel\n\
                 // starts in the notebook's directory, so Deno.cwd() is the app root.\n\
                 const {{ callTool }} = await import(`file://${{Deno.cwd()}}/sdk/call_tool.ts`);\n\
                 const result = await callTool(\"{tool_prefix}_hello\", {{ name: \"Spur\" }});\n\
                 console.log(result);"
            )),
        })
    } else {
        serde_json::json!({
            "cell_type": "code",
            "id": "app-surface",
            "metadata": { "spur": { "version": 1, "code_type": "javascript" } },
            "execution_count": null,
            "outputs": [],
            "source": code_source(&format!(
                "// Frontend cell — runs in the Deno kernel; the html output is the app surface.\n\
                 Deno.jupyter.html`<h1>{name}</h1><p>Edit app.ipynb to build your app surface.</p>`;"
            )),
        })
    };

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        SPUR_APP_METADATA_KEY.to_string(),
        serde_json::to_value(manifest)?,
    );
    let notebook = serde_json::json!({
        "cells": [intro, code_cell],
        "metadata": metadata,
        "nbformat": 4,
        "nbformat_minor": 5
    });
    let mut bytes = serde_json::to_vec_pretty(&notebook)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Split into nbformat `source` line arrays (each line keeps its trailing
/// newline except the last).
fn split_source_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn markdown_source(text: &str) -> Vec<String> {
    split_source_lines(text)
}

fn code_source(text: &str) -> Vec<String> {
    split_source_lines(text)
}

const SERVER_MAIN_PY: &str = r#""""__APP_NAME__ — Spur App MCP server built on the spur_app SDK."""
from spur_app import App

app = App("__APP_NAME__")


@app.tool()
def __TOOL_PREFIX___hello(name: str) -> str:
    """Example tool: greet *name*.

    Declare capabilities in notebook metadata.spur_app before using app.ports or
    app.artifacts — the host only injects what the manifest declares.
    """
    return f"Hello from __APP_NAME__, {name}!"


if __name__ == "__main__":
    app.run()
"#;

const SERVER_REQUIREMENTS_TXT: &str = "\
mcp>=1.0.0
# TODO(U7): replace with spur-app>=0.1.0 once published to PyPI
__SPUR_APP_REQUIREMENT__
";

const SKILL_MINIMAL_MD: &str = r#"---
name: __APP_NAME__
description: "Use when working with the __APP_NAME__ Spur App — drives the app notebook and its MCP tools."
---

# __APP_NAME__ — Spur App Skill

<HARD-GATE>
Operate the notebook and app ONLY through MCP tools:
(`notebook_run_cell`, `__TOOL_PREFIX___hello`).
Never ask the user to paste code or open files.
</HARD-GATE>

## The loop

1. Open `app.ipynb` in app mode — the host reads notebook `metadata.spur_app`,
   grants the declared capabilities, and spawns the MCP server plugin.
2. Call `__TOOL_PREFIX___hello` to verify the plugin surface is live.
3. Build the app: server tools in `server/main.py` (the `spur_app` SDK),
   frontend cells on the vendored TypeScript SDK (`sdk/call_tool.ts`).
4. Run the doctor before every pack; fix every `fail` finding.
5. Pack through the canonical packer only — never hand-roll a `.spurapp`.
"#;

const SKILL_FRONTEND_ONLY_MD: &str = r#"---
name: __APP_NAME__
description: "Use when working with the __APP_NAME__ Spur App — drives the app notebook frontend."
---

# __APP_NAME__ — Spur App Skill

<HARD-GATE>
Operate the notebook ONLY through MCP tools (`notebook_run_cell`).
Never ask the user to paste code or open files.
</HARD-GATE>

## The loop

1. Open `app.ipynb` in app mode.
2. Build the app surface as frontend cells whose outputs carry `text/html`.
3. Run the doctor before every pack; fix every `fail` finding.
4. Pack through the canonical packer only — never hand-roll a `.spurapp`.
"#;

const CONFTEST_PY: &str = r#""""Shared pytest fixtures for __APP_NAME__ (spur_app.testing re-export)."""
from spur_app.testing import fake_port_store  # noqa: F401
"#;

const TEST_APP_PY: &str = r#""""Tests for the __APP_NAME__ server — zero hand-written protocol code."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))

from main import __TOOL_PREFIX___hello
from spur_app.testing import FakePortStore


def test_hello_greets():
    assert "__APP_NAME__" in __TOOL_PREFIX___hello("Spur")


def test_port_store_round_trip():
    store = FakePortStore().add_media("clip", b"fake-bytes", mime="video/mp4")
    with store:
        assert store.port_store.read("clip").mime == "video/mp4"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spur_app::SPUR_APP_MANIFEST;

    fn options(root: &Path, name: &str, template: &str) -> ScaffoldOptions {
        ScaffoldOptions {
            app_root: root.to_path_buf(),
            name: name.to_string(),
            template: template.to_string(),
        }
    }

    #[test]
    fn templates_registry_lists_minimal_and_frontend_only() {
        let names: Vec<&str> = templates().iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["minimal", "frontend-only"]);
    }

    #[test]
    fn scaffold_minimal_writes_model_parseable_manifest_and_notebook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-app");
        let scaffolded = scaffold_app(options(&root, "my-app", "minimal")).expect("scaffold");

        assert_eq!(
            scaffolded.files,
            vec![
                "app.ipynb",
                "conftest.py",
                "sdk/call_tool.ts",
                "sdk/wire.ts",
                "server/main.py",
                "server/requirements.txt",
                "skill/SKILL.md",
                "tests/test_app.py",
            ]
        );

        assert!(!root.join(SPUR_APP_MANIFEST).exists());
        let notebook: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("app.ipynb")).expect("notebook"))
                .expect("notebook is valid JSON");
        let manifest: SpurAppManifest =
            serde_json::from_value(notebook["metadata"]["spur_app"].clone())
                .expect("embedded manifest parses through the model");
        assert_eq!(manifest.name, "my-app");
        assert!(
            manifest.runtime.features.is_empty(),
            "scaffolds must not emit deprecated runtime.features"
        );
        let server = manifest.mcp_server.expect("minimal declares a server");
        assert_eq!(server.server_type, "python");
        assert_eq!(server.entry, "server/main.py");
        assert_eq!(
            manifest.sdk,
            Some(SpurAppSdk {
                typescript: Some("sdk".to_string())
            })
        );
        assert_eq!(manifest.skill.as_deref(), Some("skill/SKILL.md"));

        assert_eq!(notebook["nbformat"], 4);
        for cell in notebook["cells"].as_array().expect("cells") {
            assert!(cell["id"].is_string(), "nbformat 4.5 cells carry ids");
        }
    }

    #[test]
    fn scaffold_minimal_vendors_canonical_typescript_sdk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-app");
        scaffold_app(options(&root, "my-app", "minimal")).expect("scaffold");

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repo root");
        for module in ["call_tool.ts", "wire.ts"] {
            let vendored = fs::read(root.join("sdk").join(module)).expect("vendored module");
            let canonical = fs::read(repo_root.join("sdk/typescript/src").join(module))
                .expect("canonical module");
            assert_eq!(
                vendored, canonical,
                "vendored {module} must be byte-identical to sdk/typescript/src/{module}"
            );
        }
    }

    #[test]
    fn scaffold_substitutes_placeholders_everywhere() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-app");
        let scaffolded = scaffold_app(options(&root, "my-app", "minimal")).expect("scaffold");

        let main_py = fs::read_to_string(root.join("server/main.py")).expect("main.py");
        assert!(main_py.contains("App(\"my-app\")"));
        assert!(main_py.contains("def my_app_hello"));

        for rel in &scaffolded.files {
            let body = fs::read_to_string(root.join(rel)).expect("scaffolded file is utf-8");
            assert!(
                !body.contains("__APP_NAME__")
                    && !body.contains("__TOOL_PREFIX__")
                    && !body.contains("__SPUR_APP_REQUIREMENT__"),
                "unsubstituted placeholder left in {rel}"
            );
        }
    }

    #[test]
    fn scaffold_resolves_python_sdk_path_inside_a_spur_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let checkout = tmp.path().join("spur");
        fs::create_dir_all(checkout.join("sdk/python")).expect("mkdirs");
        fs::write(checkout.join("sdk/python/pyproject.toml"), "[project]\n").expect("pyproject");

        let root = checkout.join("app_gallery/demo");
        scaffold_app(options(&root, "demo", "minimal")).expect("scaffold");
        let requirements =
            fs::read_to_string(root.join("server/requirements.txt")).expect("requirements");
        assert!(
            requirements.contains("../../sdk/python"),
            "expected monorepo-relative spur-app requirement, got:\n{requirements}"
        );
    }

    #[test]
    fn scaffold_falls_back_to_comment_outside_a_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("standalone-app");
        scaffold_app(options(&root, "standalone-app", "minimal")).expect("scaffold");
        let requirements =
            fs::read_to_string(root.join("server/requirements.txt")).expect("requirements");
        assert!(requirements.contains("mcp>=1.0.0"));
        assert!(requirements.contains("# spur-app: install from a spur checkout"));
    }

    #[test]
    fn scaffold_frontend_only_has_no_server_and_no_vendored_sdk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("surface");
        let scaffolded =
            scaffold_app(options(&root, "surface", "frontend-only")).expect("scaffold");

        assert_eq!(scaffolded.files, vec!["app.ipynb", "skill/SKILL.md"]);
        assert!(!root.join(SPUR_APP_MANIFEST).exists());
        let notebook: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("app.ipynb")).expect("notebook"))
                .expect("notebook is valid JSON");
        let manifest: SpurAppManifest =
            serde_json::from_value(notebook["metadata"]["spur_app"].clone())
                .expect("embedded manifest parses");
        assert!(manifest.mcp_server.is_none());
        assert!(manifest.sdk.is_none());
    }

    #[test]
    fn scaffold_refuses_to_overwrite_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("taken");
        fs::create_dir_all(&root).expect("mkdir");
        fs::write(root.join(SPUR_APP_ENTRY_NOTEBOOK), "{}").expect("preexisting notebook");

        let error =
            scaffold_app(options(&root, "taken", "minimal")).expect_err("must refuse to overwrite");
        assert!(matches!(error, ScaffoldError::AppRootNotEmpty(_)));
        // Nothing else was written.
        assert!(!root.join("server/main.py").exists());
        assert!(!root.join("skill/SKILL.md").exists());
    }

    #[test]
    fn scaffold_rejects_invalid_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for bad in ["", "My App", "UPPER", "-leading", "name with spaces"] {
            let error = scaffold_app(options(&tmp.path().join("x"), bad, "minimal"))
                .expect_err("invalid name must be rejected");
            assert!(matches!(error, ScaffoldError::InvalidName(_)), "{bad:?}");
        }
    }

    #[test]
    fn scaffold_rejects_unknown_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let error = scaffold_app(options(&tmp.path().join("x"), "x", "nope"))
            .expect_err("unknown template must be rejected");
        assert!(matches!(error, ScaffoldError::UnknownTemplate(_, _)));
    }
}
