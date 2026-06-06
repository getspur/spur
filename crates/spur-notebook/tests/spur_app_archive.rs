use std::io::{Cursor, Write};

use spur_notebook::spur_app::archive::{
    read_entry, read_manifest, write_entries, SpurAppArchiveError,
};
use spur_notebook::spur_app::{
    is_safe_archive_path, SpurAppManifest, SPUR_APP_EXTENSION, SPUR_APP_MANIFEST, SPUR_APP_SCHEMA,
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

fn raw_archive_with_entry(path: &str, contents: &[u8]) -> Vec<u8> {
    let mut archive = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut archive);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default_for_write());

    writer.start_file(path, options).expect("start raw entry");
    writer.write_all(contents).expect("write raw entry");
    writer.finish().expect("finish raw archive");

    archive.into_inner()
}
