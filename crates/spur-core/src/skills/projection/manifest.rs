use anyhow::{bail, Context as _};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Component, Path};

pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProjectionManifest {
    pub schema_version: u32,
    pub renderer_schema_version: u32,
    pub adapter: String,
    pub role: super::RuntimeRole,
    pub policy: super::SelectionPolicy,
    pub generation: String,
    pub targets: Vec<TargetRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectionMode {
    Symlink,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TargetRecord {
    pub skill_id: String,
    pub source_kind: super::resolver::ResolvedSourceKind,
    pub source_sha256: String,
    pub target_rel: String,
    pub generation_rel: String,
    pub mode: ProjectionMode,
    pub projected_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingTransaction {
    pub schema_version: u32,
    pub prior: Option<ProjectionManifest>,
    pub next: ProjectionManifest,
    pub operations: Vec<PendingOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RecordedTargetState {
    Absent,
    Symlink { destination: String },
    Copy { content_sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingOperation {
    pub target_rel: String,
    pub prior_state: RecordedTargetState,
    pub next: Option<TargetRecord>,
    pub backup_rel: Option<String>,
}

pub(crate) fn read_optional_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "projection state is not an ordinary file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", path.display()))
        .map(Some)
}

pub(crate) fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "projection state is not an ordinary file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary JSON beside {}", path.display()))?;
    let bytes = serde_json::to_vec_pretty(value).context("serialize projection JSON")?;
    temporary
        .write_all(&bytes)
        .with_context(|| format!("write temporary JSON for {}", path.display()))?;
    temporary
        .write_all(b"\n")
        .with_context(|| format!("finish temporary JSON for {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync temporary JSON for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist {}", path.display()))?;
    Ok(())
}

pub(crate) fn validate_manifest(
    manifest: &ProjectionManifest,
    adapter: &str,
) -> anyhow::Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        bail!(
            "unsupported manifest schema version {}",
            manifest.schema_version
        );
    }
    if manifest.renderer_schema_version != super::generation::RENDERER_SCHEMA_VERSION {
        bail!(
            "unsupported renderer schema version {}",
            manifest.renderer_schema_version
        );
    }
    if manifest.adapter != adapter {
        bail!(
            "manifest adapter `{}` does not match `{adapter}`",
            manifest.adapter
        );
    }
    validate_digest("generation", &manifest.generation)?;
    let mut targets = HashSet::new();
    for target in &manifest.targets {
        validate_relative_path("target", &target.target_rel)?;
        validate_relative_path("generation target", &target.generation_rel)?;
        validate_digest("source", &target.source_sha256)?;
        validate_digest("projected target", &target.projected_sha256)?;
        if !targets.insert(target.target_rel.as_str()) {
            bail!("duplicate manifest target `{}`", target.target_rel);
        }
    }
    Ok(())
}

pub(crate) fn validate_pending(pending: &PendingTransaction, adapter: &str) -> anyhow::Result<()> {
    if pending.schema_version != MANIFEST_SCHEMA_VERSION {
        bail!(
            "unsupported pending schema version {}",
            pending.schema_version
        );
    }
    if let Some(prior) = &pending.prior {
        validate_manifest(prior, adapter).context("validate pending prior manifest")?;
    }
    validate_manifest(&pending.next, adapter).context("validate pending next manifest")?;
    if pending.operations.is_empty() {
        bail!("pending transaction has no operations");
    }
    let prior_by_target = pending
        .prior
        .as_ref()
        .map(|prior| {
            prior
                .targets
                .iter()
                .map(|target| (target.target_rel.as_str(), target))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let next_by_target = pending
        .next
        .targets
        .iter()
        .map(|target| (target.target_rel.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    let mut targets = HashSet::new();
    let mut operations_by_target = BTreeMap::new();
    for operation in &pending.operations {
        validate_relative_path("pending target", &operation.target_rel)?;
        validate_pending_backup(operation)?;
        match &operation.next {
            Some(next) => {
                if next.target_rel != operation.target_rel {
                    bail!(
                        "pending operation target `{}` does not match next target `{}`",
                        operation.target_rel,
                        next.target_rel
                    );
                }
                if !pending.next.targets.iter().any(|target| target == next) {
                    bail!(
                        "pending operation for `{}` does not match its next manifest target",
                        operation.target_rel
                    );
                }
            }
            None if pending
                .next
                .targets
                .iter()
                .any(|target| target.target_rel == operation.target_rel) =>
            {
                bail!(
                    "pending removal for `{}` still appears in the next manifest",
                    operation.target_rel
                );
            }
            None => {}
        }
        if !targets.insert(operation.target_rel.as_str()) {
            bail!("duplicate pending target `{}`", operation.target_rel);
        }
        operations_by_target.insert(operation.target_rel.as_str(), operation);
        if let Some(prior) = prior_by_target.get(operation.target_rel.as_str()) {
            validate_pending_prior_state(operation, prior)?;
        }
    }

    for (target_rel, next) in &next_by_target {
        if prior_by_target.get(target_rel).copied() == Some(*next) {
            continue;
        }
        let operation = operations_by_target
            .get(target_rel)
            .with_context(|| format!("pending transaction omitted next target `{target_rel}`"))?;
        if operation.next.as_ref() != Some(*next) {
            bail!("pending operation for `{target_rel}` does not install the next target");
        }
    }
    for (target_rel, prior) in &prior_by_target {
        if next_by_target.get(target_rel).copied() == Some(*prior) {
            continue;
        }
        let operation = operations_by_target
            .get(target_rel)
            .with_context(|| format!("pending transaction omitted prior target `{target_rel}`"))?;
        if operation.next.as_ref() != next_by_target.get(target_rel).copied() {
            bail!("pending operation for `{target_rel}` does not describe the manifest delta");
        }
    }
    Ok(())
}

fn validate_pending_backup(operation: &PendingOperation) -> anyhow::Result<()> {
    match (&operation.prior_state, &operation.backup_rel) {
        (RecordedTargetState::Absent, None) => return Ok(()),
        (RecordedTargetState::Absent, Some(_)) => {
            bail!(
                "pending absent target `{}` unexpectedly has a backup",
                operation.target_rel
            );
        }
        (_, None) if operation.next.is_none() => return Ok(()),
        (_, None) => {
            bail!(
                "pending existing target `{}` is missing its backup",
                operation.target_rel
            );
        }
        (_, Some(backup)) => validate_relative_path("pending backup", backup)?,
    }

    let target = Path::new(&operation.target_rel);
    let backup = Path::new(
        operation
            .backup_rel
            .as_deref()
            .context("pending existing target has no backup")?,
    );
    if target.parent() != backup.parent() {
        bail!(
            "pending backup for `{}` is not a sibling",
            operation.target_rel
        );
    }
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .context("pending target has no UTF-8 file name")?;
    let backup_name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .context("pending backup has no UTF-8 file name")?;
    let prefix = format!(".{target_name}.spur-backup-");
    let suffix = backup_name.strip_prefix(&prefix).with_context(|| {
        format!(
            "pending backup for `{}` does not use the managed sibling name",
            operation.target_rel
        )
    })?;
    uuid::Uuid::parse_str(suffix).with_context(|| {
        format!(
            "pending backup for `{}` has an invalid transaction ID",
            operation.target_rel
        )
    })?;
    Ok(())
}

fn validate_pending_prior_state(
    operation: &PendingOperation,
    prior: &TargetRecord,
) -> anyhow::Result<()> {
    if operation.next.is_none() && operation.backup_rel.is_none() {
        return Ok(());
    }
    match (&operation.prior_state, prior.mode) {
        (RecordedTargetState::Absent, _)
        | (RecordedTargetState::Symlink { .. }, ProjectionMode::Symlink) => Ok(()),
        (RecordedTargetState::Copy { content_sha256 }, ProjectionMode::Copy)
            if content_sha256 == &prior.projected_sha256 =>
        {
            Ok(())
        }
        _ => bail!(
            "pending prior state for `{}` does not match its prior manifest target",
            operation.target_rel
        ),
    }
}

pub(crate) fn validate_relative_path(label: &str, relative: &str) -> anyhow::Result<()> {
    let path = Path::new(relative);
    if relative.is_empty() || path.as_os_str().is_empty() || path.is_absolute() {
        bail!("{label} path is not relative: `{relative}`");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("{label} path is not normalized: `{relative}`");
        }
    }
    Ok(())
}

fn validate_digest(label: &str, digest: &str) -> anyhow::Result<()> {
    if digest.len() != 64
        || !digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("{label} digest is not lowercase SHA-256: `{digest}`");
    }
    Ok(())
}
