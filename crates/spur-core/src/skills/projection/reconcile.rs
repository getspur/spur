use super::generation::{self, DesiredTarget, PublishedGeneration, TargetKind};
use super::manifest::{
    self, PendingOperation, PendingTransaction, ProjectionManifest, ProjectionMode,
    RecordedTargetState, TargetRecord, MANIFEST_SCHEMA_VERSION,
};
use super::resolver;
use super::{
    ProjectionError, ProjectionPhase, ProjectionRequest, ProjectionSkip, ProjectionSkipReason,
    ProjectionSummary, SelectedSource,
};
use anyhow::{bail, Context as _};
use fs4::fs_std::FileExt as _;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, DirEntry, OpenOptions};
use std::path::{Component, Path, PathBuf};

pub(super) async fn run(
    worktrees: &spur_worktree::manager::WorktreeManager,
    request: ProjectionRequest<'_>,
) -> Result<ProjectionSummary, ProjectionError> {
    reconcile_with_linker(worktrees, request, &NativeLinker).await
}

trait Linker: Sync {
    fn symlink(&self, source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()>;
}

struct NativeLinker;

impl Linker for NativeLinker {
    fn symlink(&self, source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()> {
        let source = if source.is_absolute() {
            relative_symlink_source(source, target).map_err(std::io::Error::other)?
        } else {
            source.to_path_buf()
        };
        native_symlink(&source, target, kind)
    }
}

#[cfg(unix)]
fn native_symlink(source: &Path, target: &Path, _kind: TargetKind) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn native_symlink(source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()> {
    match kind {
        TargetKind::Directory => std::os::windows::fs::symlink_dir(source, target),
        TargetKind::File => std::os::windows::fs::symlink_file(source, target),
    }
}

#[cfg(not(any(unix, windows)))]
fn native_symlink(_source: &Path, _target: &Path, _kind: TargetKind) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are unsupported on this platform",
    ))
}

#[derive(Debug)]
enum PlannedAction {
    Install {
        source: PathBuf,
        kind: TargetKind,
        migrated: bool,
    },
    Remove {
        migrated: bool,
    },
    Preserve,
}

#[derive(Debug)]
struct PlannedOperation {
    skill_id: String,
    journal: PendingOperation,
    action: PlannedAction,
}

#[derive(Debug)]
struct ReconciliationPlan {
    manifest: ProjectionManifest,
    operations: Vec<PlannedOperation>,
    preserved_generations: BTreeSet<String>,
    legacy_skill_ids: Vec<String>,
}

#[derive(Debug)]
struct ReconcileFailure {
    skill_id: Option<String>,
    source: anyhow::Error,
}

impl ReconcileFailure {
    fn for_skill(skill_id: impl Into<String>, source: anyhow::Error) -> Self {
        Self {
            skill_id: Some(skill_id.into()),
            source,
        }
    }
}

async fn reconcile_with_linker<L: Linker>(
    worktrees: &spur_worktree::manager::WorktreeManager,
    request: ProjectionRequest<'_>,
    linker: &L,
) -> Result<ProjectionSummary, ProjectionError> {
    let adapter = request.adapter.key().to_string();
    let projection_root = projection_root(request.launch_root, &adapter);
    prepare_projection_root(request.launch_root, &projection_root).map_err(|error| {
        projection_error(request.clone(), ProjectionPhase::Manifest, None, error)
    })?;
    let _lock = acquire_lock(projection_root.join("reconcile.lock"))
        .await
        .map_err(|error| {
            projection_error(request.clone(), ProjectionPhase::Recover, None, error)
        })?;

    let manifest_path = projection_root.join("manifest.json");
    let pending_path = projection_root.join("pending.json");
    let mut prior = load_manifest(&manifest_path, &adapter)
        .and_then(|manifest| {
            if let Some(manifest) = &manifest {
                validate_manifest_target_paths(request.launch_root, manifest)?;
                validate_manifest_generation(&projection_root, manifest)?;
            }
            Ok(manifest)
        })
        .map_err(|error| {
            projection_error(request.clone(), ProjectionPhase::Manifest, None, error)
        })?;
    prior = recover_pending(
        request.launch_root,
        &projection_root,
        &manifest_path,
        &pending_path,
        prior,
        &adapter,
    )
    .map_err(|error| projection_error(request.clone(), ProjectionPhase::Recover, None, error))?;

    let selected = resolver::resolve_effective_skills(
        request.source_repo_root,
        request.adapter,
        request.role,
        request.policy,
    )
    .map_err(|error| {
        projection_error(
            request.clone(),
            ProjectionPhase::Resolve,
            resolve_error_skill_id(&error),
            anyhow::Error::new(error),
        )
    })?;
    let generation =
        generation::publish_generation(request.clone(), &selected).map_err(|error| {
            projection_error(
                request.clone(),
                ProjectionPhase::Generate,
                generation_error_skill_id(&error),
                anyhow::Error::new(error),
            )
        })?;

    let mut summary = ProjectionSummary {
        adapter: adapter.clone(),
        generation: generation.digest.clone(),
        selected: selected
            .iter()
            .map(|skill| SelectedSource {
                skill_id: skill.payload.id.clone(),
                kind: skill.source.kind,
                content_sha256: skill.source.content_sha256.clone(),
            })
            .collect(),
        ..ProjectionSummary::default()
    };
    let ReconciliationPlan {
        manifest: mut next,
        mut operations,
        mut preserved_generations,
        legacy_skill_ids,
    } = plan_reconciliation(
        request.clone(),
        &projection_root,
        prior.as_ref(),
        &generation,
        &mut summary,
    )
    .map_err(|failure| {
        projection_error(
            request.clone(),
            ProjectionPhase::Reconcile,
            failure.skill_id,
            failure.source,
        )
    })?;

    operations.sort_by(|left, right| left.journal.target_rel.cmp(&right.journal.target_rel));
    let mut pending = PendingTransaction {
        schema_version: MANIFEST_SCHEMA_VERSION,
        prior: prior.clone(),
        next: next.clone(),
        operations: operations
            .iter()
            .map(|operation| operation.journal.clone())
            .collect(),
    };
    if !operations.is_empty() {
        manifest::validate_pending(&pending, &adapter).map_err(|error| {
            projection_error(request.clone(), ProjectionPhase::Reconcile, None, error)
        })?;
        manifest::write_atomic_json(&pending_path, &pending).map_err(|error| {
            projection_error(request.clone(), ProjectionPhase::Reconcile, None, error)
        })?;
        if let Err(failure) = apply_operations(
            request.launch_root,
            &pending_path,
            &mut pending,
            &mut operations,
            linker,
            &mut summary,
        ) {
            let rollback = rollback_transaction(request.launch_root, &projection_root, &pending);
            let journal_cleanup = cleanup_journal_after_rollback(&pending_path, &rollback);
            let error = combine_transaction_errors(failure.source, rollback, journal_cleanup);
            return Err(projection_error(
                request.clone(),
                ProjectionPhase::Reconcile,
                failure.skill_id,
                error,
            ));
        }
        next = pending.next.clone();
    }

    let excludes = exclusion_patterns(&next);
    if let Err(error) = worktrees
        .add_worktree_excludes(request.launch_root, &excludes)
        .await
    {
        let rollback = rollback_transaction(request.launch_root, &projection_root, &pending);
        let journal_cleanup = cleanup_journal_after_rollback(&pending_path, &rollback);
        let error = combine_transaction_errors(error, rollback, journal_cleanup);
        return Err(projection_error(
            request.clone(),
            ProjectionPhase::Excludes,
            None,
            error,
        ));
    }

    if prior.as_ref() != Some(&next) {
        if let Err(error) = manifest::write_atomic_json(&manifest_path, &next) {
            let rollback = rollback_transaction(request.launch_root, &projection_root, &pending);
            let journal_cleanup = cleanup_journal_after_rollback(&pending_path, &rollback);
            let error = combine_transaction_errors(error, rollback, journal_cleanup);
            return Err(projection_error(
                request.clone(),
                ProjectionPhase::Manifest,
                None,
                error,
            ));
        }
    }

    if !operations.is_empty() {
        cleanup_committed_backups(request.launch_root, &pending.operations)
            .and_then(|()| remove_file_if_exists(&pending_path))
            .map_err(|error| {
                projection_error(request.clone(), ProjectionPhase::Recover, None, error)
            })?;
    }

    crate::explore::materialize::retire_legacy_materializations(
        request.source_repo_root,
        request.launch_root,
        &legacy_skill_ids,
    )
    .map_err(|error| projection_error(request.clone(), ProjectionPhase::Reconcile, None, error))?;

    let excluded = worktrees.worktree_excluded_paths(request.launch_root).await;
    preserved_generations.extend(
        preserved_generation_references(request.launch_root, &projection_root, &excluded, &next)
            .map_err(|error| {
                projection_error(
                    request.clone(),
                    ProjectionPhase::GarbageCollect,
                    None,
                    error,
                )
            })?,
    );
    garbage_collect_generations(&projection_root, &next, &preserved_generations).map_err(
        |error| {
            projection_error(
                request.clone(),
                ProjectionPhase::GarbageCollect,
                None,
                error,
            )
        },
    )?;

    Ok(summary)
}

fn plan_reconciliation(
    request: ProjectionRequest<'_>,
    projection_root: &Path,
    prior: Option<&ProjectionManifest>,
    generation: &PublishedGeneration,
    summary: &mut ProjectionSummary,
) -> Result<ReconciliationPlan, ReconcileFailure> {
    let prior_by_target = prior
        .map(|manifest| {
            manifest
                .targets
                .iter()
                .map(|target| (target.target_rel.as_str(), target))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut next_targets = Vec::new();
    let mut operations = Vec::new();
    let mut processed = HashSet::new();
    let mut preserved_generations = BTreeSet::new();

    for desired in &generation.targets {
        processed.insert(desired.target_rel.clone());
        plan_desired_target(
            request.clone(),
            projection_root,
            prior,
            &prior_by_target,
            generation,
            desired,
            &mut next_targets,
            &mut operations,
            &mut preserved_generations,
            summary,
        )
        .map_err(|source| ReconcileFailure::for_skill(&desired.skill_id, source))?;
    }

    if let Some(prior) = prior {
        for stale in &prior.targets {
            if processed.contains(&stale.target_rel) {
                continue;
            }
            processed.insert(stale.target_rel.clone());
            plan_stale_target(
                request.launch_root,
                projection_root,
                prior,
                stale,
                &mut operations,
                &mut preserved_generations,
                summary,
            )
            .map_err(|source| ReconcileFailure::for_skill(&stale.skill_id, source))?;
        }
    }

    let legacy_skill_ids = crate::explore::materialize::legacy_materialization_skill_ids(
        request.source_repo_root,
        request.launch_root,
    );
    plan_legacy_pool_removals(
        request.clone(),
        &legacy_skill_ids,
        &mut processed,
        &mut operations,
        summary,
    )?;

    next_targets.sort_by(|left, right| left.target_rel.cmp(&right.target_rel));
    Ok(ReconciliationPlan {
        manifest: ProjectionManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            renderer_schema_version: generation::RENDERER_SCHEMA_VERSION,
            adapter: request.adapter.key().to_string(),
            role: request.role,
            policy: request.policy,
            generation: generation.digest.clone(),
            targets: next_targets,
        },
        operations,
        preserved_generations,
        legacy_skill_ids,
    })
}

#[allow(clippy::too_many_arguments)]
fn plan_desired_target(
    request: ProjectionRequest<'_>,
    projection_root: &Path,
    prior: Option<&ProjectionManifest>,
    prior_by_target: &BTreeMap<&str, &TargetRecord>,
    generation: &PublishedGeneration,
    desired: &DesiredTarget,
    next_targets: &mut Vec<TargetRecord>,
    operations: &mut Vec<PlannedOperation>,
    preserved_generations: &mut BTreeSet<String>,
    summary: &mut ProjectionSummary,
) -> anyhow::Result<()> {
    let mut next_record = target_record(desired, ProjectionMode::Symlink);
    let target = request.launch_root.join(&desired.target_rel);
    validate_target_parent(request.launch_root, &desired.target_rel)?;
    match prior_by_target.get(desired.target_rel.as_str()).copied() {
        Some(prior_record) if path_exists_no_follow(&target)? => {
            let prior_manifest = prior.context("target record has no prior manifest")?;
            if target_matches_record(projection_root, prior_manifest, prior_record)? {
                if prior_manifest.generation == generation.digest
                    && target_records_equivalent(prior_record, &next_record)
                {
                    next_record.mode = prior_record.mode;
                    next_targets.push(next_record);
                    summary.unchanged.push(target);
                } else {
                    operations.push(install_operation(
                        &desired.skill_id,
                        &desired.target_rel,
                        recorded_state(&target)?,
                        next_record.clone(),
                        generation.root.join(&desired.generation_rel),
                        desired.target_kind,
                        false,
                    ));
                    next_targets.push(next_record);
                }
            } else {
                summary.skipped.push(ProjectionSkip {
                    skill_id: desired.skill_id.clone(),
                    path: target.clone(),
                    reason: match prior_record.mode {
                        ProjectionMode::Copy => ProjectionSkipReason::UserEdited,
                        ProjectionMode::Symlink => ProjectionSkipReason::OwnershipLost,
                    },
                });
                retain_preserved_generation(
                    projection_root,
                    prior_manifest,
                    prior_record,
                    &target,
                    preserved_generations,
                )?;
                operations.push(relinquish_operation(
                    &desired.skill_id,
                    &desired.target_rel,
                    recorded_state(&target)?,
                ));
            }
        }
        Some(_) => {
            operations.push(install_operation(
                &desired.skill_id,
                &desired.target_rel,
                RecordedTargetState::Absent,
                next_record.clone(),
                generation.root.join(&desired.generation_rel),
                desired.target_kind,
                false,
            ));
            next_targets.push(next_record);
        }
        None if path_exists_no_follow(&target)? => {
            let desired_marker =
                expected_marker_id(&generation.root.join(&desired.generation_rel))?;
            match crate::skills::installer::legacy_marker_ownership(&target)? {
                crate::skills::installer::LegacyMarkerOwnership::Managed(marker)
                    if Some(marker.skill_id.as_str()) == desired_marker.as_deref() =>
                {
                    operations.push(install_operation(
                        &desired.skill_id,
                        &desired.target_rel,
                        recorded_state(&target)?,
                        next_record.clone(),
                        generation.root.join(&desired.generation_rel),
                        desired.target_kind,
                        true,
                    ));
                    next_targets.push(next_record);
                }
                crate::skills::installer::LegacyMarkerOwnership::UserEdited => {
                    summary.skipped.push(ProjectionSkip {
                        skill_id: desired.skill_id.clone(),
                        path: target,
                        reason: ProjectionSkipReason::UserEdited,
                    });
                }
                _ => {
                    summary.skipped.push(ProjectionSkip {
                        skill_id: desired.skill_id.clone(),
                        path: target,
                        reason: ProjectionSkipReason::UserOwned,
                    });
                }
            }
        }
        None => {
            operations.push(install_operation(
                &desired.skill_id,
                &desired.target_rel,
                RecordedTargetState::Absent,
                next_record.clone(),
                generation.root.join(&desired.generation_rel),
                desired.target_kind,
                false,
            ));
            next_targets.push(next_record);
        }
    }
    Ok(())
}

fn plan_stale_target(
    launch_root: &Path,
    projection_root: &Path,
    prior: &ProjectionManifest,
    stale: &TargetRecord,
    operations: &mut Vec<PlannedOperation>,
    preserved_generations: &mut BTreeSet<String>,
    summary: &mut ProjectionSummary,
) -> anyhow::Result<()> {
    let target = launch_root.join(&stale.target_rel);
    if !path_exists_no_follow(&target)? {
        operations.push(relinquish_operation(
            &stale.skill_id,
            &stale.target_rel,
            RecordedTargetState::Absent,
        ));
    } else if target_matches_record(projection_root, prior, stale)? {
        operations.push(remove_operation(
            &stale.skill_id,
            &stale.target_rel,
            recorded_state(&target)?,
            false,
        ));
    } else {
        summary.skipped.push(ProjectionSkip {
            skill_id: stale.skill_id.clone(),
            path: target.clone(),
            reason: ProjectionSkipReason::OwnershipLost,
        });
        retain_preserved_generation(
            projection_root,
            prior,
            stale,
            &target,
            preserved_generations,
        )?;
        operations.push(relinquish_operation(
            &stale.skill_id,
            &stale.target_rel,
            recorded_state(&target)?,
        ));
    }
    Ok(())
}

fn plan_legacy_pool_removals(
    request: ProjectionRequest<'_>,
    legacy_skill_ids: &[String],
    processed: &mut HashSet<String>,
    operations: &mut Vec<PlannedOperation>,
    summary: &mut ProjectionSummary,
) -> Result<(), ReconcileFailure> {
    for skill_id in legacy_skill_ids {
        let rendered = request.adapter.render_with_prefix(
            &crate::skills::SkillPayload {
                id: skill_id.clone(),
                description: String::new(),
                body: String::new(),
                source: crate::skills::SkillSource::Pool,
                role: crate::skills::SkillRole::Both,
            },
            request.launch_root,
            "",
        );
        let target = if request.adapter.target_is_directory() {
            rendered
                .path
                .parent()
                .context("legacy rendered target has no parent")
                .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?
                .to_path_buf()
        } else {
            rendered.path
        };
        let relative = normalized_relative(request.launch_root, &target)
            .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
        validate_target_parent(request.launch_root, &relative)
            .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
        if processed.contains(&relative)
            || !path_exists_no_follow(&target)
                .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?
        {
            continue;
        }
        processed.insert(relative.clone());
        let ownership = crate::skills::installer::legacy_marker_ownership(&target)
            .map_err(anyhow::Error::new)
            .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
        match ownership {
            crate::skills::installer::LegacyMarkerOwnership::Managed(marker)
                if marker.skill_id == *skill_id =>
            {
                let prior_state = recorded_state(&target)
                    .map_err(|source| ReconcileFailure::for_skill(skill_id, source))?;
                operations.push(remove_operation(skill_id, &relative, prior_state, true));
            }
            crate::skills::installer::LegacyMarkerOwnership::UserEdited => {
                summary.skipped.push(ProjectionSkip {
                    skill_id: skill_id.clone(),
                    path: target,
                    reason: ProjectionSkipReason::UserEdited,
                });
            }
            _ => summary.skipped.push(ProjectionSkip {
                skill_id: skill_id.clone(),
                path: target,
                reason: ProjectionSkipReason::UserOwned,
            }),
        }
    }
    Ok(())
}

fn target_record(target: &DesiredTarget, mode: ProjectionMode) -> TargetRecord {
    TargetRecord {
        skill_id: target.skill_id.clone(),
        source_kind: target.source.kind,
        source_sha256: target.source.content_sha256.clone(),
        target_rel: target.target_rel.clone(),
        generation_rel: target.generation_rel.clone(),
        mode,
        projected_sha256: target.content_sha256.clone(),
    }
}

fn target_records_equivalent(left: &TargetRecord, right: &TargetRecord) -> bool {
    left.skill_id == right.skill_id
        && left.source_kind == right.source_kind
        && left.source_sha256 == right.source_sha256
        && left.target_rel == right.target_rel
        && left.generation_rel == right.generation_rel
        && left.projected_sha256 == right.projected_sha256
}

fn install_operation(
    skill_id: &str,
    target_rel: &str,
    prior_state: RecordedTargetState,
    next: TargetRecord,
    source: PathBuf,
    kind: TargetKind,
    migrated: bool,
) -> PlannedOperation {
    PlannedOperation {
        skill_id: skill_id.to_string(),
        journal: PendingOperation {
            target_rel: target_rel.to_string(),
            backup_rel: backup_for_state(target_rel, &prior_state),
            prior_state,
            next: Some(next),
        },
        action: PlannedAction::Install {
            source,
            kind,
            migrated,
        },
    }
}

fn remove_operation(
    skill_id: &str,
    target_rel: &str,
    prior_state: RecordedTargetState,
    migrated: bool,
) -> PlannedOperation {
    PlannedOperation {
        skill_id: skill_id.to_string(),
        journal: PendingOperation {
            target_rel: target_rel.to_string(),
            backup_rel: backup_for_state(target_rel, &prior_state),
            prior_state,
            next: None,
        },
        action: PlannedAction::Remove { migrated },
    }
}

fn relinquish_operation(
    skill_id: &str,
    target_rel: &str,
    prior_state: RecordedTargetState,
) -> PlannedOperation {
    PlannedOperation {
        skill_id: skill_id.to_string(),
        journal: PendingOperation {
            target_rel: target_rel.to_string(),
            prior_state,
            next: None,
            backup_rel: None,
        },
        action: PlannedAction::Preserve,
    }
}

fn backup_for_state(target_rel: &str, state: &RecordedTargetState) -> Option<String> {
    if *state == RecordedTargetState::Absent {
        return None;
    }
    let target = Path::new(target_rel);
    let name = target.file_name()?.to_string_lossy();
    let backup = target
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!(".{name}.spur-backup-{}", uuid::Uuid::new_v4()));
    Some(path_to_slash(&backup))
}

fn apply_operations<L: Linker>(
    launch_root: &Path,
    pending_path: &Path,
    pending: &mut PendingTransaction,
    operations: &mut [PlannedOperation],
    linker: &L,
    summary: &mut ProjectionSummary,
) -> Result<(), ReconcileFailure> {
    for (index, operation) in operations.iter_mut().enumerate() {
        let result = (|| -> anyhow::Result<()> {
            let target = launch_root.join(&operation.journal.target_rel);
            validate_target_parent(launch_root, &operation.journal.target_rel)?;
            if !matches_recorded_state(&target, &operation.journal.prior_state)? {
                bail!(
                    "target changed after preflight: {}",
                    operation.journal.target_rel
                );
            }
            if matches!(&operation.action, PlannedAction::Preserve) {
                return Ok(());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create target parent {}", parent.display()))?;
            }
            if let Some(backup_rel) = &operation.journal.backup_rel {
                let backup = launch_root.join(backup_rel);
                if path_exists_no_follow(&backup)? {
                    bail!("transaction backup already exists: {}", backup.display());
                }
                fs::rename(&target, &backup).with_context(|| {
                    format!("move {} to backup {}", target.display(), backup.display())
                })?;
            }

            match &operation.action {
                PlannedAction::Install {
                    source,
                    kind,
                    migrated,
                } => {
                    match install_relative_symlink(linker, source, &target, *kind) {
                        Ok(()) => summary.linked.push(target.clone()),
                        Err(LinkInstallError::Create(_link_error)) => {
                            set_pending_mode(pending, index, ProjectionMode::Copy)?;
                            manifest::write_atomic_json(pending_path, pending)?;
                            let expected_sha256 = operation
                                .journal
                                .next
                                .as_ref()
                                .context("copy fallback operation has no next target")?
                                .projected_sha256
                                .clone();
                            install_copy(source, &target, *kind, &expected_sha256)?;
                            summary.copied.push(target.clone());
                        }
                        Err(LinkInstallError::Other(error)) => return Err(error),
                    }
                    if *migrated {
                        summary.migrated.push(target);
                    }
                }
                PlannedAction::Remove { migrated } => {
                    if *migrated {
                        summary.migrated.push(target);
                    } else {
                        summary.removed.push(target);
                    }
                }
                PlannedAction::Preserve => {}
            }
            Ok(())
        })();
        result.map_err(|source| ReconcileFailure::for_skill(&operation.skill_id, source))?;
    }
    Ok(())
}

fn set_pending_mode(
    pending: &mut PendingTransaction,
    operation_index: usize,
    mode: ProjectionMode,
) -> anyhow::Result<()> {
    let operation = pending
        .operations
        .get_mut(operation_index)
        .context("pending operation index escaped journal")?;
    let target_rel = operation.target_rel.clone();
    operation
        .next
        .as_mut()
        .context("copy fallback operation has no next target")?
        .mode = mode;
    pending
        .next
        .targets
        .iter_mut()
        .find(|target| target.target_rel == target_rel)
        .context("copy fallback target missing from next manifest")?
        .mode = mode;
    Ok(())
}

enum LinkInstallError {
    Create(std::io::Error),
    Other(anyhow::Error),
}

fn install_relative_symlink<L: Linker>(
    linker: &L,
    source: &Path,
    target: &Path,
    kind: TargetKind,
) -> Result<(), LinkInstallError> {
    let temporary = temporary_sibling(target, "link").map_err(LinkInstallError::Other)?;
    match linker.symlink(source, &temporary, kind) {
        Ok(()) => {}
        Err(error) => {
            let _ = remove_path_if_exists(&temporary);
            return Err(LinkInstallError::Create(error));
        }
    }
    if let Err(error) = fs::rename(&temporary, target) {
        let _ = remove_path_if_exists(&temporary);
        return Err(LinkInstallError::Other(anyhow::Error::new(error).context(
            format!(
                "rename temporary link {} to {}",
                temporary.display(),
                target.display()
            ),
        )));
    }
    Ok(())
}

fn install_copy(
    source: &Path,
    target: &Path,
    kind: TargetKind,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let temporary = temporary_sibling(target, "copy")?;
    let result = match kind {
        TargetKind::Directory => copy_directory(source, &temporary),
        TargetKind::File => copy_file(source, &temporary),
    };
    if let Err(error) = result {
        let _ = remove_path_if_exists(&temporary);
        return Err(error);
    }
    let copied_sha256 = hash_projected_target(&temporary)?;
    if copied_sha256 != expected_sha256 {
        remove_path_if_exists(&temporary)?;
        bail!(
            "copied projection digest mismatch for {}: expected {}, actual {}",
            target.display(),
            expected_sha256,
            copied_sha256
        );
    }
    fs::rename(&temporary, target).with_context(|| {
        format!(
            "rename temporary copy {} to {}",
            temporary.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn copy_directory(source: &Path, target: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("inspect copy source {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "copy source is not an ordinary directory: {}",
            source.display()
        );
    }
    fs::create_dir(target).with_context(|| format!("create copy target {}", target.display()))?;
    for entry in sorted_entries(source)? {
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        if file_type.is_symlink() {
            bail!("copy source contains symlink: {}", entry.path().display());
        }
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            copy_file(&entry.path(), &destination)?;
        } else {
            bail!(
                "copy source contains unsupported entry: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    fs::copy(source, target)
        .with_context(|| format!("copy {} to {}", source.display(), target.display()))?;
    let permissions = fs::metadata(source)
        .with_context(|| format!("read permissions for {}", source.display()))?
        .permissions();
    fs::set_permissions(target, permissions)
        .with_context(|| format!("set permissions on {}", target.display()))?;
    Ok(())
}

fn recover_pending(
    launch_root: &Path,
    projection_root: &Path,
    manifest_path: &Path,
    pending_path: &Path,
    current: Option<ProjectionManifest>,
    adapter: &str,
) -> anyhow::Result<Option<ProjectionManifest>> {
    let Some(pending) = manifest::read_optional_json::<PendingTransaction>(pending_path)? else {
        return Ok(current);
    };
    manifest::validate_pending(&pending, adapter)?;
    validate_manifest_target_paths(launch_root, &pending.next)?;
    validate_manifest_generation(projection_root, &pending.next)?;
    if let Some(prior) = &pending.prior {
        validate_manifest_target_paths(launch_root, prior)?;
        validate_manifest_generation(projection_root, prior)?;
    }
    validate_pending_prior_records(launch_root, projection_root, &pending)?;
    for operation in &pending.operations {
        validate_target_parent(launch_root, &operation.target_rel)?;
        if let Some(backup) = &operation.backup_rel {
            validate_target_parent(launch_root, backup)?;
        }
    }

    if current.as_ref() == Some(&pending.next) {
        validate_committed_backups(launch_root, &pending.operations, false)?;
        cleanup_committed_backups(launch_root, &pending.operations)?;
        remove_file_if_exists(pending_path)?;
        return Ok(Some(pending.next));
    }
    if current != pending.prior {
        bail!("pending prior manifest does not match current manifest");
    }

    let mut all_applied = true;
    for operation in &pending.operations {
        all_applied &=
            operation_next_matches(launch_root, projection_root, &pending.next, operation)?;
    }
    if all_applied {
        validate_committed_backups(launch_root, &pending.operations, true)?;
        manifest::write_atomic_json(manifest_path, &pending.next)?;
        cleanup_committed_backups(launch_root, &pending.operations)?;
        remove_file_if_exists(pending_path)?;
        return Ok(Some(pending.next));
    }

    validate_rollback(launch_root, projection_root, &pending)?;
    rollback_operations(launch_root, &pending.operations)?;
    remove_file_if_exists(pending_path)?;
    Ok(pending.prior)
}

fn operation_next_matches(
    launch_root: &Path,
    projection_root: &Path,
    next_manifest: &ProjectionManifest,
    operation: &PendingOperation,
) -> anyhow::Result<bool> {
    let target = launch_root.join(&operation.target_rel);
    match &operation.next {
        Some(next) => target_matches_record(projection_root, next_manifest, next),
        None if operation.backup_rel.is_none() => {
            matches_recorded_state(&target, &operation.prior_state)
        }
        None => Ok(!path_exists_no_follow(&target)?),
    }
}

fn validate_pending_prior_records(
    launch_root: &Path,
    projection_root: &Path,
    pending: &PendingTransaction,
) -> anyhow::Result<()> {
    let Some(prior) = &pending.prior else {
        return Ok(());
    };
    let prior_by_target = prior
        .targets
        .iter()
        .map(|target| (target.target_rel.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    for operation in &pending.operations {
        if operation.backup_rel.is_none() || operation.prior_state == RecordedTargetState::Absent {
            continue;
        }
        let Some(prior_target) = prior_by_target.get(operation.target_rel.as_str()) else {
            continue;
        };
        match (&operation.prior_state, prior_target.mode) {
            (RecordedTargetState::Copy { content_sha256 }, ProjectionMode::Copy)
                if content_sha256 == &prior_target.projected_sha256 => {}
            (RecordedTargetState::Symlink { destination }, ProjectionMode::Symlink) => {
                let target = launch_root.join(&operation.target_rel);
                let source = projection_root
                    .join("generations")
                    .join(&prior.generation)
                    .join(&prior_target.generation_rel);
                let expected = path_to_slash(&relative_symlink_source(&source, &target)?);
                if *destination != expected {
                    bail!(
                        "pending prior symlink for `{}` does not match the prior manifest",
                        operation.target_rel
                    );
                }
            }
            _ => bail!(
                "pending prior state for `{}` does not match the prior manifest",
                operation.target_rel
            ),
        }
    }
    Ok(())
}

fn validate_rollback(
    launch_root: &Path,
    projection_root: &Path,
    pending: &PendingTransaction,
) -> anyhow::Result<()> {
    for operation in &pending.operations {
        let target = launch_root.join(&operation.target_rel);
        let current_is_prior = matches_recorded_state(&target, &operation.prior_state)?;
        let current_is_next =
            operation_next_matches(launch_root, projection_root, &pending.next, operation)?;
        let current_absent = !path_exists_no_follow(&target)?;
        if let Some(backup_rel) = &operation.backup_rel {
            let backup = launch_root.join(backup_rel);
            if path_exists_no_follow(&backup)? {
                if !matches_recorded_state(&backup, &operation.prior_state)? {
                    bail!("pending backup changed: {}", backup.display());
                }
                if !(current_absent || current_is_next) {
                    bail!("pending target has unexpected state: {}", target.display());
                }
            } else if !current_is_prior {
                bail!(
                    "pending target and backup cannot be rolled back: {}",
                    target.display()
                );
            }
        } else if !(current_is_prior || current_is_next) {
            bail!("pending target has unexpected state: {}", target.display());
        }
    }
    Ok(())
}

fn rollback_operations(launch_root: &Path, operations: &[PendingOperation]) -> anyhow::Result<()> {
    for operation in operations.iter().rev() {
        let target = launch_root.join(&operation.target_rel);
        if let Some(backup_rel) = &operation.backup_rel {
            let backup = launch_root.join(backup_rel);
            if path_exists_no_follow(&backup)? {
                remove_path_if_exists(&target)?;
                fs::rename(&backup, &target).with_context(|| {
                    format!(
                        "restore backup {} to {}",
                        backup.display(),
                        target.display()
                    )
                })?;
            }
        } else if operation.prior_state == RecordedTargetState::Absent {
            remove_path_if_exists(&target)?;
        }
    }
    Ok(())
}

fn rollback_transaction(
    launch_root: &Path,
    projection_root: &Path,
    pending: &PendingTransaction,
) -> anyhow::Result<()> {
    validate_rollback(launch_root, projection_root, pending)?;
    rollback_operations(launch_root, &pending.operations)
}

fn cleanup_journal_after_rollback(
    pending_path: &Path,
    rollback: &anyhow::Result<()>,
) -> anyhow::Result<()> {
    if rollback.is_ok() {
        remove_file_if_exists(pending_path)
    } else {
        Ok(())
    }
}

fn validate_committed_backups(
    launch_root: &Path,
    operations: &[PendingOperation],
    require_present: bool,
) -> anyhow::Result<()> {
    for operation in operations {
        if let Some(backup_rel) = &operation.backup_rel {
            let backup = launch_root.join(backup_rel);
            let exists = path_exists_no_follow(&backup)?;
            if require_present && !exists {
                bail!("committed backup is missing: {}", backup.display());
            }
            if exists && !matches_recorded_state(&backup, &operation.prior_state)? {
                bail!("committed backup changed: {}", backup.display());
            }
        }
    }
    Ok(())
}

fn cleanup_committed_backups(
    launch_root: &Path,
    operations: &[PendingOperation],
) -> anyhow::Result<()> {
    for operation in operations {
        if let Some(backup_rel) = &operation.backup_rel {
            remove_path_if_exists(&launch_root.join(backup_rel))?;
        }
    }
    Ok(())
}

fn load_manifest(path: &Path, adapter: &str) -> anyhow::Result<Option<ProjectionManifest>> {
    let manifest = manifest::read_optional_json::<ProjectionManifest>(path)?;
    if let Some(manifest) = &manifest {
        manifest::validate_manifest(manifest, adapter)
            .with_context(|| format!("validate {}", path.display()))?;
    }
    Ok(manifest)
}

fn validate_manifest_generation(
    projection_root: &Path,
    manifest: &ProjectionManifest,
) -> anyhow::Result<()> {
    let generation = projection_root
        .join("generations")
        .join(&manifest.generation);
    let metadata = fs::symlink_metadata(&generation)
        .with_context(|| format!("inspect manifest generation {}", generation.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "manifest generation is not an ordinary directory: {}",
            generation.display()
        );
    }
    for target in &manifest.targets {
        let source = generation.join(&target.generation_rel);
        validate_target_parent(&generation, &target.generation_rel)?;
        if !path_exists_no_follow(&source)? {
            bail!(
                "manifest generation target is missing: {}",
                source.display()
            );
        }
        if hash_projected_target(&source)? != target.projected_sha256 {
            bail!(
                "manifest generation target digest changed: {}",
                source.display()
            );
        }
    }
    Ok(())
}

fn validate_manifest_target_paths(
    launch_root: &Path,
    manifest: &ProjectionManifest,
) -> anyhow::Result<()> {
    for target in &manifest.targets {
        validate_target_parent(launch_root, &target.target_rel)?;
    }
    Ok(())
}

fn validate_target_parent(root: &Path, relative: &str) -> anyhow::Result<()> {
    manifest::validate_relative_path("projection target", relative)?;
    let relative = Path::new(relative);
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(component) = component else {
            bail!(
                "projection target is not normalized: `{}`",
                relative.display()
            );
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!(
                "projection target parent is not an ordinary directory: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn target_matches_record(
    projection_root: &Path,
    manifest: &ProjectionManifest,
    record: &TargetRecord,
) -> anyhow::Result<bool> {
    let target = projection_root
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("projection root cannot be relativized to launch root")?
        .join(&record.target_rel);
    if !path_exists_no_follow(&target)? {
        return Ok(false);
    }
    match record.mode {
        ProjectionMode::Symlink => {
            let metadata = fs::symlink_metadata(&target)?;
            if !metadata.file_type().is_symlink() {
                return Ok(false);
            }
            let source = projection_root
                .join("generations")
                .join(&manifest.generation)
                .join(&record.generation_rel);
            Ok(fs::read_link(&target)? == relative_symlink_source(&source, &target)?)
        }
        ProjectionMode::Copy => {
            let metadata = fs::symlink_metadata(&target)?;
            Ok(!metadata.file_type().is_symlink()
                && hash_projected_target(&target)? == record.projected_sha256)
        }
    }
}

fn recorded_state(path: &Path) -> anyhow::Result<RecordedTargetState> {
    if !path_exists_no_follow(path)? {
        return Ok(RecordedTargetState::Absent);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(RecordedTargetState::Symlink {
            destination: path_to_slash(&fs::read_link(path)?),
        });
    }
    Ok(RecordedTargetState::Copy {
        content_sha256: hash_projected_target(path)?,
    })
}

fn matches_recorded_state(path: &Path, expected: &RecordedTargetState) -> anyhow::Result<bool> {
    match expected {
        RecordedTargetState::Absent => Ok(!path_exists_no_follow(path)?),
        RecordedTargetState::Symlink { destination } => {
            if !path_exists_no_follow(path)? {
                return Ok(false);
            }
            let metadata = fs::symlink_metadata(path)?;
            Ok(metadata.file_type().is_symlink()
                && path_to_slash(&fs::read_link(path)?) == *destination)
        }
        RecordedTargetState::Copy { content_sha256 } => {
            if !path_exists_no_follow(path)? {
                return Ok(false);
            }
            let metadata = fs::symlink_metadata(path)?;
            Ok(!metadata.file_type().is_symlink()
                && hash_projected_target(path)? == *content_sha256)
        }
    }
}

fn expected_marker_id(source: &Path) -> anyhow::Result<Option<String>> {
    match crate::skills::installer::legacy_marker_ownership(source)? {
        crate::skills::installer::LegacyMarkerOwnership::Managed(marker) => {
            Ok(Some(marker.skill_id))
        }
        _ => Ok(None),
    }
}

fn exclusion_patterns(manifest: &ProjectionManifest) -> Vec<String> {
    let mut patterns = manifest
        .targets
        .iter()
        .map(|target| target.target_rel.clone())
        .collect::<Vec<_>>();
    patterns.push(".spur/runtime/skill-projections/".to_string());
    patterns.sort();
    patterns.dedup();
    patterns
}

fn retain_preserved_generation(
    projection_root: &Path,
    prior: &ProjectionManifest,
    prior_record: &TargetRecord,
    target: &Path,
    preserved: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    match prior_record.mode {
        ProjectionMode::Copy => {
            preserved.insert(prior.generation.clone());
        }
        ProjectionMode::Symlink => {
            if let Some(generation) = generation_reference_for_target(projection_root, target)? {
                preserved.insert(generation);
            }
        }
    }
    Ok(())
}

fn preserved_generation_references(
    launch_root: &Path,
    projection_root: &Path,
    excluded: &[String],
    manifest: &ProjectionManifest,
) -> anyhow::Result<BTreeSet<String>> {
    let managed = manifest
        .targets
        .iter()
        .map(|target| target.target_rel.as_str())
        .collect::<HashSet<_>>();
    let mut retained = BTreeSet::new();
    for relative in excluded {
        if managed.contains(relative.as_str()) {
            continue;
        }
        if let Some(generation) =
            generation_reference_for_target(projection_root, &launch_root.join(relative))?
        {
            retained.insert(generation);
        }
    }
    Ok(retained)
}

fn generation_reference_for_target(
    projection_root: &Path,
    target: &Path,
) -> anyhow::Result<Option<String>> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect preserved target {}", target.display()));
        }
    };
    if !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let resolved = match fs::canonicalize(target) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("resolve preserved target {}", target.display()));
        }
    };
    let generations = projection_root.join("generations");
    let Ok(relative) = resolved.strip_prefix(&generations) else {
        return Ok(None);
    };
    let Some(Component::Normal(generation)) = relative.components().next() else {
        return Ok(None);
    };
    let generation = generation.to_string_lossy();
    if generation.len() != 64
        || !generation
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Ok(None);
    }
    Ok(Some(generation.into_owned()))
}

fn garbage_collect_generations(
    projection_root: &Path,
    manifest: &ProjectionManifest,
    preserved: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let generations = projection_root.join("generations");
    if !generations.is_dir() {
        return Ok(());
    }
    let mut retained = preserved.clone();
    retained.insert(manifest.generation.clone());
    for entry in sorted_entries(&generations)? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || retained.contains(&name) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())
                .with_context(|| format!("remove generation {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn hash_projected_target(path: &Path) -> anyhow::Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect projected target {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("projected target is a symlink: {}", path.display());
    }
    if metadata.is_file() {
        return hash_file(path);
    }
    if !metadata.is_dir() {
        bail!("projected target has unsupported type: {}", path.display());
    }
    let mut entries = Vec::new();
    collect_hash_entries(path, path, &mut entries)?;
    entries.sort_by(|left, right| left.1.cmp(&right.1));
    let mut hasher = Sha256::new();
    for (kind, relative, content) in entries {
        hasher.update([kind]);
        hasher.update(b"\0");
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        if let Some(content) = content {
            hasher.update(content.as_bytes());
        }
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_hash_entries(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(u8, String, Option<String>)>,
) -> anyhow::Result<()> {
    for entry in sorted_entries(directory)? {
        let path = entry.path();
        let relative = normalized_relative(root, &path)?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            bail!("projected copy contains symlink: {}", path.display());
        }
        if file_type.is_dir() {
            output.push((b'd', relative, None));
            collect_hash_entries(root, &path, output)?;
        } else if file_type.is_file() {
            output.push((b'f', relative, Some(hash_file(&path)?)));
        } else {
            bail!(
                "projected copy contains unsupported entry: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"spur-skill-projection-file\0");
    hasher.update(file_mode(path)?.to_be_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn file_mode(path: &Path) -> anyhow::Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(path: &Path) -> anyhow::Result<u32> {
    Ok(u32::from(fs::metadata(path)?.permissions().readonly()))
}

fn relative_symlink_source(source: &Path, target: &Path) -> anyhow::Result<PathBuf> {
    let source = std::path::absolute(source)
        .with_context(|| format!("absolutize symlink source {}", source.display()))?;
    let target_parent = target.parent().context("symlink target has no parent")?;
    let target_parent = std::path::absolute(target_parent).with_context(|| {
        format!(
            "absolutize symlink target parent {}",
            target_parent.display()
        )
    })?;
    let source_components = source.components().collect::<Vec<_>>();
    let parent_components = target_parent.components().collect::<Vec<_>>();
    let shared = source_components
        .iter()
        .zip(&parent_components)
        .take_while(|(left, right)| left == right)
        .count();
    if shared == 0 {
        bail!(
            "symlink source {} and target {} have no common root",
            source.display(),
            target.display()
        );
    }
    let mut relative = PathBuf::new();
    for component in &parent_components[shared..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &source_components[shared..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

fn prepare_projection_root(launch_root: &Path, projection_root: &Path) -> anyhow::Result<()> {
    let absolute = std::path::absolute(launch_root)
        .with_context(|| format!("absolutize launch root {}", launch_root.display()))?;
    for ancestor in absolute
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(ancestor)
            .with_context(|| format!("inspect launch ancestor {}", ancestor.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "launch root ancestor is not an ordinary directory: {}",
                ancestor.display()
            );
        }
    }
    let relative = projection_root
        .strip_prefix(launch_root)
        .context("projection root escaped launch root")?;
    let mut current = launch_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!(
                "projection root is not normalized: {}",
                projection_root.display()
            );
        };
        current.push(component);
        ensure_projection_directory_component_with(&current, |path| fs::create_dir(path))?;
    }
    Ok(())
}

fn ensure_projection_directory_component_with(
    path: &Path,
    create: impl FnOnce(&Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_projection_directory_component(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match create(path) {
            Ok(()) => {
                let metadata = fs::symlink_metadata(path).with_context(|| {
                    format!("inspect created projection directory {}", path.display())
                })?;
                validate_projection_directory_component(path, &metadata)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(path).with_context(|| {
                    format!(
                        "inspect concurrently created projection directory {}",
                        path.display()
                    )
                })?;
                validate_projection_directory_component(path, &metadata)
            }
            Err(error) => Err(error)
                .with_context(|| format!("create projection directory {}", path.display())),
        },
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn validate_projection_directory_component(
    path: &Path,
    metadata: &fs::Metadata,
) -> anyhow::Result<()> {
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        bail!(
            "projection component is not an ordinary directory: {}",
            path.display()
        )
    }
}

async fn acquire_lock(path: PathBuf) -> anyhow::Result<std::fs::File> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "projection lock is not an ordinary file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect projection lock {}", path.display()))
        }
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("open projection lock {}", path.display()))?;
    tokio::task::spawn_blocking(move || {
        file.lock_exclusive()
            .with_context(|| format!("lock projection {}", path.display()))?;
        Ok(file)
    })
    .await
    .context("join projection lock task")?
}

fn projection_root(launch_root: &Path, adapter: &str) -> PathBuf {
    launch_root
        .join(".spur/runtime/skill-projections")
        .join(adapter)
}

fn temporary_sibling(target: &Path, purpose: &str) -> anyhow::Result<PathBuf> {
    let parent = target.parent().context("projection target has no parent")?;
    let name = target
        .file_name()
        .context("projection target has no file name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.spur-{purpose}-{}", uuid::Uuid::new_v4())))
}

fn normalized_relative(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} escaped {}", path.display(), root.display()))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "path is not a normalized relative target: {}",
            path.display()
        );
    }
    let value = relative
        .to_str()
        .with_context(|| format!("path is not UTF-8: {}", relative.display()))?
        .replace('\\', "/");
    Ok(value)
}

fn sorted_entries(directory: &Path) -> anyhow::Result<Vec<DirEntry>> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read directory {}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

fn path_exists_no_follow(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn remove_path_if_exists(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("remove directory {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("remove target {}", path.display()))
    }
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn path_to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn combine_transaction_errors(
    primary: impl Into<anyhow::Error>,
    rollback: anyhow::Result<()>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Error {
    let primary = primary.into();
    match (rollback, cleanup) {
        (Ok(()), Ok(())) => primary,
        (rollback, cleanup) => anyhow::anyhow!(
            "{primary}; rollback: {}; journal cleanup: {}",
            rollback
                .err()
                .map_or_else(|| "ok".to_string(), |error| error.to_string()),
            cleanup
                .err()
                .map_or_else(|| "ok".to_string(), |error| error.to_string())
        ),
    }
}

fn resolve_error_skill_id(error: &resolver::ResolveError) -> Option<String> {
    match error {
        resolver::ResolveError::PoolDigestMismatch { id, .. }
        | resolver::ResolveError::PoolReplacementNotAuthorized { id } => Some(id.clone()),
        _ => None,
    }
}

fn generation_error_skill_id(error: &generation::GenerationError) -> Option<String> {
    match error {
        generation::GenerationError::UnsafeSourcePath { skill_id, .. } => Some(skill_id.clone()),
        _ => None,
    }
}

fn projection_error(
    request: ProjectionRequest<'_>,
    phase: ProjectionPhase,
    skill_id: Option<String>,
    source: anyhow::Error,
) -> ProjectionError {
    let source = match &skill_id {
        Some(skill_id) => source.context(format!("skill {skill_id}")),
        None => source,
    };
    ProjectionError {
        phase,
        launch_root: request.launch_root.to_path_buf(),
        adapter: request.adapter.key().to_string(),
        skill_id,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::materialize::{
        append_materialization_record, read_recent_materializations, MaterializationRecord,
    };
    use crate::explore::pool::Manifest;
    use crate::skills::adapters::Adapter;
    use crate::skills::projection::generation::{
        publish_generation, PublishedGeneration, TargetKind,
    };
    use crate::skills::projection::manifest::{
        write_atomic_json, PendingOperation, PendingTransaction, ProjectionManifest,
        ProjectionMode, RecordedTargetState, TargetRecord, MANIFEST_SCHEMA_VERSION,
    };
    use crate::skills::projection::test_support::ProjectionFixture;
    use crate::skills::projection::{
        reconcile, reconcile_many, ProjectionPhase, ProjectionSkipReason, RuntimeRole,
        SelectionPolicy,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AlwaysFailSymlink;

    impl Linker for AlwaysFailSymlink {
        fn symlink(
            &self,
            _source: &Path,
            _target: &Path,
            _kind: TargetKind,
        ) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected symlink denial",
            ))
        }
    }

    struct PendingObservedLinker {
        pending: PathBuf,
    }

    impl Linker for PendingObservedLinker {
        fn symlink(&self, source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()> {
            if !self.pending.is_file() {
                return Err(std::io::Error::other(
                    "pending journal was not written before mutation",
                ));
            }
            NativeLinker.symlink(source, target, kind)
        }
    }

    struct RemoveSecondSourceThenFail {
        calls: AtomicUsize,
    }

    impl RemoveSecondSourceThenFail {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Linker for RemoveSecondSourceThenFail {
        fn symlink(&self, source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                if source.is_dir() {
                    std::fs::remove_dir_all(source)?;
                } else {
                    std::fs::remove_file(source)?;
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected switch failure",
                ));
            }
            NativeLinker.symlink(source, target, kind)
        }
    }

    struct ConflictThenFail {
        calls: AtomicUsize,
        conflict: PathBuf,
    }

    impl Linker for ConflictThenFail {
        fn symlink(&self, source: &Path, target: &Path, kind: TargetKind) -> std::io::Result<()> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                std::fs::create_dir(&self.conflict)?;
                std::fs::write(self.conflict.join("USER.txt"), b"preserve me\n")?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected conflict after preflight",
                ));
            }
            NativeLinker.symlink(source, target, kind)
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn creates_relative_directory_symlink_and_excludes_projection_paths() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("linked", "brain", "BODY");

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        let target = codex_target(&fixture, "linked");
        let destination = std::fs::read_link(&target).unwrap();
        assert!(!destination.is_absolute());
        assert_eq!(summary.linked, vec![target.clone()]);
        assert_eq!(
            std::fs::canonicalize(&target).unwrap(),
            generation_target(&fixture, &summary.generation, "linked")
        );
        assert!(git_status(fixture.launch_root()).is_empty());
    }

    #[tokio::test]
    async fn symlink_failure_falls_back_to_tracked_copy() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("copy-me", "brain", "BODY");

        let summary =
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
                .await
                .unwrap();

        let target = codex_target(&fixture, "copy-me");
        assert!(target.join("SKILL.md").is_file());
        assert_eq!(summary.copied, vec![target]);
        assert!(git_status(fixture.launch_root()).is_empty());
    }

    #[tokio::test]
    async fn user_owned_collision_is_preserved_and_typed() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("collision", "both", "GENERATED");
        let target = codex_target(&fixture, "collision");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), b"USER OWNED\n").unwrap();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(target.join("SKILL.md")).unwrap(),
            b"USER OWNED\n"
        );
        assert_eq!(summary.skipped.len(), 1);
        assert_eq!(summary.skipped[0].reason, ProjectionSkipReason::UserOwned);
        assert_eq!(summary.skipped[0].skill_id, "collision");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_target_parent_is_rejected_before_external_mutation() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("escape", "both", "BODY");
        let external = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(external.path(), fixture.launch_root().join(".codex")).unwrap();

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Reconcile);
        assert_eq!(error.skill_id.as_deref(), Some("escape"));
        assert!(!external.path().join("skills/spurpower-escape").exists());
    }

    #[tokio::test]
    async fn unchanged_projection_is_idempotent() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("steady", "both", "BODY");

        let first = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();
        let target = codex_target(&fixture, "steady");
        let before = target_state_bytes(&target);
        let second = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(first.generation, second.generation);
        assert!(second.linked.is_empty());
        assert!(second.copied.is_empty());
        assert_eq!(second.unchanged, vec![target.clone()]);
        assert_eq!(target_state_bytes(&target), before);
    }

    #[tokio::test]
    async fn stale_unchanged_owned_target_is_removed() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("keep", "both", "KEEP");
        fixture.write_bundled_skill("remove", "both", "REMOVE");
        reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();
        remove_source(&fixture, "remove");

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        let stale = codex_target(&fixture, "remove");
        assert_eq!(summary.removed, vec![stale.clone()]);
        assert!(!stale.exists());
        assert!(codex_target(&fixture, "keep").exists());
    }

    #[tokio::test]
    async fn edited_copy_is_preserved_and_ownership_is_relinquished() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("edited", "both", "ORIGINAL");
        let first =
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
                .await
                .unwrap();
        let old_generation = generation_root(&fixture, &first.generation);
        let target = codex_target(&fixture, "edited");
        std::fs::write(target.join("SKILL.md"), b"USER EDIT\n").unwrap();

        let summary =
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
                .await
                .unwrap();

        assert_eq!(
            std::fs::read(target.join("SKILL.md")).unwrap(),
            b"USER EDIT\n"
        );
        assert_eq!(summary.skipped[0].reason, ProjectionSkipReason::UserEdited);
        assert!(read_manifest(&fixture).targets.is_empty());
        assert!(old_generation.exists());
    }

    #[tokio::test]
    async fn valid_legacy_marker_is_adopted_without_duplicating_marker_rules() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("legacy", "both", "LEGACY BODY");
        let selected = fixture.resolve().unwrap();
        let rendered = Adapter::Codex.render(&selected[0].payload, fixture.launch_root());
        std::fs::create_dir_all(rendered.path.parent().unwrap()).unwrap();
        std::fs::write(&rendered.path, rendered.bytes).unwrap();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        let target = codex_target(&fixture, "legacy");
        assert_eq!(summary.migrated, vec![target.clone()]);
        assert!(target.exists());
        assert_ne!(
            std::fs::read(target.join("SKILL.md")).unwrap(),
            b"LEGACY BODY"
        );
    }

    #[tokio::test]
    async fn valid_legacy_marker_with_an_extra_sibling_is_preserved() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("legacy-extra", "both", "LEGACY BODY");
        let selected = fixture.resolve().unwrap();
        let rendered = Adapter::Codex.render(&selected[0].payload, fixture.launch_root());
        let target = rendered.path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(&rendered.path, rendered.bytes).unwrap();
        std::fs::write(target.join("USER.txt"), b"preserve me\n").unwrap();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(target.join("USER.txt")).unwrap(),
            b"preserve me\n"
        );
        assert_eq!(summary.skipped.len(), 1);
        assert_eq!(summary.skipped[0].skill_id, "legacy-extra");
        assert_eq!(summary.skipped[0].path, target);
        assert_eq!(summary.skipped[0].reason, ProjectionSkipReason::UserOwned);
        assert!(read_manifest(&fixture).targets.is_empty());
    }

    #[tokio::test]
    async fn recorded_legacy_id_with_unmarked_bytes_is_preserved_and_retired() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_pool_skill("pool-old", "clean", "POOL BODY");
        let selected = fixture.resolve().unwrap();
        let rendered =
            Adapter::Codex.render_with_prefix(&selected[0].payload, fixture.launch_root(), "");
        std::fs::create_dir_all(rendered.path.parent().unwrap()).unwrap();
        std::fs::write(&rendered.path, b"OLD MATERIALIZATION\n").unwrap();
        append_materialization_record(
            fixture.repo_root(),
            &MaterializationRecord {
                recorded_at_epoch: 1,
                delegation_id: "legacy-delegation".into(),
                agent: "codex".into(),
                worktree: fixture.launch_root().display().to_string(),
                items: vec!["pool-old".into()],
            },
        )
        .unwrap();
        let legacy_target = rendered.path.parent().unwrap().to_path_buf();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(legacy_target.join("SKILL.md")).unwrap(),
            b"OLD MATERIALIZATION\n"
        );
        assert!(codex_target(&fixture, "pool-old").exists());
        assert_eq!(
            summary
                .skipped
                .iter()
                .find(|skip| skip.path == legacy_target)
                .map(|skip| skip.reason),
            Some(ProjectionSkipReason::UserOwned)
        );
        assert!(read_recent_materializations(fixture.repo_root(), 10)
            .iter()
            .all(|record| !record.items.iter().any(|item| item == "pool-old")));
    }

    #[tokio::test]
    async fn recorded_legacy_target_is_retired_after_pool_item_is_removed() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_pool_skill("retired-pool", "clean", "POOL BODY");
        let selected = fixture.resolve().unwrap();
        let rendered =
            Adapter::Codex.render_with_prefix(&selected[0].payload, fixture.launch_root(), "");
        let legacy_target = rendered.path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&legacy_target).unwrap();
        std::fs::write(&rendered.path, rendered.bytes).unwrap();
        append_materialization_record(
            fixture.repo_root(),
            &MaterializationRecord {
                recorded_at_epoch: 1,
                delegation_id: "legacy-delegation".into(),
                agent: "codex".into(),
                worktree: fixture.launch_root().display().to_string(),
                items: vec!["retired-pool".into()],
            },
        )
        .unwrap();
        let mut manifest = Manifest::load(fixture.repo_root()).unwrap();
        manifest.items.clear();
        manifest.save(fixture.repo_root()).unwrap();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert!(!legacy_target.exists());
        assert!(summary.migrated.contains(&legacy_target));
        assert!(read_recent_materializations(fixture.repo_root(), 10)
            .iter()
            .all(|record| !record.items.iter().any(|item| item == "retired-pool")));
    }

    #[tokio::test]
    async fn edited_copy_support_is_preserved_and_never_re_adopted() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("copy-assets", "both", "BODY");
        fixture.write_support("copy-assets", "scripts/check.sh", b"ORIGINAL\n");
        reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
            .await
            .unwrap();
        let target = codex_target(&fixture, "copy-assets");
        let supporting = target.join("scripts/check.sh");
        std::fs::write(&supporting, b"USER EDIT\n").unwrap();

        let second =
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
                .await
                .unwrap();
        let third =
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &AlwaysFailSymlink)
                .await
                .unwrap();

        assert_eq!(std::fs::read(&supporting).unwrap(), b"USER EDIT\n");
        assert_eq!(second.skipped[0].reason, ProjectionSkipReason::UserEdited);
        assert_eq!(third.skipped[0].reason, ProjectionSkipReason::UserOwned);
        assert!(second.migrated.is_empty());
        assert!(third.migrated.is_empty());
        assert!(read_manifest(&fixture).targets.is_empty());
    }

    #[tokio::test]
    async fn pending_journal_exists_before_the_first_target_mutation() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("journal-first", "both", "BODY");
        let linker = PendingObservedLinker {
            pending: projection_root(&fixture).join("pending.json"),
        };

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &linker)
            .await
            .unwrap();

        assert_eq!(summary.linked.len(), 1);
        assert!(!linker.pending.exists());
    }

    #[tokio::test]
    async fn switch_failure_rolls_back_prior_mutations() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("first", "both", "FIRST");
        fixture.write_bundled_skill("second", "both", "SECOND");

        let error = reconcile_with_linker(
            fixture.worktrees(),
            fixture.request(),
            &RemoveSecondSourceThenFail::new(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Reconcile);
        assert!(!codex_target(&fixture, "first").exists());
        assert!(!codex_target(&fixture, "second").exists());
        assert!(!projection_root(&fixture).join("pending.json").exists());
        assert!(!projection_root(&fixture).join("manifest.json").exists());
    }

    #[tokio::test]
    async fn failed_rollback_preserves_the_journal_and_concurrent_user_path() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("first", "both", "FIRST");
        fixture.write_bundled_skill("second", "both", "SECOND");
        let conflict = codex_target(&fixture, "second");
        let linker = ConflictThenFail {
            calls: AtomicUsize::new(0),
            conflict: conflict.clone(),
        };

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &linker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Reconcile);
        assert_eq!(
            std::fs::read(conflict.join("USER.txt")).unwrap(),
            b"preserve me\n"
        );
        assert!(projection_root(&fixture).join("pending.json").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interrupted_pending_transaction_is_recovered_before_fresh_reconcile() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("recover", "both", "BODY");
        let selected = fixture.resolve().unwrap();
        let generation = publish_generation(fixture.request(), &selected).unwrap();
        let next = manifest_for_generation(fixture.request(), &generation, ProjectionMode::Symlink);
        let record = next.targets[0].clone();
        let target = fixture.launch_root().join(&record.target_rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let source = generation.root.join(&record.generation_rel);
        let destination = relative_symlink_source(&source, &target).unwrap();
        NativeLinker
            .symlink(&destination, &target, TargetKind::Directory)
            .unwrap();
        let pending = PendingTransaction {
            schema_version: MANIFEST_SCHEMA_VERSION,
            prior: None,
            next: next.clone(),
            operations: vec![PendingOperation {
                target_rel: record.target_rel.clone(),
                prior_state: RecordedTargetState::Absent,
                next: Some(record),
                backup_rel: None,
            }],
        };
        write_atomic_json(&projection_root(&fixture).join("pending.json"), &pending).unwrap();

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(summary.generation, generation.digest);
        assert_eq!(summary.unchanged, vec![target]);
        assert_eq!(read_manifest(&fixture), next);
        assert!(!projection_root(&fixture).join("pending.json").exists());
    }

    #[tokio::test]
    async fn malformed_manifest_and_pending_state_are_fatal() {
        let manifest_fixture = ProjectionFixture::new(Adapter::Codex);
        manifest_fixture.write_bundled_skill("manifest", "both", "BODY");
        std::fs::create_dir_all(projection_root(&manifest_fixture)).unwrap();
        std::fs::write(
            projection_root(&manifest_fixture).join("manifest.json"),
            b"{ malformed",
        )
        .unwrap();
        let manifest_error = reconcile_with_linker(
            manifest_fixture.worktrees(),
            manifest_fixture.request(),
            &NativeLinker,
        )
        .await
        .unwrap_err();
        assert_eq!(manifest_error.phase, ProjectionPhase::Manifest);

        let pending_fixture = ProjectionFixture::new(Adapter::Codex);
        pending_fixture.write_bundled_skill("pending", "both", "BODY");
        std::fs::create_dir_all(projection_root(&pending_fixture)).unwrap();
        std::fs::write(
            projection_root(&pending_fixture).join("pending.json"),
            b"{ malformed",
        )
        .unwrap();
        let pending_error = reconcile_with_linker(
            pending_fixture.worktrees(),
            pending_fixture.request(),
            &NativeLinker,
        )
        .await
        .unwrap_err();
        assert_eq!(pending_error.phase, ProjectionPhase::Recover);
    }

    #[tokio::test]
    async fn pending_recovery_rejects_empty_operations_for_a_nonempty_next_manifest() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("empty-operations", "both", "BODY");
        let generation =
            publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
        let pending = PendingTransaction {
            schema_version: MANIFEST_SCHEMA_VERSION,
            prior: None,
            next: manifest_for_generation(fixture.request(), &generation, ProjectionMode::Symlink),
            operations: Vec::new(),
        };
        let pending_path = projection_root(&fixture).join("pending.json");
        write_atomic_json(&pending_path, &pending).unwrap();

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Recover);
        assert!(pending_path.is_file());
        assert!(!projection_root(&fixture).join("manifest.json").exists());
        assert!(!codex_target(&fixture, "empty-operations").exists());
    }

    #[tokio::test]
    async fn pending_recovery_rejects_an_omitted_next_target_operation() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("included", "both", "INCLUDED");
        fixture.write_bundled_skill("omitted", "both", "OMITTED");
        let generation =
            publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
        let next = manifest_for_generation(fixture.request(), &generation, ProjectionMode::Symlink);
        let included = next
            .targets
            .iter()
            .find(|target| target.skill_id == "included")
            .unwrap()
            .clone();
        let pending = PendingTransaction {
            schema_version: MANIFEST_SCHEMA_VERSION,
            prior: None,
            next,
            operations: vec![PendingOperation {
                target_rel: included.target_rel.clone(),
                prior_state: RecordedTargetState::Absent,
                next: Some(included),
                backup_rel: None,
            }],
        };
        let pending_path = projection_root(&fixture).join("pending.json");
        write_atomic_json(&pending_path, &pending).unwrap();

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Recover);
        assert!(pending_path.is_file());
        assert!(!projection_root(&fixture).join("manifest.json").exists());
        assert!(!codex_target(&fixture, "included").exists());
        assert!(!codex_target(&fixture, "omitted").exists());
    }

    #[test]
    fn pending_journal_rejects_an_operation_that_does_not_match_its_manifest() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("pending-shape", "both", "BODY");
        let generation =
            publish_generation(fixture.request(), &fixture.resolve().unwrap()).unwrap();
        let next = manifest_for_generation(fixture.request(), &generation, ProjectionMode::Symlink);
        let mut mismatched = next.targets[0].clone();
        mismatched.generation_rel = "skills/a-different-skill".to_string();
        let pending = PendingTransaction {
            schema_version: MANIFEST_SCHEMA_VERSION,
            prior: None,
            next,
            operations: vec![PendingOperation {
                target_rel: mismatched.target_rel.clone(),
                prior_state: RecordedTargetState::Absent,
                next: Some(mismatched),
                backup_rel: None,
            }],
        };

        assert!(crate::skills::projection::manifest::validate_pending(
            &pending,
            Adapter::Codex.key()
        )
        .is_err());

        let target = pending.next.targets[0].clone();
        let unrelated_backup = PendingTransaction {
            schema_version: MANIFEST_SCHEMA_VERSION,
            prior: None,
            next: pending.next.clone(),
            operations: vec![PendingOperation {
                target_rel: target.target_rel.clone(),
                prior_state: RecordedTargetState::Copy {
                    content_sha256: target.projected_sha256.clone(),
                },
                next: Some(target),
                backup_rel: Some("user-owned.txt".to_string()),
            }],
        };
        assert!(crate::skills::projection::manifest::validate_pending(
            &unrelated_backup,
            Adapter::Codex.key()
        )
        .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_pending_state_is_rejected_without_touching_external_path() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("pending-link", "both", "BODY");
        std::fs::create_dir_all(projection_root(&fixture)).unwrap();
        let external = tempfile::tempdir().unwrap();
        let external_pending = external.path().join("outside-pending.json");
        let pending = projection_root(&fixture).join("pending.json");
        std::os::unix::fs::symlink(&external_pending, &pending).unwrap();

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Recover);
        assert!(std::fs::symlink_metadata(&pending)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!external_pending.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_lock_state_is_rejected_without_creating_external_file() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("lock-link", "both", "BODY");
        std::fs::create_dir_all(projection_root(&fixture)).unwrap();
        let external = tempfile::tempdir().unwrap();
        let external_lock = external.path().join("outside.lock");
        let lock = projection_root(&fixture).join("reconcile.lock");
        std::os::unix::fs::symlink(&external_lock, &lock).unwrap();

        let error = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap_err();

        assert_eq!(error.phase, ProjectionPhase::Recover);
        assert!(!external_lock.exists());
    }

    #[test]
    fn projection_root_component_created_by_a_racing_process_is_reused() {
        let parent = tempfile::tempdir().unwrap();
        let component = parent.path().join("skill-projections");

        ensure_projection_directory_component_with(&component, |path| {
            std::fs::create_dir(path)?;
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "injected concurrent creator",
            ))
        })
        .unwrap();

        assert!(component.is_dir());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_reconciles_are_serialized_by_adapter_lock() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("locked", "both", "BODY");

        let (left, right) = tokio::join!(
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker),
            reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker),
        );
        let left = left.unwrap();
        let right = right.unwrap();

        assert_eq!(left.linked.len() + right.linked.len(), 1);
        assert_eq!(left.unchanged.len() + right.unchanged.len(), 1);
        assert!(!projection_root(&fixture).join("pending.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn garbage_collection_removes_only_unreferenced_generations() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("gc", "both", "OLD");
        let unrelated_exclude = ".claude/agents/spur-x.md".to_string();
        std::fs::create_dir_all(fixture.launch_root().join(".claude/agents")).unwrap();
        std::fs::write(fixture.launch_root().join(&unrelated_exclude), b"persona\n").unwrap();
        fixture
            .worktrees()
            .add_worktree_excludes(
                fixture.launch_root(),
                std::slice::from_ref(&unrelated_exclude),
            )
            .await
            .unwrap();
        let first = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();
        let old = generation_root(&fixture, &first.generation);
        fixture.write_bundled_skill("gc", "both", "NEW");
        let second = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();
        assert_ne!(first.generation, second.generation);
        assert!(!old.exists());

        let current = generation_root(&fixture, &second.generation);
        let target = codex_target(&fixture, "gc");
        std::fs::remove_file(&target).unwrap();
        let preserved_source = current.join("skills/gc/SKILL.md");
        NativeLinker
            .symlink(
                &relative_symlink_source(&preserved_source, &target).unwrap(),
                &target,
                TargetKind::File,
            )
            .unwrap();
        remove_source(&fixture, "gc");

        let summary = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_eq!(
            summary.skipped[0].reason,
            ProjectionSkipReason::OwnershipLost
        );
        assert!(target.exists());
        assert!(current.exists());

        fixture.write_bundled_skill("fresh", "both", "FRESH");
        let third = reconcile_with_linker(fixture.worktrees(), fixture.request(), &NativeLinker)
            .await
            .unwrap();

        assert_ne!(summary.generation, third.generation);
        assert!(current.exists());
        assert!(!generation_root(&fixture, &summary.generation).exists());
    }

    #[tokio::test]
    async fn public_entry_points_and_summary_display_use_locked_contracts() {
        let fixture = ProjectionFixture::new(Adapter::Codex);
        fixture.write_bundled_skill("public", "both", "BODY");

        let direct = reconcile(fixture.request()).await.unwrap();
        let display = direct.to_string();
        assert!(display.contains("linked=1"));
        assert!(display.contains("skipped=0"));

        let summaries = reconcile_many(
            fixture.repo_root(),
            fixture.launch_root(),
            &[Adapter::Cursor, Adapter::Codex],
        )
        .await
        .unwrap();
        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.adapter.as_str())
                .collect::<Vec<_>>(),
            vec![Adapter::Cursor.key(), Adapter::Codex.key()]
        );
        assert!(summaries.iter().all(|summary| !summary.selected.is_empty()));
    }

    fn codex_target(fixture: &ProjectionFixture, id: &str) -> PathBuf {
        fixture
            .launch_root()
            .join(format!(".codex/skills/spurpower-{id}"))
    }

    fn projection_root(fixture: &ProjectionFixture) -> PathBuf {
        fixture
            .launch_root()
            .join(".spur/runtime/skill-projections/codex")
    }

    fn generation_root(fixture: &ProjectionFixture, digest: &str) -> PathBuf {
        projection_root(fixture).join("generations").join(digest)
    }

    fn generation_target(fixture: &ProjectionFixture, digest: &str, id: &str) -> PathBuf {
        std::fs::canonicalize(generation_root(fixture, digest).join("skills").join(id)).unwrap()
    }

    fn read_manifest(fixture: &ProjectionFixture) -> ProjectionManifest {
        serde_json::from_slice(
            &std::fs::read(projection_root(fixture).join("manifest.json")).unwrap(),
        )
        .unwrap()
    }

    fn remove_source(fixture: &ProjectionFixture, id: &str) {
        let selected = fixture.resolve().unwrap();
        let source = selected
            .iter()
            .find(|skill| skill.payload.id == id)
            .unwrap()
            .source
            .source_dir
            .clone();
        std::fs::remove_dir_all(source).unwrap();
    }

    fn target_state_bytes(path: &Path) -> Vec<u8> {
        match std::fs::read_link(path) {
            Ok(destination) => destination.to_string_lossy().as_bytes().to_vec(),
            Err(_) => std::fs::read(path.join("SKILL.md")).unwrap(),
        }
    }

    fn git_status(root: &Path) -> String {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    fn manifest_for_generation(
        request: crate::skills::projection::ProjectionRequest<'_>,
        generation: &PublishedGeneration,
        mode: ProjectionMode,
    ) -> ProjectionManifest {
        ProjectionManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            renderer_schema_version: crate::skills::projection::generation::RENDERER_SCHEMA_VERSION,
            adapter: request.adapter.key().to_string(),
            role: RuntimeRole::Init,
            policy: SelectionPolicy::AllActive,
            generation: generation.digest.clone(),
            targets: generation
                .targets
                .iter()
                .map(|target| TargetRecord {
                    skill_id: target.skill_id.clone(),
                    source_kind: target.source.kind,
                    source_sha256: target.source.content_sha256.clone(),
                    target_rel: target.target_rel.clone(),
                    generation_rel: target.generation_rel.clone(),
                    mode,
                    projected_sha256: target.content_sha256.clone(),
                })
                .collect(),
        }
    }
}
