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
