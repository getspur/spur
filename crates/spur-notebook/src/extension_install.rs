use std::{
    io,
    path::{Path, PathBuf},
};

use directories::BaseDirs;

pub fn bundled_extension_filename() -> String {
    format!("spur_rest-{}.duckdb_extension", platform())
}

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux_amd64",
        ("linux", "aarch64") => "linux_arm64",
        ("macos", "aarch64") => "osx_arm64",
        ("macos", "x86_64") => "osx_amd64",
        _ => "unknown",
    }
}

pub fn extension_install_dir() -> PathBuf {
    BaseDirs::new()
        .map(|base_dirs| base_dirs.home_dir().join(".spur").join("extensions"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".spur").join("extensions"))
        })
        .unwrap_or_else(|| PathBuf::from(".spur").join("extensions"))
}

pub fn install_bundled_extension(resource_root: &Path) -> io::Result<Option<PathBuf>> {
    install_bundled_extension_into(resource_root, &extension_install_dir())
}

fn install_bundled_extension_into(
    resource_root: &Path,
    install_dir: &Path,
) -> io::Result<Option<PathBuf>> {
    let file = bundled_extension_filename();
    let dest = install_dir.join(&file);
    if dest.exists() {
        return Ok(None);
    }

    let src = resource_root.join("extensions").join(&file);
    if !src.exists() {
        return Ok(None);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&src, &dest)?;
    Ok(Some(dest))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn copies_bundled_extension_when_source_exists_and_dest_is_missing() -> io::Result<()> {
        let resource_root = tempfile::tempdir()?;
        let install_dir = tempfile::tempdir()?;
        let file = bundled_extension_filename();
        let src = resource_root.path().join("extensions").join(&file);
        let dest = install_dir.path().join(&file);

        fs::create_dir_all(src.parent().expect("src has parent"))?;
        fs::write(&src, b"extension bytes")?;

        let installed = install_bundled_extension_into(resource_root.path(), install_dir.path())?;

        assert_eq!(installed, Some(dest.clone()));
        assert_eq!(fs::read(dest)?, b"extension bytes");
        Ok(())
    }

    #[test]
    fn leaves_existing_destination_file_untouched() -> io::Result<()> {
        let resource_root = tempfile::tempdir()?;
        let install_dir = tempfile::tempdir()?;
        let file = bundled_extension_filename();
        let src = resource_root.path().join("extensions").join(&file);
        let dest = install_dir.path().join(&file);

        fs::create_dir_all(src.parent().expect("src has parent"))?;
        fs::write(&src, b"extension bytes")?;
        fs::write(&dest, b"sentinel bytes")?;

        let installed = install_bundled_extension_into(resource_root.path(), install_dir.path())?;

        assert_eq!(installed, None);
        assert_eq!(fs::read(dest)?, b"sentinel bytes");
        Ok(())
    }

    #[test]
    fn returns_none_when_bundled_source_is_missing() -> io::Result<()> {
        let resource_root = tempfile::tempdir()?;
        let install_dir = tempfile::tempdir()?;

        let installed = install_bundled_extension_into(resource_root.path(), install_dir.path())?;

        assert_eq!(installed, None);
        Ok(())
    }
}
