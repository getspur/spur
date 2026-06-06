use std::fs;
use std::io::{Cursor, Write};

use spur_notebook::spur_app::archive::{
    read_entry, read_manifest, write_entries, SpurAppArchiveError,
};
use spur_notebook::spur_app::{
    export_spur_app, import_spur_app, is_safe_archive_path, SpurAppDependencies,
    SpurAppExportOptions, SpurAppManifest, SPUR_APP_EXTENSION, SPUR_APP_MANIFEST, SPUR_APP_SCHEMA,
};

#[test]
fn spur_app_archive_manifest_defaults_to_spur_app_v1() {
    let manifest = SpurAppManifest::minimal("Forecast Dashboard", "app.ipynb");

    assert_eq!(SPUR_APP_EXTENSION, "spurapp");
    assert_eq!(SPUR_APP_MANIFEST, "spur-app.json");
    assert_eq!(manifest.schema, SPUR_APP_SCHEMA);
    assert_eq!(manifest.entry_notebook, "app.ipynb");

    let json = serde_json::to_value(&manifest).expect("serialize manifest");
    assert_eq!(json["schema"], SPUR_APP_SCHEMA);

    let decoded: SpurAppManifest = serde_json::from_value(json).expect("deserialize manifest");
    assert_eq!(decoded, manifest);
}

#[test]
fn spur_app_archive_paths_reject_absolute_and_parent_segments() {
    assert!(is_safe_archive_path("app.ipynb"));
    assert!(is_safe_archive_path("widgets/sha256-abc.mjs"));

    assert!(!is_safe_archive_path(""));
    assert!(!is_safe_archive_path("../app.ipynb"));
    assert!(!is_safe_archive_path("widgets/../../secret"));
    assert!(!is_safe_archive_path("/tmp/app.ipynb"));
}

#[test]
fn spur_app_archive_round_trip_is_deterministic() {
    let manifest = SpurAppManifest::minimal("Forecast Dashboard", "app.ipynb");
    let manifest_json = serde_json::to_vec(&manifest).expect("serialize manifest");
    let notebook_json = br#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#.to_vec();

    let entries = vec![
        ("app.ipynb".to_string(), notebook_json.clone()),
        (SPUR_APP_MANIFEST.to_string(), manifest_json.clone()),
    ];
    let reversed_entries = vec![
        (SPUR_APP_MANIFEST.to_string(), manifest_json),
        ("app.ipynb".to_string(), notebook_json.clone()),
    ];

    let mut package = Cursor::new(Vec::new());
    write_entries(&mut package, entries).expect("write archive");

    let mut reordered_package = Cursor::new(Vec::new());
    write_entries(&mut reordered_package, reversed_entries).expect("write reordered archive");

    assert_eq!(package.get_ref(), reordered_package.get_ref());

    let decoded_notebook =
        read_entry(Cursor::new(package.into_inner()), "app.ipynb").expect("read notebook entry");
    assert_eq!(decoded_notebook, notebook_json);
}

#[test]
fn spur_app_archive_write_rejects_unsafe_entry_paths() {
    let err = write_entries(
        Cursor::new(Vec::new()),
        vec![("../secret".to_string(), Vec::new())],
    )
    .expect_err("unsafe path should fail");

    assert!(matches!(err, SpurAppArchiveError::UnsafePath(path) if path == "../secret"));
}

#[test]
fn spur_app_archive_read_rejects_unsafe_entry_paths() {
    for path in ["../secret", "/tmp/app.ipynb"] {
        let archive = raw_archive_with_entry(path, b"secret");

        let err = read_entry(Cursor::new(archive), "app.ipynb")
            .expect_err("unsafe archive path should fail");

        assert!(matches!(err, SpurAppArchiveError::UnsafePath(unsafe_path) if unsafe_path == path));
    }
}

#[test]
fn spur_app_archive_manifest_reader_reports_missing_or_invalid_manifest() {
    let mut missing_manifest = Cursor::new(Vec::new());
    write_entries(
        &mut missing_manifest,
        vec![("app.ipynb".to_string(), b"{}".to_vec())],
    )
    .expect("write archive");

    let err = read_manifest(Cursor::new(missing_manifest.into_inner()))
        .expect_err("manifest should be missing");
    assert!(matches!(err, SpurAppArchiveError::MissingManifest));

    let mut invalid_manifest = Cursor::new(Vec::new());
    write_entries(
        &mut invalid_manifest,
        vec![(SPUR_APP_MANIFEST.to_string(), b"not json".to_vec())],
    )
    .expect("write archive");

    let err = read_manifest(Cursor::new(invalid_manifest.into_inner()))
        .expect_err("manifest should be invalid JSON");
    assert!(matches!(err, SpurAppArchiveError::InvalidManifestJson(_)));
}

#[test]
fn spur_app_export_and_import_round_trips_from_package_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("source.ipynb");
    let widget_path = temp.path().join("forecast-widget.mjs");
    let output_path = temp.path().join("forecast.spurapp");
    let import_root = temp.path().join("cache");

    let notebook_json = r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
    fs::write(&notebook_path, notebook_json).expect("seed notebook");
    fs::write(&widget_path, "export default {};").expect("seed widget asset");
    fs::write(temp.path().join("requirements.txt"), "pandas==2.2.0\n").expect("seed lock");

    let exported = export_spur_app(SpurAppExportOptions {
        notebook_path: notebook_path.clone(),
        output_path: output_path.clone(),
        name: Some("Forecast Dashboard".to_string()),
        widget_assets: vec![widget_path.clone()],
        include_port_snapshots: true,
        dependency_roots: vec![temp.path().to_path_buf()],
    })
    .expect("export");

    assert_eq!(exported.manifest_path, SPUR_APP_MANIFEST);
    assert_eq!(exported.asset_count, 1);
    assert_eq!(
        exported.preflight.missing_dependency_locks,
        Vec::<String>::new()
    );
    assert_eq!(exported.preflight.warnings.len(), 1);

    let archive = fs::read(&output_path).expect("read package");
    let manifest = read_manifest(Cursor::new(archive.clone())).expect("read exported manifest");
    assert_eq!(manifest.name, "Forecast Dashboard");
    assert_eq!(manifest.entry_notebook, "app.ipynb");
    assert_eq!(manifest.widgets.len(), 1);
    assert!(manifest.widgets[0].module.starts_with("widgets/sha256-"));
    assert!(manifest.widgets[0].module.ends_with(".mjs"));
    assert_eq!(
        manifest.dependencies.python.as_deref(),
        Some("env/requirements.txt")
    );

    let widget =
        read_entry(Cursor::new(archive.clone()), &manifest.widgets[0].module).expect("read widget");
    assert_eq!(widget, b"export default {};");
    let lock = read_entry(Cursor::new(archive), "env/requirements.txt").expect("read lock");
    assert_eq!(lock, b"pandas==2.2.0\n");

    fs::remove_file(&notebook_path).expect("remove source notebook");
    fs::remove_file(&widget_path).expect("remove source widget");
    fs::remove_file(temp.path().join("requirements.txt")).expect("remove source lock");

    let imported = import_spur_app(&output_path, &import_root).expect("import");
    assert_eq!(imported.manifest, manifest);
    assert_eq!(
        fs::read_to_string(&imported.notebook_path).unwrap(),
        notebook_json
    );
    assert!(imported
        .root
        .join(imported.manifest.widgets[0].module.as_str())
        .exists());
    assert!(imported.root.join("env/requirements.txt").exists());
    assert_eq!(
        imported.preflight.missing_dependency_locks,
        Vec::<String>::new()
    );
    assert_eq!(imported.preflight.warnings.len(), 1);
}

#[test]
fn spur_app_import_preflight_reports_missing_dependency_locks_without_failing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let package_path = temp.path().join("missing-lock.spurapp");
    let import_root = temp.path().join("cache");

    let mut manifest = SpurAppManifest::minimal("Missing Lock", "app.ipynb");
    manifest.dependencies = SpurAppDependencies {
        python: Some("env/uv.lock".to_string()),
        ..SpurAppDependencies::default()
    };

    let mut package = Cursor::new(Vec::new());
    write_entries(
        &mut package,
        vec![
            (
                SPUR_APP_MANIFEST.to_string(),
                serde_json::to_vec(&manifest).expect("serialize manifest"),
            ),
            ("app.ipynb".to_string(), b"{}".to_vec()),
        ],
    )
    .expect("write package");
    fs::write(&package_path, package.into_inner()).expect("persist package");

    let imported = import_spur_app(&package_path, &import_root).expect("import");

    assert!(imported.notebook_path.exists());
    assert_eq!(
        imported.preflight.missing_dependency_locks,
        vec!["env/uv.lock".to_string()]
    );
}

#[test]
fn spur_app_import_rejects_unsafe_archive_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let package_path = temp.path().join("malicious.spurapp");
    let import_root = temp.path().join("cache");
    let manifest = SpurAppManifest::minimal("Malicious", "app.ipynb");

    let package = raw_archive_with_entries(vec![
        (
            SPUR_APP_MANIFEST.to_string(),
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        ),
        ("app.ipynb".to_string(), b"{}".to_vec()),
        ("../escape.txt".to_string(), b"escape".to_vec()),
    ]);
    fs::write(&package_path, package).expect("persist package");

    let err = import_spur_app(&package_path, &import_root)
        .expect_err("unsafe package should fail import");

    assert!(matches!(err, SpurAppArchiveError::UnsafePath(path) if path == "../escape.txt"));
    assert!(!temp.path().join("escape.txt").exists());
}

fn raw_archive_with_entry(path: &str, contents: &[u8]) -> Vec<u8> {
    raw_archive_with_entries(vec![(path.to_string(), contents.to_vec())])
}

fn raw_archive_with_entries(entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut archive = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut archive);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default_for_write());

    for (path, contents) in entries {
        writer.start_file(path, options).expect("start raw entry");
        writer.write_all(&contents).expect("write raw entry");
    }
    writer.finish().expect("finish raw archive");

    archive.into_inner()
}
