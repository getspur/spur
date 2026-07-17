use super::resolver::{ResolvedSkill, ResolvedSource, ResolvedSourceKind};
use super::ProjectionRequest;
use crate::skills::adapters::{render_kiro_steering_pointer, Adapter};
use fs4::fs_std::FileExt as _;
use sha2::{Digest, Sha256};
use std::fs::{self, DirEntry, OpenOptions};
use std::path::{Component, Path, PathBuf};

/// Version of the adapter-rendering contract included in generation digests.
pub const RENDERER_SCHEMA_VERSION: u32 = 1;

/// Filesystem shape projected into an adapter discovery path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// The target links or copies a complete skill directory.
    Directory,
    /// The target links or copies one rendered file.
    File,
}

/// One adapter-native target backed by a path in an immutable generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredTarget {
    /// Canonical skill ID, or `__pointer` for the Kiro companion.
    pub skill_id: String,
    /// Selected source identity for a skill or adapter-owned companion.
    pub source: ResolvedSource,
    /// Launch-root-relative adapter discovery target.
    pub target_rel: String,
    /// Generation-root-relative source for the target.
    pub generation_rel: String,
    /// Whether the projected target is a directory or file.
    pub target_kind: TargetKind,
    /// SHA-256 of the rendered target contents.
    pub content_sha256: String,
}

/// Validated immutable generation ready for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedGeneration {
    /// Adapter whose native files the generation contains.
    pub adapter: Adapter,
    /// Content-addressed generation digest.
    pub digest: String,
    /// Absolute or launch-root-relative published generation directory.
    pub root: PathBuf,
    /// Deterministically ordered adapter targets.
    pub targets: Vec<DesiredTarget>,
}

/// Failure while staging, hashing, validating, or publishing a generation.
#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    /// A source entry could escape its selected skill directory.
    #[error("unsafe source path {path} for skill {skill_id}")]
    UnsafeSourcePath {
        /// Canonical skill ID being staged.
        skill_id: String,
        /// Unsafe source entry.
        path: PathBuf,
    },
    /// An adapter renderer returned a target outside the launch root.
    #[error("rendered target escaped launch root: {path}")]
    TargetEscaped {
        /// Renderer-provided target path.
        path: PathBuf,
    },
    /// Filesystem operation failed.
    #[error("generation I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Content hashing or immutable-generation validation failed.
    #[error("generation hashing failed: {0}")]
    Hash(#[source] anyhow::Error),
}

/// Render, validate, and atomically publish or reuse one adapter generation.
pub(crate) fn publish_generation(
    request: ProjectionRequest<'_>,
    selected: &[ResolvedSkill],
) -> Result<PublishedGeneration, GenerationError> {
    let generations = create_generation_root(request.launch_root, request.adapter)?;
    let _publication_lock = lock_generation_publication(&generations)?;
    let mut staging = StagingDirectory::create(&generations)?;
    let mut targets =
        Vec::with_capacity(selected.len() + usize::from(request.adapter == Adapter::Kiro));

    let mut ordered = selected.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.payload.id.cmp(&right.payload.id));
    for skill in ordered {
        targets.push(stage_skill(&request, skill, staging.path())?);
    }
    if request.adapter == Adapter::Kiro {
        targets.push(stage_kiro_companion(&request, staging.path())?);
    }
    targets.sort_by(|left, right| left.target_rel.cmp(&right.target_rel));

    let staged_tree_sha256 = hash_tree(staging.path())?;
    validate_targets(staging.path(), &targets)?;
    let digest = generation_digest(request.adapter, &staged_tree_sha256);
    let published_root = generations.join(&digest);

    if path_exists_no_follow(&published_root)? {
        validate_existing_generation(
            &published_root,
            request.adapter,
            &digest,
            &staged_tree_sha256,
            &targets,
        )?;
    } else {
        match fs::rename(staging.path(), &published_root) {
            Ok(()) => staging.disarm(),
            Err(source) => {
                if path_exists_no_follow(&published_root)? {
                    validate_existing_generation(
                        &published_root,
                        request.adapter,
                        &digest,
                        &staged_tree_sha256,
                        &targets,
                    )?;
                } else {
                    return Err(GenerationError::Io {
                        path: published_root,
                        source,
                    });
                }
            }
        }
    }

    Ok(PublishedGeneration {
        adapter: request.adapter,
        digest,
        root: published_root,
        targets,
    })
}

fn create_generation_root(
    launch_root: &Path,
    adapter: Adapter,
) -> Result<PathBuf, GenerationError> {
    validate_launch_root(launch_root)?;
    let mut current = launch_root.to_path_buf();
    for component in [
        ".spur",
        "runtime",
        "skill-projections",
        adapter.key(),
        "generations",
    ] {
        current.push(component);
        ensure_directory_component_with(&current, |path| fs::create_dir(path))?;
    }
    Ok(current)
}

fn ensure_directory_component_with(
    path: &Path,
    create: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), GenerationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory_component(path, &metadata),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => match create(path) {
            Ok(()) => validate_directory_component(path, &symlink_metadata(path)?),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_directory_component(path, &symlink_metadata(path)?)
            }
            Err(source) => Err(GenerationError::Io {
                path: path.to_path_buf(),
                source,
            }),
        },
        Err(source) => Err(GenerationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn validate_directory_component(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), GenerationError> {
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(GenerationError::TargetEscaped {
            path: path.to_path_buf(),
        })
    }
}

fn validate_launch_root(launch_root: &Path) -> Result<(), GenerationError> {
    let absolute = std::path::absolute(launch_root).map_err(|source| GenerationError::Io {
        path: launch_root.to_path_buf(),
        source,
    })?;
    let mut ancestors = absolute
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata = fs::symlink_metadata(ancestor).map_err(|source| GenerationError::Io {
            path: ancestor.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(GenerationError::TargetEscaped {
                path: ancestor.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn lock_generation_publication(generations: &Path) -> Result<std::fs::File, GenerationError> {
    let lock_path = generations.join(".publish.lock");
    let lock = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(lock) => lock,
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = symlink_metadata(&lock_path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(GenerationError::TargetEscaped { path: lock_path });
            }
            OpenOptions::new()
                .write(true)
                .open(&lock_path)
                .map_err(|source| GenerationError::Io {
                    path: lock_path.clone(),
                    source,
                })?
        }
        Err(source) => {
            return Err(GenerationError::Io {
                path: lock_path,
                source,
            });
        }
    };
    lock.lock_exclusive()
        .map_err(|source| GenerationError::Io {
            path: lock_path,
            source,
        })?;
    Ok(lock)
}

fn stage_skill(
    request: &ProjectionRequest<'_>,
    skill: &ResolvedSkill,
    staging_root: &Path,
) -> Result<DesiredTarget, GenerationError> {
    let skill_rel = PathBuf::from("skills").join(&skill.payload.id);
    let skill_stage = staging_root.join(&skill_rel);
    create_dir_all(&skill_stage)?;
    copy_supporting_assets(&skill.payload.id, &skill.source.source_dir, &skill_stage)?;

    let rendered = request.adapter.render(&skill.payload, request.launch_root);
    let file_name = rendered
        .path
        .file_name()
        .ok_or_else(|| GenerationError::TargetEscaped {
            path: rendered.path.clone(),
        })?;
    let rendered_stage = skill_stage.join(file_name);
    write_file(&rendered_stage, &rendered.bytes)?;

    let (target_path, generation_path, target_kind) = if request.adapter.target_is_directory() {
        let target_path = rendered
            .path
            .parent()
            .ok_or_else(|| GenerationError::TargetEscaped {
                path: rendered.path.clone(),
            })?;
        (target_path, skill_rel, TargetKind::Directory)
    } else {
        (
            rendered.path.as_path(),
            skill_rel.join(file_name),
            TargetKind::File,
        )
    };
    let target_rel = adapter_relative_path(request.launch_root, target_path)?;
    let generation_rel = normalized_relative_path(&generation_path).ok_or_else(|| {
        GenerationError::TargetEscaped {
            path: generation_path.clone(),
        }
    })?;
    let content_sha256 = hash_target(&staging_root.join(&generation_path), target_kind)?;

    Ok(DesiredTarget {
        skill_id: skill.payload.id.clone(),
        source: skill.source.clone(),
        target_rel,
        generation_rel,
        target_kind,
        content_sha256,
    })
}

fn stage_kiro_companion(
    request: &ProjectionRequest<'_>,
    staging_root: &Path,
) -> Result<DesiredTarget, GenerationError> {
    let rendered = render_kiro_steering_pointer(request.launch_root);
    let file_name = rendered
        .path
        .file_name()
        .ok_or_else(|| GenerationError::TargetEscaped {
            path: rendered.path.clone(),
        })?;
    let generation_path = PathBuf::from("companions").join(file_name);
    let staged_file = staging_root.join(&generation_path);
    write_file(&staged_file, &rendered.bytes)?;
    let content_sha256 = hash_file(&staged_file)?;

    Ok(DesiredTarget {
        skill_id: "__pointer".to_string(),
        source: ResolvedSource {
            kind: ResolvedSourceKind::Bundled,
            content_sha256: content_sha256.clone(),
            source_dir: PathBuf::new(),
        },
        target_rel: adapter_relative_path(request.launch_root, &rendered.path)?,
        generation_rel: normalized_relative_path(&generation_path).ok_or_else(|| {
            GenerationError::TargetEscaped {
                path: generation_path,
            }
        })?,
        target_kind: TargetKind::File,
        content_sha256,
    })
}

fn copy_supporting_assets(
    skill_id: &str,
    source_root: &Path,
    destination_root: &Path,
) -> Result<(), GenerationError> {
    let source_metadata = symlink_metadata(source_root)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(unsafe_source(skill_id, source_root));
    }
    let canonical_root = fs::canonicalize(source_root).map_err(|source| GenerationError::Io {
        path: source_root.to_path_buf(),
        source,
    })?;
    copy_source_directory(
        skill_id,
        source_root,
        source_root,
        &canonical_root,
        destination_root,
    )
}

fn copy_source_directory(
    skill_id: &str,
    source_root: &Path,
    directory: &Path,
    canonical_root: &Path,
    destination_root: &Path,
) -> Result<(), GenerationError> {
    for entry in sorted_entries(directory)? {
        let source_path = entry.path();
        let relative = source_path
            .strip_prefix(source_root)
            .map_err(|_| unsafe_source(skill_id, &source_path))?;
        if normalized_relative_path(relative).is_none() {
            return Err(unsafe_source(skill_id, &source_path));
        }

        let file_type = entry.file_type().map_err(|source| GenerationError::Io {
            path: source_path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(unsafe_source(skill_id, &source_path));
        }
        ensure_canonical_source(skill_id, canonical_root, &source_path)?;

        if file_type.is_dir() {
            let destination = destination_root.join(relative);
            create_dir_all(&destination)?;
            copy_source_directory(
                skill_id,
                source_root,
                &source_path,
                canonical_root,
                destination_root,
            )?;
        } else if file_type.is_file() {
            if relative == Path::new("SKILL.md") {
                continue;
            }
            let destination = destination_root.join(relative);
            if let Some(parent) = destination.parent() {
                create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination).map_err(|source| GenerationError::Io {
                path: destination.clone(),
                source,
            })?;
            normalize_copied_file_permissions(&source_path, &destination)?;
        } else {
            return Err(unsafe_source(skill_id, &source_path));
        }
    }
    Ok(())
}

fn ensure_canonical_source(
    skill_id: &str,
    canonical_root: &Path,
    source_path: &Path,
) -> Result<(), GenerationError> {
    let canonical = fs::canonicalize(source_path).map_err(|source| GenerationError::Io {
        path: source_path.to_path_buf(),
        source,
    })?;
    if !canonical.starts_with(canonical_root) {
        return Err(unsafe_source(skill_id, source_path));
    }
    Ok(())
}

fn adapter_relative_path(launch_root: &Path, target: &Path) -> Result<String, GenerationError> {
    let relative =
        target
            .strip_prefix(launch_root)
            .map_err(|_| GenerationError::TargetEscaped {
                path: target.to_path_buf(),
            })?;
    normalized_relative_path(relative).ok_or_else(|| GenerationError::TargetEscaped {
        path: target.to_path_buf(),
    })
}

fn normalized_relative_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        parts.push(part.to_str()?);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn validate_existing_generation(
    root: &Path,
    adapter: Adapter,
    digest: &str,
    expected_tree_sha256: &str,
    targets: &[DesiredTarget],
) -> Result<(), GenerationError> {
    let metadata = symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(GenerationError::Hash(anyhow::anyhow!(
            "existing generation is not an ordinary directory: {}",
            root.display()
        )));
    }
    let actual_tree_sha256 = hash_tree(root)?;
    if actual_tree_sha256 != expected_tree_sha256 {
        return Err(GenerationError::Hash(anyhow::anyhow!(
            "existing generation {} has tree digest {}, expected {}",
            root.display(),
            actual_tree_sha256,
            expected_tree_sha256
        )));
    }
    let actual_digest = generation_digest(adapter, &actual_tree_sha256);
    if actual_digest != digest {
        return Err(GenerationError::Hash(anyhow::anyhow!(
            "existing generation {} has generation digest {}, expected {}",
            root.display(),
            actual_digest,
            digest
        )));
    }
    validate_targets(root, targets)
}

fn validate_targets(
    generation_root: &Path,
    targets: &[DesiredTarget],
) -> Result<(), GenerationError> {
    for target in targets {
        let relative = Path::new(&target.generation_rel);
        if normalized_relative_path(relative).as_deref() != Some(target.generation_rel.as_str()) {
            return Err(GenerationError::Hash(anyhow::anyhow!(
                "unsafe generation target path {}",
                target.generation_rel
            )));
        }
        let target_path = generation_root.join(relative);
        let metadata = symlink_metadata(&target_path)?;
        let shape_matches = match target.target_kind {
            TargetKind::Directory => metadata.is_dir() && !metadata.file_type().is_symlink(),
            TargetKind::File => metadata.is_file() && !metadata.file_type().is_symlink(),
        };
        if !shape_matches {
            return Err(GenerationError::Hash(anyhow::anyhow!(
                "generation target has unexpected shape: {}",
                target_path.display()
            )));
        }
        let actual = hash_target(&target_path, target.target_kind)?;
        if actual != target.content_sha256 {
            return Err(GenerationError::Hash(anyhow::anyhow!(
                "generation target {} has digest {}, expected {}",
                target_path.display(),
                actual,
                target.content_sha256
            )));
        }
    }
    Ok(())
}

fn generation_digest(adapter: Adapter, staged_tree_sha256: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"spur-skill-projection-generation\0");
    hasher.update(RENDERER_SCHEMA_VERSION.to_be_bytes());
    hasher.update(b"\0");
    hasher.update(adapter.key().as_bytes());
    hasher.update(b"\0");
    hasher.update(staged_tree_sha256.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hash_target(path: &Path, kind: TargetKind) -> Result<String, GenerationError> {
    match kind {
        TargetKind::Directory => hash_tree(path),
        TargetKind::File => hash_file(path),
    }
}

fn hash_tree(root: &Path) -> Result<String, GenerationError> {
    let metadata = symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(GenerationError::Hash(anyhow::anyhow!(
            "tree root is not an ordinary directory: {}",
            root.display()
        )));
    }
    let mut entries = Vec::new();
    collect_tree_entries(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update([entry.kind]);
        hasher.update(b"\0");
        hasher.update(entry.relative.as_bytes());
        hasher.update(b"\0");
        if let Some(content_sha256) = entry.content_sha256 {
            hasher.update(content_sha256.as_bytes());
        }
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_tree_entries(
    root: &Path,
    directory: &Path,
    output: &mut Vec<TreeEntry>,
) -> Result<(), GenerationError> {
    for entry in sorted_entries(directory)? {
        let path = entry.path();
        let relative_path = path.strip_prefix(root).map_err(|error| {
            GenerationError::Hash(anyhow::anyhow!(
                "failed to relativize {} against {}: {error}",
                path.display(),
                root.display()
            ))
        })?;
        let relative = normalized_relative_path(relative_path).ok_or_else(|| {
            GenerationError::Hash(anyhow::anyhow!(
                "tree contains a non-portable path: {}",
                path.display()
            ))
        })?;
        let file_type = entry.file_type().map_err(|source| GenerationError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(GenerationError::Hash(anyhow::anyhow!(
                "tree contains a symlink: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            output.push(TreeEntry {
                relative,
                kind: b'd',
                content_sha256: None,
            });
            collect_tree_entries(root, &path, output)?;
        } else if file_type.is_file() {
            output.push(TreeEntry {
                relative,
                kind: b'f',
                content_sha256: Some(hash_file(&path)?),
            });
        } else {
            return Err(GenerationError::Hash(anyhow::anyhow!(
                "tree contains an unsupported entry: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, GenerationError> {
    let bytes = fs::read(path).map_err(|source| GenerationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"spur-skill-projection-file\0");
    hasher.update(file_mode(path)?.to_be_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn sorted_entries(directory: &Path) -> Result<Vec<DirEntry>, GenerationError> {
    let read_dir = fs::read_dir(directory).map_err(|source| GenerationError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut entries = read_dir
        .map(|result| {
            result.map_err(|source| GenerationError::Io {
                path: directory.to_path_buf(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn create_dir_all(path: &Path) -> Result<(), GenerationError> {
    fs::create_dir_all(path).map_err(|source| GenerationError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), GenerationError> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    fs::write(path, bytes).map_err(|source| GenerationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    normalize_rendered_file_permissions(path)
}

#[cfg(unix)]
fn normalize_copied_file_permissions(
    source: &Path,
    destination: &Path,
) -> Result<(), GenerationError> {
    use std::os::unix::fs::PermissionsExt as _;

    let source_metadata = fs::metadata(source).map_err(|error| GenerationError::Io {
        path: source.to_path_buf(),
        source: error,
    })?;
    let mode = if source_metadata.permissions().mode() & 0o111 == 0 {
        0o644
    } else {
        0o755
    };
    fs::set_permissions(destination, fs::Permissions::from_mode(mode)).map_err(|source| {
        GenerationError::Io {
            path: destination.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn normalize_copied_file_permissions(
    _source: &Path,
    destination: &Path,
) -> Result<(), GenerationError> {
    normalize_rendered_file_permissions(destination)
}

#[cfg(unix)]
fn normalize_rendered_file_permissions(path: &Path) -> Result<(), GenerationError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).map_err(|source| {
        GenerationError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn normalize_rendered_file_permissions(path: &Path) -> Result<(), GenerationError> {
    let mut permissions = fs::metadata(path)
        .map_err(|source| GenerationError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).map_err(|source| GenerationError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<u32, GenerationError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .map_err(|source| GenerationError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn file_mode(path: &Path) -> Result<u32, GenerationError> {
    fs::metadata(path)
        .map(|metadata| u32::from(metadata.permissions().readonly()))
        .map_err(|source| GenerationError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn symlink_metadata(path: &Path) -> Result<fs::Metadata, GenerationError> {
    fs::symlink_metadata(path).map_err(|source| GenerationError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn path_exists_no_follow(path: &Path) -> Result<bool, GenerationError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(GenerationError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn unsafe_source(skill_id: &str, path: &Path) -> GenerationError {
    GenerationError::UnsafeSourcePath {
        skill_id: skill_id.to_string(),
        path: path.to_path_buf(),
    }
}

#[derive(Debug)]
struct TreeEntry {
    relative: String,
    kind: u8,
    content_sha256: Option<String>,
}

struct StagingDirectory {
    path: PathBuf,
    armed: bool,
}

impl StagingDirectory {
    fn create(generations: &Path) -> Result<Self, GenerationError> {
        for _ in 0..8 {
            let path = generations.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path, armed: true }),
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(GenerationError::Io { path, source }),
            }
        }
        let path = generations.join(".tmp-uuid-collision");
        Err(GenerationError::Io {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique staging directory",
            ),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_directory_component_with, publish_generation, GenerationError, TargetKind};
    use crate::skills::adapters::Adapter;
    use crate::skills::projection::test_support::ProjectionFixture;

    #[test]
    fn equal_inputs_reuse_the_same_generation() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("with-assets", "both", "BODY");
        fixture.write_support("with-assets", "scripts/check.sh", b"#!/bin/sh\n");
        let mut selected = fixture.resolve().unwrap();

        let first = publish_generation(fixture.request(), &selected).unwrap();
        selected.reverse();
        let second = publish_generation(fixture.request(), &selected).unwrap();

        assert_eq!(first.digest, second.digest);
        assert_eq!(first.root, second.root);
        assert_eq!(first.targets, second.targets);
        assert!(first
            .root
            .join("skills/with-assets/scripts/check.sh")
            .is_file());
        assert!(first.root.join("skills/with-assets/SKILL.md").is_file());
        assert_eq!(first.targets.len(), 1);
        assert_eq!(
            first.targets[0].target_rel,
            ".codex/skills/spurpower-with-assets"
        );
        assert_eq!(first.targets[0].generation_rel, "skills/with-assets");
        assert_eq!(first.targets[0].target_kind, TargetKind::Directory);
    }

    #[test]
    fn directory_creation_order_does_not_change_the_generation_digest() {
        let first_fixture = ProjectionFixture::new(Adapter::Codex);
        first_fixture.write_bundled_skill("ordered", "both", "BODY");
        first_fixture.write_support("ordered", "z-last.txt", b"last\n");
        first_fixture.write_support("ordered", "a-first.txt", b"first\n");
        let first_selected = first_fixture.resolve().unwrap();

        let second_fixture = ProjectionFixture::new(Adapter::Codex);
        second_fixture.write_bundled_skill("ordered", "both", "BODY");
        second_fixture.write_support("ordered", "a-first.txt", b"first\n");
        second_fixture.write_support("ordered", "z-last.txt", b"last\n");
        let second_selected = second_fixture.resolve().unwrap();

        let first = publish_generation(first_fixture.request(), &first_selected).unwrap();
        let second = publish_generation(second_fixture.request(), &second_selected).unwrap();

        assert_eq!(first.digest, second.digest);
    }

    #[test]
    fn cursor_records_its_native_file_target_and_preserves_supporting_assets() {
        let fixture = ProjectionFixture::new(Adapter::Cursor);
        fixture.write_bundled_skill("cursor-native", "both", "CURSOR BODY");
        fixture.write_support(
            "cursor-native",
            "references/guide.md",
            b"supporting guide\n",
        );
        let selected = fixture.resolve().unwrap();

        let published = publish_generation(fixture.request(), &selected).unwrap();

        assert_eq!(published.targets.len(), 1);
        let target = &published.targets[0];
        assert_eq!(target.skill_id, "cursor-native");
        assert_eq!(
            target.target_rel,
            ".cursor/rules/spurpower-cursor-native.mdc"
        );
        assert_eq!(
            target.generation_rel,
            "skills/cursor-native/spurpower-cursor-native.mdc"
        );
        assert_eq!(target.target_kind, TargetKind::File);
        assert!(published.root.join(&target.generation_rel).is_file());
        assert!(published
            .root
            .join("skills/cursor-native/references/guide.md")
            .is_file());
        let rendered =
            std::fs::read_to_string(published.root.join(&target.generation_rel)).unwrap();
        assert!(rendered.contains("alwaysApply: true"));
        assert!(rendered.contains("CURSOR BODY"));
    }

    #[test]
    fn kiro_records_the_steering_companion_in_the_same_generation() {
        let fixture = ProjectionFixture::new(Adapter::Kiro);
        fixture.write_bundled_skill("kiro-native", "both", "KIRO BODY");
        let selected = fixture.resolve().unwrap();

        let published = publish_generation(fixture.request(), &selected).unwrap();

        assert_eq!(published.targets.len(), 2);
        let skill = published
            .targets
            .iter()
            .find(|target| target.skill_id == "kiro-native")
            .unwrap();
        assert_eq!(skill.target_rel, ".kiro/skills/spurpower-kiro-native");
        assert_eq!(skill.generation_rel, "skills/kiro-native");
        assert_eq!(skill.target_kind, TargetKind::Directory);

        let companion = published
            .targets
            .iter()
            .find(|target| target.skill_id == "__pointer")
            .unwrap();
        assert_eq!(companion.target_rel, ".kiro/steering/spurpower-pointer.md");
        assert_eq!(companion.generation_rel, "companions/spurpower-pointer.md");
        assert_eq!(companion.target_kind, TargetKind::File);
        let rendered =
            std::fs::read_to_string(published.root.join(&companion.generation_rel)).unwrap();
        assert!(rendered.contains("name: spurpower-pointer"));
        assert!(rendered.contains(".kiro/skills/spurpower-"));
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_escaping_the_skill_root_is_rejected() {
        use std::os::unix::fs::symlink;

        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("escaped", "both", "BODY");
        let selected = fixture.resolve().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, b"outside\n").unwrap();
        let link = selected[0].source.source_dir.join("references/escape.txt");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&outside_file, &link).unwrap();

        let error = publish_generation(fixture.request(), &selected).unwrap_err();

        assert!(matches!(
            error,
            GenerationError::UnsafeSourcePath {
                ref skill_id,
                ref path,
            } if skill_id == "escaped" && path == &link
        ));
        let generations = fixture
            .launch_root()
            .join(".spur/runtime/skill-projections/codex/generations");
        if generations.exists() {
            assert!(std::fs::read_dir(generations).unwrap().all(|entry| {
                entry
                    .map(|entry| entry.file_name() == ".publish.lock")
                    .unwrap_or(false)
            }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn launch_root_with_a_symlinked_ancestor_is_rejected() {
        use crate::skills::projection::{ProjectionRequest, RuntimeRole, SelectionPolicy};
        use std::os::unix::fs::symlink;

        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("escaped-launch", "both", "BODY");
        let selected = fixture.resolve().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let actual_launch = tempfile::tempdir().unwrap();
        let linked_launch = parent.path().join("linked-launch");
        symlink(actual_launch.path(), &linked_launch).unwrap();
        let request = ProjectionRequest {
            source_repo_root: fixture.repo_root(),
            launch_root: &linked_launch,
            adapter: Adapter::Codex,
            role: RuntimeRole::Init,
            policy: SelectionPolicy::AllActive,
        };

        let error = publish_generation(request, &selected).unwrap_err();

        assert!(matches!(
            error,
            GenerationError::TargetEscaped { ref path } if path == &linked_launch
        ));
        assert!(!actual_launch.path().join(".spur").exists());
    }

    #[cfg(unix)]
    #[test]
    fn supporting_asset_modes_participate_in_generation_digests() {
        use std::os::unix::fs::PermissionsExt;

        let first_fixture = ProjectionFixture::new(Adapter::Codex);
        first_fixture.write_bundled_skill("executable", "both", "BODY");
        first_fixture.write_support("executable", "scripts/check.sh", b"#!/bin/sh\n");
        let first_selected = first_fixture.resolve().unwrap();
        let first_source = first_selected[0].source.source_dir.join("scripts/check.sh");
        std::fs::set_permissions(&first_source, std::fs::Permissions::from_mode(0o644)).unwrap();

        let second_fixture = ProjectionFixture::new(Adapter::Codex);
        second_fixture.write_bundled_skill("executable", "both", "BODY");
        second_fixture.write_support("executable", "scripts/check.sh", b"#!/bin/sh\n");
        let second_selected = second_fixture.resolve().unwrap();
        let second_source = second_selected[0]
            .source
            .source_dir
            .join("scripts/check.sh");
        std::fs::set_permissions(&second_source, std::fs::Permissions::from_mode(0o755)).unwrap();

        let first = publish_generation(first_fixture.request(), &first_selected).unwrap();
        let second = publish_generation(second_fixture.request(), &second_selected).unwrap();

        assert_ne!(first.digest, second.digest);
        assert_ne!(
            first.targets[0].content_sha256,
            second.targets[0].content_sha256
        );
    }

    #[cfg(unix)]
    #[test]
    fn changed_published_file_mode_prevents_generation_reuse() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = ProjectionFixture::new(Adapter::Cursor);
        fixture.write_bundled_skill("mode-check", "both", "BODY");
        let selected = fixture.resolve().unwrap();
        let first = publish_generation(fixture.request(), &selected).unwrap();
        let rendered = first.root.join(&first.targets[0].generation_rel);
        std::fs::set_permissions(&rendered, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = publish_generation(fixture.request(), &selected).unwrap_err();

        assert!(matches!(error, GenerationError::Hash(_)));
    }

    #[test]
    fn generation_publication_is_serialized_per_adapter() {
        use fs4::fs_std::FileExt as _;
        use std::fs::OpenOptions;
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Duration;

        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("serialized", "both", "BODY");
        let selected = fixture.resolve().unwrap();
        let generations = fixture
            .launch_root()
            .join(".spur/runtime/skill-projections/codex/generations");
        std::fs::create_dir_all(&generations).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(generations.join(".publish.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();

        std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            scope.spawn(move || {
                sender
                    .send(publish_generation(fixture.request(), &selected))
                    .unwrap();
            });

            assert!(matches!(
                receiver.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout)
            ));
            fs4::fs_std::FileExt::unlock(&lock).unwrap();
            assert!(receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .is_ok());
        });
    }

    #[test]
    fn directory_component_created_by_a_racing_publisher_is_reused() {
        let parent = tempfile::tempdir().unwrap();
        let component = parent.path().join("generations");

        ensure_directory_component_with(&component, |path| {
            std::fs::create_dir(path)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "injected concurrent creator",
            ))
        })
        .unwrap();

        assert!(component.is_dir());
    }
}
