use std::fs;

use spur_notebook::commands::publish_spur_app_for_paths;
use spur_notebook::spur_app::archive;

#[test]
fn publish_command_exports_package_with_dependency_locks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("forecast.ipynb");
    let output_path = temp.path().join("dist").join("forecast.spurapp");
    fs::write(
        &notebook_path,
        r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
    )
    .expect("write notebook");
    fs::write(temp.path().join("requirements.txt"), "anywidget==0.9.18\n")
        .expect("write requirements");

    let response = publish_spur_app_for_paths(
        notebook_path,
        output_path.clone(),
        Some("Forecast Dashboard".to_string()),
        false,
    )
    .expect("publish spur app");

    assert_eq!(response.path, output_path.to_string_lossy().to_string());
    assert_eq!(response.asset_count, 0);
    assert_eq!(response.manifest.name, "Forecast Dashboard");
    assert_eq!(response.manifest.entry_notebook, "app.ipynb");
    assert_eq!(
        response.manifest.dependencies.python.as_deref(),
        Some("env/requirements.txt")
    );
    assert_eq!(
        response.preflight.missing_dependency_locks,
        Vec::<String>::new()
    );

    let manifest = archive::read_manifest(fs::File::open(output_path).expect("open package"))
        .expect("read manifest");
    assert_eq!(manifest.name, "Forecast Dashboard");
    assert_eq!(
        manifest.dependencies.python.as_deref(),
        Some("env/requirements.txt")
    );
}
