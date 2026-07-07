#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::pool::{Manifest, SourceSpec};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn sync_clones_pinned_source_and_builds_catalog() {
        let td = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        init_fixture_repo(fixture.path(), "api-design");
        let head = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
        let manifest = Manifest {
            sources: vec![SourceSpec {
                repo: "acme/repo".to_string(),
                url: Some(fixture.path().display().to_string()),
                pin: head.clone(),
            }],
            items: vec![],
        };

        let catalog = sync(td.path(), &manifest).unwrap();

        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].pinned_commit, head);
        assert_eq!(catalog.entries[0].source, "acme/repo");
        assert!(td
            .path()
            .join(".spur/explore/cache/acme-repo/.git")
            .exists());
        assert!(td.path().join(".spur/explore/index/catalog.json").exists());
        assert_eq!(sync(td.path(), &manifest).unwrap().entries.len(), 1);
    }

    #[test]
    fn sync_failure_names_source_and_leaves_existing_index_untouched() {
        let td = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        init_fixture_repo(fixture.path(), "api-design");
        let good_head = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
        let existing = crate::explore::catalog::Catalog {
            synced_at_epoch: Some(1),
            entries: vec![],
        };
        existing.save(td.path()).unwrap();
        let before = std::fs::read_to_string(td.path().join(".spur/explore/index/catalog.json"))
            .unwrap();
        let manifest = Manifest {
            sources: vec![
                SourceSpec {
                    repo: "acme/repo".to_string(),
                    url: Some(fixture.path().display().to_string()),
                    pin: good_head,
                },
                SourceSpec {
                    repo: "bad/source".to_string(),
                    url: Some(fixture.path().display().to_string()),
                    pin: "missing-ref".to_string(),
                },
            ],
            items: vec![],
        };

        let error = sync(td.path(), &manifest).unwrap_err();

        assert!(format!("{error:#}").contains("bad/source"));
        let after = std::fs::read_to_string(td.path().join(".spur/explore/index/catalog.json"))
            .unwrap();
        assert_eq!(after, before);
    }

    fn init_fixture_repo(repo: &Path, skill_name: &str) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "test@spur"]);
        git(repo, &["config", "user.name", "Test User"]);
        write_fixture_skill(repo, skill_name);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "fixture"]);
    }

    fn write_fixture_skill(repo: &Path, skill_name: &str) {
        let skill_dir = repo.join("skills").join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: Fixture skill\n---\nbody\n"),
        )
        .unwrap();
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}
