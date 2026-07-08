use crate::explore::catalog::ItemKind;
use crate::explore::pool::{pool_dir, Manifest, ManifestItem};
use crate::skills::adapters::Adapter;
use crate::skills::{SkillPayload, SkillRole, SkillSource};
use anyhow::Context;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

const WARN_TARGET: &str = "spur::worker::explore";
const MANAGED_MARKER: &str = "SPUR-MANAGED";

pub fn adapter_for_kind(kind: spur_acp::types::AgentKind) -> Option<Adapter> {
    match kind {
        spur_acp::types::AgentKind::ClaudeCodeAcp
        | spur_acp::types::AgentKind::ClaudeStreamJson => Some(Adapter::ClaudeCode),
        spur_acp::types::AgentKind::CodexAcp => Some(Adapter::Codex),
        spur_acp::types::AgentKind::Gemini => Some(Adapter::Gemini),
        spur_acp::types::AgentKind::Kiro => Some(Adapter::Kiro),
        spur_acp::types::AgentKind::OpenCode => Some(Adapter::OpenCode),
        spur_acp::types::AgentKind::Kimi => Some(Adapter::Kimi),
        spur_acp::types::AgentKind::Generic => None,
    }
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
) {
    let Some(adapter) = adapter_for_kind(kind) else {
        return;
    };
    let manifest = match Manifest::load(repo_root) {
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

    for item in &manifest.items {
        if !should_materialize(item, requested_names.as_ref()) {
            continue;
        }

        let payload = match load_pool_skill(repo_root, item) {
            Ok(payload) => payload,
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
        let rendered = adapter.render_with_prefix(&payload, worktree_path, "");
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
    }
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

fn load_pool_skill(repo_root: &Path, item: &ManifestItem) -> anyhow::Result<SkillPayload> {
    let path = pool_dir(repo_root, &item.source, &item.name, &item.pinned_commit).join("SKILL.md");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed = crate::skills::frontmatter::parse_source(&raw);
    Ok(SkillPayload {
        id: item.name.clone(),
        description: parsed.description.as_deref().unwrap_or("").to_string(),
        body: parsed.body.to_string(),
        source: SkillSource::Override,
        role: parsed.role.unwrap_or(SkillRole::Both),
    })
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
    use super::{adapter_for_kind, materialize_pool_skills};
    use crate::explore::catalog::ItemKind;
    use crate::explore::pool::{pool_dir, GateRecord, Manifest, ManifestItem};
    use spur_acp::types::AgentKind;
    use spur_worktree::manager::WorktreeManager;
    use std::path::Path;

    #[test]
    fn adapter_for_kind_maps_worker_kinds() {
        use spur_acp::types::AgentKind::*;

        assert_eq!(
            adapter_for_kind(ClaudeStreamJson),
            Some(crate::skills::adapters::Adapter::ClaudeCode)
        );
        assert_eq!(
            adapter_for_kind(ClaudeCodeAcp),
            Some(crate::skills::adapters::Adapter::ClaudeCode)
        );
        assert_eq!(
            adapter_for_kind(CodexAcp),
            Some(crate::skills::adapters::Adapter::Codex)
        );
        assert_eq!(
            adapter_for_kind(Gemini),
            Some(crate::skills::adapters::Adapter::Gemini)
        );
        assert_eq!(
            adapter_for_kind(Kiro),
            Some(crate::skills::adapters::Adapter::Kiro)
        );
        assert_eq!(
            adapter_for_kind(OpenCode),
            Some(crate::skills::adapters::Adapter::OpenCode)
        );
        assert_eq!(
            adapter_for_kind(Kimi),
            Some(crate::skills::adapters::Adapter::Kimi)
        );
        assert_eq!(adapter_for_kind(Generic), None);
    }

    #[tokio::test]
    async fn materialize_writes_subset_and_registers_excludes() {
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

    #[tokio::test]
    async fn materialize_requested_subset_and_committed_file_precedence() {
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
