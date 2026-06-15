use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use jute::backend::notebook::NotebookRoot;

pub mod archive;
pub mod scaffold;

pub const SPUR_APP_EXTENSION: &str = "spurapp";
pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";
pub const SPUR_APP_METADATA_KEY: &str = "spur_app";

const DEPENDENCY_LOCK_FILES: &[&str] = &[
    "uv.lock",
    "requirements.txt",
    "deno.json",
    "deno.lock",
    "Cargo.lock",
    "go.mod",
    "go.sum",
];
const PORTS_DIR: &str = "ports";
const PORTS_MANIFEST: &str = "ports/manifest.json";
const DEFAULT_SKILL_PATH: &str = "skill/SKILL.md";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
    #[serde(default)]
    pub entry_notebook: String,
    pub open_mode: String,
    pub runtime: SpurAppRuntime,
    #[serde(default)]
    pub widgets: Vec<SpurAppWidgetAsset>,
    #[serde(default)]
    pub ports: Option<SpurAppPorts>,
    #[serde(default)]
    pub dependencies: SpurAppDependencies,
    #[serde(default)]
    pub mcp_server: Option<SpurAppMcpServer>,
    /// Capability declarations for host provisioning. Additive — existing
    /// manifests without this field deserialize with all capabilities defaulted
    /// to off. Unknown capability keys inside the struct are refused at
    /// deserialization time (enforced by `deny_unknown_fields` on the inner
    /// struct) so the host can return a structured error on unrecognised keys.
    #[serde(default)]
    pub capabilities: SpurAppCapabilities,
    /// Path (relative to the app root) to the agent skill file for this app.
    /// Defaults to `"skill/SKILL.md"` when absent.
    #[serde(default)]
    pub skill: Option<String>,
    /// Vendored SDK declarations. When present, the doctor verifies the
    /// referenced directories exist inside the app root. Absent in existing
    /// manifests → `None` (backward compatible).
    #[serde(default)]
    pub sdk: Option<SpurAppSdk>,
}

/// Capability declarations; all fields default to off/None so that existing
/// manifests without a `capabilities` block continue to deserialise unchanged.
///
/// **Unknown field policy:** `deny_unknown_fields` is applied to this struct so
/// that a manifest containing an unrecognised capability key (e.g. a future
/// capability unknown to this host) is rejected at open time with a structured
/// error rather than silently ignored. This is intentionally strict at the
/// capability level while the manifest root itself remains permissive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpurAppCapabilities {
    /// Port-store capability: declares which port names the app reads/writes.
    /// When present, the host injects `SPUR_PORTS_ROOT` at plugin spawn.
    #[serde(default)]
    pub ports: Option<SpurAppCapabilityPorts>,
    /// When `true`, the host guarantees the canvas-capture recorder loop
    /// end-to-end (requires `active_output_scripts`).
    #[serde(default)]
    pub canvas_capture: bool,
    /// When `true`, the host shows a one-time per-app grant prompt and, after
    /// approval, opens output iframes with `allow-scripts allow-same-origin`.
    #[serde(default)]
    pub active_output_scripts: bool,
    /// When `true`, the host injects `SPUR_ARTIFACTS_DIR` at plugin spawn and
    /// creates the directory.
    #[serde(default)]
    pub artifacts_dir: bool,
}

/// Read/write port name lists for the `ports` capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppCapabilityPorts {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppMcpServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub entry: String,
    #[serde(default)]
    pub requirements: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppRuntime {
    pub jute_min: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppWidgetAsset {
    pub module: String,
    #[serde(default)]
    pub css: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppPorts {
    #[serde(default)]
    pub include_snapshots: bool,
    #[serde(default)]
    pub manifest: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppDependencies {
    #[serde(default)]
    pub python: Option<String>,
    #[serde(default)]
    pub deno: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
    #[serde(default)]
    pub go: Option<String>,
}

/// Vendored SDK directories, relative to the app root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppSdk {
    /// Directory containing the vendored TypeScript SDK modules
    /// (e.g. `"sdk"` → `<app_root>/sdk/call_tool.ts`).
    #[serde(default)]
    pub typescript: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SpurAppExportOptions {
    pub notebook_path: PathBuf,
    pub output_path: PathBuf,
    pub name: Option<String>,
    pub widget_assets: Vec<PathBuf>,
    pub include_port_snapshots: bool,
    pub dependency_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpurAppExported {
    pub output_path: PathBuf,
    pub manifest_path: String,
    pub asset_count: usize,
    pub preflight: SpurAppPreflight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSpurApp {
    pub root: PathBuf,
    pub notebook_path: PathBuf,
    pub manifest: SpurAppManifest,
    pub preflight: SpurAppPreflight,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpurAppPreflight {
    pub missing_dependency_locks: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    Embedded,
    SiblingJson(PathBuf),
}

impl SpurAppManifest {
    pub fn minimal(name: impl Into<String>, entry_notebook: impl Into<String>) -> Self {
        Self {
            schema: SPUR_APP_SCHEMA.to_string(),
            name: name.into(),
            entry_notebook: entry_notebook.into(),
            open_mode: "app".to_string(),
            runtime: SpurAppRuntime {
                jute_min: "0.1.0".to_string(),
                features: vec![
                    "frontend-cells".to_string(),
                    "anywidget-afm".to_string(),
                    "ports-arrow".to_string(),
                ],
            },
            widgets: Vec::new(),
            ports: None,
            dependencies: SpurAppDependencies::default(),
            mcp_server: None,
            capabilities: SpurAppCapabilities::default(),
            skill: None,
            sdk: None,
        }
    }
}

fn absolute_notebook_path(notebook_path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(notebook_path) {
        return path;
    }

    if notebook_path.is_absolute() {
        return notebook_path.to_path_buf();
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(notebook_path))
        .unwrap_or_else(|_| notebook_path.to_path_buf())
}

pub fn manifest_from_notebook(notebook_path: &Path) -> Option<(PathBuf, SpurAppManifest)> {
    let notebook_path = absolute_notebook_path(notebook_path);
    let raw = fs::read_to_string(&notebook_path).ok()?;
    let root: NotebookRoot = match serde_json::from_str(&raw) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %notebook_path.display(),
                "failed to parse notebook while reading embedded Spur App manifest"
            );
            return None;
        }
    };
    let value = root.metadata.other.get(SPUR_APP_METADATA_KEY)?;

    let mut manifest: SpurAppManifest = match serde_json::from_value(value.clone()) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %notebook_path.display(),
                "invalid notebook metadata.spur_app manifest"
            );
            return None;
        }
    };
    if manifest.entry_notebook.is_empty() {
        manifest.entry_notebook = notebook_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(SPUR_APP_ENTRY_NOTEBOOK)
            .to_owned();
    }

    let app_root = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Some((app_root, manifest))
}

pub fn resolve_app_manifest(
    notebook_path: &Path,
) -> Option<(PathBuf, SpurAppManifest, ManifestSource)> {
    let notebook_path = absolute_notebook_path(notebook_path);
    let app_root = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".")
        });

    if let Some((app_root, manifest)) = manifest_from_notebook(&notebook_path) {
        return Some((app_root, manifest, ManifestSource::Embedded));
    }

    let manifest_path = app_root.join(SPUR_APP_MANIFEST);
    let raw = fs::read(&manifest_path).ok()?;
    let manifest = match serde_json::from_slice::<SpurAppManifest>(&raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %manifest_path.display(),
                "invalid sibling spur-app.json manifest"
            );
            return None;
        }
    };

    Some((
        app_root,
        manifest,
        ManifestSource::SiblingJson(manifest_path),
    ))
}

pub fn export_spur_app(
    options: SpurAppExportOptions,
) -> Result<SpurAppExported, archive::SpurAppArchiveError> {
    let notebook_contents = fs::read(&options.notebook_path)?;
    let mut preflight = SpurAppPreflight::default();
    let mut app_root = options
        .notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let (mut manifest, collect_authored_files) =
        if let Some((resolved_app_root, manifest, _source)) =
            resolve_app_manifest(&options.notebook_path)
        {
            app_root = resolved_app_root;
            (manifest, true)
        } else {
            (
                SpurAppManifest::minimal(
                    options
                        .name
                        .clone()
                        .unwrap_or_else(|| default_app_name(&options.notebook_path)),
                    SPUR_APP_ENTRY_NOTEBOOK,
                ),
                false,
            )
        };
    if let Some(name) = options.name {
        manifest.name = name;
    }

    let mut entries = vec![(manifest.entry_notebook.clone(), notebook_contents)];

    manifest.widgets = collect_widget_assets(&options.widget_assets, &mut entries)?;
    manifest.dependencies = collect_dependency_locks(&options.dependency_roots, &mut entries)?;

    if options.include_port_snapshots {
        collect_port_snapshots(
            &options.notebook_path,
            &mut entries,
            &mut manifest,
            &mut preflight,
        )?;
    }

    if collect_authored_files {
        collect_app_files(&app_root, &manifest, &mut entries)?;
    }

    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(archive::SpurAppArchiveError::InvalidManifestJson)?;
    entries.push((SPUR_APP_MANIFEST.to_string(), manifest_json));

    if let Some(parent) = options.output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let output = fs::File::create(&options.output_path)?;
    archive::write_entries(output, entries)?;

    Ok(SpurAppExported {
        output_path: options.output_path,
        manifest_path: SPUR_APP_MANIFEST.to_string(),
        asset_count: manifest.widgets.len(),
        preflight,
    })
}

pub fn import_spur_app(
    package_path: impl AsRef<Path>,
    cache_root: impl AsRef<Path>,
) -> Result<ImportedSpurApp, archive::SpurAppArchiveError> {
    let package = fs::read(package_path.as_ref())?;
    let manifest = archive::read_manifest(Cursor::new(package.as_slice()))?;

    if !is_safe_archive_path(&manifest.entry_notebook) {
        return Err(archive::SpurAppArchiveError::UnsafePath(
            manifest.entry_notebook.clone(),
        ));
    }
    archive::read_entry(Cursor::new(package.as_slice()), &manifest.entry_notebook)?;

    let root = cache_root
        .as_ref()
        .join(format!("sha256-{}", blake3_hex(&package)));
    fs::create_dir_all(cache_root.as_ref())?;
    reset_extract_root(&root)?;
    fs::create_dir_all(&root)?;
    archive::extract_to_dir(Cursor::new(package.as_slice()), &root)?;

    let notebook_path = root.join(&manifest.entry_notebook);
    let preflight = build_import_preflight(&root, &manifest);

    Ok(ImportedSpurApp {
        root,
        notebook_path,
        manifest,
        preflight,
    })
}

pub fn is_safe_archive_path(raw: &str) -> bool {
    if raw.is_empty() || raw.contains('\\') {
        return false;
    }

    let path = Path::new(raw);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn collect_widget_assets(
    widget_assets: &[PathBuf],
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<Vec<SpurAppWidgetAsset>, archive::SpurAppArchiveError> {
    let mut widgets = Vec::with_capacity(widget_assets.len());

    for path in widget_assets {
        let contents = fs::read(path)?;
        let hash = blake3_hex(&contents);
        let hash_label = format!("sha256-{hash}");
        let archive_path = widget_archive_path(path, &hash_label);
        entries.push((archive_path.clone(), contents));
        widgets.push(SpurAppWidgetAsset {
            module: archive_path.clone(),
            css: is_css_asset(path).then_some(archive_path),
            hash: hash_label,
        });
    }

    Ok(widgets)
}

fn collect_dependency_locks(
    dependency_roots: &[PathBuf],
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<SpurAppDependencies, archive::SpurAppArchiveError> {
    let mut locks = BTreeMap::new();

    for root in dependency_roots {
        for filename in DEPENDENCY_LOCK_FILES {
            if locks.contains_key(filename) {
                continue;
            }

            let path = root.join(filename);
            if path.is_file() {
                locks.insert(*filename, fs::read(path)?);
            }
        }
    }

    for (filename, contents) in &locks {
        entries.push((format!("env/{filename}"), contents.clone()));
    }

    Ok(SpurAppDependencies {
        python: dependency_path(&locks, &["uv.lock", "requirements.txt"]),
        deno: dependency_path(&locks, &["deno.lock", "deno.json"]),
        rust: dependency_path(&locks, &["Cargo.lock"]),
        go: dependency_path(&locks, &["go.sum", "go.mod"]),
    })
}

fn collect_port_snapshots(
    notebook_path: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
    manifest: &mut SpurAppManifest,
    preflight: &mut SpurAppPreflight,
) -> Result<(), archive::SpurAppArchiveError> {
    let Some(notebook_dir) = notebook_path.parent() else {
        manifest.ports = Some(SpurAppPorts {
            include_snapshots: true,
            manifest: None,
        });
        preflight
            .warnings
            .push("port snapshots requested but the notebook has no parent directory".to_string());
        return Ok(());
    };

    let ports_manifest = notebook_dir.join(PORTS_MANIFEST);
    if !ports_manifest.is_file() {
        manifest.ports = Some(SpurAppPorts {
            include_snapshots: true,
            manifest: None,
        });
        preflight.warnings.push(format!(
            "port snapshots requested but {PORTS_MANIFEST} was not found"
        ));
        return Ok(());
    }

    let ports_root = notebook_dir.join(PORTS_DIR);
    collect_files_under(&ports_root, PORTS_DIR, &ports_root, entries)?;
    manifest.ports = Some(SpurAppPorts {
        include_snapshots: true,
        manifest: Some(PORTS_MANIFEST.to_string()),
    });
    Ok(())
}

fn collect_files_under(
    root: &Path,
    archive_prefix: &str,
    current: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();

        if file_type.is_dir() {
            collect_files_under(root, archive_prefix, &path, entries)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                archive::SpurAppArchiveError::UnsafePath(path.display().to_string())
            })?;
            let archive_path = prefixed_archive_path(archive_prefix, relative)?;
            entries.push((archive_path, fs::read(path)?));
        }
    }

    Ok(())
}

fn collect_app_files(
    app_root: &Path,
    manifest: &SpurAppManifest,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    if let Some(server) = &manifest.mcp_server {
        let entry_path = Path::new(&server.entry);
        if let Some(parent) = entry_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            collect_app_dir_recursive(app_root, parent, entries)?;
        } else {
            collect_app_file_if_exists(app_root, entry_path, entries)?;
        }
    }

    if let Some(sdk_dir) = manifest
        .sdk
        .as_ref()
        .and_then(|sdk| sdk.typescript.as_deref())
    {
        collect_app_dir_recursive(app_root, Path::new(sdk_dir), entries)?;
    }

    let skill_path = manifest.skill.as_deref().unwrap_or(DEFAULT_SKILL_PATH);
    collect_app_file_if_exists(app_root, Path::new(skill_path), entries)?;
    Ok(())
}

fn collect_app_dir_recursive(
    app_root: &Path,
    relative_dir: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    validate_relative_app_path(relative_dir)?;
    let absolute_dir = app_root.join(relative_dir);
    if !absolute_dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(absolute_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if should_skip_app_bundle_name(&name) {
            continue;
        }

        let relative = relative_dir.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_app_dir_recursive(app_root, &relative, entries)?;
        } else if file_type.is_file() {
            push_app_entry_once(entries, &relative, fs::read(entry.path())?)?;
        }
    }

    Ok(())
}

fn collect_app_file_if_exists(
    app_root: &Path,
    relative_file: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), archive::SpurAppArchiveError> {
    validate_relative_app_path(relative_file)?;
    let absolute_file = app_root.join(relative_file);
    if absolute_file.is_file() {
        push_app_entry_once(entries, relative_file, fs::read(absolute_file)?)?;
    }
    Ok(())
}

fn push_app_entry_once(
    entries: &mut Vec<(String, Vec<u8>)>,
    relative: &Path,
    contents: Vec<u8>,
) -> Result<(), archive::SpurAppArchiveError> {
    let archive_path = relative_app_archive_path(relative)?;
    if !entries
        .iter()
        .any(|(existing, _)| existing.as_str() == archive_path)
    {
        entries.push((archive_path, contents));
    }
    Ok(())
}

fn relative_app_archive_path(relative: &Path) -> Result<String, archive::SpurAppArchiveError> {
    validate_relative_app_path(relative)?;
    let mut archive_path = String::new();

    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(archive::SpurAppArchiveError::UnsafePath(
                relative.display().to_string(),
            ));
        };
        if !archive_path.is_empty() {
            archive_path.push('/');
        }
        archive_path.push_str(&segment.to_string_lossy());
    }

    if !is_safe_archive_path(&archive_path) {
        return Err(archive::SpurAppArchiveError::UnsafePath(archive_path));
    }
    Ok(archive_path)
}

fn validate_relative_app_path(path: &Path) -> Result<(), archive::SpurAppArchiveError> {
    let mut has_component = false;
    for component in path.components() {
        let Component::Normal(_) = component else {
            return Err(archive::SpurAppArchiveError::UnsafePath(
                path.display().to_string(),
            ));
        };
        has_component = true;
    }

    if !has_component {
        return Err(archive::SpurAppArchiveError::UnsafePath(
            path.display().to_string(),
        ));
    }
    Ok(())
}

fn should_skip_app_bundle_name(name: &str) -> bool {
    name.starts_with('.') || name == "__pycache__" || name == ".pytest_cache"
}

fn build_import_preflight(root: &Path, manifest: &SpurAppManifest) -> SpurAppPreflight {
    let mut preflight = SpurAppPreflight::default();

    for path in manifest_dependency_paths(&manifest.dependencies) {
        if !is_safe_archive_path(path) || !root.join(path).is_file() {
            preflight.missing_dependency_locks.push(path.to_string());
        }
    }

    if let Some(ports) = &manifest.ports {
        if ports.include_snapshots {
            match ports.manifest.as_deref() {
                Some(path) if is_safe_archive_path(path) && root.join(path).is_file() => {
                    preflight.warnings.push(
                        "port snapshots are bundled but automatic restoration is not supported yet"
                            .to_string(),
                    );
                }
                Some(path) => preflight
                    .warnings
                    .push(format!("port snapshots reference missing manifest {path}")),
                None => preflight
                    .warnings
                    .push("port snapshots requested but no manifest was bundled".to_string()),
            }
        }
    }

    preflight
}

fn manifest_dependency_paths(dependencies: &SpurAppDependencies) -> impl Iterator<Item = &str> {
    [
        dependencies.python.as_deref(),
        dependencies.deno.as_deref(),
        dependencies.rust.as_deref(),
        dependencies.go.as_deref(),
    ]
    .into_iter()
    .flatten()
}

fn dependency_path(locks: &BTreeMap<&'static str, Vec<u8>>, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|filename| locks.contains_key(**filename))
        .map(|filename| format!("env/{filename}"))
}

fn widget_archive_path(path: &Path, hash_label: &str) -> String {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if !extension.is_empty() => {
            format!("widgets/{hash_label}.{extension}")
        }
        _ => format!("widgets/{hash_label}"),
    }
}

fn is_css_asset(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("css"))
}

fn prefixed_archive_path(
    prefix: &str,
    relative: &Path,
) -> Result<String, archive::SpurAppArchiveError> {
    let mut archive_path = prefix.to_string();

    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(archive::SpurAppArchiveError::UnsafePath(
                relative.display().to_string(),
            ));
        };
        archive_path.push('/');
        archive_path.push_str(&segment.to_string_lossy());
    }

    if !is_safe_archive_path(&archive_path) {
        return Err(archive::SpurAppArchiveError::UnsafePath(archive_path));
    }

    Ok(archive_path)
}

fn default_app_name(notebook_path: &Path) -> String {
    notebook_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("SpurApp")
        .to_string()
}

fn reset_extract_root(root: &Path) -> Result<(), archive::SpurAppArchiveError> {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return Ok(());
    };

    if metadata.is_dir() {
        fs::remove_dir_all(root)?;
    } else {
        fs::remove_file(root)?;
    }

    Ok(())
}

fn blake3_hex(contents: &[u8]) -> String {
    blake3::hash(contents).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded_notebook(manifest: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec_pretty(&serde_json::json!({
            "cells": [],
            "metadata": {
                "spur_app": manifest
            },
            "nbformat": 4,
            "nbformat_minor": 5
        }))
        .expect("notebook serializes")
    }

    #[test]
    fn manifest_from_notebook_reads_spur_app_metadata_and_app_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("embedded-app");
        std::fs::create_dir_all(&root).expect("mkdir");
        let notebook_path = root.join("dashboard.ipynb");
        std::fs::write(
            &notebook_path,
            embedded_notebook(serde_json::json!({
                "schema": "spur.app/v1",
                "name": "Embedded Dashboard",
                "open_mode": "app",
                "runtime": {
                    "jute_min": "0.1.0"
                }
            })),
        )
        .expect("write notebook");

        let (app_root, manifest) =
            manifest_from_notebook(&notebook_path).expect("embedded manifest");

        assert_eq!(app_root, root);
        assert_eq!(manifest.name, "Embedded Dashboard");
        assert_eq!(manifest.entry_notebook, "dashboard.ipynb");
    }

    #[test]
    fn resolve_app_manifest_embedded_only_uses_absolute_app_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("embedded-only");
        std::fs::create_dir_all(&root).expect("mkdir");
        let notebook_path = root.join("dashboard.ipynb");
        std::fs::write(
            &notebook_path,
            embedded_notebook(serde_json::json!({
                "schema": "spur.app/v1",
                "name": "Embedded Only",
                "open_mode": "app",
                "runtime": {
                    "jute_min": "0.1.0"
                }
            })),
        )
        .expect("write notebook");

        let (app_root, manifest, source) =
            resolve_app_manifest(&notebook_path).expect("embedded manifest resolves");

        assert!(app_root.is_absolute());
        assert_eq!(app_root, root.canonicalize().expect("canonical root"));
        assert_eq!(manifest.name, "Embedded Only");
        assert_eq!(manifest.entry_notebook, "dashboard.ipynb");
        assert_eq!(source, ManifestSource::Embedded);
    }

    #[test]
    fn resolve_app_manifest_relative_notebook_path_yields_absolute_root() {
        let cwd = std::env::current_dir().expect("cwd");
        let tmp = tempfile::tempdir_in(&cwd).expect("tempdir in cwd");
        let root = tmp.path().join("relative-app");
        std::fs::create_dir_all(&root).expect("mkdir");
        let notebook_path = root.join("app.ipynb");
        std::fs::write(
            &notebook_path,
            embedded_notebook(serde_json::json!({
                "schema": "spur.app/v1",
                "name": "Relative App",
                "entry_notebook": "app.ipynb",
                "open_mode": "app",
                "runtime": {
                    "jute_min": "0.1.0"
                }
            })),
        )
        .expect("write notebook");
        let relative = notebook_path
            .strip_prefix(&cwd)
            .expect("notebook path is under cwd");

        let (app_root, manifest, source) =
            resolve_app_manifest(relative).expect("relative embedded manifest resolves");

        assert!(app_root.is_absolute());
        assert_eq!(app_root, root.canonicalize().expect("canonical root"));
        assert_eq!(manifest.name, "Relative App");
        assert_eq!(source, ManifestSource::Embedded);
    }

    #[test]
    fn resolve_app_manifest_falls_back_to_sibling_spur_app_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("sibling-app");
        std::fs::create_dir_all(&root).expect("mkdir");
        let notebook_path = root.join("app.ipynb");
        std::fs::write(
            &notebook_path,
            b"{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":5}",
        )
        .expect("write notebook");
        std::fs::write(
            root.join(SPUR_APP_MANIFEST),
            r#"{
              "schema": "spur.app/v1",
              "name": "Sibling App",
              "entry_notebook": "app.ipynb",
              "open_mode": "app",
              "runtime": { "jute_min": "0.1.0" }
            }"#,
        )
        .expect("write manifest");

        let (app_root, manifest, source) =
            resolve_app_manifest(&notebook_path).expect("sibling manifest resolves");

        assert!(app_root.is_absolute());
        assert_eq!(manifest.name, "Sibling App");
        assert_eq!(
            source,
            ManifestSource::SiblingJson(app_root.join(SPUR_APP_MANIFEST))
        );
    }

    #[test]
    fn export_bundles_authored_manifest_server_skill_and_sdk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-app");
        std::fs::create_dir_all(root.join("server")).unwrap();
        std::fs::create_dir_all(root.join("skill")).unwrap();
        std::fs::create_dir_all(root.join("sdk")).unwrap();
        std::fs::write(
            root.join("app.ipynb"),
            b"{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":5}",
        )
        .unwrap();
        std::fs::write(root.join("server/main.py"), b"print('hi')\n").unwrap();
        std::fs::write(root.join("server/requirements.txt"), b"mcp>=1.0.0\n").unwrap();
        std::fs::write(root.join("skill/SKILL.md"), b"# skill\n").unwrap();
        std::fs::write(root.join("sdk/call_tool.ts"), b"// vendored\n").unwrap();

        let mut manifest = SpurAppManifest::minimal("authored-app", SPUR_APP_ENTRY_NOTEBOOK);
        manifest.mcp_server = Some(SpurAppMcpServer {
            server_type: "python".into(),
            entry: "server/main.py".into(),
            requirements: Some("server/requirements.txt".into()),
            env: Default::default(),
        });
        manifest.skill = Some("skill/SKILL.md".into());
        manifest.sdk = Some(SpurAppSdk {
            typescript: Some("sdk".into()),
        });
        std::fs::write(
            root.join(SPUR_APP_MANIFEST),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let out = tmp.path().join("out.spurapp");
        let exported = export_spur_app(SpurAppExportOptions {
            notebook_path: root.join("app.ipynb"),
            output_path: out.clone(),
            name: None,
            widget_assets: vec![],
            include_port_snapshots: false,
            dependency_roots: vec![root.clone()],
        })
        .expect("export");

        let read = archive::read_manifest(std::fs::File::open(&exported.output_path).unwrap())
            .expect("manifest");
        assert_eq!(read.name, "authored-app", "authored manifest must win");
        assert!(read.mcp_server.is_some());

        let zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).expect("zip");
        let names = zip.file_names().map(ToOwned::to_owned).collect::<Vec<_>>();
        for expected in [
            "server/main.py",
            "server/requirements.txt",
            "skill/SKILL.md",
            "sdk/call_tool.ts",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn export_bundles_embedded_manifest_as_archive_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("embedded-app");
        std::fs::create_dir_all(&root).expect("mkdir");
        let notebook_path = root.join("surface.ipynb");
        std::fs::write(
            &notebook_path,
            embedded_notebook(serde_json::json!({
                "schema": "spur.app/v1",
                "name": "Embedded Surface",
                "open_mode": "app",
                "runtime": {
                    "jute_min": "0.1.0"
                }
            })),
        )
        .expect("write notebook");

        let out = tmp.path().join("embedded.spurapp");
        export_spur_app(SpurAppExportOptions {
            notebook_path,
            output_path: out.clone(),
            name: None,
            widget_assets: vec![],
            include_port_snapshots: false,
            dependency_roots: vec![],
        })
        .expect("export");

        let read = archive::read_manifest(std::fs::File::open(&out).unwrap()).expect("manifest");
        assert_eq!(read.name, "Embedded Surface");
        assert_eq!(read.entry_notebook, "surface.ipynb");

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).expect("zip");
        zip.by_name(SPUR_APP_MANIFEST)
            .expect("archive contains spur-app.json entry");
    }

    #[test]
    fn manifest_sdk_block_round_trips_and_defaults_to_none() {
        // Absent field → None (backward compatible with existing manifests).
        let minimal = SpurAppManifest::minimal("App", "app.ipynb");
        assert!(minimal.sdk.is_none());
        let json = serde_json::to_string(&minimal).expect("serialize minimal");
        let back: SpurAppManifest = serde_json::from_str(&json).expect("deserialize minimal");
        assert!(back.sdk.is_none());

        // Declared block round-trips.
        let mut manifest = SpurAppManifest::minimal("App", "app.ipynb");
        manifest.sdk = Some(SpurAppSdk {
            typescript: Some("sdk".to_string()),
        });
        let json = serde_json::to_string(&manifest).expect("serialize with sdk");
        let back: SpurAppManifest = serde_json::from_str(&json).expect("deserialize with sdk");
        assert_eq!(
            back.sdk,
            Some(SpurAppSdk {
                typescript: Some("sdk".to_string())
            })
        );
    }
}
