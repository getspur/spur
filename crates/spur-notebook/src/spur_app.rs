use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

pub mod archive;

pub const SPUR_APP_EXTENSION: &str = "spurapp";
pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
    pub entry_notebook: String,
    pub open_mode: String,
    pub runtime: SpurAppRuntime,
    #[serde(default)]
    pub widgets: Vec<SpurAppWidgetAsset>,
    #[serde(default)]
    pub ports: Option<SpurAppPorts>,
    #[serde(default)]
    pub dependencies: SpurAppDependencies,
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
        }
    }
}

pub fn export_spur_app(
    options: SpurAppExportOptions,
) -> Result<SpurAppExported, archive::SpurAppArchiveError> {
    let notebook_contents = fs::read(&options.notebook_path)?;
    let mut entries = vec![(SPUR_APP_ENTRY_NOTEBOOK.to_string(), notebook_contents)];
    let mut preflight = SpurAppPreflight::default();

    let name = options
        .name
        .unwrap_or_else(|| default_app_name(&options.notebook_path));
    let mut manifest = SpurAppManifest::minimal(name, SPUR_APP_ENTRY_NOTEBOOK);

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
