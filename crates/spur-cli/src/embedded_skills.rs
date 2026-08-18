//! Compile-time bundled skill assets and their filesystem materializer.

use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

struct EmbeddedSkillFile {
    path: &'static str,
    bytes: &'static [u8],
    mode: u32,
}

include!(concat!(env!("OUT_DIR"), "/embedded_skills.rs"));

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Make the embedded asset tree available as spur-core's lowest-priority
/// bundled skill source.
pub fn register() {
    spur_core::skills::register_embedded_skill_root(materialize_for_core);
}

fn materialize_for_core() -> Result<PathBuf, String> {
    (|| {
        let cache_base = default_cache_base()?;
        materialize_under(&cache_base)
    })()
    .map_err(|error| format!("{error:#}"))
}

fn default_cache_base() -> Result<PathBuf> {
    let base_dirs = directories::BaseDirs::new();
    cache_base_from_home(base_dirs.as_ref().map(directories::BaseDirs::home_dir))
}

fn cache_base_from_home(home_dir: Option<&Path>) -> Result<PathBuf> {
    let home_dir = home_dir.context("cannot resolve a per-user home directory for skill cache")?;
    Ok(home_dir.join(".spur/cache/embedded-skills"))
}

fn materialize_under(cache_base: &Path) -> Result<PathBuf> {
    let generation = cache_base.join(EMBEDDED_SKILL_DIGEST);
    let skill_root = generation.join("skills");
    let completion_marker = generation.join(".complete");
    if completion_marker.is_file() && skill_root.is_dir() {
        return Ok(skill_root);
    }

    std::fs::create_dir_all(cache_base)
        .with_context(|| format!("create embedded skill cache {}", cache_base.display()))?;
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staging = cache_base.join(format!(
        ".{}-{}-{sequence}",
        EMBEDDED_SKILL_DIGEST,
        std::process::id()
    ));
    let staging_skill_root = staging.join("skills");

    let staged = (|| -> Result<()> {
        std::fs::create_dir(&staging)
            .with_context(|| format!("create embedded skill staging {}", staging.display()))?;
        std::fs::create_dir(&staging_skill_root).with_context(|| {
            format!(
                "create embedded skill staging root {}",
                staging_skill_root.display()
            )
        })?;
        for file in EMBEDDED_SKILL_FILES {
            write_embedded_file(&staging_skill_root, file)?;
        }
        std::fs::write(staging.join(".complete"), EMBEDDED_SKILL_DIGEST)
            .context("write embedded skill completion marker")?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    match std::fs::rename(&staging, &generation) {
        Ok(()) => Ok(skill_root),
        Err(_) if completion_marker.is_file() && skill_root.is_dir() => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(skill_root)
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(error).with_context(|| {
                format!(
                    "publish embedded skill cache generation {}",
                    generation.display()
                )
            })
        }
    }
}

fn write_embedded_file(root: &Path, file: &EmbeddedSkillFile) -> Result<()> {
    let relative = Path::new(file.path);
    if relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        bail!("invalid embedded skill path: {}", file.path);
    }

    let destination = root.join(relative);
    let parent = destination
        .parent()
        .context("embedded skill file must have a parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create embedded skill directory {}", parent.display()))?;
    std::fs::write(&destination, file.bytes)
        .with_context(|| format!("write embedded skill file {}", destination.display()))?;
    set_mode(&destination, file.mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("set embedded skill permissions for {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_complete_tree_and_nested_resources() {
        let cache = tempfile::tempdir().unwrap();

        let root = materialize_under(cache.path()).unwrap();

        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
        let source_files = source_files(&source_root);
        assert_eq!(EMBEDDED_SKILL_FILES.len(), source_files.len());
        for source in source_files {
            let relative = source.strip_prefix(&source_root).unwrap();
            assert_eq!(
                std::fs::read(root.join(relative)).unwrap(),
                std::fs::read(source).unwrap()
            );
        }
        assert!(root
            .join("explainer-video-editor/scripts/validate-delivery.sh")
            .is_file());
        assert!(root
            .join("notebook-mcp/references/tool-surface.md")
            .is_file());
    }

    #[test]
    fn reuses_completed_cache_generation() {
        let cache = tempfile::tempdir().unwrap();
        let first = materialize_under(cache.path()).unwrap();

        let second = materialize_under(cache.path()).unwrap();

        assert_eq!(first, second);
        assert!(first.parent().unwrap().join(".complete").is_file());
    }

    #[test]
    fn requires_a_per_user_home_for_the_cache() {
        let error = cache_base_from_home(None).unwrap_err();

        assert!(error.to_string().contains("per-user home directory"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_canonical_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let cache = tempfile::tempdir().unwrap();
        let root = materialize_under(cache.path()).unwrap();
        let executable_mode = std::fs::metadata(root.join("systematic-debugging/find-polluter.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let ordinary_mode = std::fs::metadata(root.join("code-explore/SKILL.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(executable_mode, 0o755);
        assert_eq!(ordinary_mode, 0o644);
    }

    fn source_files(root: &Path) -> Vec<PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    files.push(entry.path());
                }
            }
        }
        files
    }
}
