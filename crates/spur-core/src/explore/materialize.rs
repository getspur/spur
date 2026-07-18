use crate::explore::catalog::ItemKind;
use crate::explore::pool::{Manifest, ManifestItem};
use crate::skills::adapters::Adapter;
use crate::skills::{SkillPayload, SkillRole, SkillSource};
use anyhow::{bail, Context};
use fs4::fs_std::FileExt as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const WARN_TARGET: &str = "spur::worker::explore";
const MANAGED_MARKER: &str = "SPUR-MANAGED";
const MATERIALIZATION_ITEM_IDS: &str = "spur_item_ids";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationRecord {
    pub recorded_at_epoch: u64,
    pub delegation_id: String,
    pub agent: String,
    pub worktree: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LegacyMaterializationHint {
    item_id: Uuid,
    pub(crate) skill_id: String,
}

impl LegacyMaterializationHint {
    fn new(item_id: Uuid, skill_id: String) -> Self {
        Self { item_id, skill_id }
    }
}

#[derive(Debug)]
enum MaterializationLine {
    Record {
        original: String,
        ending: String,
        value: serde_json::Value,
        record: Box<MaterializationRecord>,
        item_ids: Option<Vec<Uuid>>,
        changed: bool,
    },
    Raw(String),
}

pub struct MaterializeMeta<'a> {
    pub request_id: &'a str,
    pub agent: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedPoolSkill {
    pub payload: SkillPayload,
    pub source_dir: PathBuf,
}

/// Render the gated pool subset into a worker worktree.
///
/// This is harness-native materialization: failures degrade to select-only
/// behavior for the worker and are reported through warnings instead of
/// returning errors to delegation setup.
pub async fn materialize_pool_skills(
    worktrees: &spur_worktree::manager::WorktreeManager,
    worktree_path: &Path,
    kind: spur_acp::types::AgentKind,
    repo_root: &Path,
    requested: Option<&[String]>,
    meta: Option<MaterializeMeta<'_>>,
) {
    let Some(adapter) = Adapter::for_agent_kind(kind) else {
        return;
    };
    let manifest = match Manifest::load_layered(repo_root) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                target: WARN_TARGET,
                error = %error,
                "explore manifest load failed; select-only"
            );
            return;
        }
    };
    let requested_names =
        requested.map(|names| names.iter().map(String::as_str).collect::<HashSet<_>>());
    let mut excludes = Vec::new();
    let mut written = Vec::new();
    let mut written_names = Vec::new();

    for item in &manifest.items {
        if !should_materialize(item, requested_names.as_ref()) {
            continue;
        }

        let loaded = match load_pool_skill(repo_root, item) {
            Ok(loaded) => loaded,
            Err(error) => {
                tracing::warn!(
                    target: WARN_TARGET,
                    skill = %item.name,
                    error = %error,
                    "pool skill load failed; select-only for skill"
                );
                continue;
            }
        };
        let rendered = adapter.render_with_prefix(&loaded.payload, worktree_path, "");
        let rel_path = match rendered.path.strip_prefix(worktree_path) {
            Ok(path) => path.to_string_lossy().replace('\\', "/"),
            Err(error) => {
                tracing::warn!(
                    target: WARN_TARGET,
                    skill = %item.name,
                    path = %rendered.path.display(),
                    error = %error,
                    "pool skill target path escaped worktree; select-only for skill"
                );
                continue;
            }
        };

        if !target_is_owned_or_absent(&rendered.path, &rel_path) {
            continue;
        }

        if let Err(error) = atomic_write(&rendered.path, &rendered.bytes) {
            tracing::warn!(
                target: WARN_TARGET,
                skill = %item.name,
                path = %rel_path,
                error = %error,
                "pool skill write failed; select-only for skill"
            );
            continue;
        }

        excludes.push(rel_path);
        written.push(rendered.path);
        written_names.push(item.name.clone());
    }

    if excludes.is_empty() {
        return;
    }

    if let Err(error) = worktrees
        .add_worktree_excludes(worktree_path, &excludes)
        .await
    {
        for path in written {
            let _ = std::fs::remove_file(path);
        }
        tracing::warn!(
            target: WARN_TARGET,
            paths = ?excludes,
            error = %error,
            "explore skill exclude setup failed; removed injected files, select-only"
        );
        return;
    }

    if let Some(meta) = meta.filter(|_| !written_names.is_empty()) {
        written_names.sort();
        let record = MaterializationRecord {
            recorded_at_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            delegation_id: meta.request_id.to_string(),
            agent: meta.agent.to_string(),
            worktree: worktree_path.display().to_string(),
            items: written_names,
        };
        if let Err(error) = append_materialization_record(repo_root, &record) {
            tracing::warn!(
                target: WARN_TARGET,
                error = %error,
                "materialization record write failed"
            );
        }
    }
}

pub fn append_materialization_record(
    repo_root: &Path,
    record: &MaterializationRecord,
) -> anyhow::Result<()> {
    let dir = repo_root.join(".spur/explore/cache");
    let _lock = lock_materialization_records(&dir)?;
    let path = dir.join("materializations.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut reserved = materialization_item_ids(&parse_materialization_lines(&raw));
    let item_ids = fresh_item_ids(record.items.len(), &mut reserved);
    let mut value = serde_json::to_value(record)?;
    set_item_ids(&mut value, &item_ids)?;
    let line = serde_json::to_string(&value)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn lock_materialization_records(cache_dir: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(cache_dir)?;
    let lock_path = cache_dir.join("materializations.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    lock.lock_exclusive()
        .with_context(|| format!("lock {}", lock_path.display()))?;
    Ok(lock)
}

pub fn read_recent_materializations(repo_root: &Path, limit: usize) -> Vec<MaterializationRecord> {
    let path = repo_root.join(".spur/explore/cache/materializations.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut records: Vec<MaterializationRecord> = raw
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    records.reverse();
    records.truncate(limit);
    records
}

/// Exact legacy pool materialization items observed for one worktree.
///
/// These records are migration hints only. Callers must independently prove
/// the exact on-disk target before adopting or removing it.
pub(crate) fn legacy_materialization_hints(
    repo_root: &Path,
    worktree: &Path,
) -> Vec<LegacyMaterializationHint> {
    let cache_dir = repo_root.join(".spur/explore/cache");
    let _lock = match lock_materialization_records(&cache_dir) {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(
                target: WARN_TARGET,
                error = %error,
                "materialization hint lock failed; preserving legacy records"
            );
            return Vec::new();
        }
    };
    let path = cache_dir.join("materializations.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(
                target: WARN_TARGET,
                path = %path.display(),
                error = %error,
                "materialization hint read failed; preserving legacy records"
            );
            return Vec::new();
        }
    };
    let mut lines = parse_materialization_lines(&raw);
    let counts = item_id_counts(&lines);
    let mut reserved = counts.keys().copied().collect::<HashSet<_>>();
    let mut upgraded = false;
    for line in &mut lines {
        let MaterializationLine::Record {
            value,
            record,
            item_ids,
            changed,
            ..
        } = line
        else {
            continue;
        };
        let needs_fresh_ids = item_ids.as_ref().is_none_or(|ids| {
            ids.iter()
                .any(|item_id| counts.get(item_id).copied() != Some(1))
        });
        if needs_fresh_ids {
            let ids = fresh_item_ids(record.items.len(), &mut reserved);
            if let Err(error) = set_item_ids(value, &ids) {
                tracing::warn!(
                    target: WARN_TARGET,
                    path = %path.display(),
                    error = %error,
                    "materialization hint upgrade failed; preserving legacy records"
                );
                return Vec::new();
            }
            *item_ids = Some(ids);
            *changed = true;
            upgraded = true;
        }
    }
    if upgraded {
        let output = render_materialization_lines(&lines);
        if let Err(error) = atomic_rewrite_materializations(&path, output.as_bytes()) {
            tracing::warn!(
                target: WARN_TARGET,
                path = %path.display(),
                error = %error,
                "materialization hint upgrade write failed; preserving legacy records"
            );
            return Vec::new();
        }
    }
    lines
        .into_iter()
        .filter_map(|line| match line {
            MaterializationLine::Record {
                record,
                item_ids: Some(item_ids),
                ..
            } if Path::new(&record.worktree) == worktree => Some(
                record
                    .items
                    .into_iter()
                    .zip(item_ids)
                    .map(|(skill_id, item_id)| LegacyMaterializationHint::new(item_id, skill_id))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Consume legacy materialization hints after one successful reconciliation.
pub(crate) fn retire_legacy_materializations(
    repo_root: &Path,
    hints: &[LegacyMaterializationHint],
) -> anyhow::Result<()> {
    if hints.is_empty() {
        return Ok(());
    }
    let cache_dir = repo_root.join(".spur/explore/cache");
    let _lock = lock_materialization_records(&cache_dir)?;
    let path = cache_dir.join("materializations.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let retired = hints.iter().cloned().collect::<HashSet<_>>();
    let mut lines = parse_materialization_lines(&raw);
    let counts = item_id_counts(&lines);
    let mut changed_any = false;
    for line in &mut lines {
        let MaterializationLine::Record {
            value,
            record,
            item_ids: Some(item_ids),
            changed,
            ..
        } = line
        else {
            continue;
        };
        let mut kept_items = Vec::with_capacity(record.items.len());
        let mut kept_ids = Vec::with_capacity(item_ids.len());
        for (skill_id, item_id) in record.items.iter().zip(item_ids.iter().copied()) {
            let hint = LegacyMaterializationHint::new(item_id, skill_id.clone());
            if counts.get(&item_id).copied() == Some(1) && retired.contains(&hint) {
                *changed = true;
                changed_any = true;
            } else {
                kept_items.push(skill_id.clone());
                kept_ids.push(item_id);
            }
        }
        if *changed {
            record.items = kept_items;
            *item_ids = kept_ids;
            set_record_items(value, &record.items)?;
            set_item_ids(value, item_ids)?;
        }
    }
    if changed_any {
        let output = render_materialization_lines(&lines);
        atomic_rewrite_materializations(&path, output.as_bytes())?;
    }
    Ok(())
}

fn parse_materialization_lines(raw: &str) -> Vec<MaterializationLine> {
    raw.split_inclusive('\n')
        .map(|original| {
            let (body, ending) = split_line_ending(original);
            let parsed = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| {
                    serde_json::from_value::<MaterializationRecord>(value.clone())
                        .ok()
                        .map(|record| (value, record))
                });
            match parsed {
                Some((value, record)) => {
                    let item_ids = parse_item_ids(&value, record.items.len());
                    MaterializationLine::Record {
                        original: original.to_string(),
                        ending: ending.to_string(),
                        value,
                        record: Box::new(record),
                        item_ids,
                        changed: false,
                    }
                }
                None => MaterializationLine::Raw(original.to_string()),
            }
        })
        .collect()
}

fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

fn parse_item_ids(value: &serde_json::Value, expected: usize) -> Option<Vec<Uuid>> {
    let values = value.get(MATERIALIZATION_ITEM_IDS)?.as_array()?;
    if values.len() != expected {
        return None;
    }
    values
        .iter()
        .map(|value| Uuid::parse_str(value.as_str()?).ok())
        .collect()
}

fn materialization_item_ids(lines: &[MaterializationLine]) -> HashSet<Uuid> {
    lines
        .iter()
        .filter_map(|line| match line {
            MaterializationLine::Record {
                item_ids: Some(item_ids),
                ..
            } => Some(item_ids.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn item_id_counts(lines: &[MaterializationLine]) -> HashMap<Uuid, usize> {
    let mut counts = HashMap::new();
    for item_id in materialization_item_ids(lines) {
        counts.insert(item_id, 0);
    }
    for line in lines {
        if let MaterializationLine::Record {
            item_ids: Some(item_ids),
            ..
        } = line
        {
            for item_id in item_ids {
                *counts.entry(*item_id).or_default() += 1;
            }
        }
    }
    counts
}

fn fresh_item_ids(count: usize, reserved: &mut HashSet<Uuid>) -> Vec<Uuid> {
    (0..count)
        .map(|_| loop {
            let candidate = Uuid::new_v4();
            if reserved.insert(candidate) {
                break candidate;
            }
        })
        .collect()
}

fn set_record_items(value: &mut serde_json::Value, items: &[String]) -> anyhow::Result<()> {
    let object = value
        .as_object_mut()
        .context("materialization record is not a JSON object")?;
    object.insert("items".to_string(), serde_json::to_value(items)?);
    Ok(())
}

fn set_item_ids(value: &mut serde_json::Value, item_ids: &[Uuid]) -> anyhow::Result<()> {
    let object = value
        .as_object_mut()
        .context("materialization record is not a JSON object")?;
    object.insert(
        MATERIALIZATION_ITEM_IDS.to_string(),
        serde_json::Value::Array(
            item_ids
                .iter()
                .map(|item_id| serde_json::Value::String(item_id.to_string()))
                .collect(),
        ),
    );
    Ok(())
}

fn render_materialization_lines(lines: &[MaterializationLine]) -> String {
    let mut output = String::new();
    for line in lines {
        match line {
            MaterializationLine::Record {
                original,
                ending,
                value,
                record,
                changed,
                ..
            } if *changed => {
                if !record.items.is_empty() {
                    output.push_str(
                        &serde_json::to_string(value)
                            .expect("materialization JSON value serialization is infallible"),
                    );
                    output.push_str(ending);
                }
            }
            MaterializationLine::Record { original, .. } | MaterializationLine::Raw(original) => {
                output.push_str(original);
            }
        }
    }
    output
}

fn atomic_rewrite_materializations(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "create temporary materialization cache in {}",
            parent.display()
        )
    })?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("write temporary materialization cache {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("sync temporary materialization cache {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist {}", path.display()))?;
    Ok(())
}

fn should_materialize(item: &ManifestItem, requested_names: Option<&HashSet<&str>>) -> bool {
    if item.kind != ItemKind::Skill {
        return false;
    }
    if !matches!(
        item.gate.verdict.as_str(),
        "clean" | "overridden" | "replaced-bundled"
    ) {
        return false;
    }
    match requested_names {
        Some(names) => names.contains(item.name.as_str()),
        None => true,
    }
}

pub(crate) fn load_pool_skill(
    repo_root: &Path,
    item: &ManifestItem,
) -> anyhow::Result<LoadedPoolSkill> {
    let source_dir = checked_pool_source_dir(repo_root, item)?;
    let path = source_dir.join("SKILL.md");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed = crate::skills::frontmatter::parse_source(&raw);
    Ok(LoadedPoolSkill {
        payload: SkillPayload {
            id: item.name.clone(),
            description: parsed.description.as_deref().unwrap_or("").to_string(),
            body: parsed.body.to_string(),
            source: SkillSource::Pool,
            role: parsed.role.unwrap_or(SkillRole::Both),
        },
        source_dir,
    })
}

fn checked_pool_source_dir(repo_root: &Path, item: &ManifestItem) -> anyhow::Result<PathBuf> {
    if item.source.split('/').any(|part| !safe_path_part(part))
        || !safe_path_part(&item.pinned_commit)
    {
        bail!(
            "unsafe pool source path for {}: source `{}` at `{}`",
            item.name,
            item.source,
            item.pinned_commit
        );
    }

    let source_dir = crate::explore::store::layered_pool_dir(
        repo_root,
        &item.source,
        &item.name,
        &item.pinned_commit,
    );
    let canonical_source = std::fs::canonicalize(&source_dir)
        .with_context(|| format!("resolve pool source {}", source_dir.display()))?;
    let mut pool_roots = vec![crate::explore::store::local_root(repo_root).join("pool")];
    if let Some(global_root) = crate::explore::store::global_root() {
        pool_roots.push(global_root.join("pool"));
    }
    if pool_roots.into_iter().any(|root| {
        std::fs::canonicalize(root)
            .map(|root| canonical_source.starts_with(root))
            .unwrap_or(false)
    }) {
        return Ok(canonical_source);
    }

    bail!(
        "unsafe pool source path for {}: {} is outside configured pool roots",
        item.name,
        source_dir.display()
    )
}

fn safe_path_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.contains('/')
        && !part.contains('\\')
        && !part.contains('\0')
}

fn target_is_owned_or_absent(target: &Path, rel_path: &str) -> bool {
    if !target.exists() {
        return true;
    }

    let existing = match std::fs::read_to_string(target) {
        Ok(existing) => existing,
        Err(error) => {
            tracing::warn!(
                target: WARN_TARGET,
                path = %rel_path,
                error = %error,
                "pool skill ownership check failed; select-only for skill"
            );
            return false;
        }
    };
    if existing.contains(MANAGED_MARKER) {
        return true;
    }

    tracing::warn!(
        target: WARN_TARGET,
        path = %rel_path,
        "committed agent skill file exists; select-only against it"
    );
    false
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file in {}", parent.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("write {}", tmp.path().display()))?;
    tmp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        append_materialization_record, legacy_materialization_hints, lock_materialization_records,
        materialize_pool_skills, read_recent_materializations, retire_legacy_materializations,
        MaterializationRecord, MaterializeMeta,
    };
    use crate::explore::catalog::ItemKind;
    use crate::explore::pool::{pool_dir, GateRecord, Manifest, ManifestItem};
    use spur_acp::types::AgentKind;
    use spur_worktree::manager::WorktreeManager;
    use std::path::Path;

    #[test]
    fn adapter_for_kind_maps_worker_kinds() {
        use spur_acp::types::AgentKind::*;

        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(ClaudeStreamJson),
            Some(crate::skills::adapters::Adapter::ClaudeCode)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(ClaudeCodeAcp),
            Some(crate::skills::adapters::Adapter::ClaudeCode)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(CodexAcp),
            Some(crate::skills::adapters::Adapter::Codex)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(Gemini),
            Some(crate::skills::adapters::Adapter::Gemini)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(Kiro),
            Some(crate::skills::adapters::Adapter::Kiro)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(OpenCode),
            Some(crate::skills::adapters::Adapter::OpenCode)
        );
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(Kimi),
            Some(crate::skills::adapters::Adapter::Kimi)
        );
        assert_eq!(crate::skills::adapters::Adapter::for_agent_kind(Grok), None);
        assert_eq!(
            crate::skills::adapters::Adapter::for_agent_kind(Generic),
            None
        );
    }

    #[tokio::test]
    async fn materialize_writes_subset_and_registers_excludes() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());

        let manifest = Manifest {
            sources: Vec::new(),
            items: vec![
                write_pool_skill(repo.path(), "clean-a", "clean"),
                write_pool_skill(repo.path(), "clean-b", "clean"),
                write_pool_skill(repo.path(), "reviewed", "overridden"),
                write_pool_skill(repo.path(), "blocked", "blocked"),
            ],
        };
        manifest.save(repo.path()).unwrap();

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        materialize_pool_skills(
            &manager,
            worktree.path(),
            AgentKind::CodexAcp,
            repo.path(),
            None,
            None,
        )
        .await;

        for name in ["clean-a", "clean-b", "reviewed"] {
            let path = worktree
                .path()
                .join(".codex/skills")
                .join(name)
                .join("SKILL.md");
            let contents = std::fs::read_to_string(&path).unwrap();
            assert!(contents.contains("SPUR-MANAGED"), "{name} lacks marker");
            assert!(contents.contains(&format!("name: {name}")));
        }
        assert!(!worktree
            .path()
            .join(".codex/skills/blocked/SKILL.md")
            .exists());

        let status = git_status(worktree.path());
        assert!(
            !status.contains(".codex/skills"),
            "rendered skills must be excluded from status: {status}"
        );
    }

    #[test]
    fn empty_repo_inherits_global_pool_skills_during_materialization() {
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());
        let home = tempfile::tempdir().unwrap();
        let global_store = home.path().join(".spur/explore");
        let _global_root =
            crate::explore::store::force_global_root_for_tests(Some(global_store.clone()));
        let item = write_pool_skill_in_store(&global_store, "global-clean", "clean");
        Manifest {
            sources: Vec::new(),
            items: vec![item.clone()],
        }
        .save_to_store(&global_store)
        .unwrap();
        write_catalog_in_store(&global_store, &item);

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            materialize_pool_skills(
                &manager,
                worktree.path(),
                AgentKind::CodexAcp,
                repo.path(),
                None,
                None,
            )
            .await;
        });

        let rendered = worktree.path().join(".codex/skills/global-clean/SKILL.md");
        let contents = std::fs::read_to_string(rendered).unwrap();
        assert!(contents.contains("SPUR-MANAGED"));
        assert!(contents.contains("name: global-clean"));
        assert!(!repo.path().join(".spur/explore").exists());
        assert!(!repo.path().join(".spur/explore.toml").exists());
    }

    #[tokio::test]
    async fn materialize_requested_subset_and_committed_file_precedence() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());

        let manifest = Manifest {
            sources: Vec::new(),
            items: vec![
                write_pool_skill(repo.path(), "clean-a", "clean"),
                write_pool_skill(repo.path(), "clean-b", "clean"),
            ],
        };
        manifest.save(repo.path()).unwrap();

        let existing = worktree.path().join(".codex/skills/clean-a/SKILL.md");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "user owned\n").unwrap();
        commit_all(worktree.path(), "user-owned skill");

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        let requested = vec!["clean-a".to_string()];
        materialize_pool_skills(
            &manager,
            worktree.path(),
            AgentKind::CodexAcp,
            repo.path(),
            Some(&requested),
            None,
        )
        .await;

        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "user owned\n");
        assert!(!worktree
            .path()
            .join(".codex/skills/clean-b/SKILL.md")
            .exists());
        assert!(
            !git_status(worktree.path()).contains(".codex/skills/clean-a/SKILL.md"),
            "committed user file should remain clean"
        );
    }

    #[tokio::test]
    async fn materialize_appends_record_readable_by_reader() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());

        let manifest = Manifest {
            sources: Vec::new(),
            items: vec![
                write_pool_skill(repo.path(), "clean-a", "clean"),
                write_pool_skill(repo.path(), "clean-b", "clean"),
                write_pool_skill(repo.path(), "reviewed", "overridden"),
                write_pool_skill(repo.path(), "blocked", "blocked"),
            ],
        };
        manifest.save(repo.path()).unwrap();

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        materialize_pool_skills(
            &manager,
            worktree.path(),
            AgentKind::CodexAcp,
            repo.path(),
            None,
            Some(MaterializeMeta {
                request_id: "del-42",
                agent: "codex",
            }),
        )
        .await;

        let records = read_recent_materializations(repo.path(), 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].delegation_id, "del-42");
        assert_eq!(records[0].agent, "codex");
        assert_eq!(records[0].worktree, worktree.path().display().to_string());
        assert_eq!(
            records[0].items,
            vec![
                "clean-a".to_string(),
                "clean-b".to_string(),
                "reviewed".to_string()
            ]
        );
        assert!(records[0].recorded_at_epoch > 0);
    }

    #[test]
    fn append_and_retire_share_the_materialization_record_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let repo = tempfile::tempdir().unwrap();
        let worktree = repo.path().join("worker");
        let record = |delegation: &str, item: &str| MaterializationRecord {
            recorded_at_epoch: 1,
            delegation_id: delegation.into(),
            agent: "codex".into(),
            worktree: worktree.display().to_string(),
            items: vec![item.into()],
        };
        append_materialization_record(repo.path(), &record("old", "shared-skill")).unwrap();
        let observed = legacy_materialization_hints(repo.path(), &worktree);

        let cache = repo.path().join(".spur/explore/cache");
        let guard = lock_materialization_records(&cache).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let repo_path = repo.path().to_path_buf();
        let appended = record("new", "shared-skill");
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(append_materialization_record(&repo_path, &appended))
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result_rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();

        let guard = lock_materialization_records(&cache).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let repo_path = repo.path().to_path_buf();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(retire_legacy_materializations(&repo_path, &observed))
                .unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result_rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();

        let records = read_recent_materializations(repo.path(), 10);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].delegation_id, "new");
        assert_eq!(records[0].items, vec!["shared-skill"]);
    }

    #[test]
    fn retirement_preserves_a_later_byte_identical_record_occurrence() {
        let repo = tempfile::tempdir().unwrap();
        let worktree = repo.path().join("worker");
        let record = MaterializationRecord {
            recorded_at_epoch: 1,
            delegation_id: "duplicate".into(),
            agent: "codex".into(),
            worktree: worktree.display().to_string(),
            items: vec!["shared-skill".into()],
        };
        let cache = repo.path().join(".spur/explore/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let line = serde_json::to_string(&record).unwrap();
        std::fs::write(
            cache.join("materializations.jsonl"),
            format!("{line}\n{line}\n"),
        )
        .unwrap();
        let observed = legacy_materialization_hints(repo.path(), &worktree);

        assert_eq!(observed.len(), 2);
        retire_legacy_materializations(repo.path(), &observed[..1]).unwrap();

        let records = read_recent_materializations(repo.path(), 10);
        assert_eq!(records, vec![record]);
    }

    #[test]
    fn retirement_distinguishes_same_metadata_records_by_full_occurrence() {
        let repo = tempfile::tempdir().unwrap();
        let worktree = repo.path().join("worker");
        let record = |items: &[&str]| MaterializationRecord {
            recorded_at_epoch: 1,
            delegation_id: "same-metadata".into(),
            agent: "codex".into(),
            worktree: worktree.display().to_string(),
            items: items.iter().map(|item| (*item).to_string()).collect(),
        };
        let first = record(&["shared-skill", "first-only"]);
        let second = record(&["shared-skill", "second-only"]);
        let cache = repo.path().join(".spur/explore/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("materializations.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&second).unwrap()
            ),
        )
        .unwrap();
        let observed = legacy_materialization_hints(repo.path(), &worktree);
        let first_shared = observed
            .iter()
            .find(|hint| hint.skill_id == "shared-skill")
            .unwrap()
            .clone();

        retire_legacy_materializations(repo.path(), &[first_shared]).unwrap();

        let records = read_recent_materializations(repo.path(), 10);
        assert_eq!(
            records,
            vec![
                record(&["shared-skill", "second-only"]),
                record(&["first-only"]),
            ]
        );
    }

    #[tokio::test]
    async fn materialize_without_meta_or_items_writes_no_record() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());

        let manifest = Manifest {
            sources: Vec::new(),
            items: vec![write_pool_skill(repo.path(), "clean-a", "clean")],
        };
        manifest.save(repo.path()).unwrap();

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        materialize_pool_skills(
            &manager,
            worktree.path(),
            AgentKind::CodexAcp,
            repo.path(),
            None,
            None,
        )
        .await;
        assert!(read_recent_materializations(repo.path(), 10).is_empty());

        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let worktree = tempfile::tempdir().unwrap();
        init_git_repo(worktree.path());
        let manifest = Manifest {
            sources: Vec::new(),
            items: vec![write_pool_skill(repo.path(), "blocked-skill", "blocked")],
        };
        manifest.save(repo.path()).unwrap();

        let manager = WorktreeManager::new(worktree.path().to_path_buf());
        materialize_pool_skills(
            &manager,
            worktree.path(),
            AgentKind::CodexAcp,
            repo.path(),
            None,
            Some(MaterializeMeta {
                request_id: "del-43",
                agent: "codex",
            }),
        )
        .await;
        assert!(read_recent_materializations(repo.path(), 10).is_empty());
    }

    #[test]
    fn read_recent_returns_newest_first_and_respects_limit() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5 {
            append_materialization_record(
                root.path(),
                &MaterializationRecord {
                    recorded_at_epoch: 100 + i,
                    delegation_id: format!("d{i}"),
                    agent: "codex".into(),
                    worktree: "/w".into(),
                    items: vec!["s".into()],
                },
            )
            .unwrap();
        }

        let records = read_recent_materializations(root.path(), 3);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].delegation_id, "d4");
        assert_eq!(records[1].delegation_id, "d3");
        assert_eq!(records[2].delegation_id, "d2");
    }

    fn write_pool_skill(root: &Path, name: &str, verdict: &str) -> ManifestItem {
        let source = "acme/skills";
        let pinned_commit = "abcdef1234567890abcdef1234567890abcdef12";
        let dir = pool_dir(root, source, name, pinned_commit);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\nUse {name}.\n"),
        )
        .unwrap();
        let content_sha256 = crate::explore::content_hash(&dir).unwrap();

        ManifestItem {
            name: name.to_string(),
            kind: ItemKind::Skill,
            source: source.to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: pinned_commit.to_string(),
            content_sha256,
            license: None,
            gate: GateRecord {
                verdict: verdict.to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        }
    }

    fn write_pool_skill_in_store(store_root: &Path, name: &str, verdict: &str) -> ManifestItem {
        let source = "acme/skills";
        let pinned_commit = "abcdef1234567890abcdef1234567890abcdef12";
        let dir = store_root
            .join("pool")
            .join("acme")
            .join(format!("{name}@abcdef1"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\nUse {name}.\n"),
        )
        .unwrap();
        let content_sha256 = crate::explore::content_hash(&dir).unwrap();

        ManifestItem {
            name: name.to_string(),
            kind: ItemKind::Skill,
            source: source.to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: pinned_commit.to_string(),
            content_sha256,
            license: None,
            gate: GateRecord {
                verdict: verdict.to_string(),
                justification: None,
                decided_at_epoch: None,
            },
        }
    }

    fn write_catalog_in_store(store_root: &Path, item: &ManifestItem) {
        let catalog = crate::explore::catalog::Catalog {
            synced_at_epoch: Some(1),
            entries: vec![crate::explore::catalog::CatalogEntry {
                kind: item.kind,
                name: item.name.clone(),
                source: item.source.clone(),
                rel_path: item.rel_path.clone(),
                pinned_commit: item.pinned_commit.clone(),
                description: format!("{} skill", item.name),
                license: item.license.clone(),
                content_sha256: item.content_sha256.clone(),
            }],
        };
        let dir = store_root.join("index");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("catalog.json"),
            serde_json::to_string_pretty(&catalog).unwrap(),
        )
        .unwrap();
    }

    fn init_git_repo(path: &Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.email", "test@example.com"]);
        run_git(path, &["config", "user.name", "Test User"]);
    }

    fn commit_all(path: &Path, message: &str) {
        run_git(path, &["add", "."]);
        run_git(path, &["commit", "-m", message]);
    }

    fn git_status(path: &Path) -> String {
        git_output(path, &["status", "--porcelain"])
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}
