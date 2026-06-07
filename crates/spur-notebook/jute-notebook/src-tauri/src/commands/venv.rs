//! Commands for the management of local virtual environments with `uv`.

use std::io;

use ini::Ini;
use serde::Serialize;
use tauri::{AppHandle, Manager as _};
use tauri_plugin_shell::ShellExt as _;
use tracing::{error, info};

use crate::{
    entity::{Entity, EntityId},
    Error,
};

/// Return a list of Python versions that can be used to create a virtual
/// environment.
#[tauri::command]
pub async fn venv_list_python_versions(app: AppHandle) -> Result<Vec<String>, Error> {
    venv_list_python_versions_impl(&app).await
}

/// Implementation for [`venv_list_python_versions`] that accepts a borrowed app handle.
pub async fn venv_list_python_versions_impl(app: &AppHandle) -> Result<Vec<String>, Error> {
    let output = app
        .shell()
        .sidecar("uv")?
        .args(["--color", "never"])
        .args(["python", "list", "--all-versions"])
        .args(["--python-preference", "only-managed"])
        .output()
        .await?;

    if output.status.success() {
        Ok(parse_python_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(Error::Subprocess(io::Error::other(message.trim())))
    }
}

fn parse_python_versions(output: &str) -> Vec<String> {
    let mut versions = Vec::new();
    for line in output.lines() {
        if let Some(version_string) = line.split_whitespace().next() {
            // Some versions are prefixed with `pypy-`, ignore those for now.
            if let Some(stripped) = version_string.strip_prefix("cpython-") {
                let version_number = match stripped.find('-') {
                    Some(index) => &stripped[..index],
                    None => stripped,
                };
                versions.push(version_number.to_owned());
            }
        }
    }
    versions
}

/// Create a new virtual environment, and return its ID.
#[tauri::command]
pub async fn venv_create(python_version: &str, app: AppHandle) -> Result<EntityId, Error> {
    venv_create_impl(python_version, &app).await
}

/// Implementation for [`venv_create`] that accepts a borrowed app handle.
pub async fn venv_create_impl(python_version: &str, app: &AppHandle) -> Result<EntityId, Error> {
    let venv_id = EntityId::new(Entity::Venv);
    let venv_path = app
        .path()
        .app_data_dir()?
        .join("venv")
        .join(venv_id.to_string());

    let output = app
        .shell()
        .sidecar("uv")?
        .args(["--color", "never"])
        .args(["venv", "--no-project", "--seed", "--relocatable"])
        .args([
            "--python",
            python_version,
            "--python-preference",
            "only-managed",
        ])
        .arg(&venv_path)
        .output()
        .await?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Subprocess(io::Error::other(message.trim())));
    }

    info!("created venv at {venv_path:?}");
    let venv_python_path = venv_path.join("bin/python");

    let packages = ["ipykernel", "black", "basedpyright"];

    let output = app
        .shell()
        .sidecar("uv")?
        .args(["--color", "never"])
        .args(["pip", "install"])
        .arg("--python")
        .arg(&venv_python_path)
        .args(packages)
        .output()
        .await?;

    if !output.status.success() {
        error!("failed to install packages in venv, will remove");
        _ = tokio::fs::remove_dir_all(&venv_path).await;
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Subprocess(io::Error::other(message.trim())));
    }

    Ok(venv_id)
}

/// List item returned by [`venv_list`].
#[derive(Serialize, Debug)]
pub struct VenvListItem {
    venv_id: EntityId,
    python_version: Option<String>,
    uv_version: Option<String>,
    implementation: Option<String>,
    home: Option<String>,
}

/// Return a list of virtual environments managed by Jute.
#[tauri::command]
pub async fn venv_list(app: AppHandle) -> Result<Vec<VenvListItem>, Error> {
    venv_list_impl(&app).await
}

/// Implementation for [`venv_list`] that accepts a borrowed app handle.
pub async fn venv_list_impl(app: &AppHandle) -> Result<Vec<VenvListItem>, Error> {
    let venv_dir = app.path().app_data_dir()?.join("venv");
    let mut venvs = Vec::new();
    let mut it = match tokio::fs::read_dir(venv_dir).await {
        Ok(it) => it,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(venvs),
        Err(err) => return Err(Error::Filesystem(err)),
    };
    while let Some(entry) = it.next_entry().await.map_err(Error::Filesystem)? {
        if entry.file_type().await.is_ok_and(|f| f.is_dir()) {
            if let Ok(venv_id) = entry.file_name().into_string() {
                if let Ok(venv_id) = venv_id.parse::<EntityId>() {
                    // Read the venv metadata file to get the Python version.
                    let metadata_path = entry.path().join("pyvenv.cfg");
                    let mut python_version = None;
                    let mut uv_version = None;
                    let mut implementation = None;
                    let mut home = None;
                    if let Ok(metadata) = tokio::fs::read_to_string(&metadata_path).await {
                        if let Ok(conf) = Ini::load_from_str(&metadata) {
                            let sec = conf.general_section();
                            python_version = sec.get("version_info").map(String::from);
                            uv_version = sec.get("uv").map(String::from);
                            implementation = sec.get("implementation").map(String::from);
                            home = sec.get("home").map(String::from);
                        }
                    }
                    venvs.push(VenvListItem {
                        venv_id,
                        python_version,
                        uv_version,
                        implementation,
                        home,
                    });
                }
            }
        }
    }
    Ok(venvs)
}

/// Delete a virtual environment by ID.
#[tauri::command]
pub async fn venv_delete(venv_id: EntityId, app: AppHandle) -> Result<bool, Error> {
    venv_delete_impl(venv_id, &app).await
}

/// Implementation for [`venv_delete`] that accepts a borrowed app handle.
pub async fn venv_delete_impl(venv_id: EntityId, app: &AppHandle) -> Result<bool, Error> {
    let venv_dir = app.path().app_data_dir()?.join("venv");
    let venv_path = venv_dir.join(venv_id.to_string());
    if tokio::fs::metadata(&venv_path).await.is_ok() {
        tokio::fs::remove_dir_all(&venv_path)
            .await
            .map_err(Error::Filesystem)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_managed_cpython_versions() {
        let versions = parse_python_versions(
            "\
cpython-3.13.2-macos-aarch64-none     <download available>
pypy-3.10.16-macos-aarch64-none        <download available>
cpython-3.12.9-macos-aarch64-none      /Users/me/.local/bin/python
cpython-3.11.11
",
        );

        assert_eq!(versions, vec!["3.13.2", "3.12.9", "3.11.11"]);
    }
}
